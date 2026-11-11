//! Linux-only dedicated broker child scope. No global process enumeration, PID
//! attachment, or ambient controller subreaper mutation is performed here.
use crate::process::OwnedChild;
use std::{
    io,
    marker::PhantomData,
    process::ExitStatus,
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
};
static CLAIMED: AtomicBool = AtomicBool::new(false);

/// A process-wide capability, retained on the broker's serialized native worker.
/// The broker has at most one active generation and no unrelated child jobs.
/// A completed generation may transfer this token only through explicit retirement.
pub struct DedicatedBrokerScope {
    process: libc::pid_t,
    primary_status: Option<ExitStatus>,
    reaped_descendants: u64,
    joined: bool,
    _worker: PhantomData<Rc<()>>,
}
impl DedicatedBrokerScope {
    /// Establish the capability only at dedicated broker-process startup.
    ///
    /// # Safety
    /// This process must be an explicitly owned, dedicated broker with no other
    /// child creators/reapers, signal handlers that reap children, or concurrent
    /// activation. These invariants must hold until process exit. Do not call in
    /// the controller/helper/test-runner process. The caller must retain this
    /// token with the original activation and must never restore/revoke the
    /// kernel subreaper while unresolved children exist.
    pub unsafe fn establish_for_exclusive_process() -> io::Result<Self> {
        if CLAIMED.swap(true, Ordering::AcqRel) {
            return Err(io::Error::other("dedicated broker scope already claimed"));
        }
        let mut action = std::mem::MaybeUninit::<libc::sigaction>::uninit();
        if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), action.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let action = unsafe { action.assume_init() };
        if action.sa_sigaction != libc::SIG_DFL || action.sa_flags & libc::SA_NOCLDWAIT != 0 {
            return Err(io::Error::other(
                "dedicated broker requires preserved default SIGCHLD disposition",
            ));
        }
        let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
        let result = unsafe {
            libc::waitid(
                libc::P_ALL,
                0,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == 0 {
            return Err(io::Error::other("dedicated broker has a preexisting child"));
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ECHILD) {
            return Err(error);
        }
        let mut previous = 0 as libc::c_int;
        if unsafe { libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut previous) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if previous != 0 {
            return Err(io::Error::other(
                "dedicated broker already had subreaper authority",
            ));
        }
        if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1 as libc::c_int) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            process: unsafe { libc::getpid() },
            primary_status: None,
            reaped_descendants: 0,
            joined: false,
            _worker: PhantomData,
        })
    }
    fn validate_generation_retirement(&self) -> io::Result<()> {
        if !self.joined || self.primary_status.is_none() {
            return Err(io::Error::other(
                "original generation has not joined its child scope",
            ));
        }
        Ok(())
    }
    pub(crate) fn rearm_after_join(&mut self) -> io::Result<()> {
        self.validate_generation_retirement()?;
        if self.process != unsafe { libc::getpid() } {
            return Err(io::Error::other(
                "original scope crossed a process boundary",
            ));
        }
        let mut subreaper = 0 as libc::c_int;
        if unsafe { libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut subreaper) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if subreaper != 1 {
            return Err(io::Error::other("original subreaper authority was revoked"));
        }
        let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
        if unsafe {
            libc::waitid(
                libc::P_ALL,
                0,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        } == 0
        {
            return Err(io::Error::other(
                "new child exists at original generation retirement",
            ));
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ECHILD) {
            return Err(error);
        }
        // The kernel capability is unchanged. Only per-generation accounting
        // resets, after fresh ECHILD and the prior original join receipt.
        self.primary_status = None;
        self.reaped_descendants = 0;
        self.joined = false;
        Ok(())
    }
    pub fn reaped_descendants(&self) -> u64 {
        self.reaped_descendants
    }
    pub fn primary_exit_status(&self) -> Option<ExitStatus> {
        self.primary_status
    }
    /// Bounded child-wait polling; ECHILD is authoritative only under the
    /// exclusive-process contract. An escaped but live original descendant
    /// remains an adopted child and keeps the scope pending, never complete.
    pub(crate) fn try_join(&mut self, child: &mut OwnedChild) -> io::Result<Option<ExitStatus>> {
        if self.process != unsafe { libc::getpid() } {
            return Err(io::Error::other(
                "dedicated broker scope crossed a process boundary",
            ));
        }
        if self.joined {
            return Ok(self.primary_status);
        }
        if self.primary_status.is_none() {
            child.terminate_owned()?;
            if !child.exited_reserved()? {
                return Ok(None);
            }
            self.primary_status = child.try_reap_and_release()?;
            if self.primary_status.is_none() {
                return Ok(None);
            }
        }
        for _ in 0..64 {
            let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
            if unsafe { libc::waitid(libc::P_ALL, 0, &mut info, libc::WEXITED | libc::WNOHANG) }
                != 0
            {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ECHILD) {
                    self.joined = true;
                    return Ok(self.primary_status);
                }
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if unsafe { info.si_pid() } == 0 {
                return Ok(None);
            }
            self.reaped_descendants = self.reaped_descendants.saturating_add(1);
        }
        Ok(None)
    }
}

#[cfg(test)]
mod generation_tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    fn scope(joined: bool, status: bool) -> DedicatedBrokerScope {
        DedicatedBrokerScope {
            process: std::process::id() as libc::pid_t,
            primary_status: status.then(|| ExitStatus::from_raw(0)),
            reaped_descendants: 0,
            joined,
            _worker: PhantomData,
        }
    }
    #[test]
    fn generation_rearm_requires_original_join_and_primary_receipt() {
        assert!(
            scope(false, false)
                .validate_generation_retirement()
                .is_err()
        );
        assert!(scope(false, true).validate_generation_retirement().is_err());
        assert!(scope(true, false).validate_generation_retirement().is_err());
        assert!(scope(true, true).validate_generation_retirement().is_ok());
    }
}
