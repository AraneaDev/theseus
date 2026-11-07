//! Explicit authenticated local broker transport and standalone serving adapter.
mod config;
mod connection;
#[cfg(target_os = "macos")]
mod entry_macos;
#[cfg(windows)]
mod entry_windows;
pub(crate) mod generation;
#[cfg(test)]
pub(crate) mod generation_tests;
mod launch;
#[cfg(target_os = "linux")]
pub(crate) mod native_linux;
#[cfg(target_os = "linux")]
mod native_linux_factory;
#[cfg(target_os = "macos")]
pub(crate) mod native_macos;
pub(crate) mod native_policy;
#[cfg(windows)]
pub(crate) mod native_windows;
pub(crate) mod owned_entry;
mod server;
pub use config::BrokerServerConfig;
pub use connection::{BrokerConnection, ConnectionFailure};
pub use server::{BrokerServer, ServerPoll, ServerStartFailure};
#[cfg(test)]
pub(crate) mod native_routing_tests;
#[cfg(all(test, feature = "test-support"))]
mod tests;

#[cfg(target_os = "linux")]
pub use native_linux_factory::{
    LinuxFactoryInitializer, QualifiedLinuxBrokerConfig, StockBrokerConfig,
    qualified_linux_sdk_images,
};
