use alloy::{
    eips::eip2718::Encodable2718,
    network::{Ethereum, EthereumWallet, NetworkWallet, TransactionBuilder},
    primitives::{
        utils::{format_units, parse_ether},
        Address, U256,
    },
    providers::{
        fillers::{ChainIdFiller, FillProvider, GasFiller, JoinFill, NonceFiller, WalletFiller},
        Provider, RootProvider,
    },
    rpc::{
        json_rpc::{ErrorPayload, RpcError},
        types::eth::{TransactionReceipt, TransactionRequest},
    },
    signers::local::{coins_bip39::English, MnemonicBuilder},
    transports::{BoxTransport, TransportError, TransportErrorKind},
};
use eyre::{eyre, Result};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::{sync, task::JoinSet, time};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, info_span, instrument, Instrument, Level};

use crate::{
    actor::{
        error::TxRpcError,
        nonce::{NonceHandle, NonceManager, TxFailContext},
    },
    builder::TxBuilder,
    gas_oracle::GasPriceChannel,
    rate::{RateControllerHandle, SendConfig},
};

pub mod error;
pub mod nonce;

// const MAX_TPS_PER_ACTOR: u64 = 50;
// const ETH_DUST_TO_IGNORE: u128 = 1_000_000_000_000_000; // 0.001 ETH

pub type RecommendedProvider = FillProvider<
    JoinFill<JoinFill<JoinFill<alloy::providers::Identity, GasFiller>, NonceFiller>, ChainIdFiller>,
    RootProvider<BoxTransport>,
    BoxTransport,
    Ethereum,
>;

pub type RecommendedProviderWithSigner = FillProvider<
    JoinFill<
        JoinFill<
            JoinFill<JoinFill<alloy::providers::Identity, GasFiller>, NonceFiller>,
            ChainIdFiller,
        >,
        WalletFiller<EthereumWallet>,
    >,
    RootProvider<BoxTransport>,
    BoxTransport,
    Ethereum,
>;

