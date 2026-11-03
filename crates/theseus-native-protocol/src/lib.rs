//! Transport-independent protocol foundation; this crate performs no machine operations.
#![forbid(unsafe_code)]

mod frame;
mod json;
mod strict;
mod types;
pub use frame::{DecodeProgress, Decoder, MAX_BODY_BYTES, MAX_FRAME_BYTES, encode};
pub use types::{Envelope, Message, ProtocolError, Refusal, Role, SessionId};
mod handshake;
pub use handshake::HandshakeGuard;

mod framing;
pub mod v2;
