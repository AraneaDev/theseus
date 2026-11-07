//! Explicit-domain, provider-qualified launchd observation and original runtime control.
//! Native execution is macOS-only; the finite parser/model is exercised on Linux.
mod command;
mod model;
mod ownership;
mod parser;
pub use model::*;
pub use ownership::RegistrationOwner;
pub use parser::normalize_print;
mod native;
pub use native::*;
