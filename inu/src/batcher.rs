use std::sync::Arc;

use alloy::{
    hex,
    network::eip2718::Encodable2718,
    primitives::FixedBytes,
    rpc::client::{BatchRequest, RpcClient},
    transports::Transport,
};
use eyre::Result;
use tokio::{
    sync::mpsc,
    task::{self, JoinHandle, JoinSet},
    time,
};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};

use crate::actor::TxSendRequest;

pub struct RawTxBatcher<T: Transport + Clone> {
    rx_stream: ReceiverStream<TxSendRequest>,
    client: Arc<RpcClient<T>>,
}

impl<T: Transport + Clone> RawTxBatcher<T> {
    pub fn new(rx: mpsc::Receiver<TxSendRequest>, client: RpcClient<T>) -> Self {
        Self {
            rx_stream: ReceiverStream::new(rx),
            client: Arc::new(client),
        }
    }

    pub async fn send_batch(client: Arc<RpcClient<T>>, requests: Vec<TxSendRequest>) -> Result<()> {
        let mut batch = BatchRequest::new(client.get_ref());
        for tx_req in requests.into_iter() {
            // println!("{:?}", tx.transaction);
            let w = batch.add_call::<_, FixedBytes<32>>(
                "eth_sendRawTransaction",
                &(hex::encode_prefixed(tx_req.transaction.encoded_2718()),),
            )?;
            task::spawn(async move {
                let res = w.await;
                match &res {
                    Ok(hash) => {
                        println!("{:?}", hash);
                    }
                    Err(e) => {
                        println!("{}", e);
                        tx_req.reset_notify.notify_one();
                    }
                }
            });
        }
        // let _ = task::spawn(async { batch.send().await });
        batch.send().await?;
        Ok(())
    }

    pub async fn start(&mut self, max_tps: u64) -> Result<()> {
        let mut interval = time::interval(time::Duration::from_secs(1));
        let mut buffer = vec![];
        while let Some(tx) = self.rx_stream.next().await {
            // println!("{:?}", tx.transaction);
            // self.client
            //     .request::<_, FixedBytes<32>>(
            //         "eth_sendRawTransaction",
            //         &(hex::encode_prefixed(tx.transaction.encoded_2718()),),
            //     )
            //     .await?;

            buffer.push(tx);
            if buffer.len() >= max_tps as usize {
                let mut handles = vec![];
                for chunks in buffer.chunks(100).into_iter() {
                    let client = self.client.clone();
                    handles.push(task::spawn(Self::send_batch(client, chunks.to_vec())));
                }
                // order matters for nonce
                for handle in handles.into_iter() {
                    let _ = handle.await?;
                }
                // println!("{:#?}", buffer);
                buffer.clear();
                interval.tick().await;
            }
        }

        Ok(())
    }
}
