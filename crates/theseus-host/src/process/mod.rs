//! Owned native process mechanisms. Application supervision remains outside this crate.
mod command;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
pub use command::{ChildEnvironment, NativeCommand};
#[cfg(unix)]
pub use unix::{OwnedChild, PipeReader, PipeWriter};
#[cfg(windows)]
pub use windows::{OwnedChild, PipeReader, PipeWriter};

use std::io;

/// Historical launch fact; cleanup never downgrades it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LaunchPhase {
    NeverCreated,
    CreatedSuspended,
    MayRun,
}

#[derive(Debug)]
pub enum HostCleanup {
    Complete,
    Pending(OwnedSpawnCleanup),
}

/// Retains unresolved process, primary thread, group/job and endpoint ownership.
/// Endpoint setup can require this guard even before a child exists.
#[derive(Debug)]
pub struct OwnedSpawnCleanup {
    pub(crate) child: Option<Box<OwnedChild>>,
    pub(crate) readers: Vec<PipeReader>,
    pub(crate) writers: Vec<PipeWriter>,
    #[cfg(all(windows, feature = "test-support"))]
    pub(crate) cleanup_faults: u32,
}
impl OwnedSpawnCleanup {
    /// Transfer unresolved endpoint ownership without losing native operation storage.
    pub fn from_endpoints(readers: Vec<PipeReader>, writers: Vec<PipeWriter>) -> Self {
        Self {
            child: None,
            readers,
            writers,
            #[cfg(all(windows, feature = "test-support"))]
            cleanup_faults: 0,
        }
    }

    /// Retry cleanup using retained authority. On error this guard remains owned.
    pub fn try_complete(&mut self) -> io::Result<bool> {
        #[cfg(all(windows, feature = "test-support"))]
        if self.cleanup_faults > 0 {
            self.cleanup_faults -= 1;
            return if self.cleanup_faults % 2 == 0 {
                Ok(false)
            } else {
                Err(io::Error::other("injected owned cleanup failure"))
            };
        }
        let mut error = None;
        for reader in &mut self.readers {
            if let Err(e) = reader.cancel_and_complete() {
                error.get_or_insert(e);
            }
        }
        for writer in &mut self.writers {
            if let Err(e) = writer.close() {
                error.get_or_insert(e);
            }
        }
        let child_complete = match &mut self.child {
            Some(child) => match child.cleanup() {
                Ok(complete) => complete,
                Err(e) => {
                    error.get_or_insert(e);
                    false
                }
            },
            None => true,
        };
        if let Some(error) = error {
            return Err(error);
        }
        if child_complete {
            self.child = None;
            self.readers.clear();
            self.writers.clear();
        }
        Ok(child_complete)
    }
}

#[derive(Debug)]
pub struct SpawnFailure {
    pub launch: LaunchPhase,
    pub cleanup: HostCleanup,
    pub error: io::Error,
    pub cleanup_errors: Vec<io::Error>,
}
impl SpawnFailure {
    pub(crate) fn never(error: io::Error) -> Self {
        Self {
            launch: LaunchPhase::NeverCreated,
            cleanup: HostCleanup::Complete,
            error,
            cleanup_errors: Vec::new(),
        }
    }
}
impl std::fmt::Display for SpawnFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "native launch {:?}: {}", self.launch, self.error)
    }
}
impl std::error::Error for SpawnFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// The child receives either the null device or an owned synchronous stdin pipe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputMode {
    Null,
    Pipe,
}

#[derive(Debug)]
pub struct AcquiredChild {
    pub child: OwnedChild,
    pub stdin: Option<PipeWriter>,
    pub stdout: PipeReader,
    pub stderr: PipeReader,
}

impl OwnedChild {
    /// Transfer retained process authority after an unsuccessful cleanup attempt.
    pub fn into_cleanup(self) -> OwnedSpawnCleanup {
        OwnedSpawnCleanup {
            child: Some(Box::new(self)),
            readers: vec![],
            writers: vec![],
            #[cfg(all(windows, feature = "test-support"))]
            cleanup_faults: 0,
        }
    }

    /// Acquire an owned process/group or job and bounded, interruptible endpoints.
    pub fn spawn(command: &NativeCommand, input: InputMode) -> Result<AcquiredChild, SpawnFailure> {
        Self::acquire(command, input, Self::spawn_files, true)
    }
    fn acquire(
        command: &NativeCommand,
        input: InputMode,
        spawn: impl FnOnce(
            &NativeCommand,
            Option<std::fs::File>,
            Option<std::fs::File>,
            Option<std::fs::File>,
        ) -> Result<OwnedChild, SpawnFailure>,
        settle: bool,
    ) -> Result<AcquiredChild, SpawnFailure> {
        #[cfg(unix)]
        use unix as platform;
        #[cfg(windows)]
        use windows::pipe as platform;
        command.validate().map_err(SpawnFailure::never)?;
        let mut readers = Vec::new();
        let mut writers = Vec::new();
        let result = (|| {
            let (stdout, output) = platform::reader()?;
            readers.push(stdout);
            let (stderr, errors) = platform::reader()?;
            readers.push(stderr);
            let input = match input {
                InputMode::Null => None,
                InputMode::Pipe => {
                    let (stdin, input) = platform::writer()?;
                    writers.push(stdin);
                    Some(input)
                }
            };
            spawn(command, input, Some(output), Some(errors))
        })();
        match result {
            Ok(child) => Ok(AcquiredChild {
                child,
                stdin: writers.pop(),
                stderr: readers.pop().unwrap(),
                stdout: readers.pop().unwrap(),
            }),
            Err(mut failure) => {
                let mut guard = match failure.cleanup {
                    HostCleanup::Complete => OwnedSpawnCleanup {
                        child: None,
                        readers: Vec::new(),
                        writers: Vec::new(),
                        #[cfg(all(windows, feature = "test-support"))]
                        cleanup_faults: 0,
                    },
                    HostCleanup::Pending(guard) => guard,
                };
                guard.readers.extend(readers);
                guard.writers.extend(writers);
                if !settle {
                    failure.cleanup = HostCleanup::Pending(guard);
                    return Err(failure);
                }
                failure.cleanup = match guard.try_complete() {
                    Ok(true) => HostCleanup::Complete,
                    Ok(false) => HostCleanup::Pending(guard),
                    Err(error) => {
                        failure.cleanup_errors.push(error);
                        HostCleanup::Pending(guard)
                    }
                };
                Err(failure)
            }
        }
    }
}

/// Per-call native launch faults for application acceptance only. Not enabled in
/// the normal application dependency or release build.
#[cfg(all(windows, feature = "test-support"))]
pub mod test_support {
    use super::*;
    pub fn fail_cleanup_attempts(owner: &mut OwnedSpawnCleanup, attempts: u32) {
        assert!(attempts <= 32, "bounded cleanup fault schedule");
        owner.cleanup_faults = attempts;
    }

    #[derive(Clone, Copy)]
    pub enum LaunchFault {
        BeforeCreate,
        CreatedSuspended,
        ResumeUncertain,
        CleanupPending,
        BeforeCreatePending,
    }
    pub fn spawn(
        command: &NativeCommand,
        input: InputMode,
        fault: LaunchFault,
    ) -> Result<AcquiredChild, SpawnFailure> {
        OwnedChild::acquire(
            command,
            input,
            |command, stdin, stdout, stderr| {
                super::windows::spawn_fault(command, stdin, stdout, stderr, fault)
            },
            !matches!(
                fault,
                LaunchFault::CleanupPending | LaunchFault::BeforeCreatePending
            ),
        )
    }
}
