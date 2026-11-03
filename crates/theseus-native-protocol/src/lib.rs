//! Transport-independent protocol foundation; this crate performs no machine operations.
#![forbid(unsafe_code)]

mod types;
pub use types::{Envelope, Message, ProtocolError, Refusal, Role, SessionId};
