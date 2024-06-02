use actor::ActorManager;
use alloy::rpc::client::{BuiltInConnectionString, ClientBuilder};
use batcher::RawTxBatcher;
use clap::Parser;
use dotenv::dotenv;
use eyre::Result;
use std::time::Duration;
use tokio::{sync::mpsc, time};
use tracing::Level;
use tracing_subscriber::EnvFilter;

pub mod actor;
pub mod batcher;
pub mod builder;
pub mod v2;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[clap(short, long, required = true, help = "Sets the RPC URL")]
    rpc_url: String,
    #[clap(short, long, required = true, help = "Sets the maximum TPS")]
    max_tps: u32,
    #[clap(short, long, help = "Sets the duration in seconds")]
    duration: Option<u64>,
    #[clap(
        short,
        long,
        help = "Set the timeout for tx before marking it stuck",
        default_value = "15"
    )]
    tx_timeout: u64,
}

async fn execute(args: Args) -> Result<()> {
    let phrase = std::env::var("MNEMONIC")?;
    let Args {
        rpc_url,
        max_tps,
        duration,
        tx_timeout: _timeout,
    } = args;

    let connect: BuiltInConnectionString = rpc_url.parse()?;
    let client = ClientBuilder::default().connect_boxed(connect).await?;
    // let provider = ProviderBuilder::new()
    //     .with_recommended_fillers()
    //     .on_builtin(&rpc_url)
    //     .await?;

    let (tx, rx) = mpsc::channel((max_tps * 10) as usize);
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

    // construct a subscriber that prints formatted traces to stdout
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        // .with_max_level(Level::DEBUG)
        .with_ansi(false)
        .init();

    v2::execute(&args).await?;
    Ok(())
}

// #[tokio::main]
// async fn main() -> Result<()> {
//     let connect: BuiltInConnectionString = "ws://op-inu.astar.network:8546".parse()?;
//     let client = ClientBuilder::default().connect_boxed(connect).await?;

//     let poller = GasPricePoller::new(client.get_weak()).with_poll_interval(Duration::from_secs(1));
//     let ch1 = poller.spawn();
//     let ch2 = ch1.clone();

//     let h1 = tokio::spawn(async move {
//         let mut stream = ch1.into_stream();
//         while let Some(price) = stream.next().await {
//             println!("Thread1: Gas price: {}", price);
//         }
//     });

//     let h2 = tokio::spawn(async move {
//         let mut stream = ch2.into_stream();
//         while let Some(price) = stream.next().await {
//             println!("Thread2: Gas price: {}", price);
//         }
//     });

//     h1.await?;
//     h2.await?;

//     Ok(())
// }
