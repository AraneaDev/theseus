//! Owned inherited pipe transport. This module never writes diagnostics.
//!
//! Unix callers must dedicate both inherited endpoints: duplicate descriptors
//! share file-status flags with every same-end alias. Windows uses one owned
//! synchronous worker per direction, not the process module's OVERLAPPED pipes.
//! Only local, synchronous byte pipes in wait mode qualify. Equal bounded pipe
//! names are conservatively rejected, including ambiguous separate instances.
//! The serving binary must install its quiet panic hook before acquisition;
//! catch_unwind retains worker payloads but does not suppress the global hook.
//! Callers must retain this owner (including acquisition failures) until stopped
//! and settled, or until their binary's forced-exit deadline. Dropping an unsettled
//! owner leaks its entire bounded native state as a memory-safety last resort;
//! it neither joins, detaches, nor represents successful cleanup.
use std::{cell::Cell, fmt, io, marker::PhantomData};
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
mod windows_worker;
#[cfg(unix)]
use unix::Native;
#[cfg(windows)]
use windows::Native;
#[cfg(feature = "test-support")]
pub mod test_support;

pub const MAX_OPERATION: usize = 4096;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadStep {
    Bytes(usize),
    Pending,
    Eof,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteStep {
    Bytes(usize),
    Pending,
}

pub struct Stdio {
    // The guard exists before acquisition can submit native work or mutate flags.
    native: Option<Box<Native>>,
    _not_sync: PhantomData<Cell<()>>,
}
pub struct AcquireFailure {
    error: io::Error,
    owner: Stdio,
}
impl fmt::Debug for Stdio {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Stdio")
            .field("settled", &self.is_settled())
            .finish_non_exhaustive()
    }
}
impl fmt::Debug for AcquireFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AcquireFailure")
            .field("error", &self.error)
            .field("owner", &self.owner)
            .finish()
    }
}
impl fmt::Display for AcquireFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}
impl std::error::Error for AcquireFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}
impl AcquireFailure {
    pub fn error(&self) -> &io::Error {
        &self.error
    }
    pub fn request_stop(&mut self) {
        self.owner.request_stop();
    }
    pub fn poll_stop_or_restore(&mut self) -> io::Result<bool> {
        self.owner.poll_stop_or_restore()
    }
    pub fn is_settled(&self) -> bool {
        self.owner.is_settled()
    }
}
impl Stdio {
    pub fn acquire() -> Result<Self, AcquireFailure> {
        Self::acquire_native(Native::new())
    }
    fn acquire_native(native: Native) -> Result<Self, AcquireFailure> {
        let mut owner = Self {
            native: Some(Box::new(native)),
            _not_sync: PhantomData,
        };
        match owner.inner().acquire() {
            Ok(()) => Ok(owner),
            Err(error) => {
                owner.request_stop();
                Err(AcquireFailure { error, owner })
            }
        }
    }
    fn inner(&mut self) -> &mut Native {
        self.native.as_mut().expect("stdio owner")
    }
    pub fn try_read(&mut self, bytes: &mut [u8]) -> io::Result<ReadStep> {
        self.inner().try_read(bytes)
    }
    /// At most 4096 bytes are submitted. On Windows Pending may already have
    /// written bytes: retain the same slice address, length and contents until
    /// acknowledgment. A changed prefix is rejected without releasing its owner.
    pub fn try_write(&mut self, bytes: &[u8]) -> io::Result<WriteStep> {
        self.inner().try_write(bytes)
    }
    pub fn write_in_flight(&self) -> bool {
        self.native.as_ref().is_some_and(|n| n.write_in_flight())
    }
    pub fn request_stop(&mut self) {
        self.inner().request_stop();
    }
    /// Nonblocking shutdown progress. True means native I/O stopped, actual
    /// worker joins observed, shared modes restored, and owned endpoints closed.
    /// Errors preserve all still-live authority and may be polled again.
    pub fn poll_stop_or_restore(&mut self) -> io::Result<bool> {
        self.inner().poll_stop_or_restore()
    }
    pub fn is_settled(&self) -> bool {
        self.native.as_ref().is_none_or(|n| n.is_settled())
    }
}
impl Drop for Stdio {
    fn drop(&mut self) {
        if !self.is_settled() {
            // Never run destructors of pending IOSBs, thread handles or buffers.
            // Supported callers retain this state themselves until binary exit.
            if let Some(native) = self.native.take() {
                let _ = Box::leak(native);
            }
        }
    }
}
fn stopped() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "stdio transport stopped")
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
