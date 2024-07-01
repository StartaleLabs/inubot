use std::{collections::HashMap, fmt};

use alloy::{
    network::TransactionBuilder,
    primitives::{Address, U256},
    providers::Provider,
    rpc::types::eth::TransactionRequest,
    sol,
    sol_types::SolCall,
};
use eyre::Result;
use rand::distributions::{Distribution, WeightedIndex};
use serde::{Deserialize, Serialize};
use tracing::{info, instrument};

// Organic contract interface for generating transactions
// data and deploying the contract
sol!(
    #[sol(rpc)]
    Organic,
    "contract_artifacts/Organic.json"
);

/// Builder for creating a `TransactionRandomizer`
pub struct TransactionRandomizerBuilder {
    // organic contract address
    organic_address: Option<Address>,
    // hashmap of transactions with their probablity
    tx_probabilities: HashMap<OrganicTransaction, f64>,
}

impl TransactionRandomizerBuilder {
    pub fn new(organic_address: Option<Address>) -> Self {
        Self {
            organic_address,
            tx_probabilities: HashMap::new(),
        }
    }

    /// Set the transactions and their probabilities
    ///
    /// NOTE: This will overwrite the existing transactions
    pub fn with_txs(mut self, tx_probabilities: HashMap<OrganicTransaction, f64>) -> Self {
        self.tx_probabilities = tx_probabilities;
        self
    }

    /// Set the transaction and its probability
    pub fn with_tx(mut self, tx: OrganicTransaction, prob: f64) -> Self {
        self.tx_probabilities.insert(tx, prob);
        self
    }

    /// Build the `TransactionRandomizer`
    ///
    /// Deploys the organic contract if not provided
    #[instrument(name = "build_tx_randomizer", skip_all)]
    pub async fn build<P: Provider>(self, provider: P) -> Result<TransactionRandomizer> {
        let orgainc_address = {
            if let Some(organic_address) = self.organic_address {
                organic_address
            } else {
                // deploy the organic contract, if not provided
                // some clients does not support `eth_feeHistory` so falling back to legacy gas price
                let gas_price = provider.get_gas_price().await?;
                let organic_address = Organic::deploy_builder(provider)
                    .gas_price(gas_price)
                    .deploy()
                    .await?;
                info!("organic contract deployed at: {}", organic_address);
                organic_address
            }
        };
        let (txs, probabilities): (_, Vec<_>) = self.tx_probabilities.into_iter().unzip();
        let dist: WeightedIndex<f64> = WeightedIndex::new(probabilities)?;

        Ok(TransactionRandomizer {
            organic_address: orgainc_address,
            txs,
            dist,
        })
    }
}

/// Generate random transactions based on the probabilities
#[derive(Debug, Clone)]
pub struct TransactionRandomizer {
    organic_address: Address,
    txs: Vec<OrganicTransaction>,
    dist: WeightedIndex<f64>,
}

impl TransactionRandomizer {
    fn sample_tx(&self) -> OrganicTransaction {
        self.txs[self.dist.sample(&mut rand::thread_rng())]
    }

    #[instrument(name = "random_tx", skip(self), fields(tx))]
    pub fn random(&self, from: Address) -> TransactionRequest {
        let tx = self.sample_tx();
        tracing::Span::current().record("tx", tx.to_string());
        tx.build(from, self.organic_address)
    }
}

/// The transaction type available for generating
#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub enum OrganicTransaction {
    Transfer,
    // erc20
    ERC20Deploy,
    ERC20Mint,
    //erc721
    ERC721Deploy,
    ERC721Mint,
    //erc1155
    ERC1155Deploy,
    ERC1155Mint,
}

impl fmt::Display for OrganicTransaction {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl OrganicTransaction {
    pub fn build(&self, from: Address, organic_address: Address) -> TransactionRequest {
        match self {
            OrganicTransaction::Transfer => TransactionRequest::default()
                .with_from(from)
                .to(from)
                .with_value(U256::from(10))
                .with_gas_limit(21_000),
            OrganicTransaction::ERC20Deploy => {
                let data = Organic::deploy_erc20Call::new(()).abi_encode();
                TransactionRequest::default()
                    .with_from(from)
                    .to(organic_address)
                    .with_input(data)
                    .with_gas_limit(500_000)
            }
            OrganicTransaction::ERC20Mint => {
                let data = Organic::mint_erc20Call::new(()).abi_encode();
                TransactionRequest::default()
                    .with_from(from)
                    .to(organic_address)
                    .with_input(data)
                    .with_gas_limit(50_000)
            }
            OrganicTransaction::ERC721Deploy => {
                let data = Organic::deploy_erc721Call::new(()).abi_encode();
                TransactionRequest::default()
                    .with_from(from)
                    .to(organic_address)
                    .with_input(data)
                    .with_gas_limit(950_000)
            }
            OrganicTransaction::ERC721Mint => {
                let data = Organic::mint_erc721Call::new(()).abi_encode();
                TransactionRequest::default()
                    .with_from(from)
                    .to(organic_address)
                    .with_input(data)
                    .with_gas_limit(80_000)
            }
            OrganicTransaction::ERC1155Deploy => {
                let data = Organic::deploy_erc1155Call::new(()).abi_encode();
                TransactionRequest::default()
                    .with_from(from)
                    .to(organic_address)
                    .with_input(data)
                    .with_gas_limit(1_000_000)
            }
            OrganicTransaction::ERC1155Mint => {
                let data = Organic::mint_erc1155Call::new(()).abi_encode();
                TransactionRequest::default()
                    .with_from(from)
                    .to(organic_address)
                    .with_input(data)
                    .with_gas_limit(50_000)
            }
        }
    }
}
