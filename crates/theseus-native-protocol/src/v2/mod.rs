//! Explicit v2 trusted-fixture protocol domain. No host effects or authentication.
mod error;
mod types;
pub use crate::{Role, SessionId};
pub use error::Error;
pub use types::*;
pub const MAX_FRAME_BYTES: usize = 65536;
pub const MAX_BODY_BYTES: usize = 65532;
pub const MAX_FILE_BYTES: usize = 4096;
pub const MAX_PROGRAMS: usize = 8;
pub const MAX_INFLIGHT_REQUESTS: usize = 4;
pub const MAX_OPERATIONS: usize = 128;
pub const MAX_STREAM_BYTES: usize = 16384;
pub const MAX_LEAF_BYTES: usize = 64;
pub const MAX_PROGRAM_ID_BYTES: usize = 32;
pub const MAX_SID_BYTES: usize = 256;

mod frame;
mod json;
pub mod ledger;
mod session;

pub use frame::{DecodeProgress, Decoder, encode};
pub use ledger::request_digest;
pub use session::SessionGuard;

pub mod integration;
pub use integration::*;
