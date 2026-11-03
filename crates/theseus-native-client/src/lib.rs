//! Explicitly supplied local fixture helper, over owned native pipes.
//!
//! Recovery state survives connection loss only within this live Client. For
//! recovery after controller process failure the caller must durably record the
//! expected binding/manifest/marker, lease, operation, complete typed Run or CreateFile request (including path and bytes), and
//! canonical `v2::request_digest` BEFORE submitting the mutation. A fresh Client invents,
//! discovers and reexecutes nothing. Transport settlement is not remote operation
//! cleanup, a lease release receipt, authentication, or guest acceptance.
#![forbid(unsafe_code)]
mod client;
mod error;
mod pump;
mod recovery;
mod transport;
pub use client::{
    Action, Client, ClientMode, Event, ExpectedHelper, Poll, RecoveredOperation, RecoveryImage,
    RequestId, StartFailure,
};
pub use error::{ClientError, Failure};
