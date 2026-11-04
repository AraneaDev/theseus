//! Evidence foundation for the Theseus installation lifecycle workbench.
#[cfg(test)]
extern crate self as theseus;
pub mod checks;
pub mod cli;
pub mod comparison;
pub mod engine;
pub mod model;
pub mod native;
pub mod preview;
pub mod recipe;
pub mod report;
pub mod runner;
pub mod snapshot;
pub mod target_authority;
pub mod tui;

pub(crate) mod shell;

pub mod mcp;
mod mcp_server;
mod strict_json;

#[cfg(feature = "mcp-test-support")]
pub use mcp_server::coordinator::fixture::main as mcp_fixture_main;
