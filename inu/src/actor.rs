use alloy::{
    consensus::TxEnvelope,
    network::{Ethereum, EthereumSigner, TransactionBuilder},
    primitives::{utils::parse_ether, Address, U256},
    providers::{
        fillers::{ChainIdFiller, FillProvider, GasFiller, JoinFill, NonceFiller, SignerFiller},
        Provider, ProviderBuilder, RootProvider,
    },
    rpc::types::eth::TransactionRequest,
    signers::wallet::{coins_bip39::English, MnemonicBuilder},
    transports::{BoxTransport, Transport},
};
use eyre::{eyre, Result};
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::{mpsc, Mutex, Notify},
    task::JoinSet,
    time,
};

use crate::builder::TxBuilder;

type RecommendedProvider = FillProvider<
    JoinFill<
        JoinFill<
            JoinFill<JoinFill<alloy::providers::Identity, GasFiller>, NonceFiller>,
            ChainIdFiller,
        >,
        SignerFiller<EthereumSigner>,
    >,
    RootProvider<BoxTransport>,
    BoxTransport,
    Ethereum,
>;

#[derive(Debug, Clone)]
pub struct TxSendRequest {
    pub transaction: TxEnvelope,
    pub reset_notify: Arc<Notify>,
}

#[derive(Clone, Debug)]
pub struct Actor<T: Transport + Clone, P: Provider<T> + Clone + 'static> {
    pub inner: Arc<ActorInner<T, P>>,
}

impl<T: Transport + Clone, P: Provider<T> + Clone + 'static> Actor<T, P> {
    pub async fn new(
        address: Address,
        signer: Arc<EthereumSigner>,
        provider: Arc<P>,
        sender: mpsc::Sender<TxSendRequest>,
    ) -> Result<Actor<T, P>> {
        let inner = Arc::new(ActorInner::new(address, signer, provider, sender).await?);
        Ok(Actor { inner })
    }

    pub fn start(&self, set: &mut JoinSet<()>) {
        let inner1 = self.inner.clone();
        set.spawn(async move {
            loop {
                inner1.send_tx().await.unwrap();
            }
        });

        let inner2 = self.inner.clone();
        set.spawn(async move {
            inner2.listen_reset().await;
        });
    }
}

#[derive(Clone, Debug)]
pub struct ActorInner<T: Transport + Clone, P: Provider<T>> {
    pub address: Address,
    pub signer: Arc<EthereumSigner>,
    pub nonce: Arc<Mutex<u64>>,
    pub provider: Arc<P>,
    pub chain_id: u64,
    sender: mpsc::Sender<TxSendRequest>,
    reset_notify: Arc<Notify>,
    _phantom: std::marker::PhantomData<T>,
}

impl<T: Transport + Clone, P: Provider<T>> ActorInner<T, P> {
    pub async fn new(
        address: Address,
        signer: Arc<EthereumSigner>,
        provider: Arc<P>,
        sender: mpsc::Sender<TxSendRequest>,
    ) -> Result<ActorInner<T, P>> {
        println!("Actor address: {}", address);
        let nonce = Arc::new(Mutex::new(provider.get_transaction_count(address).await?));
        let chain_id = provider.get_chain_id().await?;

        Ok(ActorInner {
            address,
            signer,
            nonce,
            chain_id,
            provider: provider.clone(),
            sender,
            reset_notify: Arc::new(Notify::new()),
            _phantom: std::marker::PhantomData,
        })
    }

    /// TODO: take pending/in-flight transactions in account
    pub async fn reset_nonce(&self) -> Result<()> {
        println!("Resetting nonce");
        let latest_nonce = self.provider.get_transaction_count(self.address).await?;
        *self.nonce.lock().await = latest_nonce;
        Ok(())
    }

    pub async fn next_tx(&self) -> Result<TxEnvelope> {
        let mut nonce_lock = self.nonce.lock().await;
        let nonce = *nonce_lock;
        *nonce_lock += 1;
        drop(nonce_lock);

        let res = Ok(TxBuilder::build(self.address)
            .with_nonce(nonce)
            .with_chain_id(self.chain_id)
            .with_gas_limit(21_000)
            .with_max_priority_fee_per_gas(1_000_000_000)
            .with_max_fee_per_gas(20_000_000_000)
            .build(self.signer.as_ref())
            .await?);
        res
    }

    pub async fn send_tx(&self) -> Result<()> {
        let rlp_tx = self.next_tx().await?;

        self.sender
            .send(TxSendRequest {
                transaction: rlp_tx,
                reset_notify: self.reset_notify.clone(),
            })
            .await?;
        Ok(())
    }

    pub async fn listen_reset(&self) {
        let future = self.reset_notify.notified();
        tokio::pin!(future);

        loop {
            future.as_mut().await;
            // attempt to reset nonce
            let _ = self.reset_nonce().await;
            time::sleep(Duration::from_secs(2)).await;
        }
    }
}

