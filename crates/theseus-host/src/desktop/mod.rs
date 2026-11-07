//! Explicit desktop-session admission and original Host-owned executable lifetime.
//!
//! These mechanisms do not install a broker, discover a session, validate an
//! application entry, or implement a native launcher. Durable broker acceptance
//! must precede calling the owned-executable mechanism.
#[cfg(target_os = "linux")]
pub mod linux;
pub mod macos;
pub mod owned;
#[cfg(target_os = "linux")]
pub mod scope;
pub mod session;
pub mod transport;
pub mod windows;
