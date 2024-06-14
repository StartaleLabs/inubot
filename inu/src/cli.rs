use clap::{Args, Parser};
use eyre::{eyre, OptionExt, Result};
use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf, time::Duration};
use tracing::{info, instrument};

use crate::commands::Commands;

#[derive(Debug)]
pub struct InuConfig {
    global: GlobalOptions,
    mnemonic: String,
    network: Network,
}

impl InuConfig {
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
            config_file
                .networks
                .remove(&name)
                .ok_or(eyre!("Network {} not found", name))?
        } else if let Some(url) = cli.rpc_url {
            Network {
                rpc_url: url,
                name: None,
                block_time: None,
                default: false,
            }
        } else if config_file.networks.len() == 1 {
            config_file.networks.values().next().unwrap().clone()
        } else {
            // find the default network
            config_file
                .networks
                .values()
                .find(|network| network.default)
                .ok_or_eyre("Please specify a network, no default network found")?
                .clone()
        };

        if cli.block_time.is_some() {
            network.block_time = cli.block_time;
        }

        info!(
            "loaded config, globals={:?}, network={:?}, command={:?}",
            config_file.global, network, cli.command
        );

        Ok((
            Self {
                global: config_file.global,
                mnemonic: config_file.mnemonic,
                network,
            },
            cli.command,
        ))
    }

    pub fn get_network(&self) -> &Network {
        &self.network
    }

    pub fn get_global(&self) -> &GlobalOptions {
        &self.global
    }

    pub fn get_mnemonic(&self) -> &str {
        &self.mnemonic
    }
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct InuConfigFile {
    #[serde(flatten)]
    global: GlobalOptions,
    networks: HashMap<String, Network>,
    // the mnemonic is only palceholder here and only to be fetch from env
    // this is not serialised
    #[serde(skip_serializing)]
    mnemonic: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GlobalOptions {
    #[serde(with = "humantime_serde")]
    pub tx_timeout: Duration,
    pub tps_per_actor: u32,
}

impl Default for GlobalOptions {
    fn default() -> Self {
        Self {
            tx_timeout: Duration::from_secs(15),
            tps_per_actor: 50,
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
}

///
/// CLI
///

#[derive(Debug, Parser, Serialize)]
#[command(version, about, long_about = None)]
struct InuCli {
    #[arg(value_hint = clap::ValueHint::FilePath, global = true)]
    config: Option<PathBuf>,

    // both mutually exclusive
    #[arg(short, long, conflicts_with = "network", value_hint = clap::ValueHint::Url, global = true)]
    rpc_url: Option<String>,
    #[arg(short, long, global = true)]
    network: Option<String>,
    #[arg(short, long, global = true, value_parser = humantime::parse_duration)]
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
struct GlobalArgs {
    #[serde(skip_serializing_if = "Option::is_none", with = "humantime_serde")]
    #[arg(long, global = true, value_parser = humantime::parse_duration)]
    tx_timeout: Option<Duration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long, global = true)]
    tps_per_actor: Option<u64>,
}
