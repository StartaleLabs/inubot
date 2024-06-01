use alloy::{
    eips::eip2718::Encodable2718,
    network::{Ethereum, EthereumSigner, NetworkSigner, TransactionBuilder},
    primitives::{utils::format_units, Address, U256},
    providers::{
        fillers::{ChainIdFiller, FillProvider, GasFiller, JoinFill, NonceFiller, SignerFiller},
        Provider, RootProvider,
    },
    rpc::{
        json_rpc::{ErrorPayload, RpcError},
        types::eth::TransactionRequest,
    },
    signers::wallet::{coins_bip39::English, MnemonicBuilder},
    transports::{BoxTransport, TransportError, TransportErrorKind},
};
use eyre::{eyre, Result};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::{sync, task::JoinSet, time};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::{
    builder::TxBuilder,
    v2::{
        gas_oracle::GasPriceChannel,
        nonce::NonceManager,
        rate::{RateControllerHandle, SendConfig},
    },
};

use super::nonce::FailContext;

const MAX_TPS_PER_ACTOR: u64 = 50;
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
        SignerFiller<EthereumSigner>,
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

impl<P: Provider<BoxTransport>, S: NetworkSigner<Ethereum>> Actor<P, S> {
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

    pub async fn send_tx(&self, timeout: Duration) -> Result<(), ActorError> {
        let (nonce_handle, failed_context) = self
            .nonce_manager
            .next_nonce()
            .await
            // reset the nonce handle to new, and take the failed context if any
            .consume_failed();
        let nonce = nonce_handle.get();
        let mut gas_price = self.get_gas_price();

        if let Some(FailContext {
            gas_price: old_gas_price,
            might_be_timeout,
            error,
        }) = failed_context
        {
            match error {
                RpcError::ErrorResp(resp) => {
                    // TODO: find and handle other cases
                    if resp.message.to_lowercase().contains("insufficient fund") {
                        return Err(ActorError::NotSufficentFunds {
                            resp,
                            actor_address: self.address,
                            balance: self.provider.get_balance(self.address).await?,
                        });
                    } else if resp.message.contains("already imported") {
                        // nonce is alredy used, skip it and continue
                        // TODO: handle this better
                        debug!("skipping as nonce {} already imported", nonce);
                        return Ok(());
                    } else if resp.message.contains("replacement transaction underpriced") {
                        gas_price = self.apply_gas_multiplier(gas_price.max(old_gas_price));
                        debug!(
                            "gas price increased to {} from {} for nonce {}",
                            gas_price, old_gas_price, nonce
                        );
                    }
                }
                // tricky to handle this case since pending tx timeout also return this error
                // we rely on the `might_be_timeout` flag to differentiate
                RpcError::Transport(TransportErrorKind::BackendGone)
                    if might_be_timeout == true =>
                {
                    // nonce might be stucked, increase gas price
                    // use the max of the increased old gas price and the new gas price
                    gas_price = self.apply_gas_multiplier(gas_price.max(old_gas_price));
                }
                e => {
                    // for any other error rpc error we just return
                    return Err(e.into());
                }
            }
        }

        let encoded_tx = TxBuilder::build(self.address.clone())
            .with_nonce(nonce)
            .with_chain_id(self.chain_id)
            .with_gas_price(gas_price)
            .with_gas_limit(21_000)
            // .with_max_priority_fee_per_gas(1_000_000_000)
            // .with_max_fee_per_gas(20_000_000_000)
            .build(&*self.signer)
            .await
            .map_err(|e| ActorError::OtherError(e.into()))?
            .encoded_2718();

        let config = SendConfig {
            encoded_tx,
            gas_price,
            nonce_handle,
            timeout,
        };

        if let Err(c) = self.rate_controller.send_tx(config).await {
            c.nonce_handle.free().await;
            return Err(eyre!("rate controller dropped! failed to sent tx").into());
        }

        Ok(())
    }

    pub async fn cleanup_nonce(pending_timeout: Duration) {
        // sleep till nonce timout gets freed in nonce manager
        // after this
        // WIP: to be completed
        time::sleep(pending_timeout.saturating_add(Duration::from_secs(1))).await
    }
}

pub struct ActorManager {
    actors: Vec<Arc<Actor<RecommendedProviderWithSigner, EthereumSigner>>>,
    provider: Arc<RecommendedProviderWithSigner>,
    master_address: Address,
    tasks: JoinSet<()>,
    actor_cancel_token: CancellationToken,
    actor_error_notify: Arc<sync::Notify>,
}

impl ActorManager {
    pub async fn new(
        phrase: &str,
        max_tps: u32,
        provider: RecommendedProvider,
        rate_handle: RateControllerHandle,
        gas_multiplier: f64,
        gas_oracle: GasPriceChannel,
    ) -> Result<Self> {
        let num_actors = estimate_actors_count(max_tps);
        // first account is master account and only used to topup other actors
        let (signer, master_address, actor_addresses) = build_signer(phrase, num_actors).await?;
        let provider = Arc::new(provider.join_with(SignerFiller::new(signer.clone())));
        let signer: Arc<EthereumSigner> = Arc::new(signer);
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
    pub async fn setup(&self) -> Result<()> {
        debug!("topping up actors...");
        let master_balance = self.provider.get_balance(self.master_address).await?;
        debug!(
            "master balance: {} eth",
            format_units(master_balance, "eth")?
        );
        // we need to send funds to all actors while retaining equal part in master account
        let amount: U256 = master_balance.div_ceil(U256::from(self.actors.len() + 1));
        debug!(
            "sending {} eth to each actor from master",
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
            let tx_timeout = tx_timeout.clone();
            let error_notify = self.actor_error_notify.clone();
            // spawn loop to send tx
            self.tasks.spawn(async move {
                'shutdown: loop {
                    // biased select to prioritize tx send over shutdown signal
                    // to prevent nonce race conditions
                    tokio::select! {
                        biased;
                        res = actor.send_tx(tx_timeout.clone()) => {
                            // if error, notify error and break the loop
                            if let Err(e) = res {
                                error_notify.notify_one();
                                debug!("actor {} send tx failed: {}", actor.address, e);
                                break 'shutdown;
                            }
                        }
                        _ = cancel_token.cancelled() => {
                            debug!("shutdown signal, actor {} shutting down...", actor.address);
                            break 'shutdown;
                        }
                    }
                }
            });
        }
    }

    pub async fn wait_for_error(&self) {
        self.actor_error_notify.notified().await;
    }

    pub async fn shutdown(&mut self) {
        // cancel all the tasks
        self.actor_cancel_token.cancel();
        // wait for tasks to shutdown
        while self.tasks.join_next().await.is_some() {}
    }

    pub async fn attempt_nonce_cleanup(&self) -> Result<()> {
        Ok(())
    }
}

///
/// Utlilty functions
///

async fn build_signer(
    phrase: &str,
    num_actors: u32,
) -> Result<(EthereumSigner, Address, Vec<Address>)> {
    let master = MnemonicBuilder::<English>::default()
        .phrase(phrase)
        .index(0)
        .unwrap()
        .build()?;
    let master_address = master.address();
    let mut actor_address = vec![];
    let mut signer = EthereumSigner::new(master);

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

fn estimate_actors_count(max_tps: u32) -> u32 {
    (max_tps as f64 / MAX_TPS_PER_ACTOR as f64).ceil() as u32
}
