use alloy::{primitives::Address, providers::Provider, transports::utils::Spawnable};
use eyre::{eyre, Result};
use governor::{DefaultDirectRateLimiter, Quota};
use std::{num::NonZeroU32, sync::Arc, time::Duration};
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{debug, info_span, trace, warn, Instrument};

use crate::actor::nonce::{NonceHandle, TxFailContext};

/// Error encountered while processing a send request.
#[derive(Error, Debug)]
pub enum InuRateError {
    #[error("tx is missing nonce")]
    NonceMissing,
    #[error("from is missing from tx")]
    FromMissing,
    #[error("gas price missing from tx")]
    GasPriceMissing,
    #[error("network signer does not have key for {0} address")]
    SignerMissing(Address),
}

/// Configuration for sending a transaction via rate controller
pub struct SendConfig {
    /// Address of the sender, just for tracing purpose
    pub from: Address,
    /// encoded signed tx to send
    pub encoded_tx: Vec<u8>,
    /// gas price of the tx, just for tracing purpose
    pub gas_price: u128,
    /// nonce handle of the tx nonce, to free the nonce in case of error
    pub nonce_handle: NonceHandle,
    /// timeout for the tx
    pub timeout: Duration,
}

/// Handle to send a transaction via rate controller
#[derive(Debug, Clone)]
pub struct RateControllerHandle {
    tx: mpsc::Sender<SendConfig>,
}

impl RateControllerHandle {
    /// Send a transaction to the rate controller for processing
    ///
    /// The call will wait if the rate controller's channel is full
    /// and return only when the config is successfully sent to the rate controller
    pub async fn send_tx(&self, config: SendConfig) -> Result<(), SendConfig> {
        match self.tx.send(config).await {
            Ok(_) => Ok(()),
            Err(e) => Err(e.0),
        }
    }
}

/// A simple governor based rate controller for sending transactions
///
/// The rate controller will send transactions at a rate of `max_tps` transactions per second
/// and handle the tx for enitre lifecycle, freeing nonce if tx fails
pub struct RateController<P> {
    provider: Arc<P>,
    max_tps: NonZeroU32,
}

impl<P> RateController<P> {
    /// Create a new rate controller with the given provider
    /// with default TPS of 1
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
    /// Handle a new send request by spwaning a new tokio task,
    /// It does not wait for the task to complete
    fn handle_new_request(&self, config: SendConfig) {
        let SendConfig {
            encoded_tx,
            gas_price,
            nonce_handle,
            timeout,
            from,
        } = config;

        let nonce = nonce_handle.get();
        let span = info_span!(
            "handle_new_request",
            from = from.to_string(),
            nonce = nonce,
            gas_price = gas_price
        );

        // send rpc request and await for reciept
        // TODO: make this deterministically sequential, otherwise rpc requests with higher nonce might
        // be sent before the lower nonce
        let provider_clone = self.provider.clone();
        let fut = async move {
            // broadcast the tx via provider
            match provider_clone.send_raw_transaction(&encoded_tx).await {
                // if succeeded, wait for the receipt
                Ok(pending) => {
                    match pending.with_timeout(Some(timeout)).get_receipt().await {
                        // tx was successful
                        Ok(receipt) => {
                            trace!("tx succesful: {}", receipt.transaction_hash);
                        }
                        // waiting for reciept failed, it can be due to timeout
                        // free the nonce and mark it as timeout
                        Err(error) => {
                            warn!("nonce freed with timeout, error waiting tx: {:?}", error);
                            nonce_handle
                                .failed(TxFailContext {
                                    gas_price,
                                    error,
                                    might_be_timeout: true,
                                })
                                .free()
                                .await;
                        }
                    }
                }
                // rpc call failed, free the nonce without marking as timeout
                Err(error) => {
                    warn!("nonce freed, error sending tx: {:?}", error);
                    nonce_handle
                        .failed(TxFailContext {
                            gas_price,
                            might_be_timeout: false,
                            error,
                        })
                        .free()
                        .await;
                }
            };
        };
        fut.instrument(span).spawn_task();
    }

    async fn into_future(self, mut ixns: mpsc::Receiver<SendConfig>) {
        let limiter = DefaultDirectRateLimiter::direct(Quota::per_second(self.max_tps));
        debug!("loop started");
        while let Some(config) = ixns.recv().await {
            // wait until permitted
            limiter.until_ready().await;
            self.handle_new_request(config);
        }
        debug!("channel closed");
    }

    pub fn spawn(self) -> RateControllerHandle {
        // channel size is min(TPS, 64) to handle back pressure
        //      For TPS < 64, channel size is TPS
        //      For TPS => 64, channel size is 64
        // TODO: revist channel size, maybe make it configurable
        let (ix_tx, ixns) = mpsc::channel(self.max_tps.get().min(64) as usize);
        let span = info_span!("rate_controller", max_tps = %self.max_tps);

        self.into_future(ixns).instrument(span).spawn_task();

        RateControllerHandle { tx: ix_tx }
    }
}
