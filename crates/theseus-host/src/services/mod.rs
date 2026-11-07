//! Explicitly admitted service-manager mechanisms. No default manager discovery.
#[cfg(target_os = "linux")]
pub mod systemd;

mod endpoint;

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub mod readiness;

#[cfg(any(target_os = "macos", test))]
pub mod launchd;

#[cfg(any(windows, test))]
pub mod scm;
