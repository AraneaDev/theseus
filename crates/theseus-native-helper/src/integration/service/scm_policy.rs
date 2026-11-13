//! Pure SCM policy seams shared by the native SCM adapter and host-independent tests.
use std::io;

// Shared policy seams used by the native SCM adapter and host-independent
// sequence regressions; the actual observations still come from its SC_HANDLE.
pub(super) fn pin_scm_activation_pid(original: &mut Option<u32>, observed: u32) -> io::Result<()> {
    if observed == 0 || original.is_some_and(|pid| pid != observed) {
        return Err(io::Error::other("original SCM activation PID changed"));
    }
    if original.is_none() {
        *original = Some(observed);
    }
    Ok(())
}
pub(super) fn scm_uninstall_allows_handle_close(state: Option<u32>) -> bool {
    state == Some(1)
}
// This initial profile admits only a settled native stopped/running observation.
// Paused can retain a live process and cannot prove the shared Stopped predicate.
pub(super) fn scm_runtime_complete(state: u32) -> bool {
    matches!(state, 1 | 4)
}
