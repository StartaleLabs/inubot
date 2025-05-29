use alloy::{
    providers::{Provider, ProviderBuilder},
    rpc::{
        client::BuiltInConnectionString,
        types::{eth::Block, BlockTransactionsKind},
    },
};
use colored::Colorize;
use eyre::{eyre, Result};
use futures::stream::BoxStream;
use futures_util::StreamExt;
use tokio::sync::watch;
use tokio_stream::wrappers::WatchStream;
use tracing::{debug, info_span, warn, Instrument};

use crate::cli::Network;

const MAX_BLOCK_GAS_LIMIT: u128 = 30_000_000;

/// Welford stable single pass weighted moving average implementation.
/// https://en.wikipedia.org/wiki/Algorithms_for_calculating_variance#Online
#[derive(Debug, Default, Clone)]
struct WelfordMovingAverage {
    count: u128,
    mean: f64,
}

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
#[derive(Debug, Default, Clone)]
pub struct ChainStats {
    block_gas_limit: u128,
    expected_block_time: u64,
    last_block: Option<Block>,
    first_block: Option<Block>,
    block_time: Option<u64>,
    avg_block_time: WelfordMovingAverage,
    avg_tps: WelfordMovingAverage,
    tx_per_block: Vec<u128>,
    mgas_per_block: Vec<f64>,
    avg_gas_used: WelfordMovingAverage,
}

impl ChainStats {
    pub fn new(block_time: u64) -> Self {
        Self {
            expected_block_time: block_time,
            block_gas_limit: MAX_BLOCK_GAS_LIMIT,
            ..Default::default()
        }
    }
    /// Update the statistics with the new block
    /// This require full blocks, some RPCs may not return transactions
    pub fn update(&mut self, block: &Block) {
        if self.first_block.is_none() {
            self.first_block = Some(block.clone());
        }

        let span = match &self.last_block {
            Some(last_block) => block.header.timestamp - last_block.header.timestamp,
            None => self.expected_block_time,
        };

        self.avg_block_time.update(span as f64, 1);
        self.avg_tps.update(block.transactions.len() as f64, span);
        self.avg_gas_used.update(block.header.gas_used as f64, 1);

        self.last_block = Some(block.clone());
        self.block_time = Some(span);
        self.block_gas_limit = block.header.gas_limit;

        self.tx_per_block.push(block.transactions.len() as u128);
        self.mgas_per_block
            .push(block.header.gas_used as f64 / 1_000_000f64);
    }

    pub fn average_tps(&self) -> f64 {
        if self.last_block.is_some() {
            self.avg_tps.mean()
        } else {
            0.0
        }
    }

    pub fn average_block_time(&self) -> f64 {
        match (&self.last_block, &self.first_block) {
            (Some(last_block), Some(first_block)) => {
                let duration = last_block.header.timestamp - first_block.header.timestamp;
                let blocks =
                    last_block.header.number.unwrap_or(0) - first_block.header.number.unwrap_or(0);
                blocks as f64 / duration as f64
            }
            _ => self.expected_block_time as f64,
        }
    }

    pub fn block_tps(&self) -> f64 {
        if let Some(block) = &self.last_block {
            block.transactions.len() as f64
                / self.block_time.unwrap_or(self.expected_block_time) as f64
        } else {
            0.0
        }
    }

    pub fn block_tx(&self) -> f64 {
        if let Some(block) = &self.last_block {
            block.transactions.len() as f64
        } else {
            0.0
        }
    }

    pub fn average_utilization(&self) -> f64 {
        (self.avg_gas_used.mean() * 100.0) / MAX_BLOCK_GAS_LIMIT as f64
    }

    pub fn block_utlization(&self) -> f64 {
        if let Some(block) = &self.last_block {
            (block.header.gas_used * 100) as f64 / MAX_BLOCK_GAS_LIMIT as f64
        } else {
            0.0
        }
    }

    pub fn block_mgas(&self) -> f64 {
        if let Some(block) = &self.last_block {
            (block.header.gas_used / 1_000_000) as f64
        } else {
            0f64
        }
    }

    pub fn average_block_mgas(&self) -> f64 {
        self.avg_gas_used.mean() / 1_000_000f64
    }

