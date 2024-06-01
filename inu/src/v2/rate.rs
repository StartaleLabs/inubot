use alloy::{
    primitives::Address,
    providers::{PendingTransactionBuilder, Provider},
    transports::utils::Spawnable,
};
use eyre::{eyre, Result};
use governor::{DefaultDirectRateLimiter, Quota};
use std::{num::NonZeroU32, sync::Arc, time::Duration};
use thiserror::Error;
use tokio::{sync::mpsc, task};
use tracing::debug;

use crate::v2::nonce::{FailContext, NonceHandle};

#[derive(Error, Debug)]
pub enum InuError {
    #[error("tx is missing nonce")]
    NonceMissing,
    #[error("from is missing from tx")]
    FromMissing,
    #[error("gas price missing from tx")]
    GasPriceMissing,
    #[error("network signer does not have key for {0} address")]
    SignerMissing(Address),
}
pub struct SendConfig {
    pub encoded_tx: Vec<u8>,
    pub gas_price: u128,
    pub nonce_handle: NonceHandle,
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct RateControllerHandle {
    tx: mpsc::Sender<SendConfig>,
}

impl RateControllerHandle {
    pub async fn send_tx(&self, config: SendConfig) -> Result<(), SendConfig> {
        match self.tx.send(config).await {
            Ok(_) => Ok(()),
            Err(e) => Err(e.0),
        }
    }
}

pub struct RateController<P> {
    provider: Arc<P>,
    max_tps: NonZeroU32,
}

impl<P> RateController<P> {
    pub fn new(provider: Arc<P>) -> Self {
        Self {
            provider,
            // 1 TPS Default
            max_tps: NonZeroU32::MIN,
        }
    }

    pub fn with_tps(mut self, max_tps: u32) -> Result<Self> {
        self.max_tps = NonZeroU32::new(max_tps).ok_or(eyre!("TPS must be greater than 0"))?;
        Ok(self)
    }
}

impl<P: Provider + 'static> RateController<P> {
    async fn handle_new_request(&self, config: SendConfig) {
        let SendConfig {
            encoded_tx,
            gas_price,
            nonce_handle,
            timeout,
        } = config;

        let nonce = nonce_handle.get();

        // send rpc request to get hash
        // TODO: make this non-awaiting, right now we wait till server send the request back which is slow
        //       for each request.
        //       Solution: make batch requests
        let provider_clone = self.provider.clone();
        match provider_clone.send_raw_transaction(&encoded_tx).await {
            // if succeeded, spawn a task to wait for receipt
            Ok(pending) => {
                let hash = pending.tx_hash().clone();
                task::spawn(async move {
                    let pending = PendingTransactionBuilder::new(provider_clone.root(), hash);
                    // free the nonce if error, assuming tx is stuck
                    // TODO: better error handling
                    if let Err(error) = pending.with_timeout(Some(timeout)).get_receipt().await {
                        debug!("nonce {} freed! error waiting tx: {:?}", nonce, error);
                        nonce_handle
                            .failed(FailContext {
                                gas_price,
                                error,
                                might_be_timeout: true,
                            })
                            .free()
                            .await;
                    };
                });
            }
            // rpc call failed, free the nonce without marking as timeout
            Err(error) => {
                debug!("nonce {} freed! error sending tx: {:?}", nonce, error);
                nonce_handle
                    .failed(FailContext {
                        gas_price,
                        might_be_timeout: false,
                        error,
                    })
                    .free()
                    .await;
            }
        };
    }

    async fn into_future(self, mut ixns: mpsc::Receiver<SendConfig>) {
        'shutdown: loop {
            let limiter = DefaultDirectRateLimiter::direct(Quota::per_second(self.max_tps));
            loop {
                // wait until permitted
                limiter.until_ready().await;

                // fetch next send request
                let Some(config) = ixns.recv().await else {
                    debug!("channel closed");
                    break 'shutdown;
                };

                self.handle_new_request(config).await;
            }
        }
    }

    pub fn spawn(self) -> RateControllerHandle {
        let (ix_tx, ixns) = mpsc::channel(1);

        self.into_future(ixns).spawn_task();

        RateControllerHandle { tx: ix_tx }
    }
}
