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
    signers::{
        k256::ecdsa::SigningKey,
        local::{coins_bip39::English, LocalSigner, MnemonicBuilder},
    },
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
    builder::TransactionRandomizer,
    cli::GlobalOptions,
    gas_oracle::GasPriceChannel,
    rate::{RateControllerHandle, SendConfig},
};

pub mod error;
pub mod nonce;

/// A Provider with all the necessary fillers for sending transactions except the wallet
pub type RecommendedProvider = FillProvider<
    JoinFill<JoinFill<JoinFill<alloy::providers::Identity, GasFiller>, NonceFiller>, ChainIdFiller>,
    RootProvider<BoxTransport>,
    BoxTransport,
    Ethereum,
>;

/// A Provider with all the necessary fillers for sending transactions
pub type RecommendedProviderWithWallet = FillProvider<
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
pub enum InuActorError {
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

/// Actor is entity controlling a single address and managing it's nonce
pub struct Actor<P, S> {
    /// Address of the actor
    address: Address,
    chain_id: u64,
    /// Nonce manager
    nonce_manager: NonceManager,
    gas_oracle: GasPriceChannel,
    /// gas multiplier to apply on gas price
    gas_multiplier: f64,
    provider: Arc<P>,
    signer: Arc<S>,
    rate_controller: RateControllerHandle,
    tx_builder: Arc<TransactionRandomizer>,
}

impl<P, S> Actor<P, S> {
    fn apply_gas_multiplier(&self, gas_price: u128) -> u128 {
        (gas_price as f64 * self.gas_multiplier) as u128
    }

