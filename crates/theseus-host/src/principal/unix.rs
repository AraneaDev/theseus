use super::CurrentPrincipal;
use std::io;

pub(super) fn current_principal() -> io::Result<CurrentPrincipal> {
    Ok(CurrentPrincipal::Posix {
        effective_uid: rustix::process::geteuid().as_raw(),
        effective_gid: rustix::process::getegid().as_raw(),
    })
}
