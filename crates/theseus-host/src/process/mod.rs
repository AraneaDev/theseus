//! Owned native process mechanisms. Application supervision remains outside this crate.
mod command;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
pub use command::{ChildEnvironment, NativeCommand};
#[cfg(unix)]
pub use unix::OwnedChild;
#[cfg(windows)]
pub use windows::OwnedChild;

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

/// Retains process, primary thread and group/job authority after incomplete cleanup.
#[derive(Debug)]
pub struct OwnedSpawnCleanup {
    pub(crate) child: OwnedChild,
}
impl OwnedSpawnCleanup {
    /// Retry cleanup using retained authority. On error this guard remains owned.
    pub fn try_complete(&mut self) -> io::Result<bool> {
        self.child.cleanup()
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
