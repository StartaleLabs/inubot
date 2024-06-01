use alloy::{
    providers::{Provider, ProviderBuilder},
    rpc::{
        client::{BuiltInConnectionString, WsConnect},
        types::eth::Block,
    },
};
use clap::Parser;
use eyre::{eyre, Result};
use futures_util::StreamExt;

const MAX_BLOCK_GAS_LIMIT: u128 = 30_000_000;

#[derive(Debug, Default)]
struct WelfordMovingAverage {
    count: u128,
    mean: f64,
}

/// Welford stable single pass weighted moving average implementation.
/// https://en.wikipedia.org/wiki/Algorithms_for_calculating_variance#Online
impl WelfordMovingAverage {
    fn update(&mut self, sum: f64, span: u64) {
        self.count += span as u128;
        let delta = sum - (self.mean * span as f64);
        self.mean += delta / self.count as f64;
    }

    fn mean(&self) -> f64 {
        self.mean
    }
}

/// Helper struct to save chain statistics and operate over it
#[derive(Debug, Default)]
struct ChainStats {
    block_time: u64,
    last_block: Option<Block>,
    tps: WelfordMovingAverage,
    gas_used: WelfordMovingAverage,
}

impl ChainStats {
    pub fn new(block_time: u64) -> Self {
        Self {
            block_time,
            ..Default::default()
        }
    }
    /// Update the statistics with the new block
    /// This require full blocks, some RPCs may not return transactions
    pub fn update(&mut self, block: &Block) {
        let span = match &self.last_block {
            Some(last_block) => block.header.timestamp - last_block.header.timestamp,
            None => self.block_time,
        };

        self.tps.update(block.transactions.len() as f64, span);
        self.gas_used.update(block.header.gas_used as f64, 1);

        self.last_block = Some(block.clone());
    }

    pub fn average_tps(&self) -> f64 {
        if self.last_block.is_some() {
            self.tps.mean()
        } else {
            0.0
        }
    }

    pub fn block_tps(&self) -> f64 {
        if let Some(block) = &self.last_block {
            block.transactions.len() as f64 / self.block_time as f64
        } else {
            0.0
        }
    }

    pub fn average_utilization(&self) -> f64 {
        (self.gas_used.mean() * 100.0) / MAX_BLOCK_GAS_LIMIT as f64
    }

    pub fn block_utlization(&self) -> f64 {
        if let Some(block) = &self.last_block {
            (block.header.gas_used * 100) as f64 / MAX_BLOCK_GAS_LIMIT as f64
        } else {
            0.0
        }
    }

    pub fn print_summary(&self) {
        if let Some(block) = &self.last_block {
            println!(
                "[Block #{:?}] TPS: (Avg={}, Blk={}), Utilz: (Avg={:.2}, Blk={:.2})",
                block.header.number.unwrap_or_default(),
                self.average_tps(),
                self.block_tps(),
                self.average_utilization(),
                self.block_utlization()
            );
        }
    }
}

/// Build a furture stream to poll the chain for new blocks and update the statistics
/// NOTE: The Polling put a lot of stress on the node, use with caution
async fn polling_update(stats: &mut ChainStats, rpc_url: String) -> Result<()> {
    let provider = ProviderBuilder::new().on_http(rpc_url.parse()?);
    let mut stream = provider
        .watch_blocks()
        .await?
        .into_stream()
        .flat_map(futures_util::stream::iter);

    while let Some(block) = stream.next().await {
        let block = provider.get_block(block.into(), true).await?.unwrap();
        stats.update(&block);
        stats.print_summary();
    }

    Ok(())
}

/// Build a future stream to subscribe to new blocks and update the statistics
/// Only works with Websocket providers
/// TODO: add support for IBC and other pubsub protocols
async fn pubsub_update(stats: &mut ChainStats, rpc_url: String) -> Result<()> {
    // Create a provider.
    let ws = WsConnect::new(rpc_url);
    let provider = ProviderBuilder::new().on_ws(ws).await?;

    // // Subscribe to blocks.
    let subscription = provider.subscribe_blocks().await?;
    let mut stream = subscription.into_stream();
    while let Some(mut block) = stream.next().await {
        if block.header.hash.is_some() && block.transactions.is_empty() {
            block = provider
                .get_block(block.header.hash.unwrap().into(), true)
                .await?
                .unwrap();
        }
        stats.update(&block);
        stats.print_summary();
    }

    Ok(())
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[clap(short, long, required = true, help = "Sets the RPC URL")]
    rpc_url: String,
    #[clap(
        short,
        long,
        default_value = "2",
        help = "Sets the block time in seconds"
    )]
    block_time: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let Args {
        rpc_url,
        block_time,
    } = Args::parse();

    let connection: BuiltInConnectionString = rpc_url.parse()?;
    let mut stats = ChainStats::new(block_time);
    loop {
        let res = match connection {
            BuiltInConnectionString::Http(_) => polling_update(&mut stats, rpc_url.clone()).await,
            BuiltInConnectionString::Ws(_, _) => pubsub_update(&mut stats, rpc_url.clone()).await,
            _ => Err(eyre!("Unsupported connection type")),
        };

        match res {
            Ok(_) => println!("Connection closed. Reconnecting..."),
            Err(e) => eprintln!("Error: {:?}", e),
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    }
}
