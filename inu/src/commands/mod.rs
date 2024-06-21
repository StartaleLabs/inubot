use std::{collections::HashMap, sync::Arc, time::Duration};

use alloy::{
    hex,
    network::EthereumWallet,
    primitives::Address,
    providers::{fillers::WalletFiller, Provider, ProviderBuilder},
    signers::local::{
        coins_bip39::{English, Mnemonic},
        MnemonicBuilder,
    },
};
use clap::{ArgAction, Args, Subcommand};
use eyre::{eyre, Result};
use serde::{Deserialize, Serialize};
use tokio::{select, signal};
use tokio_stream::StreamExt;
use tracing::{debug, info, info_span, Instrument};

use crate::{
    actor::{build_master_signer, ActorManager, RecommendedProvider},
    builder::{Organic, OrganicTransaction, TransactionRandomizerBuilder},
    cli::{InuConfig, Network},
    commands::metrics::spwan_metrics_channel,
    gas_oracle::GasPricePoller,
    rate::RateController,
};

pub mod metrics;

#[derive(Debug, Serialize, Deserialize, Args)]
#[command(next_help_heading = "Run Options")]
pub struct RunArgs {
    /// Maximum TPS to be achieved
    #[arg(short, long)]
    pub max_tps: u32,
    /// Duration for which bot will continue to run
    #[serde(with = "humantime_serde")]
    #[arg(short, long, value_parser = humantime::parse_duration, default_value = "100years")]
    pub duration: Duration,
    /// Show metrics as well
    #[arg(long, action(ArgAction::SetTrue))]
    pub metrics: bool,
}

#[derive(Debug, Serialize, Deserialize, Args)]
#[command(next_help_heading = "Withdraw Options")]
pub struct WithdrawArgs {
    #[arg(short, long)]
    pub max_tps: u32,
}

#[derive(Debug, Serialize, Deserialize, Subcommand)]
pub enum Commands {
    /// Start sending the transactions to network
    Run(RunArgs),
    /// Withdraw the funds back from actors account to master
    Withdraw(WithdrawArgs),
    /// Only run the chain metrics
    Metrics,
    /// Generate a random account
    Mnemonic,
    /// Deploy the helper contract
    Deploy,
}

