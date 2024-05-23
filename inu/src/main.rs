use std::sync::Arc;

use actor::Actor;
use alloy::{
    network::{eip2718::Encodable2718, EthereumSigner, TransactionBuilder},
    node_bindings::Anvil,
    primitives::{hex, Address, ChainId, FixedBytes, U256},
    providers::{Provider, ProviderBuilder},
    rpc::{
        self,
        client::{BatchRequest, ClientBuilder, RpcClient, WsConnect},
        types::eth::TransactionRequest,
    },
    signers::wallet::{coins_bip39::English, MnemonicBuilder},
    transports::http::reqwest::Url,
};
use batcher::RawTxBatcher;
use eyre::Result;
use tokio::{
    sync::{mpsc, Notify},
    task,
};

pub mod actor;
pub mod batcher;
pub mod builder;

async fn execute() -> Result<()> {
    let phrase = "tree unlock also open enroll legal bike success innocent retreat business into reopen loyal couch witness mesh language squeeze subway proof fix predict flush";
    // Create a provider.
    // let rpc_url: Url = "https://op-inu.astar.network".parse()?;
    let rpc_url: Url = "http://localhost:12345".parse()?;
    let client = ClientBuilder::default().http(rpc_url.clone());
    // let rpc_url = WsConnect::new("ws://localhost:12345");
    // let client = ClientBuilder::default().ws(rpc_url.clone()).await?;
    let provider = Arc::new(
        ProviderBuilder::new()
            .with_recommended_fillers()
            .on_http(rpc_url),
    );

    let (tx, rx) = mpsc::channel(2000);
    let alice = Actor::new(&phrase, 0, &provider, tx.clone()).await?;
    let bob = Actor::new(&phrase, 1, &provider, tx).await?;
    let mut batcher = RawTxBatcher::new(rx, client);

    task::spawn(async move {
        loop {
            alice.start().await;
        }
    });

    task::spawn(async move {
        loop {
            bob.start().await;
        }
    });

    task::spawn(async move {
        batcher.start(700).await.unwrap();
    })
    .await?;

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    execute().await?;
    Ok(())
}
