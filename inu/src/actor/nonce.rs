use alloy::transports::TransportError;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{instrument, trace};

/// Context for a failed transaction
#[derive(Debug)]
pub struct TxFailContext {
    pub gas_price: u128,
    pub might_be_timeout: bool,
    pub error: TransportError,
}

#[derive(Debug)]
enum NonceVal {
    Failed { val: u64, context: TxFailContext },
    New { val: u64 },
}

impl NonceVal {
    pub fn get(&self) -> u64 {
        match self {
            NonceVal::Failed { val, .. } => *val,
            NonceVal::New { val } => *val,
        }
    }
}

impl Eq for NonceVal {}
impl PartialEq for NonceVal {
    fn eq(&self, other: &Self) -> bool {
        self.get() == other.get()
    }
}

impl PartialOrd for NonceVal {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.get().cmp(&other.get()))
    }
}

impl Ord for NonceVal {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.get().cmp(&other.get())
    }
}

#[derive(Debug)]
pub struct NonceManagerInner {
    next_nonce: AtomicU64,
    free_nonces: Mutex<BTreeSet<NonceVal>>,
}

impl NonceManagerInner {
    fn new(initial_nonce: u64) -> Self {
        Self {
            next_nonce: AtomicU64::new(initial_nonce),
            free_nonces: Mutex::new(BTreeSet::new()),
        }
    }

    #[instrument(skip(self))]
    async fn next_nonce(&self) -> NonceVal {
        let mut free_lock = self.free_nonces.lock().await;
        let free_nonce = free_lock.pop_first();
        drop(free_lock);

        if let Some(nonce) = free_nonce {
            trace!("got freed nonce: {}", nonce.get());
            nonce
        } else {
            let next_nonce = self.next_nonce.fetch_add(1, Ordering::Relaxed);
            trace!("got new nonce: {}", next_nonce);
            NonceVal::New { val: next_nonce }
        }
    }

    async fn free_nonce(&self, val: NonceVal) {
        let mut free_lock = self.free_nonces.lock().await;
        free_lock.insert(val);
    }

    fn head(&self) -> u64 {
        self.next_nonce.load(Ordering::Relaxed)
    }
}

pub struct NonceHandle {
    val: NonceVal,
    manager: Arc<NonceManagerInner>,
}

impl NonceHandle {
    pub fn failed(mut self, context: TxFailContext) -> Self {
        self.val = NonceVal::Failed {
            val: self.val.get(),
            context,
        };
        self
    }

    pub fn consume_failed(self) -> (Self, Option<TxFailContext>) {
        match self.val {
            NonceVal::Failed { val, context } => (
                NonceHandle {
                    val: NonceVal::New { val },
                    manager: self.manager.clone(),
                },
                Some(context),
            ),
            _ => (self, None),
        }
    }

    pub async fn free(self) {
        self.manager.free_nonce(self.val).await;
    }

    pub fn get(&self) -> u64 {
        self.val.get()
    }
}

/// Nonce manager which keeps tracks of failed nonces and provides lowest
/// available nonce
///
/// Based on the ideas from here - https://ethereum.stackexchange.com/a/40286
///
/// The NonceManager is responsible for managing the allocation and deallocation
/// of nonces, which are used to ensure proper nonce management. It maintains a set
/// of free nonces and provides the next available nonce when requested. If a transaction
/// fails, the failed nonce can be freed with error info and can be reused in the future.
///
/// The NonceManager consists of two main components:
/// - `NonceManagerInner`: This struct represents the inner state of the Nonce Manager and contains the logic for allocating and freeing nonces.
/// - `NonceHandle`: This struct represents a handle to a nonce and provides methods for marking a nonce as failed and freeing a nonce.
///
/// Example usage:
/// ```
/// use inu::actor::nonce::{NonceManager, TxFailContext};
/// use alloy::transports::TransportErrorKind;
/// use tokio::task::spawn;
///
/// #[tokio::main]
/// async fn main() {
///     let manager = NonceManager::new(0);
///     let nonce1 = manager.next_nonce().await;
///     let nonce2 = manager.next_nonce().await;
///     
///     // Mark nonce2 as failed
///     nonce2.failed(TxFailContext {
///         gas_price: 100,
///         might_be_timeout: false,
///         error: TransportErrorKind::backend_gone(),
///     }).free().await;
///     
///     // Free nonce1
///     nonce1.free().await;
///     
///     // Get the next available nonce
///     let nonce3 = manager.next_nonce().await;
///     
///     println!("Nonce 3: {}", nonce3.get());
/// }
/// ```
#[derive(Debug, Clone)]
pub struct NonceManager(Arc<NonceManagerInner>);
impl NonceManager {
    pub fn new(initial_nonce: u64) -> Self {
        Self(Arc::new(NonceManagerInner::new(initial_nonce)))
    }

    fn inner(&self) -> &Arc<NonceManagerInner> {
        &self.0
    }

    pub async fn next_nonce(&self) -> NonceHandle {
        let nonce_val = self.inner().next_nonce().await;
        NonceHandle {
            val: nonce_val,
            manager: self.inner().clone(),
        }
    }

    pub fn head(&self) -> u64 {
        self.inner().head()
    }
}

#[cfg(test)]
mod test {
    use alloy::transports::TransportErrorKind;

    use super::*;

    #[tokio::test]
    async fn basic() {
        let manager = NonceManager::new(0);
        // get 0
        let nonce0 = manager.next_nonce().await;
        // get 1
        let nonce1 = manager.next_nonce().await;
        // get 2
        let nonce2 = manager.next_nonce().await;

        let task1 = tokio::spawn(async move {
            // free 0
            nonce0.free().await;
            // free 1 with error
            nonce1
                .failed(TxFailContext {
                    gas_price: 100,
                    might_be_timeout: false,
                    error: TransportErrorKind::backend_gone(),
                })
                .free()
                .await;
        });

        let task2 = tokio::spawn(async move {
            // free 2
            nonce2.free().await;
        });

        let manager_clone = manager.clone();
        let task3 = tokio::spawn(async move {
            // get 0
            let _ = manager_clone.next_nonce().await;
            // get 1
            let _ = manager_clone.next_nonce().await;
        });

        tokio::try_join!(task1, task2, task3).unwrap();

        // get 2
        let nonce2 = manager.next_nonce().await;
        // get 3
        let nonce3 = manager.next_nonce().await;

        assert_eq!(nonce2.val.get(), 2);
        assert_eq!(nonce3.val.get(), 3);
    }
}
