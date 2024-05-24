use actor::ActorManager;
use alloy::rpc::client::{BuiltInConnectionString, ClientBuilder};
use batcher::RawTxBatcher;
use clap::Parser;
use dotenv::dotenv;
use eyre::Result;
use std::time::Duration;
use tokio::{sync::mpsc, time};

pub mod actor;
pub mod batcher;
pub mod builder;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[clap(short, long, required = true, help = "Sets the RPC URL")]
    rpc_url: String,
    #[clap(short, long, required = true, help = "Sets the maximum TPS")]
    max_tps: usize,
    #[clap(short, long, help = "Sets the duration in seconds")]
    duration: Option<u64>,
}

async fn execute(args: Args) -> Result<()> {
    let phrase = std::env::var("MNEMONIC")?;
    let Args {
        rpc_url,
        max_tps,
        duration,
    } = args;

    let connect: BuiltInConnectionString = rpc_url.parse()?;
    let client = ClientBuilder::default().connect_boxed(connect).await?;
    // let provider = ProviderBuilder::new()
    //     .with_recommended_fillers()
    //     .on_builtin(&rpc_url)
    //     .await?;

    let (tx, rx) = mpsc::channel(max_tps * 10);
    let mut manager =
        ActorManager::init_actors(&phrase, max_tps as u64, &rpc_url, tx.clone()).await?;
    let mut batcher = RawTxBatcher::new(rx, client);
    // start actors
    manager.spawn();

    if let Some(duration) = duration {
        match time::timeout(Duration::from_secs(duration), batcher.start(max_tps as u64)).await {
            Ok(res) => {
                if let Err(e) = res {
                    println!("Batcher error: {:?}", e);
                }
            }
            Err(_) => {}
        }
    } else {
        batcher.start(max_tps as u64).await?;
    }

    manager.shutdown().await;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv()?;
    let args = Args::parse();
    execute(args).await?;
    Ok(())
}
