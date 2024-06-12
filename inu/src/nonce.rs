use alloy::transports::TransportError;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::debug;

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

    async fn next_nonce(&self) -> NonceVal {
        let mut free_lock = self.free_nonces.lock().await;
        // debug!("freed nonces count: {}", free_lock.len());
        let free_nonce = free_lock.pop_first();
        drop(free_lock);

        if let Some(nonce) = free_nonce {
            // debug!("got freed nonce: {}", nonce.get());
            nonce
        } else {
            let next_nonce = self.next_nonce.fetch_add(1, Ordering::Relaxed);
            // debug!("got new nonce: {}", next_nonce);
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

    pub async fn freed_count(&self) -> usize {
        self.inner().free_nonces.lock().await.len()
    }

    pub fn head(&self) -> u64 {
        self.inner().head()
    }

    pub async fn pop_freed(&self) -> Option<NonceHandle> {
        let mut free_lock = self.inner().free_nonces.lock().await;
        free_lock.pop_first().map(|val| NonceHandle {
            val,
            manager: self.inner().clone(),
        })
    }
}

#[cfg(test)]
mod test {
    use alloy::transports::TransportErrorKind;

    use super::*;

    #[tokio::test]
    async fn basic() {
        let manager = NonceManager::new(0);
        // get 1
        let nonce1 = manager.next_nonce().await;
        // get 2
        let nonce2 = manager.next_nonce().await;
        // get 3
        let nonce3 = manager.next_nonce().await;

        let task1 = tokio::spawn(async move {
            // free 1
            nonce1.free().await;
            // free 2 with error
            nonce2
                .failed(TxFailContext {
                    gas_price: 100,
                    might_be_timeout: false,
                    error: TransportErrorKind::backend_gone(),
                })
                .free()
                .await;
        });

        let task2 = tokio::spawn(async move {
            // free 3
            nonce3.free().await;
        });

        let manager_clone = manager.clone();
        let task3 = tokio::spawn(async move {
            let _ = manager_clone.next_nonce().await;
            let _ = manager_clone.next_nonce().await;
        });

        tokio::try_join!(task1, task2, task3).unwrap();

        let nonce4 = manager.next_nonce().await;
        let nonce5 = manager.next_nonce().await;

        assert_eq!(nonce4.val.get(), 3);
        assert_eq!(nonce5.val.get(), 4);
    }
}
