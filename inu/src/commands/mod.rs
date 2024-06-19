use std::{sync::Arc, time::Duration};

use alloy::{
    hex,
    providers::{Provider, ProviderBuilder},
    signers::wallet::{
        coins_bip39::{English, Mnemonic},
        MnemonicBuilder,
    },
};
use clap::{ArgAction, Args, Subcommand};
use eyre::Result;
use serde::{Deserialize, Serialize};
use tokio::{select, signal};
use tokio_stream::StreamExt;
use tracing::{debug, info};

use crate::{
    actor::{ActorManager, RecommendedProvider},
    cli::{InuConfig, Network},
    commands::metrics::spwan_metrics_channel,
    gas_oracle::GasPricePoller,
    rate::RateController,
};

pub mod metrics;

#[derive(Debug, Serialize, Deserialize, Args)]
pub struct RunArgs {
    #[arg(short, long)]
    pub max_tps: u32,
    #[serde(with = "humantime_serde")]
    #[arg(short, long, value_parser = humantime::parse_duration, default_value = "100years")]
    pub duration: Duration,
    #[arg(long, action(ArgAction::SetTrue))]
    pub metrics: bool,
}

#[derive(Debug, Serialize, Deserialize, Args)]
pub struct WithdrawArgs {
    #[arg(short, long)]
    pub max_tps: u32,
}

#[derive(Debug, Serialize, Deserialize, Subcommand)]
pub enum Commands {
    Run(RunArgs),
    Withdraw(WithdrawArgs),
    Metrics,
    Mnemonic,
}

impl Commands {
    pub async fn execute(&self, config: &InuConfig) -> Result<()> {
        let network = config.get_network();
        let global_agrs = config.get_global();
        let provider = ProviderBuilder::new()
            .with_recommended_fillers()
            .on_builtin(&network.rpc_url)
            .await?;

        match self {
            Commands::Run(args) => {
                info!("Initializing Run command..");
                let RunArgs {
                    max_tps,
                    duration,
                    metrics,
                } = args;

                let mut manager = setup_manager(
                    provider,
                    *max_tps,
                    global_agrs.tps_per_actor,
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

                let manager = setup_manager(
                    provider,
                    *max_tps,
                    global_agrs.tps_per_actor,
                    config.get_mnemonic(),
                )
                .await?;

                // withraw fund
                manager
                    .attempt_to_send_funds_back(global_agrs.tx_timeout)
                    .await
            }
            Commands::Metrics => {
                spwan_metrics(network.clone()).await?.await?;
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
                            hex::encode(wallet.signer().to_bytes())
                        );
                    } else {
                        info!("Account {}: {}", i, wallet.address());
                    }
                }
            }
        }
        Ok(())
    }
}

async fn setup_manager(
    provider: RecommendedProvider,
    max_tps: u32,
    tps_per_actor: u32,
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

    let manager = ActorManager::new(
        phrase,
        max_tps,
        tps_per_actor,
        provider,
        rate_handle,
        1.5,
        gas_oracle,
    )
    .await?;

    Ok(manager)
}

async fn spwan_metrics(network: Network) -> Result<tokio::task::JoinHandle<()>> {
    let mut channel = spwan_metrics_channel(network.clone()).await?;
    Ok(tokio::spawn(async move {
        while let Some(stats) = channel.next().await {
            info!("{}", stats.get_summary());
        }
    }))
}
