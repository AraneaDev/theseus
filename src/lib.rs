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
mod strict_json;
