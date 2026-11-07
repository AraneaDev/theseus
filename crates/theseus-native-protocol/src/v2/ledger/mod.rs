//! Pure canonical records and immutable restart projections; no persistence or effects.
mod codec;
mod replay;
mod types;
pub use codec::{DecodeProgress, Decoder, encode, request_digest};
pub use replay::{LedgerView, ReplayDecision};
pub use types::*;
pub const MAX_RECORD_BODY_BYTES: usize = 16384;
pub const MAX_RECORD_BYTES: usize = 16420;
pub const MAX_LEDGER_BYTES: usize = 1048576;
pub const MAX_RECORDS: usize = 260;
pub const FINISH_RESERVE: usize = 4096;
pub const CLOSE_RESERVE: usize = 8192;
