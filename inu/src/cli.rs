use crate::{builder::OrganicTransaction, commands::Commands};
use alloy::primitives::Address;
use clap::{Args, Parser};
use eyre::{eyre, Result};
use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf, time::Duration};
use tracing::{info, instrument};

#[derive(Debug)]
pub struct InuConfig {
    global: GlobalOptions,
    mnemonic: Option<String>,
    network: Option<Network>,
    transactions: HashMap<OrganicTransaction, f64>,
}

impl InuConfig {
    /// Load the configs from file, env and cli and merge them to produce final config
    #[instrument(name = "config_load")]
    pub fn load() -> Result<(Self, Commands)> {
        let cli = InuCli::parse();

        let mut config_file = Figment::new()
            // first default config file layer
            .merge(Serialized::defaults(InuConfigFile::default()))
            // then read from envs
            .merge(Env::prefixed("INU_").split("_"));

        // Add configuration from file if specified
        if let Some(config_path) = &cli.config {
            config_file = config_file.merge(Toml::file(config_path));
        } else {
            config_file = config_file.merge(Toml::file("inu_config.toml"));
        }

        // Merge CLI arguments
        config_file = config_file.merge(Serialized::defaults(&cli));

        // Extract the final configuration
        let mut config_file: InuConfigFile = config_file.extract()?;

        // get the network
        let mut network = if let Some(name) = cli.network {
            Some(
                config_file
                    .networks
                    .remove(&name)
                    .ok_or(eyre!("Network {} not found", name))?,
            )
        } else if let Some(url) = cli.rpc_url {
            Some(Network {
                rpc_url: url,
                name: None,
                block_time: None,
                default: false,
                organic_address: None,
            })
        } else if config_file.networks.len() == 1 {
            config_file.networks.values().next().cloned()
        } else {
            // find the default network
            config_file
                .networks
                .values()
                .find(|network| network.default)
                .cloned()
        };

        if let Some(block_time) = cli.block_time {
            network = network.map(|mut n| {
                n.block_time = Some(block_time);
                n
            })
        }

        if config_file.transactions.is_empty() {
            config_file.transactions = default_transaction_probablities();
        }

        info!(
            "loaded config, globals={:?}, network={:?}, transactions={:?}, command={:?}, ",
            config_file.global, network, config_file.transactions, cli.command,
        );

        Ok((
            Self {
                global: config_file.global,
                mnemonic: config_file.mnemonic,
                transactions: config_file.transactions,
                network,
            },
            cli.command,
        ))
    }

    pub fn get_network(&self) -> &Option<Network> {
        &self.network
    }

    pub fn get_global(&self) -> &GlobalOptions {
        &self.global
    }

    pub fn get_mnemonic(&self) -> &Option<String> {
        &self.mnemonic
    }

    pub fn get_tx_probabilities(&self) -> &HashMap<OrganicTransaction, f64> {
        &self.transactions
    }
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct InuConfigFile {
    #[serde(flatten)]
    global: GlobalOptions,
    networks: HashMap<String, Network>,
    transactions: HashMap<OrganicTransaction, f64>,
    // the mnemonic is only palceholder here and only to be fetch from env
    // this is not serialised
    #[serde(skip_serializing)]
    mnemonic: Option<String>,
}

fn default_transaction_probablities() -> HashMap<OrganicTransaction, f64> {
    [
        (OrganicTransaction::Transfer, 0.95),       // 93%
        (OrganicTransaction::ERC20Mint, 0.02),      // 2%
        (OrganicTransaction::ERC721Mint, 0.015),    // 1.5%
        (OrganicTransaction::ERC1155Mint, 0.015),   // 1.5%
        (OrganicTransaction::ERC20Deploy, 0.01),    // 1%
        (OrganicTransaction::ERC1155Deploy, 0.005), // 0.5%
        (OrganicTransaction::ERC721Deploy, 0.005),  // 0.5%
    ]
    .into()
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GlobalOptions {
    #[serde(with = "humantime_serde")]
    pub tx_timeout: Duration,
    pub tps_per_actor: u32,
    pub gas_multiplier: f64,
    pub mnemonic_start_index: u32,
}

impl Default for GlobalOptions {
    fn default() -> Self {
        Self {
            tx_timeout: Duration::from_secs(5 * 60),
            tps_per_actor: 50,
            gas_multiplier: 1.5,
            mnemonic_start_index: 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Network {
    pub rpc_url: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub name: Option<String>,
    #[serde(
        with = "humantime_serde",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub block_time: Option<Duration>,
    #[serde(default)]
    default: bool,
    pub organic_address: Option<Address>,
}

///
/// CLI
///

#[derive(Debug, Parser, Serialize)]
#[command(version, about, long_about = None)]
struct InuCli {
    #[arg(short, long, value_hint = clap::ValueHint::FilePath, global = true, help="Path to the inu configuration file")]
    config: Option<PathBuf>,
    // both mutually exclusive
    /// RPC url of the network
    #[arg(short, long, conflicts_with = "network", value_hint = clap::ValueHint::Url, global = true, help_heading = "Network Options")]
    rpc_url: Option<String>,
    #[arg(short, long, global = true, help_heading = "Network Options")]
    /// Name of the network(from config file)
    network: Option<String>,
    /// Override block time of the network (default: 2s)
    #[arg(short, long, global = true, value_parser = humantime::parse_duration, help_heading = "Network Options")]
    block_time: Option<Duration>,

    // global options, flatten
    #[command(flatten)]
    #[serde(flatten)]
    global: GlobalArgs,

    // subcommands
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Serialize, Deserialize, Args)]
#[command(next_help_heading = "Global Options")]
struct GlobalArgs {
    /// Timeout for each transaction (default: 120s)
    #[serde(skip_serializing_if = "Option::is_none", with = "humantime_serde")]
    #[arg(long, global = true, value_parser = humantime::parse_duration)]
    tx_timeout: Option<Duration>,
    
    /// Per actor TPS (default: 50)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long, global = true)]
    tps_per_actor: Option<u64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    /// Gas multiplier (default: 1.5)
    #[arg(long, global = true)]
    gas_multiplier: Option<f64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long, global = true)]
    mnemonic_start_index: Option<u32>,
}