#[derive(Debug, Error)]
pub enum ActorError {
    /// Low funds
    #[error("Not enough funds on actor {} with current balance {}, rpc response - {}", .actor_address, .balance, .resp)]
    NotSufficentFunds {
        resp: ErrorPayload,
        actor_address: Address,
        balance: U256,
    },
    #[error("Unhandled rpc error - {0}")]
    UnhandledRpcError(ErrorPayload),
    /// Any unhandled transport error that is not potentially tx timeout
    #[error(transparent)]
    UnhandledTransportError(#[from] TransportError),
    /// Any other generic error propagated from inside actor
    #[error(transparent)]
    OtherError(#[from] eyre::Report),
}

pub struct Actor<P, S> {
    address: Address,
    chain_id: u64,
    nonce_manager: NonceManager,
    gas_oracle: GasPriceChannel,
    gas_multiplier: f64,
    provider: Arc<P>,
    signer: Arc<S>,
    rate_controller: RateControllerHandle,
}

impl<P, S> Actor<P, S> {
    fn apply_gas_multiplier(&self, gas_price: u128) -> u128 {
        (gas_price as f64 * self.gas_multiplier) as u128
    }

    fn get_gas_price(&self) -> u128 {
        self.apply_gas_multiplier(*self.gas_oracle.borrow())
    }
}

impl<P: Provider<BoxTransport>, S: NetworkWallet<Ethereum>> Actor<P, S> {
    pub async fn new(
        address: Address,
        gas_oracle: GasPriceChannel,
        gas_multiplier: f64,
        provider: Arc<P>,
        signer: Arc<S>,
        rate_controller: RateControllerHandle,
    ) -> Result<Self> {
        let nonce = provider.get_transaction_count(address).await?;
        let chain_id = provider.get_chain_id().await?;
        let nonce_manager = NonceManager::new(nonce);
        Ok(Self {
            address,
            chain_id,
            nonce_manager,
            gas_oracle,
            gas_multiplier,
            provider,
            signer,
            rate_controller,
        })
    }

    #[instrument(skip_all, fields(nonce), err(level = Level::ERROR))]
    async fn apply_nonce(
        &self,
        mut transaction: TransactionRequest,
    ) -> Result<Option<(TransactionRequest, NonceHandle, u128)>, ActorError> {
        let (nonce_handle, failed_context) = self
            .nonce_manager
            .next_nonce()
            .await
            // reset the nonce handle to new, and take the failed context if any
            .consume_failed();
        let nonce = nonce_handle.get();
        let mut gas_price = self.get_gas_price();

        tracing::Span::current().record("nonce", nonce);

        if let Some(TxFailContext {
            gas_price: old_gas_price,
            might_be_timeout,
            error,
        }) = failed_context
        {
            match error {
                RpcError::ErrorResp(resp) => {
                    // TODO: find and handle other cases
                    match resp.clone().into() {
                        TxRpcError::NonceTooLow
                        | TxRpcError::AlreadyImported
                        | TxRpcError::AlreadyKnown => {
                            // nonce is alredy used, skip it and continue
                            // TODO: handle this better
                            debug!("skipping as nonce already imported");
                            return Ok(None);
                        }
                        TxRpcError::Underpriced => {
                            gas_price = self.apply_gas_multiplier(gas_price.max(old_gas_price));
                            debug!(
                                "underpriced, gas price increased to {} from {}",
                                gas_price, old_gas_price
                            );
                        }
                        TxRpcError::InsufficientFunds => {
                            return Err(ActorError::NotSufficentFunds {
                                resp,
                                actor_address: self.address,
                                balance: self.provider.get_balance(self.address).await?,
                            });
                        }
                        TxRpcError::Other { .. } => {
                            // TODO: we return error for now to identify cases that are not handled
                            //       once we cover enough, this should be removed
                            return Err(ActorError::UnhandledRpcError(resp));
                        }
                    }
                }
                // tricky to handle this case since pending tx timeout also return this error
                // we rely on the `might_be_timeout` flag to differentiate
                RpcError::Transport(TransportErrorKind::BackendGone) if might_be_timeout => {
                    // nonce might be stucked, increase gas price
                    // use the max of the increased old gas price and the new gas price
                    gas_price = self.apply_gas_multiplier(gas_price.max(old_gas_price));
                    debug!(
                        "stucked?, gas price increased to {} from {}",
                        gas_price, old_gas_price
                    );
                }
                e => {
                    // for any other error rpc error we just return
                    return Err(e.into());
                }
            }
        }

        transaction = transaction.with_nonce(nonce).with_gas_price(gas_price);
        Ok(Some((transaction, nonce_handle, gas_price)))
    }

    #[instrument(skip_all, fields(nonce), err(level = Level::ERROR))]
    pub async fn send_tx(&self, timeout: Duration) -> Result<(), ActorError> {
        let tx = TxBuilder::build(self.address).with_chain_id(self.chain_id);
        if let Some((ready_tx, nonce_handle, gas_price)) = self.apply_nonce(tx).await? {
            tracing::Span::current().record("nonce", nonce_handle.get());

            let encoded_tx = ready_tx
                .build(&*self.signer)
                .await
                .map_err(|e| ActorError::OtherError(e.into()))?
                .encoded_2718();

            let config = SendConfig {
                encoded_tx,
                gas_price,
                nonce_handle,
                timeout,
                from: self.address,
            };

            if let Err(c) = self.rate_controller.send_tx(config).await {
                c.nonce_handle.free().await;
                return Err(eyre!("rate controller dropped! failed to sent tx").into());
            }
        }

        Ok(())
    }

    #[instrument(skip_all)]
    async fn wait_for_pending_tx(&self) -> Result<()> {
        loop {
            let offchain_nonce = self.nonce_manager.head().saturating_sub(1);
            let onchain_nonce = self.provider.get_transaction_count(self.address).await?;
            debug!(
                "offchain nonce: {}, onchain nonce: {}",
                offchain_nonce, onchain_nonce
            );
            if offchain_nonce == onchain_nonce {
                break;
            }
            // sleep for 1 sec to not spam the rpc
            time::sleep(Duration::from_secs(1)).await;
        }

        Ok(())
    }

    #[instrument(skip_all, fields(timeout=humantime::format_duration(pending_timeout).to_string()), err(level = Level::ERROR))]
    pub async fn cleanup_nonce(&self, pending_timeout: Duration) -> Result<()> {
        // sleep till nonce timout gets freed in nonce manager
        // after this
        // WIP: to be completed
        debug!("waiting for pending tx to clear till timeout...",);

        let res = time::timeout(pending_timeout, self.wait_for_pending_tx()).await;
        if let Ok(Ok(_)) = &res {
            // if onchain & offchain nonce matched within timeout then we are sure nonces are good!
            return Ok(());
        }

        // if we are here then it means account nonce might be stuck
        // Strategy: we pop the freed/errored nonces and re-send them untill we reach the
        // nonce that maybe not be stuck (not timeout and not underpriced error)
        // WIP: to be completed

        Ok(())
    }

    // send all the funds to a EOA account
    // NOTE: should not be used with contract address otheriwse gas estimation will be wrong
    #[instrument(skip_all, err(level = Level::ERROR))]
    pub async fn send_all_funds(
        &self,
        to: Address,
        tx_timeout: Duration,
    ) -> Result<TransactionReceipt> {
        let code = self.provider.get_code_at(to).await?;
        if !code.is_empty() {
            return Err(eyre!("cannot send funds to contract address"));
        }

        let nonce = self.provider.get_transaction_count(self.address).await?;
        let gas_price = self.get_gas_price();
        let gas_limit = 21_000;
        let balance = self.provider.get_balance(self.address).await?;
        // TODO: take op l1 cost into account
        // for now just subtract 0.001 eth
        // let cost = gas_price * gas_limit;
        let cost = parse_ether("0.001")?;
        let transferrable = balance.saturating_sub(U256::from(cost));
        debug!(
            "balance: {}, transferrable: {}, cost: {}, nonce: {}, gas_price: {}",
            balance, transferrable, cost, nonce, gas_price
        );
        if transferrable.is_zero() {
            return Err(eyre!("not enough funds to send"));
        }

        let tx = TransactionRequest::default()
            .from(self.address)
            .to(to)
            .value(U256::from(transferrable))
            .with_gas_price(gas_price)
            .with_gas_limit(gas_limit)
            .with_nonce(nonce)
            .with_chain_id(self.chain_id)
            .build(&*self.signer)
            .await?;

        let reciept = self
            .provider
            .send_raw_transaction(&tx.encoded_2718())
            .await?
            .with_timeout(Some(tx_timeout))
            .get_receipt()
            .await
            .map_err(|e| {
                if matches!(e, RpcError::Transport(TransportErrorKind::BackendGone)) {
                    eyre!("tx timeout")
                } else {
                    e.into()
                }
            })?;

        Ok(reciept)
    }
}

pub struct ActorManager {
    actors: Vec<Arc<Actor<RecommendedProviderWithSigner, EthereumWallet>>>,
    provider: Arc<RecommendedProviderWithSigner>,
    master_address: Address,
    tasks: JoinSet<()>,
    actor_cancel_token: CancellationToken,
    actor_error_notify: Arc<sync::Notify>,
}

impl ActorManager {
    #[instrument(name = "manager::new", skip(phrase, provider, rate_handle, gas_oracle))]
    pub async fn new(
        phrase: &str,
        max_tps: u32,
        tps_per_actor: u32,
        provider: RecommendedProvider,
        rate_handle: RateControllerHandle,
        gas_multiplier: f64,
        gas_oracle: GasPriceChannel,
    ) -> Result<Self> {
        let num_actors = estimate_actors_count(max_tps, tps_per_actor);
        // first account is master account and only used to topup other actors
        let (signer, master_address, actor_addresses) = build_signer(phrase, num_actors).await?;
        let provider = Arc::new(provider.join_with(WalletFiller::new(signer.clone())));
        let signer: Arc<EthereumWallet> = Arc::new(signer);

        info!(
            "master address: {}, actors: {:?}",
            master_address, actor_addresses
        );
        //sanity check
        assert_eq!(num_actors, actor_addresses.len() as u32);

        // initalize all the actors
        let actors: Vec<_> = futures::future::try_join_all(
            actor_addresses
                .into_iter()
                .map(|address| {
                    let gas_oracle = gas_oracle.clone();
                    let provider = provider.clone();
                    let signer = signer.clone();
                    let rate_handle = rate_handle.clone();
                    async move {
                        Actor::new(
                            address,
                            gas_oracle,
                            gas_multiplier,
                            provider,
                            signer,
                            rate_handle,
                        )
                        .await
                    }
                })
                .collect::<Vec<_>>(),
        )
        .await?
        .into_iter()
        .map(Arc::new)
        .collect();

        let tasks = JoinSet::new();
        let actor_cancel_token = CancellationToken::new();
        let actor_error_notify = Arc::new(sync::Notify::new());
        let manager = ActorManager {
            actors,
            tasks,
            actor_cancel_token,
            actor_error_notify,
            master_address,
            provider,
        };

        Ok(manager)
    }

    /// topup all the actors with some eth
    #[instrument(skip(self), err)]
    pub async fn fund_actors(&self) -> Result<()> {
        let master_balance = self.provider.get_balance(self.master_address).await?;
        // we need to send funds to all actors while retaining equal part in master account
        let amount: U256 = master_balance.div_ceil(U256::from(self.actors.len() + 1));
        info!(
            "master balance: {} eth, sending {} eth to each actor from master",
            format_units(master_balance, "eth")?,
            format_units(amount, "eth")?
        );

        let mut futs = vec![];
        for actor in self.actors.iter() {
            let tx = TransactionRequest::default()
                .from(self.master_address)
                .to(actor.address)
                .value(amount);
            futs.push(self.provider.send_transaction(tx).await?.get_receipt());
        }

        for (i, fut) in futs.into_iter().enumerate() {
            let reciept = fut.await?;
            debug!(
                "sent {} eth to actor {}, tx hash - {}",
                format_units(amount, "eth")?,
                self.actors[i].address,
                reciept.transaction_hash
            );
        }
        debug!("actors topped up successfully!");
        Ok(())
    }

    pub fn spawn_actors(&mut self, tx_timeout: Duration) {
        for actor in self.actors.iter() {
            let cancel_token = self.actor_cancel_token.clone();
            let actor = actor.clone();
            let error_notify = self.actor_error_notify.clone();
            // spawn loop to send tx

            let span = info_span!("actor_task", actor = actor.address.to_string());
            let fut = async move {
                info!("spawned");
                'shutdown: loop {
                    // biased select to prioritize tx send over shutdown signal
                    // to prevent nonce race conditions
                    tokio::select! {
                        biased;
                        res = actor.send_tx(tx_timeout) => {
                            // if error, notify error and break the loop
                            if let Err(e) = res {
                                error!("sending shutdown signal, error in tx sending {}", e);
                                error_notify.notify_one();
                                break 'shutdown;
                            }
                        }
                        _ = cancel_token.cancelled() => {
                            info!("shutdown signal recieved, cleaning up..");
                            let _ = actor.cleanup_nonce(tx_timeout).await;
                            break 'shutdown;
                        }
                    }
                }
            };
            self.tasks.spawn(fut.instrument(span));
        }
    }

    pub async fn wait_for_error(&self) {
        self.actor_error_notify.notified().await;
    }

    pub async fn shutdown(&mut self, tx_timeout: Duration) {
        // cancel all the tasks
        self.actor_cancel_token.cancel();
        // wait for tasks to shutdown
        while self.tasks.join_next().await.is_some() {}
        // attempt to send back all the funds to master account
        self.attempt_to_send_funds_back(tx_timeout).await;
    }

    pub async fn attempt_to_send_funds_back(&self, tx_timeout: Duration) {
        let handles = self.actors.iter().map(|actor| {
            let actor = actor.clone();
            let master_address = self.master_address;
            let span = info_span!("funds_back", actor = actor.address.to_string());
            let fut = async move {
                if let Ok(reciept) = actor.send_all_funds(master_address, tx_timeout).await {
                    info!("success, tx hash - {}", reciept.transaction_hash);
                }
            };

            fut.instrument(span)
        });

        futures::future::join_all(handles).await;
    }
}

///
/// Utlilty functions
///

async fn build_signer(
    phrase: &str,
    num_actors: u32,
) -> Result<(EthereumWallet, Address, Vec<Address>)> {
    let master = MnemonicBuilder::<English>::default()
        .phrase(phrase)
        .index(0)
        .unwrap()
        .build()?;
    let master_address = master.address();
    let mut actor_address = vec![];
    let mut signer = EthereumWallet::new(master);

    for i in 1..(num_actors + 1) {
        let wallet = MnemonicBuilder::<English>::default()
            .phrase(phrase)
            .index(i)
            .unwrap()
            .build()?;
        actor_address.push(wallet.address());
        signer.register_signer(wallet);
    }

    Ok((signer, master_address, actor_address))
}

#[instrument(ret(level = Level::DEBUG))]
fn estimate_actors_count(max_tps: u32, tps_per_actor: u32) -> u32 {
    (max_tps as f64 / tps_per_actor as f64).ceil() as u32
}
