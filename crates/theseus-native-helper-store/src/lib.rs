//! Persistence receipts attest observed file barriers, never effect authority.
#![forbid(unsafe_code)]
mod bootstrap;
mod error;
mod intent;
mod store;
pub use bootstrap::{
    BootstrapReceipt, BootstrapRequest, BootstrapScope, bootstrap, bootstrap_integration,
    bootstrap_native,
};
pub use error::{IntentError, StoreError};
pub use intent::{IntentRecord, IntentStore, PreparedAcquire, VerifiedOriginalIntent};
pub use store::{CommittedRecord, LedgerStore, StoreBinding};
