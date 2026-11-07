#![forbid(unsafe_code)]
mod contract;
mod error;
mod files;
mod invocation;
pub use error::{InvocationError, RuntimeError};
pub use invocation::{CatalogEntry, Invocation};

mod cache;
mod dispatcher;
mod frames;
mod pump;
mod runtime;
mod worker;
pub use runtime::{Poll, Runtime, StartFailure};

mod operation;

pub use contract::OwnedWorkerFailure as WorkerFailure;

mod config;
pub use config::{ConfigError, config_manifest_digest, load_config};

#[cfg(feature = "test-support")]
pub mod test_support;

pub mod broker;
pub mod broker_transport;
mod integration;

#[cfg(feature = "test-support")]
pub use config::{load_source_desktop_config, source_desktop_config_manifest_digest};
