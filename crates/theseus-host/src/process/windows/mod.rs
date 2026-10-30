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
        let job_result = self.job.terminate();
        // Also cover failed membership validation: process authority is independent.
        let process_result = if !self.exited_reserved()?
            && unsafe { TerminateProcess(self.process.as_raw_handle(), 1) } == 0
        {
            // Process exit may race TerminateProcess. A signaled retained handle
            // proves the process is gone without reopening by PID.
            let error = io::Error::last_os_error();
            if self.exited_reserved()? {
                Ok(())
            } else {
                Err(error)
            }
        } else {
            Ok(())
        };
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
