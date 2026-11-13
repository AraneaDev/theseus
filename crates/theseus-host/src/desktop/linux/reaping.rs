//! Original reserved GIO child wait/reap primitives (unsafe libc).
use super::{NativePrimaryState, invalid};
use std::io;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ChildWait {
    Pending,
    Running,
    Exited { code: i32, status: i32 },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ChildReservation {
    Running,
    Exited { code: i32, status: i32 },
    Reaped,
    Unknown,
}
pub(super) struct OriginalInvocation {
    pub(super) process: libc::pid_t,
}
/// No public PID import, serialization, process open or signal authority.
pub(super) struct OriginalGioChild {
    pub(super) pid: libc::pid_t,
    process: libc::pid_t,
    pub(super) state: ChildReservation,
}
impl OriginalGioChild {
    pub(super) fn retain_callback(invocation: &OriginalInvocation, pid: libc::pid_t) -> Self {
        Self {
            pid,
            process: invocation.process,
            state: ChildReservation::Running,
        }
    }
    pub(super) fn poll_with(
        &mut self,
        wait: impl FnOnce(libc::pid_t) -> io::Result<ChildWait>,
    ) -> io::Result<NativePrimaryState> {
        match self.state {
            ChildReservation::Reaped => return Ok(NativePrimaryState::Exited),
            ChildReservation::Unknown => return Ok(NativePrimaryState::Unknown),
            _ => {}
        }
        let previous = self.state;
        match wait(self.pid) {
            Ok(ChildWait::Pending) => Ok(if matches!(previous, ChildReservation::Exited { .. }) {
                NativePrimaryState::Exited
            } else {
                NativePrimaryState::Running
            }),
            Ok(ChildWait::Running) if matches!(previous, ChildReservation::Exited { .. }) => {
                self.state = ChildReservation::Unknown;
                Err(invalid("original reserved exit regressed"))
            }
            Ok(ChildWait::Running) => Ok(NativePrimaryState::Running),
            Ok(ChildWait::Exited { code, status }) if matches!(previous,ChildReservation::Exited {code:old,status:old_status} if old!=code||old_status!=status) =>
            {
                self.state = ChildReservation::Unknown;
                Err(invalid("original reserved exit identity/status changed"))
            }
            Ok(ChildWait::Exited { code, status }) => {
                self.state = ChildReservation::Exited { code, status };
                Ok(NativePrimaryState::Exited)
            }
            Err(error) => {
                self.state = ChildReservation::Unknown;
                Err(error)
            }
        }
    }
    pub(super) fn poll(&mut self) -> io::Result<NativePrimaryState> {
        if let Err(error) = verify_wait_contract(self.process) {
            self.state = ChildReservation::Unknown;
            return Err(error);
        }
        self.poll_with(wait_reserved_child)
    }
    pub(super) fn reap_with(
        &mut self,
        reap: impl FnOnce(libc::pid_t) -> io::Result<Option<i32>>,
    ) -> io::Result<bool> {
        let (code, expected) = match self.state {
            ChildReservation::Reaped => return Ok(true),
            ChildReservation::Exited { code, status } => (code, status),
            _ => return Ok(false),
        };
        let result = (|| {
            let Some(status) = reap(self.pid)? else {
                return Ok(false);
            };
            let same = if code == libc::CLD_EXITED {
                libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == expected
            } else {
                libc::WIFSIGNALED(status) && libc::WTERMSIG(status) == expected
            };
            if !same {
                return Err(invalid(
                    "original GIO child reap differs from reserved exit",
                ));
            }
            Ok(true)
        })();
        match result {
            Ok(true) => self.state = ChildReservation::Reaped,
            Err(_) => self.state = ChildReservation::Unknown,
            _ => {}
        }
        result
    }
    pub(super) fn reap(&mut self) -> io::Result<bool> {
        if let Err(error) = verify_wait_contract(self.process) {
            self.state = ChildReservation::Unknown;
            return Err(error);
        }
        self.reap_with(reap_reserved_child)
    }
}
// Private primitives accept only internal invocation-reserved children; no
// public ownership import exists. Source fixtures exercise syscall semantics
// with their separately held std::process::Child, never as GIO activation.
pub(super) fn wait_reserved_child(pid: libc::pid_t) -> io::Result<ChildWait> {
    let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
    if unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    } != 0
    {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(ChildWait::Pending);
        }
        return Err(error);
    }
    let observed = unsafe { info.si_pid() };
    if observed == 0 {
        return Ok(ChildWait::Running);
    }
    if observed != pid
        || !matches!(
            info.si_code,
            libc::CLD_EXITED | libc::CLD_KILLED | libc::CLD_DUMPED
        )
    {
        return Err(invalid("original GIO child wait identity/status unproved"));
    }
    Ok(ChildWait::Exited {
        code: info.si_code,
        status: unsafe { info.si_status() },
    })
}
pub(super) fn reap_reserved_child(pid: libc::pid_t) -> io::Result<Option<i32>> {
    let mut status = 0;
    let observed = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
    if observed < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(None);
        }
        return Err(error);
    }
    if observed == 0 {
        return Ok(None);
    }
    if observed != pid {
        return Err(invalid("original GIO child reap identity unproved"));
    }
    Ok(Some(status))
}
pub(super) fn verify_wait_contract(process: libc::pid_t) -> io::Result<()> {
    if process != unsafe { libc::getpid() } {
        return Err(invalid("original GIO broker process changed"));
    }
    let mut action = std::mem::MaybeUninit::<libc::sigaction>::uninit();
    if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), action.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let action = unsafe { action.assume_init() };
    if action.sa_sigaction != libc::SIG_DFL || action.sa_flags & libc::SA_NOCLDWAIT != 0 {
        return Err(invalid(
            "original GIO child requires unchanged default SIGCHLD",
        ));
    }
    Ok(())
}
pub(super) fn require_no_existing_children() -> io::Result<()> {
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
        return Err(invalid(
            "GIO dedicated broker has an unrelated/preexisting child",
        ));
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ECHILD) {
        Ok(())
    } else {
        Err(error)
    }
}
