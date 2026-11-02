use super::{ChildEnvironment, NativeCommand, SpawnFailure};
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};

#[derive(Debug)]
pub struct OwnedChild {
    child: Child,
    // None after any consuming wait or ECHILD: numeric identity is no longer reserved.
    group: Option<Pid>,
    termination_requested: bool,
    status: Option<ExitStatus>,
}
impl OwnedChild {
    /// Shared null/file launch primitive for owned endpoints and mechanism tests.
    #[doc(hidden)]
    pub fn spawn_files(
        command: &NativeCommand,
        stdin: Option<File>,
        stdout: Option<File>,
        stderr: Option<File>,
    ) -> Result<Self, SpawnFailure> {
        Self::spawn_files_with_session(
            command,
            stdin,
            stdout,
            stderr,
            #[cfg(feature = "test-support")]
            false,
        )
    }

    fn spawn_files_with_session(
        command: &NativeCommand,
        stdin: Option<File>,
        stdout: Option<File>,
        stderr: Option<File>,
        #[cfg(feature = "test-support")] session: bool,
    ) -> Result<Self, SpawnFailure> {
        command.validate().map_err(SpawnFailure::never)?;
        let mut native = Command::new(&command.program);
        native
            .args(&command.args)
            .stdin(stdin.map_or_else(Stdio::null, Stdio::from))
            .stdout(stdout.map_or_else(Stdio::null, Stdio::from))
            .stderr(stderr.map_or_else(Stdio::null, Stdio::from));
        #[cfg(feature = "test-support")]
        if session {
            // Only async-signal-safe syscalls run after fork. std has already
            // installed fd 0, our slave. Do not make a group leader before setsid.
            unsafe {
                native.pre_exec(|| {
                    if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::tcsetpgrp(0, libc::getpid()) == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        } else {
            native.process_group(0);
        }
        #[cfg(not(feature = "test-support"))]
        native.process_group(0);
        if let Some(cwd) = &command.cwd {
            native.current_dir(cwd);
        }
        if let ChildEnvironment::Replace(entries) = &command.environment {
            native.env_clear().envs(entries.iter().cloned());
        }
        let child = native.spawn().map_err(SpawnFailure::never)?;
        // std's Unix Child::id is a positive pid_t, which rustix also represents.
        let group = Some(Pid::from_child(&child));
        Ok(Self {
            child,
            group,
            termination_requested: false,
            status: None,
        })
    }

    /// Observe without consuming the waitable leader, which reserves the PGID.
    pub fn exited_reserved(&mut self) -> io::Result<bool> {
        let group = self
            .group
            .ok_or_else(|| io::Error::other("child identity is no longer reserved"))?;
        match waitid(
            WaitId::Pid(group),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        ) {
            Ok(status) => Ok(status.is_some()),
            Err(rustix::io::Errno::CHILD) => {
                self.group = None;
                Err(io::Error::other(
                    "child identity is no longer reserved (ECHILD)",
                ))
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Signal the group before its leader is reaped. ECHILD permanently disarms it.
    pub fn terminate_owned(&mut self) -> io::Result<()> {
        self.terminate_owned_after_observation(
            #[cfg(test)]
            |_, _| {},
        )
    }

    fn terminate_owned_after_observation(
        &mut self,
        #[cfg(test)] after_observation: impl FnOnce(&mut Self, bool),
    ) -> io::Result<()> {
        if self.group.is_none() || self.termination_requested {
            return Ok(());
        }
        let _exited = self.exited_reserved()?;
        #[cfg(test)]
        after_observation(self, _exited);
        let group = self.group.expect("reserved after observation");
        match kill_process_group(group, Signal::KILL) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => {}
            #[cfg(target_os = "macos")]
            Err(rustix::io::Errno::PERM)
                // The child may exit after the initial observation but before
                // killpg. Prove its current waitable, leader-only state afresh.
                if self.exited_reserved()? && only_reserved_leader(group) && self.exited_reserved()? => {}
            Err(error) => return Err(error.into()),
        }
        // Repeated cleanup polls must not reissue an accepted termination request.
        // The consuming wait still retains the leader until group signalling ends.
        self.termination_requested = true;
        Ok(())
    }

    /// Observe and release using the retained child. None and Err keep this owner.
    /// A confirmed status is cached so repeated completion observation is safe.
    pub fn try_reap_and_release(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.status {
            return Ok(Some(status));
        }
        self.terminate_owned()?;
        if !self.exited_reserved()? {
            return Ok(None);
        }
        // Signalling is finished. Disarm before the consuming wait, even on error.
        self.group = None;
        let status = self.child.wait()?;
        self.status = Some(status);
        Ok(Some(status))
    }
    pub fn reap_and_release(mut self) -> io::Result<ExitStatus> {
        loop {
            if let Some(status) = self.try_reap_and_release()? {
                return Ok(status);
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    pub(crate) fn cleanup(&mut self) -> io::Result<bool> {
        if self.group.is_none() {
            return Ok(true);
        }
        self.terminate_owned()?;
        if !self.exited_reserved()? {
            return Ok(false);
        }
        self.group = None;
        self.child.wait()?;
        Ok(true)
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.group.is_some() && self.terminate_owned().is_ok() {
            self.group = None;
            let _ = self.child.wait();
        }
    }
}

// XNU killpg1 excludes SZOMB and can report EPERM for a zombie-only group.
// Accept only a complete group snapshot containing the reserved leader alone.
// Apple's proc_info.c traverses allproc + zombproc under one proc_list lock;
// a two-slot buffer distinguishes this exact case from any additional member.
// proc_listpids returns BYTES (and zero for failure), not a PID count.
#[cfg(target_os = "macos")]
fn only_reserved_leader(group: Pid) -> bool {
    const PROC_PGRP_ONLY: u32 = 2; // xnu/bsd/sys/proc_info.h
    let mut members = [0 as libc::pid_t; 2];
    let bytes = unsafe {
        libc::proc_listpids(
            PROC_PGRP_ONLY,
            group.as_raw_nonzero().get() as u32,
            members.as_mut_ptr().cast(),
            std::mem::size_of_val(&members) as libc::c_int,
        )
    };
    // Zero/error, full/truncated, malformed, or another PID never proves safety.
    bytes == std::mem::size_of::<libc::pid_t>() as libc::c_int
        && members[0] == group.as_raw_nonzero().get()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_group_observation_requires_exact_reserved_leader() {
        use std::time::{Duration, Instant};
        let command = NativeCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "exit 0".into()],
            cwd: None,
            environment: ChildEnvironment::Inherit,
        };
        let mut child = OwnedChild::spawn_files(&command, None, None, None).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !child.exited_reserved().unwrap() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        let group = child.group.unwrap();
        assert!(only_reserved_leader(group));
        assert_eq!(
            kill_process_group(group, Signal::KILL),
            Err(rustix::io::Errno::PERM)
        );
        child.terminate_owned().unwrap();
        assert!(child.reap_and_release().unwrap().success());
        assert!(
            !only_reserved_leader(group),
            "zero/absent group must not count as a complete observation"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_exit_after_initial_observation_uses_fresh_reserved_group_proof() {
        use std::time::{Duration, Instant};
        let (input, release) = std::io::pipe().unwrap();
        let command = NativeCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "read token".into()],
            cwd: None,
            environment: ChildEnvironment::Inherit,
        };
        let mut child = OwnedChild::spawn_files(
            &command,
            Some(std::os::fd::OwnedFd::from(input).into()),
            None,
            None,
        )
        .unwrap();
        child
            .terminate_owned_after_observation(move |child, was_exited| {
                assert!(
                    !was_exited,
                    "open stdin keeps the child alive at the first observation"
                );
                drop(release);
                let deadline = Instant::now() + Duration::from_secs(10);
                while !child.exited_reserved().unwrap() {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert_eq!(
                    kill_process_group(child.group.unwrap(), Signal::KILL),
                    Err(rustix::io::Errno::PERM)
                );
            })
            .unwrap();
        assert!(child.termination_requested);
        assert_eq!(child.reap_and_release().unwrap().code(), Some(1));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_group_observation_rejects_full_buffer_with_live_descendant() {
        use std::time::{Duration, Instant};
        let output = tempfile::NamedTempFile::new().unwrap();
        let command = NativeCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "sleep 30 & printf ready; wait".into()],
            cwd: None,
            environment: ChildEnvironment::Inherit,
        };
        let mut child =
            OwnedChild::spawn_files(&command, None, Some(output.reopen().unwrap()), None).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::fs::read(output.path()).unwrap() != b"ready" {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            !only_reserved_leader(child.group.unwrap()),
            "a full/truncated two-slot buffer is not the leader-only case"
        );
        child.terminate_owned().unwrap();
        child.reap_and_release().unwrap();
    }

    #[test]
    fn owned_pipe_ends_are_cloexec_and_only_controller_is_nonblocking() -> io::Result<()> {
        use rustix::fs::{OFlags, fcntl_getfl};
        use rustix::io::{FdFlags, fcntl_getfd};
        for reading in [true, false] {
            let (controller, child) = pair(reading)?;
            assert!(fcntl_getfd(&controller)?.contains(FdFlags::CLOEXEC));
            assert!(fcntl_getfd(&child)?.contains(FdFlags::CLOEXEC));
            assert!(fcntl_getfl(&controller)?.contains(OFlags::NONBLOCK));
            assert!(!fcntl_getfl(&child)?.contains(OFlags::NONBLOCK));
        }
        Ok(())
    }

    #[test]
    fn external_reap_disarms_group_before_any_later_signal() {
        let mut native = Command::new("/bin/sh");
        native.args(["-c", "exit 0"]).process_group(0);
        let child = native.spawn().unwrap();
        let pid = Pid::from_raw(child.id() as i32).unwrap();
        let mut owned = OwnedChild {
            child,
            group: Some(pid),
            termination_requested: false,
            status: None,
        };
        rustix::process::waitpid(Some(pid), rustix::process::WaitOptions::empty()).unwrap();
        assert!(owned.exited_reserved().is_err());
        assert!(owned.group.is_none());
        assert!(owned.try_reap_and_release().is_err());
        assert!(
            owned.status.is_none(),
            "external reap cannot fabricate a confirmed status"
        );
        assert!(
            owned.group.is_none(),
            "borrowed retry must never rearm numeric identity"
        );
        owned.terminate_owned().unwrap();
        assert!(owned.cleanup().unwrap());
    }
}

/// Controller end only is nonblocking; child stdin/stdout retain blocking semantics.
fn pair(reading: bool) -> io::Result<(File, File)> {
    // std uses pipe2(O_CLOEXEC) where supported and checked CLOEXEC setup on
    // Darwin. Returned descriptors are CLOEXEC; Darwin creation is not atomic
    // against unrelated foreign forks during std's setup interval.
    let (read, write) = std::io::pipe()?;
    let read = std::os::fd::OwnedFd::from(read);
    let write = std::os::fd::OwnedFd::from(write);
    let (controller, child) = if reading {
        (read, write)
    } else {
        (write, read)
    };
    let flags = rustix::fs::fcntl_getfl(&controller)?;
    rustix::fs::fcntl_setfl(&controller, flags | rustix::fs::OFlags::NONBLOCK)?;
    Ok((controller.into(), child.into()))
}
pub(super) fn reader() -> Result<(PipeReader, File), SpawnFailure> {
    let (file, child) = pair(true).map_err(SpawnFailure::never)?;
    Ok((PipeReader { file: Some(file) }, child))
}
pub(super) fn writer() -> Result<(PipeWriter, File), SpawnFailure> {
    let (file, child) = pair(false).map_err(SpawnFailure::never)?;
    Ok((PipeWriter { file: Some(file) }, child))
}
#[derive(Debug)]
pub struct PipeReader {
    file: Option<File>,
}
impl PipeReader {
    /// A nonempty destination returns zero only after real peer EOF.
    pub fn try_read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let file = self.file.as_mut().ok_or_else(stopped)?;
        let count = out.len().min(16384);
        file.read(&mut out[..count])
    }
    /// Stop new I/O. No asynchronous operation exists on a Unix nonblocking fd.
    pub fn cancel_and_complete(&mut self) -> io::Result<()> {
        self.file.take();
        Ok(())
    }
}
#[derive(Debug)]
pub struct PipeWriter {
    file: Option<File>,
}
impl PipeWriter {
    pub fn try_write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let file = self.file.as_mut().ok_or_else(stopped)?;
        file.write(&bytes[..bytes.len().min(16384)])
    }
    /// Closing the last owned write descriptor delivers real stdin EOF to the child.
    pub fn close(&mut self) -> io::Result<()> {
        self.file.take();
        Ok(())
    }
}
fn stopped() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "pipe endpoint stopped")
}

#[cfg(feature = "test-support")]
pub(super) mod terminal;
