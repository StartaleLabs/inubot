use alloy::{
    consensus::TxEnvelope,
    network::{EthereumSigner, TransactionBuilder},
    primitives::Address,
    providers::Provider,
    signers::wallet::{coins_bip39::English, MnemonicBuilder},
    transports::Transport,
};
use eyre::Result;
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::{mpsc, watch, Mutex, Notify},
    task, time,
};

use crate::builder::TxBuilder;

#[derive(Debug, Clone)]
pub struct TxSendRequest {
    pub transaction: TxEnvelope,
    pub reset_notify: Arc<Notify>,
}

#[derive(Clone, Debug)]
pub struct Actor<T: Transport + Clone, P: Provider<T> + 'static> {
    pub inner: Arc<ActorInner<T, P>>,
}

impl<T: Transport + Clone, P: Provider<T> + 'static> Actor<T, P> {
    pub async fn new(
        phrase: &str,
        index: u32,
        provider: &Arc<P>,
        sender: mpsc::Sender<TxSendRequest>,
    ) -> Result<Actor<T, P>> {
        let inner = Arc::new(ActorInner::new(phrase, index, provider, sender).await?);
        Ok(Actor { inner })
    }

    pub async fn start(&self) {
        let inner1 = self.inner.clone();
        tokio::spawn(async move {
            loop {
                inner1.send_tx().await.unwrap();
            }
        });

        let inner2 = self.inner.clone();
        tokio::spawn(async move {
            inner2.listen_reset().await;
        });
    }
}

#[derive(Clone, Debug)]
pub struct ActorInner<T: Transport + Clone, P: Provider<T>> {
    pub address: Address,
    pub signer: EthereumSigner,
    pub nonce: Arc<Mutex<u64>>,
    pub provider: Arc<P>,
    pub chain_id: u64,
    sender: mpsc::Sender<TxSendRequest>,
    reset_notify: Arc<Notify>,
    _phantom: std::marker::PhantomData<T>,
}

impl<T: Transport + Clone, P: Provider<T>> ActorInner<T, P> {
    pub async fn new(
        phrase: &str,
        index: u32,
        provider: &Arc<P>,
        sender: mpsc::Sender<TxSendRequest>,
    ) -> Result<ActorInner<T, P>> {
        let wallet = MnemonicBuilder::<English>::default()
            .phrase(phrase)
            .index(index)?
            .build()?;
        println!("Actor address: {}", wallet.address(),);
        let address = wallet.address();
        let signer = wallet.into();
        let nonce = Arc::new(Mutex::new(provider.get_transaction_count(address).await?));
        let chain_id = provider.get_chain_id().await?;

        Ok(ActorInner {
            address,
            signer,
            nonce,
            chain_id,
            provider: provider.clone(),
            sender,
            reset_notify: Arc::new(Notify::new()),
            _phantom: std::marker::PhantomData,
        })
    }

    /// TODO: take pending/in-flight transactions in account
    pub async fn reset_nonce(&self) -> Result<()> {
        println!("Resetting nonce");
        let latest_nonce = self.provider.get_transaction_count(self.address).await?;
        *self.nonce.lock().await = latest_nonce;
        Ok(())
    }

    pub async fn next_tx(&self) -> Result<TxEnvelope> {
        let mut nonce_lock = self.nonce.lock().await;
        let nonce = *nonce_lock;
        *nonce_lock += 1;
        drop(nonce_lock);

        let res = Ok(TxBuilder::build(self.address)
            .with_nonce(nonce)
            .with_chain_id(self.chain_id)
            .with_gas_limit(21_000)
            .with_max_priority_fee_per_gas(1_000_000_000)
            .with_max_fee_per_gas(20_000_000_000)
            .build(&self.signer)
            .await?);
        res
    }

    pub async fn send_tx(&self) -> Result<()> {
        let rlp_tx = self.next_tx().await?;

        self.sender
            .send(TxSendRequest {
                transaction: rlp_tx,
                reset_notify: self.reset_notify.clone(),
            })
            .await?;
        Ok(())
    }

    pub async fn listen_reset(&self) {
        let future = self.reset_notify.notified();
        tokio::pin!(future);

        loop {
            future.as_mut().await;
            // attempt to reset nonce
            let _ = self.reset_nonce().await;
            time::sleep(Duration::from_secs(2)).await;
        }
    }
}
