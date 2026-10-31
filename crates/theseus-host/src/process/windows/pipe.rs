//! One stable owned operation per controller endpoint, synchronous child peers.
//! Cancellation is bounded under a healthy kernel completing CancelIoEx. We never
//! free live I/O to manufacture a deadline: Drop retains an unresolved allocation.
use crate::process::{HostCleanup, OwnedSpawnCleanup, SpawnFailure};
use crate::windows_security::PrivateSecurity;
use std::cell::{Cell, UnsafeCell};
use std::fs::File;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{NonNull, null, null_mut};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::{IO::*, Pipes::*, Threading::*};

const BUFFER: usize = 16384;
fn blocked() -> io::Error {
    io::ErrorKind::WouldBlock.into()
}
fn stopped() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "pipe endpoint stopped")
}
fn code(error: &io::Error, value: u32) -> bool {
    error.raw_os_error() == Some(value as i32)
}

// Kernel-visible allocations have a raw owner, not a Box that is reborrowed
// mutably when an endpoint moves or is polled. All access to Operation is shared;
// native mutation is confined to UnsafeCells, controller metadata to Cells.
struct Operation {
    handle: OwnedHandle,
    event: OwnedHandle,
    overlapped: UnsafeCell<OVERLAPPED>,
    buffer: UnsafeCell<[u8; BUFFER]>,
    pending: Cell<bool>,
    setup_peer: Cell<Option<File>>,
}
struct OperationOwner(NonNull<Operation>);
impl OperationOwner {
    fn get(&self) -> &Operation {
        // SAFETY: new initializes exactly one allocation and the only reclaim
        // site is this owner's Drop after observed completion. Never make an
        // exclusive reference to this allocation while kernel access is possible.
        unsafe { self.0.as_ref() }
    }
}
// SAFETY: Moving this raw owner transfers Rust access without touching/retagging
// the allocation. No completion callback has thread affinity. Endpoint methods
// require &mut self, and this owner is !Sync: there cannot be concurrent Rust
// callers. Kernel access is only through stable raw pointers into UnsafeCells.
unsafe impl Send for OperationOwner {}
impl Drop for OperationOwner {
    fn drop(&mut self) {
        if !self.get().pending.get() {
            // SAFETY: this is the sole Box::from_raw for the sole Box::into_raw.
            // A false pending flag proves no submission remains kernel-owned.
            // No Operation reference escapes the endpoint/owner's borrow.
            unsafe {
                drop(Box::from_raw(self.0.as_ptr()));
            }
        }
        // Exceptional unresolved completion retains the entire allocation,
        // including both handles and any setup peer. Never free live native I/O.
    }
}
impl Operation {
    fn allocate_owned(handle: OwnedHandle) -> io::Result<OperationOwner> {
        let event = unsafe { CreateEventW(null(), 1, 0, null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        let event = unsafe { OwnedHandle::from_raw_handle(event) };
        let overlapped = OVERLAPPED {
            hEvent: event.as_raw_handle(),
            ..Default::default()
        };
        let allocation = Box::new(Self {
            handle,
            event,
            overlapped: UnsafeCell::new(overlapped),
            buffer: UnsafeCell::new([0; BUFFER]),
            pending: Cell::new(false),
            setup_peer: Cell::new(None),
        });
        // Box's unique reference ends before any pointer is submitted to Windows.
        Ok(OperationOwner(
            NonNull::new(Box::into_raw(allocation)).unwrap(),
        ))
    }
    fn reset(&self) -> io::Result<()> {
        assert!(!self.pending.get(), "operation must complete before reuse");
        if unsafe { ResetEvent(self.event.as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // No operation is pending; the kernel has released both mutable fields.
        unsafe {
            *self.overlapped.get() = OVERLAPPED {
                hEvent: self.event.as_raw_handle(),
                ..Default::default()
            };
        }
        Ok(())
    }
    fn result(&self, wait: bool) -> io::Result<usize> {
        // Wait only on our retained operation event, never the pipe handle. A
        // signaled event precedes inspection of kernel-written operation storage.
        let timeout = if wait { INFINITE } else { 0 };
        match unsafe { WaitForSingleObject(self.event.as_raw_handle(), timeout) } {
            WAIT_TIMEOUT => return Err(blocked()),
            WAIT_OBJECT_0 => {}
            _ => return Err(io::Error::last_os_error()),
        }
        let mut count = 0;
        let success = unsafe {
            GetOverlappedResult(
                self.handle.as_raw_handle(),
                self.overlapped.get(),
                &mut count,
                0,
            )
        };
        if success != 0 {
            self.pending.set(false);
            return Ok(count as usize);
        }
        let error = io::Error::last_os_error();
        if code(&error, ERROR_IO_INCOMPLETE) || code(&error, ERROR_IO_PENDING) {
            return Err(blocked());
        }
        // With valid retained handles and the signaled dedicated event, this is
        // the terminal operation status (including cancellation), not a request.
        self.pending.set(false);
        Err(error)
    }
    fn submitted(&self, success: i32) -> io::Result<usize> {
        if success != 0 {
            // Immediate completion also has a valid completed OVERLAPPED. Read
            // its count through the documented API rather than directly racing it.
            let mut count = 0;
            if unsafe {
                GetOverlappedResult(
                    self.handle.as_raw_handle(),
                    self.overlapped.get(),
                    &mut count,
                    0,
                )
            } != 0
            {
                self.pending.set(false);
                return Ok(count as usize);
            }
            let error = io::Error::last_os_error();
            if code(&error, ERROR_IO_INCOMPLETE) || code(&error, ERROR_IO_PENDING) {
                self.pending.set(true);
                return Err(blocked());
            }
            self.pending.set(false);
            return Err(error);
        }
        let error = io::Error::last_os_error();
        if code(&error, ERROR_IO_PENDING) {
            self.pending.set(true);
            return Err(blocked());
        }
        self.pending.set(false);
        Err(error)
    }
}
struct Endpoint {
    operation: Option<OperationOwner>,
    stopped: bool,
}
impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("stopped", &self.stopped)
            .field(
                "pending",
                &self
                    .operation
                    .as_ref()
                    .is_some_and(|owner| owner.get().pending.get()),
            )
            .finish()
    }
}
impl Endpoint {
    fn active(&self) -> io::Result<&Operation> {
        if self.stopped {
            return Err(stopped());
        }
        self.operation
            .as_ref()
            .map(OperationOwner::get)
            .ok_or_else(stopped)
    }
    fn stop(&mut self) -> io::Result<()> {
        self.stopped = true; // Publish stop BEFORE requesting cancellation.
        let Some(owner) = &self.operation else {
            return Ok(());
        };
        let op = owner.get();
        let mut cancellation_error = None;
        if op.pending.get() {
            if unsafe { CancelIoEx(op.handle.as_raw_handle(), op.overlapped.get()) } == 0 {
                let error = io::Error::last_os_error();
                if !code(&error, ERROR_NOT_FOUND) {
                    cancellation_error = Some(error);
                }
            }
            // ERROR_NOT_FOUND only means the cancellation request lost a race.
            // Regardless of its result, observe completion before releasing anything.
            let result = op.result(true);
            if op.pending.get() {
                return result.map(|_| ());
            }
            if let Err(error) = result
                && !code(&error, ERROR_OPERATION_ABORTED)
                && !code(&error, ERROR_BROKEN_PIPE)
            {
                cancellation_error.get_or_insert(error);
            }
        }
        self.operation.take(); // closes server, peer (setup only), event, then storage
        match cancellation_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        let _ = self.stop();
        // If stop could not observe completion, OperationOwner::drop retains the
        // allocation. Otherwise stop has already released its completed owner.
    }
}

fn server(reading: bool) -> io::Result<(OperationOwner, Vec<u16>)> {
    let mut random = [0u8; 16];
    crate::random_bytes(&mut random)?;
    let suffix = format!("{:032x}", u128::from_le_bytes(random));
    let name: Vec<u16> = format!(r"\\.\pipe\theseus-{suffix}")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut security = PrivateSecurity::new()?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security.as_ptr().cast_mut().cast(),
        bInheritHandle: 0,
    };
    let access = if reading {
        PIPE_ACCESS_INBOUND
    } else {
        PIPE_ACCESS_OUTBOUND
    };
    let raw = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            access | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            BUFFER as u32,
            BUFFER as u32,
            1000,
            &attributes,
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    Ok((Operation::allocate_owned(handle)?, name))
}
fn pair(reading: bool) -> Result<(Endpoint, File), SpawnFailure> {
    let (owner, name) = server(reading).map_err(SpawnFailure::never)?;
    let operation = owner.get();
    // Open our synchronous client first. No wait-for-client blocking call: with a
    // fresh first-instance server this either connects immediately or fails.
    let access = if reading { GENERIC_WRITE } else { GENERIC_READ };
    let raw = unsafe {
        CreateFileW(
            name.as_ptr(),
            access,
            0,
            null(),
            OPEN_EXISTING,
            0,
            null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(SpawnFailure::never(io::Error::last_os_error()));
    }
    operation
        .setup_peer
        .set(Some(unsafe { File::from_raw_handle(raw) }));
    operation.reset().map_err(SpawnFailure::never)?;
    // Arm ownership before submission; there is no fallible Rust work between
    // this store and the FFI call. Only a terminal call/completion disarms it.
    operation.pending.set(true);
    let success =
        unsafe { ConnectNamedPipe(operation.handle.as_raw_handle(), operation.overlapped.get()) };
    if success == 0 {
        let error = io::Error::last_os_error();
        if code(&error, ERROR_IO_PENDING) {
            operation.pending.set(true);
            // Unexpected but still owned. Bound connection setup and retain the
            // complete owner on failure, including its peer and operation memory.
            if unsafe { WaitForSingleObject(operation.event.as_raw_handle(), 1000) }
                != WAIT_OBJECT_0
            {
                return Err(setup_failure(
                    Endpoint {
                        operation: Some(owner),
                        stopped: false,
                    },
                    reading,
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "owned pipe connection did not complete",
                    ),
                ));
            }
            if let Err(error) = operation.result(false) {
                return Err(setup_failure(
                    Endpoint {
                        operation: Some(owner),
                        stopped: false,
                    },
                    reading,
                    error,
                ));
            }
        } else {
            operation.pending.set(false);
            if !code(&error, ERROR_PIPE_CONNECTED) {
                return Err(setup_failure(
                    Endpoint {
                        operation: Some(owner),
                        stopped: false,
                    },
                    reading,
                    error,
                ));
            }
        }
    } else {
        operation.pending.set(false);
    }
    let peer = operation.setup_peer.take().expect("owned setup peer");
    Ok((
        Endpoint {
            operation: Some(owner),
            stopped: false,
        },
        peer,
    ))
}
fn setup_failure(endpoint: Endpoint, reading: bool, error: io::Error) -> SpawnFailure {
    let mut guard = OwnedSpawnCleanup {
        child: None,
        readers: Vec::new(),
        writers: Vec::new(),
        #[cfg(feature = "test-support")]
        cleanup_faults: 0,
    };
    if reading {
        guard.readers.push(PipeReader::new(endpoint));
    } else {
        guard.writers.push(PipeWriter::new(endpoint));
    }
    SpawnFailure {
        launch: crate::process::LaunchPhase::NeverCreated,
        cleanup: HostCleanup::Pending(guard),
        error,
        cleanup_errors: Vec::new(),
    }
}
pub(in crate::process) fn reader() -> Result<(PipeReader, File), SpawnFailure> {
    let (endpoint, peer) = pair(true)?;
    Ok((PipeReader::new(endpoint), peer))
}
pub(in crate::process) fn writer() -> Result<(PipeWriter, File), SpawnFailure> {
    let (endpoint, peer) = pair(false)?;
    Ok((PipeWriter::new(endpoint), peer))
}
#[derive(Debug)]
pub struct PipeReader {
    endpoint: Endpoint,
    ready: std::ops::Range<usize>,
    eof: bool,
}
impl PipeReader {
    fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            ready: 0..0,
            eof: false,
        }
    }
    fn finish_read(&mut self, result: io::Result<usize>) -> io::Result<()> {
        match result {
            Ok(0) => Err(blocked()), // Windows TRUE/0 is NOT Unix read(2) EOF.
            Ok(n) => {
                self.ready = 0..n;
                Ok(())
            }
            Err(error) if code(&error, ERROR_BROKEN_PIPE) => {
                self.eof = true;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
    /// Ok(0) with a nonempty buffer is confirmed peer EOF, after buffered bytes.
    /// Cancellation remains Interrupted; it never establishes complete capture.
    pub fn try_read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.endpoint.active()?;
        if out.is_empty() {
            return Ok(0);
        }
        if self.ready.is_empty() && !self.eof {
            let op = self.endpoint.active()?;
            let result = if op.pending.get() {
                op.result(false)
            } else {
                op.reset()?;
                op.pending.set(true);
                let success = unsafe {
                    ReadFile(
                        op.handle.as_raw_handle(),
                        op.buffer.get().cast::<u8>(),
                        BUFFER as u32,
                        null_mut(),
                        op.overlapped.get(),
                    )
                };
                op.submitted(success)
            };
            self.finish_read(result)?;
        }
        let count = out.len().min(self.ready.len());
        if count > 0 {
            let op = self.endpoint.active()?;
            // Completion was observed before any Rust reference into this buffer.
            assert!(!op.pending.get());
            out[..count].copy_from_slice(unsafe {
                &(&*op.buffer.get())[self.ready.start..self.ready.start + count]
            });
            self.ready.start += count;
        }
        Ok(count)
    }
    pub fn cancel_and_complete(&mut self) -> io::Result<()> {
        self.endpoint.stop()
    }
}
#[derive(Debug)]
pub struct PipeWriter {
    endpoint: Endpoint,
    submitted: usize,
    // Controller-only copy: never submitted to Windows. Pending retry validation
    // must not read even an immutable slice of the native WriteFile buffer.
    retry: Box<[u8; BUFFER]>,
}
impl PipeWriter {
    fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            submitted: 0,
            retry: Box::new([0; BUFFER]),
        }
    }
    /// Pending writes own a bounded copy and return WouldBlock. Retry the same
    /// prefix until acknowledged; changing it is InvalidInput, never an ack.
    pub fn try_write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let op = self.endpoint.active()?;
        let result = if op.pending.get() {
            if bytes.len() < self.submitted
                || bytes[..self.submitted] != self.retry[..self.submitted]
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "pending write requires the same prefix",
                ));
            }
            op.result(false)
        } else {
            if bytes.is_empty() {
                return Ok(0);
            }
            op.reset()?;
            self.submitted = bytes.len().min(BUFFER);
            self.retry[..self.submitted].copy_from_slice(&bytes[..self.submitted]);
            // reset succeeded with pending=false. This is the only writer buffer
            // access, and its Rust reference ends before WriteFile begins.
            unsafe {
                (&mut *op.buffer.get())[..self.submitted]
                    .copy_from_slice(&self.retry[..self.submitted]);
            }
            op.pending.set(true);
            let success = unsafe {
                WriteFile(
                    op.handle.as_raw_handle(),
                    op.buffer.get().cast::<u8>(),
                    self.submitted as u32,
                    null_mut(),
                    op.overlapped.get(),
                )
            };
            op.submitted(success)
        };
        if !op.pending.get() {
            self.submitted = 0;
        }
        match result {
            Ok(0) => Err(blocked()),
            other => other,
        }
    }
    /// Stop, cancel and observe any write before closing the last controller handle.
    /// Never flush: pipe flushing can wait indefinitely for a non-reading peer.
    pub fn close(&mut self) -> io::Result<()> {
        self.endpoint.stop()
    }
}

#[cfg(test)]
mod tests;
