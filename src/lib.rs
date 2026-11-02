//! Evidence foundation for the Theseus installation lifecycle workbench.
pub mod checks;
pub mod cli;
pub mod comparison;
pub mod engine;
pub mod model;
pub mod preview;
pub mod recipe;
pub mod report;
pub mod runner;
pub mod snapshot;
pub mod tui;

pub(crate) mod shell;

pub mod mcp;
#[allow(dead_code, reason = "MCP core is integrated by subsequent roadmap tasks")]
mod mcp_server;
mod strict_json;
