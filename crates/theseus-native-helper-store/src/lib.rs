//! Persistence receipts attest observed file barriers, never effect authority.
#![forbid(unsafe_code)]
mod bootstrap;
mod error;
mod intent;
mod qualification;
mod store;
pub use bootstrap::{
    BootstrapReceipt, BootstrapRequest, BootstrapScope, bootstrap, bootstrap_declared_integration,
    bootstrap_integration, bootstrap_native, bootstrap_provider_broker,
    bootstrap_provider_declared_integration,
};
pub use error::{IntentError, StoreError};
pub use intent::{IntentRecord, IntentStore, PreparedAcquire, VerifiedOriginalIntent};
pub use qualification::{ProviderLinuxGioQualification, VerifiedLinuxGioBootstrap};
pub use store::{CommittedRecord, LedgerStore, StoreBinding};
