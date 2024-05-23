use alloy::{
    providers::{Provider, ProviderBuilder},
    rpc::{client::WsConnect, types::eth::Block},
    transports::http::reqwest::Url,
};
use eyre::Result;
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

#[derive(Debug, Default)]
struct ChainStats<const BLOCK_TIME: u64 = 2> {
    last_block: Option<Block>,
    tps: WelfordMovingAverage,
    gas_used: WelfordMovingAverage,
}

impl<const BLOCK_TIME: u64> ChainStats<BLOCK_TIME> {
    pub fn update(&mut self, block: &Block) {
        let span = match &self.last_block {
            Some(last_block) => block.header.timestamp - last_block.header.timestamp,
            None => BLOCK_TIME,
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
            block.transactions.len() as f64 / BLOCK_TIME as f64
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
}

async fn update_on_blocks(stats: &mut ChainStats) -> Result<()> {
    // Create a provider.
    // let ws = WsConnect::new("ws://localhost:12345");
    // let provider = ProviderBuilder::new().on_ws(ws).await?;

    // // Subscribe to blocks.
    // let subscription = provider.subscribe_blocks().await?;
    // let rpc: Url = "https://op-inu.astar.network".parse()?;
    let rpc: Url = "http://localhost:12345".parse()?;
    let provider = ProviderBuilder::new().on_http(rpc);
    let mut stream = provider
        .watch_blocks()
        .await?
        .into_stream()
        .flat_map(futures_util::stream::iter);

    while let Some(block) = stream.next().await {
        let block = provider.get_block(block.into(), true).await?.unwrap();
        stats.update(&block);
        println!(
            "Block: {:?}, Timestamp: {}, Last Block TPS: {}, Last Block Utlization: {}",
            block.header.number,
            block.header.timestamp,
            // stats.average_tps(),
            stats.block_tps(),
            // stats.average_utilization(),
            stats.block_utlization()
        );
        // println!(
        //     "Block: {:?}, Timestamp: {}, Avg. TPS: {}, Last Block TPS: {}, Avg. Utilization: {}, Last Block Utlization: {}",
        //     block.header.number,
        //     block.header.timestamp,
        //     stats.average_tps(),
        //     stats.block_tps(),
        //     stats.average_utilization(),
        //     stats.block_utlization()
        // );
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut stats = ChainStats::<2>::default();
    loop {
        let res = update_on_blocks(&mut stats).await;
        match res {
            Ok(_) => println!("Connection closed. Reconnecting..."),
            Err(e) => eprintln!("Error: {}", e),
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    }
}
