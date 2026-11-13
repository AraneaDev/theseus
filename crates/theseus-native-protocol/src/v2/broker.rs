//! Durable original desktop operations in the existing native ledger domain.
//! These serializable records are diagnostic knowledge, never native authority.
mod entry_retirement;
mod generation;
pub use entry_retirement::*;
mod native_lifetime;
mod owned_lifetime;
use super::{
    BindingId, Digest, Error, IntentId, LeaseId, LocalEndpoint, OperationId, ResourceSlot, SidText,
    WorkspaceMarker,
    ledger::{Entry, LedgerError, Record},
};
pub use generation::*;
pub use native_lifetime::*;
pub use owned_lifetime::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
mod bind;
mod desktop;
mod header;
mod ledger_view;
mod state;
pub use bind::*;
pub use desktop::*;
pub use header::*;
pub use ledger_view::*;
pub use state::*;