    pub fn get_summary(&self) -> String {
        self.last_block
            .as_ref()
            .map(|block| {
                let block_number = block.header.number.unwrap_or_default();

                let avg_tx = self.tx_per_block.iter().sum::<u128>() as f64 / self.tx_per_block.len() as f64;

                let avg_mgas = self
                    .mgas_per_block
                    .iter()
                    .sum::<f64>()
                    / self.mgas_per_block.len() as f64;

                let block_print = if block_number % 2_000 == 0 {
                    format!("Epoch #{}", block.header.number.unwrap_or_default()).bold().on_bright_red()
                } else {
                    format!("Block #{}", block.header.number.unwrap_or_default()).bold()
                };

                let is_close_to_full = if block.header.gas_used as f64 / block.header.gas_limit as f64 > 0.98 {
                    "FULL".green().bold()
                } else {
                    "\t".normal()
                };

                let age_secs = humantime::format_duration(std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH + std::time::Duration::from_secs(block.header.timestamp))
                    .unwrap_or_default());

                format!(
                    "[{}, {}s ago] {:.2} Tx, {:.2} MGas {} \tAvg({:.2}s Block Time, {:.2} Tx, {:.2} MGas)",
                    block_print,
                    age_secs.to_string().italic(),
                    self.block_tx(),
                    self.block_mgas(),
                    format!("{}", is_close_to_full),
                    self.average_block_time(),
                    avg_tx,
                    avg_mgas,
                )
            })
            .unwrap_or("No data".to_string())
    }
}

/// Stream blocks from the ws provider
async fn ws_block_stream<P: Provider + Clone + 'static>(
    provider: P,
) -> Result<BoxStream<'static, Result<Block>>> {
    let subscription = provider.subscribe_blocks().await?.into_stream();
    Ok(subscription
        .then(move |b| {
            let provider_clone = provider.clone();
            let Some(hash) = b.header.hash else {
                return futures::future::Either::Right(async { Err(eyre!("no block")) });
            };

            futures::future::Either::Left(async move {
                provider_clone
                    .get_block(hash.into(), BlockTransactionsKind::Full)
                    .await
                    .map_err(Into::into)
                    .and_then(|b| b.ok_or(eyre!("no block")))
            })
        })
        .boxed())
}

/// Poll blocks from the http provider
async fn http_block_stream<P: Provider + Clone + 'static>(
    provider: P,
) -> Result<BoxStream<'static, Result<Block>>> {
    let stream = provider
        .watch_blocks()
        .await?
        .into_stream()
        .flat_map(futures_util::stream::iter);

    Ok(stream
        .then(move |hash| {
            let provider_clone = provider.clone();
            async move {
                provider_clone
                    .get_block(hash.into(), BlockTransactionsKind::Full)
                    .await
                    .map_err(Into::into)
                    .and_then(|b| b.ok_or(eyre!("no block")))
            }
        })
        .boxed())
}

/// Create a block stream from the rpc url
async fn block_stream(rpc_url: &str) -> Result<BoxStream<'static, Result<Block>>> {
    let provider = ProviderBuilder::new().on_builtin(rpc_url).await?;
    let connection: BuiltInConnectionString = rpc_url.parse()?;
    match connection {
        BuiltInConnectionString::Http(_) => http_block_stream(provider.clone()).await,
        BuiltInConnectionString::Ws(_, _) => ws_block_stream(provider.clone()).await,
        _ => Err(eyre!("Unsupported connection type")),
    }
}

/// Spwan the metrics stream which is updated on new blocks
///
/// On network error, it will try to reconnect after 2 seconds
pub async fn spwan_metrics_channel(network: Network) -> Result<WatchStream<ChainStats>> {
    let Network {
        rpc_url,
        block_time,
        ..
    } = network;

    // default block time is 2 seconds, most op based chains have a 2 second block time
    let block_time = block_time.map_or(2, |b| b.as_secs());

    // confirm block stream can be produced
    let _ = block_stream(&rpc_url).await?;

    let (tx, rx) = watch::channel(ChainStats::new(block_time));
    let span = info_span!("metrics_channel");
    let fut = async move {
        'outer: loop {
            if let Ok(mut stream) = block_stream(&rpc_url).await {
                while let Some(Ok(block)) = stream.next().await {
                    if tx.is_closed() {
                        debug!("channel closed");
                        break 'outer;
                    }
                    tx.send_modify(|stats: &mut ChainStats| stats.update(&block))
                }
            };

            warn!("Reconnecting in 2s, connection closed");
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        }
    };

    tokio::spawn(fut.instrument(span));
    Ok(rx.into())
}
