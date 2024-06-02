use std::{sync::Arc, time::Duration};

use crate::Args;
use alloy::providers::{Provider, ProviderBuilder};
use eyre::Result;
use tokio::{select, signal, time};
use tracing::debug;

use self::{actor::ActorManager, gas_oracle::GasPricePoller, rate::RateController};

pub mod actor;
pub mod gas_oracle;
pub mod nonce;
pub mod rate;
pub mod error;

pub async fn execute(args: &Args) -> Result<()> {
    let phrase = std::env::var("MNEMONIC")?;
    let Args {
        rpc_url,
        max_tps,
        duration,
        tx_timeout,
    } = args;

    let provider = ProviderBuilder::new()
        .with_recommended_fillers()
        .on_builtin(&rpc_url)
        .await?;
    let provider_shared = Arc::new(provider.clone());

    // init the gas poller and spwan it
    let gas_oracle = GasPricePoller::new(provider.weak_client())
        .with_init_value(0)
        .spawn();

    // init the rate controller and spawn it
    let rate_handle = RateController::new(provider_shared.clone())
        .with_tps(*max_tps)?
        .spawn();

    let mut manager =
        ActorManager::new(&phrase, *max_tps, provider, rate_handle, 1.5, gas_oracle).await?;

    //setup the actors
    manager.setup().await?;

    manager.spawn_actors(Duration::from_secs(*tx_timeout));

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
            _ = time::sleep(Duration::from_secs(duration.unwrap_or(u64::MAX))) => {
                manager.shutdown().await;
                break 'main;
            }
        }
    }

    Ok(())
}
