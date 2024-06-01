use alloy::{
    network::Network,
    primitives::U128,
    providers::{Provider, RootProvider},
    rpc::client::{PollerBuilder, WeakClient},
    transports::{utils::Spawnable, Transport},
};
use std::{
    ops::{Deref, DerefMut},
    time::Duration,
};
use tokio::sync::watch;
use tokio_stream::{wrappers::WatchStream, StreamExt};
use tracing::{debug, debug_span, Instrument};

/// Poller task for fetching gas price.
pub struct GasPricePoller<T> {
    init_value: u128,
    poll_task: PollerBuilder<T, (), U128>,
}

impl<T: Transport + Clone> GasPricePoller<T> {
    pub fn from_root<N: Network>(p: &RootProvider<T, N>) -> Self {
        Self::new(p.weak_client())
    }

    pub fn new(client: WeakClient<T>) -> Self {
        let init_value = Default::default();
        Self {
            poll_task: PollerBuilder::new(client, "eth_gasPrice", ()),
            init_value,
        }
    }

    pub fn with_init_value(mut self, init_value: u128) -> Self {
        self.init_value = init_value;
        self
    }

    pub fn with_poll_interval(mut self, poll_interval: Duration) -> Self {
        self.poll_task.set_poll_interval(poll_interval);
        self
    }

    pub fn spawn(self) -> GasPriceChannel {
        let (tx, rx) = watch::channel(self.init_value);
        let span = debug_span!("gas_poller", init_value = %self.init_value);

        let fut = async move {
            let mut poll_task = self.poll_task.spawn().into_stream().map(|p| p.to::<u128>());
            'main: while let Some(price) = poll_task.next().await {
                // debug!("gas price updated: {}", price);
                if tx.send(price).is_err() {
                    debug!("channel closed");
                    break 'main;
                }
            }
        };

        fut.instrument(span).spawn_task();
        rx.into()
    }
}

#[derive(Debug, Clone)]
pub struct GasPriceChannel {
    rx: watch::Receiver<u128>,
}

impl From<watch::Receiver<u128>> for GasPriceChannel {
    fn from(rx: watch::Receiver<u128>) -> Self {
        Self { rx }
    }
}

impl Deref for GasPriceChannel {
    type Target = watch::Receiver<u128>;

    fn deref(&self) -> &Self::Target {
        &self.rx
    }
}

impl DerefMut for GasPriceChannel {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.rx
    }
}

impl GasPriceChannel {
    pub fn into_stream(self) -> WatchStream<u128> {
        self.rx.into()
    }
}
