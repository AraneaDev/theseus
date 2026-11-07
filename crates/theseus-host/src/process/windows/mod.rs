//! Retained Windows process/thread/job authority. No numeric-ID reopening.
mod encoding;
mod job;
mod launch;
pub(super) mod pipe;
use super::{NativeCommand, SpawnFailure};
use job::Job;
pub use pipe::{PipeReader, PipeWriter};
use std::fs::File;
use std::io;
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::os::windows::process::ExitStatusExt;
use std::process::ExitStatus;
use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, GetProcessId, TerminateProcess, WaitForSingleObject,
};

#[derive(Debug)]
pub struct OwnedChild {
    process: OwnedHandle,
    thread: OwnedHandle,
    job: Job,
    released: bool,
    status: Option<ExitStatus>,
    job_termination_requested: bool,
    process_termination_requested: bool,
}
impl OwnedChild {
    /// Correlation metadata queried only from the original retained handle.
    /// It never grants PID-based reopening, reattachment, or process control.
    pub fn original_peer_pid(&self) -> io::Result<u32> {
        let pid = unsafe { GetProcessId(self.process.as_raw_handle()) };
        if pid == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(pid)
        }
    }

    /// Completion requires the original retained process plus zero active job
    /// members; a primary process exit alone is never descendant completion.
    pub fn try_join_original_scope(&mut self) -> io::Result<Option<ExitStatus>> {
        self.try_reap_and_release()
    }

    /// Shared null/file launch primitive for owned endpoints and mechanism tests.
    #[doc(hidden)]
    pub fn spawn_files(
        command: &NativeCommand,
        stdin: Option<File>,
        stdout: Option<File>,
        stderr: Option<File>,
    ) -> Result<Self, SpawnFailure> {
        launch::spawn(command, stdin, stdout, stderr)
    }
    pub fn exited_reserved(&mut self) -> io::Result<bool> {
        // A retained process handle remains identity even after leader exit.
        match unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(io::Error::last_os_error()),
        }
    }
    pub fn terminate_owned(&mut self) -> io::Result<()> {
        if self.job_termination_requested && self.process_termination_requested {
            return Ok(());
        }
        // Membership is only an optimization here: an unsuccessful query falls
        // back to retained process termination. It cannot discard either owner.
        let member = matches!(self.job.contains(self.process.as_raw_handle()), Ok(true));
        let job_result = if self.job_termination_requested {
            Ok(())
        } else {
            self.job
                .terminate()
                .inspect(|_| self.job_termination_requested = true)
        };
        let process_result =
            if self.process_termination_requested || (self.job_termination_requested && member) {
                // TerminateJobObject already requested this process's termination.
                // TerminateProcess is asynchronous; a second request can fail with
                // ACCESS_DENIED before the process wait object becomes signaled.
                Ok(())
            } else if !self.exited_reserved()?
                && unsafe { TerminateProcess(self.process.as_raw_handle(), 1) } == 0
            {
                let error = io::Error::last_os_error();
                if self.exited_reserved()? {
                    Ok(())
                } else {
                    Err(error)
                }
            } else {
                Ok(())
            };
        if process_result.is_ok() {
            self.process_termination_requested = true;
        }
        // Track successful requests separately even if the other request failed.
        // Acceptance is not completion: cleanup still waits for both retained
        // process exit and zero active job members on every retry.
        job_result.and(process_result)
    }
    pub(crate) fn cleanup(&mut self) -> io::Result<bool> {
        if self.released {
            return Ok(true);
        }
        self.terminate_owned()?;
        if self.exited_reserved()? && self.job.empty()? {
            self.released = true;
            Ok(true)
        } else {
            Ok(false)
        }
    }
    /// None and Err retain all process/thread/job handles. Only confirmed native
    /// process exit plus empty-job accounting permits release and a cached status.
    pub fn try_reap_and_release(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.status {
            return Ok(Some(status));
        }
        self.terminate_owned()?;
        if !self.exited_reserved()? || !self.job.empty()? {
            return Ok(None);
        }
        let mut code = 0;
        if unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &mut code) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let status = ExitStatus::from_raw(code);
        self.status = Some(status);
        self.released = true;
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
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.released {
            let _ = self.terminate_owned();
            // Closing the last job handle also kills descendants; ordinary explicit
            // cleanup checks job accounting and process wait before claiming Complete.
        }
    }
}

#[cfg(feature = "test-support")]
pub(super) fn spawn_fault(
    command: &NativeCommand,
    stdin: Option<File>,
    stdout: Option<File>,
    stderr: Option<File>,
    fault: super::test_support::LaunchFault,
) -> Result<OwnedChild, SpawnFailure> {
    use super::test_support::LaunchFault;
    let fault = match fault {
        LaunchFault::BeforeCreate | LaunchFault::BeforeCreatePending => launch::Fault::BeforeCreate,
        LaunchFault::CreatedSuspended => launch::Fault::AfterCreate,
        LaunchFault::ResumeUncertain => launch::Fault::ResumeUncertain,
        LaunchFault::CleanupPending => launch::Fault::CleanupPending,
    };
    launch::spawn_impl(command, stdin, stdout, stderr, fault)
}

#[cfg(feature = "test-support")]
pub(super) mod terminal;
