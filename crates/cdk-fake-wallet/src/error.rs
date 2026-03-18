//! Fake Wallet Error

use thiserror::Error;

/// Fake Wallet Error
#[derive(Debug, Error)]
pub enum Error {
    /// Invoice amount not defined
    #[error("Unknown invoice amount")]
    UnknownInvoiceAmount,
    /// Unknown invoice
    #[error("Unknown invoice")]
    UnknownInvoice,
    /// Unknown invoice
    #[error("No channel receiver")]
    NoReceiver,
    /// Payment not found
    #[error("Payment not found: {0}")]
    PaymentNotFound(String),
    /// Payment already approved
    #[error("Payment already approved: {0}")]
    PaymentAlreadyApproved(String),
    /// Invalid arbitrary request format
    #[error("Invalid arbitrary request format: {0}")]
    InvalidArbitraryRequest(String),
}

impl From<Error> for cdk_common::payment::Error {
    fn from(e: Error) -> Self {
        Self::Lightning(Box::new(e))
    }
}
