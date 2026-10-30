//! Retained Windows process/thread/job authority. No numeric-ID reopening.
mod encoding;
mod job;
mod launch;
use super::{NativeCommand, SpawnFailure};
use job::Job;
use std::fs::File;
use std::io;
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::os::windows::process::ExitStatusExt;
use std::process::ExitStatus;
use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, TerminateProcess, WaitForSingleObject,
};

#[derive(Debug)]
pub struct OwnedChild {
    process: OwnedHandle,
    thread: OwnedHandle,
    job: Job,
    released: bool,
    job_termination_requested: bool,
    process_termination_requested: bool,
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
    pub fn reap_and_release(mut self) -> io::Result<ExitStatus> {
        self.terminate_owned()?;
        // No pipe operation or worker is involved in this Task 4 consuming wait.
        while !self.cleanup()? {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let mut code = 0;
        if unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &mut code) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(ExitStatus::from_raw(code))
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
