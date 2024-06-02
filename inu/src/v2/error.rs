use alloy::rpc::json_rpc::ErrorPayload;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Clone)]
pub enum TxRpcError {
    /// The nonce provided is too low, means already used by another transaction
    #[error("nonce too low")]
    NonceTooLow,
    /// Insufficient funds left for the transaction
    #[error("insufficient funds for gas * price + value")]
    InsufficientFunds,
    /// The transaction is already in the mempool but not yet included in a block
    /// This could mean it's either stuck duw to low gas price or lower nonce is not confirmed
    #[error("transaction already imported")]
    AlreadyImported,
    /// The replacement transaction, the tx with same nonce already present in mempool
    /// has either same or low gas price
    #[error("replacement transaction underpriced")]
    Underpriced,
    /// Other errors not covered by the above
    #[error("unknow error: code: {code}, message: {messgae}")]
    Other { code: i64, messgae: String },
}

impl From<ErrorPayload> for TxRpcError {
    fn from(payload: ErrorPayload) -> Self {
        let msg = payload.message.to_lowercase();
        if msg.contains("nonce too low") {
            TxRpcError::NonceTooLow
        } else if msg.contains("insufficient funds") {
            TxRpcError::InsufficientFunds
        } else if msg.contains("already imported") {
            TxRpcError::AlreadyImported
        } else if msg.contains("replacement transaction underpriced") {
            TxRpcError::Underpriced
        } else {
            TxRpcError::Other {
                code: payload.code,
                messgae: payload.message,
            }
        }
    }
}