    /// Get the gas price with the applied multiplier
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
        tx_builder: Arc<TransactionRandomizer>,
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
            tx_builder,
        })
    }

    /// Fetch the next nonce & gas and apply it to to the transaction
    ///
    /// Handle all the cases where nonce is errored, might be stucked or gas price needs to be increased
    #[instrument(skip_all, fields(nonce), err(level = Level::ERROR))]
    async fn apply_nonce(
        &self,
        mut transaction: TransactionRequest,
    ) -> Result<Option<(TransactionRequest, NonceHandle, u128)>, InuActorError> {
        let (nonce_handle, failed_context) = self
            .nonce_manager
            .next_nonce()
            .await
            // reset the nonce handle to new, and take the failed context if any
            .consume_failed();
        let nonce = nonce_handle.get();
        let mut gas_price = self.get_gas_price();

        tracing::Span::current().record("nonce", nonce);

        // if nonce is failed, handle the error
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
                            return Err(InuActorError::NotSufficentFunds {
                                resp,
                                actor_address: self.address,
                                balance: self.provider.get_balance(self.address).await?,
                            });
                        }
                        TxRpcError::Other { .. } => {
                            // TODO: we return error for now to identify cases that are not handled
                            //       once we cover enough, this should be removed
                            return Err(InuActorError::UnhandledRpcError(resp));
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

    /// Send a new random transaction with the given timeout to rate controller
    #[instrument(skip_all, fields(nonce), err(level = Level::ERROR))]
    pub async fn send_tx(&self, timeout: Duration) -> Result<(), InuActorError> {
        // fetch the random transaction
        let tx = self
            .tx_builder
            .random(self.address)
            .with_chain_id(self.chain_id);

        // apply the nonce and gas price
        if let Some((ready_tx, nonce_handle, gas_price)) = self.apply_nonce(tx).await? {
            tracing::Span::current().record("nonce", nonce_handle.get());

            // encode the signed tx
            let encoded_tx = ready_tx
                .build(&*self.signer)
                .await
                .map_err(|e| InuActorError::OtherError(e.into()))?
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

    /// Wait for offchain nonce to match onchain nonce.
    /// This is used to wait for pending tx to clear
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
        debug!("waiting for pending tx to clear till timeout...",);
        let _ = time::timeout(pending_timeout, self.wait_for_pending_tx()).await;
        Ok(())
    }

    /// send all the funds to a EOA account
    ///
    /// NOTE: should not be used with contract address otheriwse gas estimation will be wrong
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

/// A Manager to orchestrate multiple actors and their funds
pub struct ActorManager {
    actors: Vec<Arc<Actor<RecommendedProviderWithWallet, EthereumWallet>>>,
    provider: Arc<RecommendedProviderWithWallet>,
    master_address: Address,
    tasks: JoinSet<()>,
    actor_cancel_token: CancellationToken,
    actor_error_notify: Arc<sync::Notify>,
}

impl ActorManager {
    #[instrument(
        name = "manager::new",
        skip(phrase, provider, rate_handle, gas_oracle, tx_builder)
    )]
    pub async fn new(
        phrase: &str,
        global_args: GlobalOptions,
        max_tps: f64,
        provider: RecommendedProvider,
        rate_handle: RateControllerHandle,
        gas_oracle: GasPriceChannel,
        tx_builder: Arc<TransactionRandomizer>,
    ) -> Result<Self> {
        let GlobalOptions {
            tps_per_actor,
            gas_multiplier,
            ..
        } = global_args;
        let num_actors = estimate_actors_count(max_tps, tps_per_actor);
        // first account is master account and only used to topup other actors
        let (signer, master_address, actor_addresses) = build_wallet(phrase, num_actors)?;
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
                    let tx_builder = tx_builder.clone();
                    async move {
                        Actor::new(
                            address,
                            gas_oracle,
                            gas_multiplier,
                            provider,
                            signer,
                            rate_handle,
                            tx_builder,
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

    /// Topup all the actors with some eth
    ///
    /// This will send equal amount of eth to all actors from master account
    /// while retaining equal part in master account
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
        // get balance of actor: skip if balance is superior of actor (already been funded in previous runs)
        let actor_balance = self.provider.get_balance(actor.address).await?;
        if actor_balance > master_balance {
            return Ok(())
        }
            let tx = TransactionRequest::default()
                .from(self.master_address)
                .to(actor.address)
                .value(amount)
                // some clients does not support `eth_feeHistory` so falling back to legacy gas price
                .with_gas_price(self.provider.get_gas_price().await?);
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

    /// Spwan the actors tasks to start sending tx
    pub fn spawn_actors(&mut self, tx_timeout: Duration) {
        for actor in self.actors.iter() {
            let cancel_token = self.actor_cancel_token.clone();
            let actor = actor.clone();
            let error_notify = self.actor_error_notify.clone();

            let span = info_span!("actor_task", actor = actor.address.to_string());
            let fut = async move {
                info!("spawned");
                'shutdown: loop {
                    // biased select to prioritize tx send over shutdown signal
                    // to prevent race conditions
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

    /// Wait for error notification from actors
    pub async fn wait_for_error(&self) {
        self.actor_error_notify.notified().await;
    }

    /// Teardown the actors tasks and peform cleanup,
    /// send back all the funds to master account
    pub async fn shutdown(&mut self, tx_timeout: Duration) {
        // cancel all the tasks
        self.actor_cancel_token.cancel();
        // wait for tasks to shutdown
        while self.tasks.join_next().await.is_some() {}
        // attempt to send back all the funds to master account
        self.attempt_to_send_funds_back(tx_timeout).await;
    }

    /// Attempt to send all the funds back to master account,
    /// errors are logged and ignored
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

/// Get the master (index 0) signer from the phrase
pub fn build_master_signer(phrase: &str) -> Result<LocalSigner<SigningKey>> {
    MnemonicBuilder::<English>::default()
        .phrase(phrase)
        .index(0)?
        .build()
        .map_err(Into::into)
}

/// Get the wallet with master (index 0) and actors signers along with their address from the phrase
fn build_wallet(phrase: &str, num_actors: u32) -> Result<(EthereumWallet, Address, Vec<Address>)> {
    let master = build_master_signer(phrase)?;
    let master_address = master.address();
    let mut actor_address = vec![];
    let mut signer = EthereumWallet::new(master);

    for i in 1..(num_actors + 1) {
        let wallet = MnemonicBuilder::<English>::default()
            .phrase(phrase)
            .index(i)?
            .build()?;
        actor_address.push(wallet.address());
        signer.register_signer(wallet);
    }

    Ok((signer, master_address, actor_address))
}

#[instrument(ret(level = Level::DEBUG))]
fn estimate_actors_count(max_tps: f64, tps_per_actor: u32) -> u32 {
    (max_tps / tps_per_actor as f64).ceil() as u32
}
