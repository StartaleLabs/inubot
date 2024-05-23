use alloy::{
    network::TransactionBuilder,
    primitives::{Address, U256},
    rpc::types::eth::TransactionRequest,
};

pub struct TxBuilder;
impl TxBuilder {
    pub fn build(from: Address) -> TransactionRequest {
        TransactionRequest::default()
            .with_from(from)
            .to(from)
            .with_value(U256::from(10))
    }
}
