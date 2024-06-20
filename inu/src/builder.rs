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
use tracing::instrument;

sol!(
    #[sol(rpc)]
    Organic,
    "contract_artifacts/Organic.json"
);

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

    pub fn with_txs(mut self, tx_probabilities: HashMap<OrganicTransaction, f64>) -> Self {
        self.tx_probabilities = tx_probabilities;
        self
    }

    pub fn with_tx(mut self, tx: OrganicTransaction, prob: f64) -> Self {
        self.tx_probabilities.insert(tx, prob);
        self
    }

    pub async fn build<P: Provider>(self, provider: P) -> Result<TransactionRandomizer> {
        let orgainc_address = {
            if let Some(organic_address) = self.organic_address {
                organic_address
            } else {
                let organic = Organic::deploy(provider).await?;
                organic.address().clone()
            }
        };
        let (txs, probabilities): (_, Vec<_>) = self
            .tx_probabilities
            .into_iter()
            .map(|(k, v)| (k, v))
            .unzip();
        let dist: WeightedIndex<f64> = WeightedIndex::new(&probabilities)?;

        Ok(TransactionRandomizer {
            organic_address: orgainc_address,
            txs,
            dist,
        })
    }
}

#[derive(Debug, Clone)]
pub struct TransactionRandomizer {
    // organic contract address
    organic_address: Address,
    // hashmap of transactions with their probablity
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
        tx.build(from, self.organic_address.clone())
    }
}

#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq)]
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
