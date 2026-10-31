//! Scoped registration into the caller's existing cancellation flag.
//!
//! Console close/logoff/shutdown events only request cancellation. Windows may
//! terminate the process when its system-controlled handler deadline expires;
//! registration cannot guarantee application cleanup during those events.
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(unix)]
pub use unix::CancellationRegistration;
#[cfg(windows)]
pub use windows::CancellationRegistration;
