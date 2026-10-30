use super::{ChildEnvironment, NativeCommand, SpawnFailure};
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};
use std::fs::File;
use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};

#[derive(Debug)]
pub struct OwnedChild {
    child: Child,
    // None after any consuming wait or ECHILD: numeric identity is no longer reserved.
    group: Option<Pid>,
}
impl OwnedChild {
    /// Temporary Task 4 acceptance bridge; Task 5 reuses it for real endpoints.
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
        Ok(Self { child, group })
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
        if self.group.is_none() {
            return Ok(());
        }
        self.exited_reserved()?;
        match kill_process_group(
            self.group.expect("reserved after observation"),
            Signal::KILL,
        ) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
            Err(error) => Err(error.into()),
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn external_reap_disarms_group_before_any_later_signal() {
        let mut native = Command::new("/bin/sh");
        native.args(["-c", "exit 0"]).process_group(0);
        let child = native.spawn().unwrap();
        let pid = Pid::from_raw(child.id() as i32).unwrap();
        let mut owned = OwnedChild {
            child,
            group: Some(pid),
        };
        rustix::process::waitpid(Some(pid), rustix::process::WaitOptions::empty()).unwrap();
        assert!(owned.exited_reserved().is_err());
        assert!(owned.group.is_none());
        owned.terminate_owned().unwrap();
        assert!(owned.cleanup().unwrap());
    }
}