pub struct ActorManager {
    master: Address,
    provider: Arc<RecommendedProvider>,
    actors: Vec<Actor<BoxTransport, RecommendedProvider>>,
    tasks: JoinSet<()>,
}

const MAX_TPS_PER_ACTOR: u64 = 50;

impl ActorManager {
    /// Build the multi signer with master and actor address
    /// Returns the signer, master address and actor addresses
    async fn build_signer(
        phrase: &str,
        num_actors: u32,
    ) -> Result<(EthereumSigner, Address, Vec<Address>)> {
        let master = MnemonicBuilder::<English>::default()
            .phrase(phrase)
            .index(0)
            .unwrap()
            .build()?;
        let master_address = master.address();
        let mut actor_address = vec![];
        let mut signer = EthereumSigner::new(master);

        for i in 1..(num_actors + 1) {
            let wallet = MnemonicBuilder::<English>::default()
                .phrase(phrase)
                .index(i)
                .unwrap()
                .build()?;
            actor_address.push(wallet.address());
            signer.register_signer(wallet);
        }

        Ok((signer, master_address, actor_address))
    }

    pub async fn init_actors(
        phrase: &str,
        max_tps: u64,
        rpc_url: &str,
        sender: mpsc::Sender<TxSendRequest>,
    ) -> Result<ActorManager> {
        let num_accounts = (max_tps as f64 / MAX_TPS_PER_ACTOR as f64).ceil() as u32;
        // first account is master account and only used to topup other actors
        let (signer, master, actor_addresses) = Self::build_signer(phrase, num_accounts).await?;
        let signer_clone = Arc::new(signer.clone());
        //sanity check
        assert_eq!(num_accounts, actor_addresses.len() as u32);

        let provider = Arc::new(
            ProviderBuilder::new()
                .with_recommended_fillers()
                .filler(SignerFiller::new(signer))
                .on_builtin(&rpc_url)
                .await?,
        );

        let mut actors = Vec::new();
        for addr in actor_addresses {
            let actor =
                Actor::new(addr, signer_clone.clone(), provider.clone(), sender.clone()).await?;
            actors.push(actor);
        }

        let tasks = JoinSet::new();
        let manager = ActorManager {
            actors,
            tasks,
            master,
            provider: provider.clone(),
        };

        // send funds
        manager.send_funds().await?;
        Ok(manager)
    }

    pub fn spawn(&mut self) {
        for actor in self.actors.iter() {
            actor.start(&mut self.tasks);
        }
    }

    pub async fn send_funds(&self) -> Result<()> {
        println!("Sending funds..");
        // send funds to actors
        let total = self
            .provider
            .get_balance(self.master)
            .await?
            // save 0.1 for later other operations
            // TODO: better error handling
            .checked_sub(parse_ether("0.1")?)
            .ok_or(eyre!("Insufficient balance"))?;

        // we need to send funds to all actors while retaining equal part in master account
        let amount: U256 = total.div_ceil(U256::from(self.actors.len() + 1));
        let mut futs = vec![];
        for actor in self.actors.iter() {
            let tx = TransactionRequest::default()
                .from(self.master)
                .to(actor.inner.address)
                .value(amount);
            futs.push(self.provider.send_transaction(tx).await?.get_receipt());
        }

        for fut in futs {
            fut.await?;
            println!("one of funds to be sent..");
        }
        println!("Funds sent");
        Ok(())
    }

    pub async fn shutdown(&mut self) {
        self.tasks.abort_all();
        println!("Attempting to return funds before shutdown..");
        for actor in self.actors.drain(..) {
            loop {
                // wait for some time to get pending tx to be processed
                time::sleep(Duration::from_secs(5)).await;

                let mut balance = match self.provider.get_balance(actor.inner.address).await {
                    Ok(b) => b,
                    Err(e) => {
                        eprintln!("Failed to get balance for actor: {:?}", e);
                        continue;
                    }
                };

                // safe to unwrap since we know parse ether won't fail.
                balance = balance.saturating_sub(parse_ether("0.0001").unwrap());
                if balance.is_zero() {
                    // no need to trasnfer anything
                    break;
                }

                let tx = TransactionRequest::default()
                    .from(actor.inner.address)
                    .to(self.master)
                    .value(balance);

                match self.provider.send_transaction(tx).await {
                    Ok(fut) => match fut.watch().await {
                        Ok(_) => {
                            println!("Funds returned for actor: {}", actor.inner.address);
                            break;
                        }
                        Err(e) => {
                            eprintln!("Failed to transfer funds, tx reverted: {:?}", e);
                            eprintln!("Attempting again in 5s");
                            continue;
                        }
                    },
                    Err(e) => {
                        eprintln!("Failed to transfer funds: {:?}", e);
                        continue;
                    }
                }
            }
        }
    }
}
