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
        command.validate().map_err(SpawnFailure::never)?;
        let mut native = Command::new(&command.program);
        native
            .args(&command.args)
            .process_group(0)
            .stdin(stdin.map_or_else(Stdio::null, Stdio::from))
            .stdout(stdout.map_or_else(Stdio::null, Stdio::from))
            .stderr(stderr.map_or_else(Stdio::null, Stdio::from));
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
        if self.group.is_none() || self.termination_requested {
            return Ok(());
        }
        #[cfg(target_os = "macos")]
        let exited = self.exited_reserved()?;
        #[cfg(not(target_os = "macos"))]
        self.exited_reserved()?;
        let group = self.group.expect("reserved after observation");
        match kill_process_group(group, Signal::KILL) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => {}
            #[cfg(target_os = "macos")]
            Err(rustix::io::Errno::PERM)
                if exited && only_reserved_leader(group) && self.exited_reserved()? => {}
            Err(error) => return Err(error.into()),
        }
        // Repeated cleanup polls must not reissue an accepted termination request.
        // The consuming wait still retains the leader until group signalling ends.
        self.termination_requested = true;
        Ok(())
    }

    pub fn reap_and_release(mut self) -> io::Result<ExitStatus> {
        self.terminate_owned()?;
        // Never allow Drop to signal a number after a consuming wait, even on error.
        self.group = None;
        self.child.wait()
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
    fn external_reap_disarms_group_before_any_later_signal() {
        let mut native = Command::new("/bin/sh");
        native.args(["-c", "exit 0"]).process_group(0);
        let child = native.spawn().unwrap();
        let pid = Pid::from_raw(child.id() as i32).unwrap();
        let mut owned = OwnedChild {
            child,
            group: Some(pid),
            termination_requested: false,
        };
        rustix::process::waitpid(Some(pid), rustix::process::WaitOptions::empty()).unwrap();
        assert!(owned.exited_reserved().is_err());
        assert!(owned.group.is_none());
        owned.terminate_owned().unwrap();
        assert!(owned.cleanup().unwrap());
    }
}

/// Controller end only is nonblocking; child stdin/stdout retain blocking semantics.
fn pair(reading: bool) -> io::Result<(File, File)> {
    let (read, write) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)?;
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
