//! Explicit local Windows SCM observation and controls on retained original handles.
//! Configuration describes the next start; status and application readiness are separate.
mod model;
pub use model::*;
#[cfg(windows)]
mod native;
#[cfg(windows)]
pub use native::*;

#[cfg(any(windows, test))]
mod buffer;