impl Commands {
    pub async fn execute(&self, config: &InuConfig) -> Result<()> {
        let global_agrs: &crate::cli::GlobalOptions = config.get_global();
        match self {
            Commands::Run(args) => {
                info!("Initializing Run command..");
                let RunArgs {
                    max_tps,
                    duration,
                    metrics,
                } = args;

                let network = config
                    .get_network()
                    .clone()
                    .ok_or(eyre!("Network not found"))?;
                let provider = ProviderBuilder::new()
                    .with_recommended_fillers()
                    .on_builtin(&network.rpc_url)
                    .await?;

                let mut manager = setup_manager(
                    provider,
                    *max_tps,
                    global_agrs.tps_per_actor,
                    global_agrs.gas_multiplier,
                    network.organic_address,
                    config.get_tx_probabilities().clone(),
                    config.get_mnemonic(),
                )
                .await?;

                // setup the actors
                manager.fund_actors().await?;
                // spawn actors
                manager.spawn_actors(global_agrs.tx_timeout);

                // spawn metrics
                if *metrics {
                    spwan_metrics(network.clone()).await?;
                }

                'main: loop {
                    select! {
                        _ = manager.wait_for_error() => {
                            manager.shutdown(global_agrs.tx_timeout).await;
                            break 'main;
                        }
                        res = signal::ctrl_c() => {
                            if res.is_ok() {
                                debug!("ctrl-c received!");
                                manager.shutdown(global_agrs.tx_timeout).await;
                                break 'main;
                            } else {
                                debug!("failed to receive ctrl-c signal, ignoring..");
                            }
                        }
                        _ = tokio::time::sleep(*duration) => {
                            manager.shutdown(global_agrs.tx_timeout).await;
                            break 'main;
                        }
                    }
                }
            }
            Commands::Withdraw(args) => {
                info!("Initializing Withdraw command..");
                let WithdrawArgs { max_tps } = args;

                let network = config
                    .get_network()
                    .clone()
                    .ok_or(eyre!("Network not found"))?;
                let provider = ProviderBuilder::new()
                    .with_recommended_fillers()
                    .on_builtin(&network.rpc_url)
                    .await?;

                let manager = setup_manager(
                    provider,
                    *max_tps,
                    global_agrs.tps_per_actor,
                    global_agrs.gas_multiplier,
                    network.organic_address.or(
                        // just to prevent the deployment of organic contract we use zero address
                        Some(Address::ZERO),
                    ),
                    config.get_tx_probabilities().clone(),
                    config.get_mnemonic(),
                )
                .await?;

                // withraw fund
                manager
                    .attempt_to_send_funds_back(global_agrs.tx_timeout)
                    .await
            }
            Commands::Metrics => {
                let network = config
                    .get_network()
                    .clone()
                    .ok_or(eyre!("Network not found"))?;

                spwan_metrics(network).await?.await?;
            }
            Commands::Mnemonic => {
                // Generate a random wallet (24 word phrase)
                let phrase = Mnemonic::<English>::new_with_count(&mut rand::thread_rng(), 24)?;
                info!("Mnemonic: {}", phrase.to_phrase());

                for i in 0..5 {
                    let wallet = MnemonicBuilder::<English>::default()
                        .phrase(phrase.to_phrase())
                        .index(i)?
                        .build()?;

                    if i == 0 {
                        info!(
                            "(master)Account {}: {} (pk: 0x{})",
                            i,
                            wallet.address(),
                            hex::encode(wallet.credential().to_bytes())
                        );
                    } else {
                        info!("Account {}: {}", i, wallet.address());
                    }
                }
            }
            Commands::Deploy => {
                let network = config
                    .get_network()
                    .clone()
                    .ok_or(eyre!("Network not found"))?;
                let provider = ProviderBuilder::new()
                    .with_recommended_fillers()
                    .on_builtin(&network.rpc_url)
                    .await?;

                let master = build_master_signer(config.get_mnemonic())?;
                info!(
                    "deploying organic contract with master account - {}",
                    master.address()
                );
                let provider = provider
                    .clone()
                    .join_with(WalletFiller::<EthereumWallet>::new(master.into()));

                let organic = Organic::deploy(provider).await?;
                info!("organic contract deployed at: {}", organic.address());
            }
        }
        Ok(())
    }
}

async fn setup_manager(
    provider: RecommendedProvider,
    max_tps: u32,
    tps_per_actor: u32,
    gas_multiplier: f64,
    organic_address: Option<Address>,
    tx_probabilities: HashMap<OrganicTransaction, f64>,
    phrase: &str,
) -> Result<ActorManager> {
    let provider_shared = Arc::new(provider.clone());
    // init the gas poller and spwan it
    let gas_oracle = GasPricePoller::new(provider_shared.weak_client())
        .with_init_value(0)
        .spawn();

    // init the rate controller and spawn it
    let rate_handle = RateController::new(provider_shared.clone())
        .with_tps(max_tps)?
        .spawn();

    let randomizer = TransactionRandomizerBuilder::new(organic_address)
        .with_txs(tx_probabilities)
        .build(
            provider
                .clone()
                .join_with(WalletFiller::<EthereumWallet>::new(
                    build_master_signer(phrase)?.into(),
                )),
        )
        .await?;

    let manager = ActorManager::new(
        phrase,
        max_tps,
        tps_per_actor,
        provider,
        rate_handle,
        gas_multiplier,
        gas_oracle,
        Arc::new(randomizer),
    )
    .await?;

    Ok(manager)
}

async fn spwan_metrics(network: Network) -> Result<tokio::task::JoinHandle<()>> {
    let mut channel = spwan_metrics_channel(network.clone()).await?;
    Ok(tokio::spawn(
        async move {
            while let Some(stats) = channel.next().await {
                info!("{}", stats.get_summary());
            }
        }
        .instrument(info_span!("metrics")),
    ))
}
