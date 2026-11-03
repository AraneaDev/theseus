//! Persistence receipts attest observed file barriers, never effect authority.
#![forbid(unsafe_code)]
mod bootstrap;
mod error;
mod intent;
mod store;
pub use bootstrap::{BootstrapReceipt, BootstrapRequest, BootstrapScope, bootstrap};
pub use error::{IntentError, StoreError};
pub use intent::{IntentRecord, IntentStore, PreparedAcquire};
pub use store::{CommittedRecord, LedgerStore, StoreBinding};
