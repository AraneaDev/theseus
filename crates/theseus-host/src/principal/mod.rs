//! Facts about the effective caller, not authentication or permission guarantees.
//!
//! In particular, a Windows thread token does not establish the identity used by
//! `CreateProcessW`, which uses the process token.
use std::io;

#[cfg(unix)]
mod unix;
#[cfg(any(windows, test))]
mod windows;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CurrentPrincipal {
    Posix {
        effective_uid: u32,
        effective_gid: u32,
    },
    Windows {
        token_user_sid: String,
    },
}

/// Capture the current effective Unix IDs or Windows token user SID.
///
/// Windows queries the thread token first, falling back to the process token
/// only when the thread has no token. Other native failures remain errors.
pub fn current_principal() -> io::Result<CurrentPrincipal> {
    #[cfg(unix)]
    return unix::current_principal();
    #[cfg(windows)]
    return windows::native::current_principal();
    #[cfg(not(any(unix, windows)))]
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "principal query unsupported",
    ))
}
