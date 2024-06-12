use crate::{actor::ActorManager, gas_oracle::GasPricePoller, rate::RateController};
use alloy::providers::{Provider, ProviderBuilder};
use cli::{Commands, InuConfig, RunArgs};
use dotenv::dotenv;
use eyre::Result;
use std::sync::Arc;
use tokio::{select, signal, time};
use tracing::{debug, info};
use tracing_subscriber::EnvFilter;

pub mod actor;
pub mod builder;
pub mod cli;
pub mod error;
pub mod gas_oracle;
pub mod nonce;
pub mod rate;

async fn start_inu(config: InuConfig) -> Result<()> {
    let network = config.get_network();
    let global_agrs = config.get_global();
    let provider = ProviderBuilder::new()
        .with_recommended_fillers()
        .on_builtin(&network.rpc_url)
        .await?;

    match config.get_command() {
        Commands::Run(args) => {
            let RunArgs { max_tps, duration } = args;
            let provider_shared = Arc::new(provider.clone());
            // init the gas poller and spwan it
            let gas_oracle = GasPricePoller::new(provider_shared.weak_client())
                .with_init_value(0)
                .spawn();

            // init the rate controller and spawn it
            let rate_handle = RateController::new(provider_shared.clone())
                .with_tps(*max_tps)?
                .spawn();

            let mut manager = ActorManager::new(
                config.get_mnemonic(),
                *max_tps,
                global_agrs.tps_per_actor,
                provider,
                rate_handle,
                1.5,
                gas_oracle,
            )
            .await?;

            // setup the actors
            manager.fund_actors().await?;
            // spawn actors
            manager.spawn_actors(global_agrs.tx_timeout);

            'main: loop {
                select! {
                    _ = manager.wait_for_error() => {
                        manager.shutdown().await;
                        break 'main;
                    }
                    res = signal::ctrl_c() => {
                        if res.is_ok() {
                            debug!("ctrl-c received!");
                            manager.shutdown().await;
                            break 'main;
                        } else {
                            debug!("failed to receive ctrl-c signal, ignoring..");
                        }
                    }
                    _ = time::sleep(*duration) => {
                        manager.shutdown().await;
                        break 'main;
                    }
                }
            }
        }
        Commands::Withdraw(args) => {
            let RunArgs { max_tps, .. } = args;
            let provider_shared = Arc::new(provider.clone());
            // init the gas poller and spwan it
            let gas_oracle = GasPricePoller::new(provider_shared.weak_client())
                .with_init_value(0)
                .spawn();

            // init the rate controller and spawn it
            let rate_handle = RateController::new(provider_shared.clone())
                .with_tps(*max_tps)?
                .spawn();

            let manager = ActorManager::new(
                config.get_mnemonic(),
                *max_tps,
                global_agrs.tps_per_actor,
                provider,
                rate_handle,
                1.5,
                gas_oracle,
            )
            .await?;

            // withraw fund
            manager.attempt_to_send_funds_back().await
        }
    }

    Ok(())
}

fn setup_tracing() {
    // construct a subscriber that prints formatted traces to stdout
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        // .with_max_level(Level::DEBUG)
        .with_ansi(false)
        .init();
}

#[tokio::main]
async fn main() -> Result<()> {
    setup_tracing();
    dotenv()?;
    info!("Initializing");
    let config = InuConfig::load()?;

    start_inu(config).await?;
    Ok(())
}
