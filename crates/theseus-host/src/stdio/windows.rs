#[cfg(feature = "test-support")]
use super::test_support::{CancellationEvidence, Control, Event, Trace};
use super::windows_worker::{Command, Completion, ReadBuffer, Worker};
use super::{MAX_OPERATION, ReadStep, WriteStep, invalid, stopped};
#[cfg(feature = "test-support")]
use serde_json::json;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::Arc;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_ACCESS_INFORMATION, FILE_MODE_INFORMATION, FILE_PIPE_INFORMATION,
    FILE_PIPE_LOCAL_INFORMATION, FileAccessInformation, FileIsRemoteDeviceInformation,
    FileModeInformation, FileNameInformation, FilePipeInformation, FilePipeLocalInformation,
    NtQueryInformationFile,
};
use windows_sys::Wdk::System::SystemServices::FILE_IS_REMOTE_DEVICE_INFORMATION;
use windows_sys::Win32::{
    Foundation::*,
    Globalization::{CSTR_EQUAL, CompareStringOrdinal},
    Storage::FileSystem::*,
    System::{Console::*, IO::*, Threading::*},
};

#[path = "windows_profile.rs"]
mod profile;

// At most 4096 bytes total; no retry with attacker-selected allocation sizes.
const NAME_UNITS: usize = 2046;
#[repr(C)]
struct PipeName {
    length: u32,
    units: [u16; NAME_UNITS],
}
impl Default for PipeName {
    fn default() -> Self {
        Self {
            length: 0,
            units: [0; NAME_UNITS],
        }
    }
}
enum QueryOutput {
    Name(Box<PipeName>),
    Remote(Box<FILE_IS_REMOTE_DEVICE_INFORMATION>),
    Access(Box<FILE_ACCESS_INFORMATION>),
    Mode(Box<FILE_MODE_INFORMATION>),
    Pipe(Box<FILE_PIPE_LOCAL_INFORMATION>),
    PipeMode(Box<FILE_PIPE_INFORMATION>),
}
impl QueryOutput {
    fn parts(&mut self) -> (*mut std::ffi::c_void, u32, i32) {
        match self {
            Self::Name(value) => (
                (&mut **value as *mut PipeName).cast(),
                std::mem::size_of::<PipeName>() as u32,
                FileNameInformation,
            ),
            Self::Remote(value) => (
                (&mut **value as *mut FILE_IS_REMOTE_DEVICE_INFORMATION).cast(),
                std::mem::size_of::<FILE_IS_REMOTE_DEVICE_INFORMATION>() as u32,
                FileIsRemoteDeviceInformation,
            ),
            Self::Access(value) => (
                (&mut **value as *mut FILE_ACCESS_INFORMATION).cast(),
                std::mem::size_of::<FILE_ACCESS_INFORMATION>() as u32,
                FileAccessInformation,
            ),
            Self::Mode(value) => (
                (&mut **value as *mut FILE_MODE_INFORMATION).cast(),
                std::mem::size_of::<FILE_MODE_INFORMATION>() as u32,
                FileModeInformation,
            ),
            Self::PipeMode(value) => (
                (&mut **value as *mut FILE_PIPE_INFORMATION).cast(),
                std::mem::size_of::<FILE_PIPE_INFORMATION>() as u32,
                FilePipeInformation,
            ),
            Self::Pipe(value) => (
                (&mut **value as *mut FILE_PIPE_LOCAL_INFORMATION).cast(),
                std::mem::size_of::<FILE_PIPE_LOCAL_INFORMATION>() as u32,
                FilePipeLocalInformation,
            ),
        }
    }
}
struct Query {
    iosb: Box<IO_STATUS_BLOCK>,
    output: QueryOutput,
    #[cfg(test)]
    _witness: Option<QueryWitness>,
}
#[cfg(test)]
struct QueryWitness(Arc<std::sync::atomic::AtomicBool>);
#[cfg(test)]
impl Drop for QueryWitness {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
}
struct WriteReceipt {
    address: usize,
    length: usize,
    submitted: usize,
}
pub(super) struct Native {
    input: Option<Arc<OwnedHandle>>,
    output: Option<Arc<OwnedHandle>>,
    query: Option<Query>,
    query_pending: bool,
    reader: Option<Worker>,
    writer: Option<Worker>,
    read_buffer: Option<Box<ReadBuffer>>,
    read_ready: Option<(Completion, usize)>,
    read_in_flight: bool,
    write_buffer: Arc<[u8; MAX_OPERATION]>,
    write_receipt: Option<WriteReceipt>,
    stopping: bool,
    settled: bool,
    #[cfg(feature = "test-support")]
    cancellations: CancellationEvidence,
    #[cfg(feature = "test-support")]
    pub(super) trace: Trace,
    #[cfg(feature = "test-support")]
    pub(super) control: Control,
}
impl Native {
    pub(super) fn new() -> Self {
        Self {
            input: None,
            output: None,
            query: None,
            query_pending: false,
            reader: None,
            writer: None,
            read_buffer: Some(Box::new(ReadBuffer {
                bytes: [0; MAX_OPERATION],
            })),
            read_ready: None,
            read_in_flight: false,
            write_buffer: Arc::new([0; MAX_OPERATION]),
            write_receipt: None,
            stopping: false,
            settled: true,
            #[cfg(feature = "test-support")]
            cancellations: CancellationEvidence::default(),
            #[cfg(feature = "test-support")]
            trace: Trace::default(),
            #[cfg(feature = "test-support")]
            control: Control::Normal,
        }
    }
    pub(super) fn acquire(&mut self) -> io::Result<()> {
        self.settled = false;
        self.input = Some(Arc::new(self.duplicate(true)?));
        self.output = Some(Arc::new(self.duplicate(false)?));
        let input_configuration = self.qualify(true)?;
        let output_configuration = self.qualify(false)?;
        if unsafe { CompareObjectHandles(self.handle(true), self.handle(false)) } != 0 {
            #[cfg(feature = "test-support")]
            self.trace.push(Event {
                phase: "alias_rejected",
                endpoint: "control",
                api: "CompareObjectHandles",
                ok: true,
                status: Some(1),
                error: None,
                observed: json!({"same_file_object":true}),
            });
            return Err(invalid("stdin and stdout alias one pipe endpoint"));
        }
        let QueryOutput::Name(input_name) = self.query(true, QueryOutput::Name(Box::default()))?
        else {
            unreachable!()
        };
        let QueryOutput::Name(output_name) =
            self.query(false, QueryOutput::Name(Box::default()))?
        else {
            unreachable!()
        };
        if input_name.length == 0 || output_name.length == 0 {
            // Ordinary Rust child pipes may have no name. Empty names provide
            // no identity: require the already checked whole-pipe configurations
            // to prove that these cannot be opposite ends of the same pipe.
            let disjoint = profile::disjoint_unnamed(
                input_name.length,
                output_name.length,
                input_configuration,
                output_configuration,
            );
            #[cfg(feature = "test-support")]
            self.trace.push(Event {
                phase: if disjoint { "pipe_identity" } else { "alias_rejected" },
                endpoint: "control",
                api: "FilePipeLocalInformation",
                ok: true,
                status: None,
                error: None,
                observed: json!({"input_name_bytes":input_name.length,"output_name_bytes":output_name.length,"input_configuration":input_configuration,"output_configuration":output_configuration,"opposite_one_way_configurations":disjoint}),
            });
            if !disjoint {
                return Err(invalid("unnamed stdio pipe identity is ambiguous"));
            }
        } else {
            let compared = unsafe {
                CompareStringOrdinal(
                    input_name.units.as_ptr(),
                    (input_name.length / 2) as i32,
                    output_name.units.as_ptr(),
                    (output_name.length / 2) as i32,
                    1,
                )
            };
            if compared == 0 {
                return Err(io::Error::last_os_error());
            }
            // Equal names may also denote distinct named-pipe instances; that
            // ambiguous profile is conservatively unsupported. No handle-number
            // inequality is treated as underlying pipe identity proof.
            if compared == CSTR_EQUAL {
                #[cfg(feature = "test-support")]
                self.trace.push(Event {
                    phase: "alias_rejected",
                    endpoint: "control",
                    api: "CompareStringOrdinal",
                    ok: true,
                    status: Some(compared as i64),
                    error: None,
                    observed: json!({"same_pipe_name":true}),
                });
                return Err(invalid("stdin and stdout share an ambiguous pipe name"));
            }
            #[cfg(feature = "test-support")]
            self.trace.push(Event {
                phase: "pipe_identity",
                endpoint: "control",
                api: "CompareStringOrdinal",
                ok: true,
                status: Some(compared as i64),
                error: None,
                observed: json!({"different_checked_names":true,"max_name_bytes":NAME_UNITS*2}),
            });
        }
        #[cfg(feature = "test-support")]
        if self.control == Control::PartialAcquire {
            self.trace.push(Event {phase:"rollback_injection",endpoint:"control",api:"synthetic acquisition failure",ok:true,status:None,error:None,observed:json!({"obligation":"acquired_handles","acquired_endpoints":2,"mutated_endpoints":0,"synthetic":true})});
            return Err(io::Error::other("injected partial acquisition"));
        }
        self.reader = Some(Worker::spawn(
            self.input.as_ref().unwrap().clone(),
            true,
            #[cfg(feature = "test-support")]
            matches!(self.control, Control::BeforeCall | Control::HoldBeforeCall),
            #[cfg(test)]
            None,
        )?);
        #[cfg(feature = "test-support")]
        if self.control == Control::WriterSpawnFailure {
            self.trace.push(Event {
                phase: "worker_start_injection",
                endpoint: "control",
                api: "synthetic writer spawn failure",
                ok: true,
                status: None,
                error: None,
                observed: json!({"actual_reader_join_owned":true,"synthetic_fault":true}),
            });
            return Err(io::Error::other(
                "injected writer spawn failure after reader start",
            ));
        }
        self.writer = Some(Worker::spawn(
            self.output.as_ref().unwrap().clone(),
            false,
            #[cfg(feature = "test-support")]
            false,
            #[cfg(test)]
            None,
        )?);
        Ok(())
    }
    fn duplicate(&mut self, reading: bool) -> io::Result<OwnedHandle> {
        let raw = unsafe {
            GetStdHandle(if reading {
                STD_INPUT_HANDLE
            } else {
                STD_OUTPUT_HANDLE
            })
        };
        let mut copy = std::ptr::null_mut();
        let status = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                raw,
                GetCurrentProcess(),
                &mut copy,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        let error = (status == 0).then(io::Error::last_os_error);
        let owned = (status != 0).then(|| unsafe { OwnedHandle::from_raw_handle(copy) });
        let same_object = status != 0 && unsafe { CompareObjectHandles(raw, copy) } != 0;
        #[cfg(feature = "test-support")]
        self.trace.push(Event {
            phase: "duplicate",
            endpoint: label(reading),
            api: "DuplicateHandle",
            ok: status != 0,
            status: Some(status as i64),
            error: error.as_ref().and_then(io::Error::raw_os_error),
            observed: json!({"same_access":true,"inheritable":false}),
        });
        #[cfg(feature = "test-support")]
        if status != 0 {
            self.trace.push(Event {
                phase: "mode_sharing",
                endpoint: label(reading),
                api: "CompareObjectHandles original/duplicate",
                ok: same_object,
                status: None,
                error: None,
                observed: json!({"same_file_object":same_object,"mode_mutation":false}),
            });
        }
        if status != 0 && !same_object {
            return Err(io::Error::other("duplicate pipe object identity unproven"));
        }
        owned.ok_or_else(|| error.unwrap())
    }
    fn handle(&self, reading: bool) -> HANDLE {
        if reading {
            self.input.as_ref()
        } else {
            self.output.as_ref()
        }
        .unwrap()
        .as_raw_handle()
    }
    fn query(&mut self, reading: bool, output: QueryOutput) -> io::Result<QueryOutput> {
        // Owner is stored BEFORE issuing a query; any unwind preserves its heap.
        self.query = Some(Query {
            iosb: Box::default(),
            output,
            #[cfg(test)]
            _witness: None,
        });
        let handle = self.handle(reading);
        let query = self.query.as_mut().unwrap();
        let (buffer, size, class) = query.output.parts();
        let status =
            unsafe { NtQueryInformationFile(handle, &mut *query.iosb, buffer, size, class) };
        if status == STATUS_PENDING {
            self.query_pending = true;
            #[cfg(feature = "test-support")]
            self.trace.push(Event {
                phase: "query_pending",
                endpoint: label(reading),
                api: "NtQueryInformationFile",
                ok: false,
                status: Some(status as i64),
                error: None,
                observed: json!({"class":class,"storage_retained":true}),
            });
            // There is no qualified nonblocking completion proof for this profile.
            // Retain handle, IOSB and output untouched for the binary exit path.
            return Err(io::Error::other("native stdio query unexpectedly pending"));
        }
        let completion = unsafe { query.iosb.Anonymous.Status };
        let returned = query.iosb.Information;
        let expected = match &query.output {
            QueryOutput::Name(name)
                if profile::valid_name_reply(
                    status,
                    completion,
                    returned,
                    name.length,
                    NAME_UNITS * 2,
                ) =>
            {
                Some(4 + name.length as usize)
            }
            QueryOutput::Name(_) => None,
            _ => Some(size as usize),
        };
        let ok = status >= 0 && completion >= 0 && expected == Some(returned);
        #[cfg(feature = "test-support")]
        self.trace.push(Event {phase:"query",endpoint:label(reading),api:"NtQueryInformationFile",ok,status:Some(status as i64),error:None,observed:json!({"class":class,"completion_status":completion,"returned_bytes":returned,"expected_bytes":expected})});
        let query = self.query.take().unwrap();
        if !ok {
            return Err(io::Error::other(
                "native pipe query failed or returned wrong size",
            ));
        }
        Ok(query.output)
    }
    fn qualify(&mut self, reading: bool) -> io::Result<u32> {
        let kind = unsafe { GetFileType(self.handle(reading)) };
        #[cfg(feature = "test-support")]
        self.trace.push(Event {
            phase: "query",
            endpoint: label(reading),
            api: "GetFileType",
            ok: kind == FILE_TYPE_PIPE,
            status: Some(kind as i64),
            error: None,
            observed: json!({"type":kind}),
        });
        if kind != FILE_TYPE_PIPE {
            return Err(invalid("stdio requires pipes"));
        }
        // Locality precedes other potentially remote pipe information queries.
        let QueryOutput::Remote(remote) =
            self.query(reading, QueryOutput::Remote(Box::default()))?
        else {
            unreachable!()
        };
        if remote.IsRemote {
            return Err(invalid("remote stdio pipe unsupported"));
        }
        let QueryOutput::Access(access) =
            self.query(reading, QueryOutput::Access(Box::default()))?
        else {
            unreachable!()
        };
        let QueryOutput::Mode(mode) = self.query(reading, QueryOutput::Mode(Box::default()))?
        else {
            unreachable!()
        };
        let required = if reading {
            FILE_READ_DATA
        } else {
            FILE_WRITE_DATA
        };
        if access.AccessFlags & required == 0 || mode.Mode & 0x30 == 0 {
            return Err(invalid(
                "stdio requires synchronous pipe handles with data access",
            ));
        }
        let QueryOutput::Pipe(pipe) = self.query(reading, QueryOutput::Pipe(Box::default()))?
        else {
            unreachable!()
        };
        let QueryOutput::PipeMode(pipe_mode) =
            self.query(reading, QueryOutput::PipeMode(Box::default()))?
        else {
            unreachable!()
        };
        let ok =
            pipe.NamedPipeType == 0 && pipe_mode.ReadMode == 0 && pipe_mode.CompletionMode == 0;
        #[cfg(feature = "test-support")]
        self.trace.push(Event {phase:"qualified",endpoint:label(reading),api:"native pipe profile",ok,status:None,error:None,observed:json!({"access":access.AccessFlags,"mode":mode.Mode,"pipe_type":pipe.NamedPipeType,"remote":remote.IsRemote,"pipe_end":pipe.NamedPipeEnd,"pipe_configuration":pipe.NamedPipeConfiguration,"read_mode":pipe_mode.ReadMode,"completion_mode":pipe_mode.CompletionMode})});
        if !ok {
            return Err(invalid(
                "stdio requires local synchronous byte pipes with data access",
            ));
        }
        Ok(pipe.NamedPipeConfiguration)
    }
    pub(super) fn try_read(&mut self, bytes: &mut [u8]) -> io::Result<ReadStep> {
        if self.stopping {
            return Err(stopped());
        }
        if bytes.is_empty() {
            return Ok(ReadStep::Pending);
        }
        if self.read_in_flight
            && let Some(completed) = self.reader.as_ref().unwrap().completion()?
        {
            self.read_in_flight = false;
            #[cfg(feature = "test-support")]
            self.record_io(true, &completed);
            self.read_ready = Some((completed, 0));
        }
        if let Some((completed, offset)) = &mut self.read_ready {
            let result = completed.result(true)?;
            match result {
                None => {
                    self.read_buffer = self.read_ready.take().unwrap().0.buffer;
                    return Ok(ReadStep::Eof);
                }
                Some(0) => {
                    self.read_buffer = self.read_ready.take().unwrap().0.buffer;
                    return Ok(ReadStep::Pending);
                }
                Some(total) => {
                    let count = (total - *offset).min(bytes.len()).min(MAX_OPERATION);
                    bytes[..count].copy_from_slice(
                        &completed.buffer.as_ref().unwrap().bytes[*offset..*offset + count],
                    );
                    *offset += count;
                    if *offset == total {
                        self.read_buffer = self.read_ready.take().unwrap().0.buffer;
                    }
                    return Ok(ReadStep::Bytes(count));
                }
            }
        }
        if !self.read_in_flight {
            if let Err(command) = self.reader.as_ref().unwrap().dispatch(Command::Read(
                self.read_buffer.take().unwrap(),
                bytes.len().min(MAX_OPERATION),
            )) {
                if let Command::Read(buffer, _) = command {
                    self.read_buffer = Some(buffer);
                }
                self.request_stop();
                return Err(io::Error::other("stdio reader mailbox unavailable"));
            }
            self.read_in_flight = true;
        }
        Ok(ReadStep::Pending)
    }
    pub(super) fn try_write(&mut self, bytes: &[u8]) -> io::Result<WriteStep> {
        if self.stopping {
            return Err(stopped());
        }
        if let Some(receipt) = &self.write_receipt {
            if receipt.address != bytes.as_ptr() as usize
                || receipt.length != bytes.len()
                || bytes[..receipt.submitted] != self.write_buffer[..receipt.submitted]
            {
                return Err(invalid("pending stdio write requires unchanged slice"));
            }
            let Some(completed) = self.writer.as_ref().unwrap().completion()? else {
                return Ok(WriteStep::Pending);
            };
            #[cfg(feature = "test-support")]
            self.record_io(false, &completed);
            // An error preserves the receipt: caller must close, never splice.
            let count = completed.result(false)?.unwrap_or(0);
            self.write_receipt = None;
            return Ok(if count > 0 {
                WriteStep::Bytes(count)
            } else {
                WriteStep::Pending
            });
        }
        if bytes.is_empty() {
            return Ok(WriteStep::Pending);
        }
        let len = bytes.len().min(MAX_OPERATION);
        let buffer = Arc::get_mut(&mut self.write_buffer)
            .ok_or_else(|| io::Error::other("write buffer still owned by native worker"))?;
        buffer[..len].copy_from_slice(&bytes[..len]);
        self.write_receipt = Some(WriteReceipt {
            address: bytes.as_ptr() as usize,
            length: bytes.len(),
            submitted: len,
        });
        if self
            .writer
            .as_ref()
            .unwrap()
            .dispatch(Command::Write(self.write_buffer.clone(), len))
            .is_err()
        {
            self.request_stop();
            return Err(io::Error::other("stdio writer mailbox unavailable"));
        }
        Ok(WriteStep::Pending)
    }
    pub(super) fn write_in_flight(&self) -> bool {
        self.write_receipt.is_some()
    }
    pub(super) fn request_stop(&mut self) {
        self.stopping = true;
        for worker in [&self.reader, &self.writer].into_iter().flatten() {
            worker.request_stop();
        }
    }
    pub(super) fn poll_stop_or_restore(&mut self) -> io::Result<bool> {
        self.request_stop();
        if self.query_pending {
            return Ok(false);
        }
        let mut joined = true;
        for (reading, worker) in [(true, &mut self.reader), (false, &mut self.writer)] {
            let Some(worker) = worker else { continue };
            let had_join = worker.join.is_some();
            if !worker.poll_join() {
                let _cancellation = worker.cancel();
                #[cfg(feature = "test-support")]
                if let Some((status, error)) = _cancellation {
                    self.cancellations
                        .record(&mut self.trace, reading, status, error);
                }
                joined = false;
            } else if had_join {
                #[cfg(feature = "test-support")]
                self.trace.push(Event {
                    phase: "joined",
                    endpoint: label(reading),
                    api: "JoinHandle::join",
                    ok: true,
                    status: None,
                    error: None,
                    observed: json!({"actual_join":true}),
                });
            }
            #[cfg(not(feature = "test-support"))]
            let _ = (reading, had_join);
        }
        #[cfg(feature = "test-support")]
        if self.control == Control::BeforeCall {
            self.release_control();
        }
        if !joined {
            return Ok(false);
        }
        #[cfg(feature = "test-support")]
        if self.control == Control::HoldRestore {
            return Err(io::Error::other("synthetic retained restoration failure"));
        }
        // Completion messages are not join proof. They are collected only after
        // actual joins; worker panic payloads remain in their Worker owners.
        if let Some(worker) = &self.reader
            && let Ok(Some(completed)) = worker.completion()
        {
            #[cfg(feature = "test-support")]
            self.record_io(true, &completed);
            self.read_ready = Some((completed, 0));
        }
        if let Some(worker) = &self.writer
            && let Ok(Some(completed)) = worker.completion()
        {
            #[cfg(feature = "test-support")]
            self.record_io(false, &completed);
            #[cfg(not(feature = "test-support"))]
            let _ = completed;
        }
        self.read_in_flight = false;
        self.write_receipt = None;
        self.output.take();
        self.input.take();
        self.settled = true;
        if [&self.reader, &self.writer]
            .into_iter()
            .flatten()
            .any(|worker| worker.panic.is_some())
        {
            return Err(io::Error::other("stdio native worker panicked"));
        }
        Ok(true)
    }
    pub(super) fn is_settled(&self) -> bool {
        self.settled
    }
    #[cfg(feature = "test-support")]
    pub(super) fn before_call(&self) -> bool {
        self.reader
            .as_ref()
            .is_some_and(|worker| worker.barrier.load(std::sync::atomic::Ordering::Acquire))
    }
    #[cfg(feature = "test-support")]
    pub(super) fn release_control(&mut self) {
        for worker in [&self.reader, &self.writer].into_iter().flatten() {
            worker
                .release
                .store(true, std::sync::atomic::Ordering::Release);
        }
        self.control = Control::Normal;
    }
    #[cfg(feature = "test-support")]
    pub(super) fn ownership(&self) -> serde_json::Value {
        json!({"owned_endpoints":usize::from(self.input.is_some())+usize::from(self.output.is_some()),"retained_joins":self.reader.iter().chain(self.writer.iter()).filter(|w|w.join.is_some()).count(),"query_pending":self.query_pending,"write_in_flight":self.write_in_flight(),"read_in_flight":self.read_in_flight,"actual_input_type":self.input.as_ref().map(|h|unsafe {GetFileType(h.as_raw_handle())}),"actual_output_type":self.output.as_ref().map(|h|unsafe {GetFileType(h.as_raw_handle())})})
    }
    #[cfg(feature = "test-support")]
    fn record_io(&mut self, reading: bool, completed: &Completion) {
        let result = completed.result(reading);
        self.trace.push(Event {phase:"io",endpoint:label(reading),api:if reading {"ReadFile(synchronous)"} else {"WriteFile(synchronous)"},ok:result.is_ok(),status:Some(completed.status as i64),error:completed.error,observed:json!({"requested":completed.requested,"count":completed.count,"result":match result {Ok(Some(n)) if n>0=>"Bytes",Ok(Some(_))=>"Pending",Ok(None)=>"Eof",Err(_)=>"Error"}})});
    }
}
#[cfg(feature = "test-support")]
fn label(reading: bool) -> &'static str {
    if reading { "stdin" } else { "stdout" }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn synthetic_pending_query_retains_real_handle_and_query_storage_on_early_drop() {
        // Synthetic pending status, actual kernel pipe handle. This is an owner
        // lifetime test, not evidence that Windows queued this profile query.
        let (mut read, mut write) = (std::ptr::null_mut(), std::ptr::null_mut());
        assert_ne!(
            unsafe {
                windows_sys::Win32::System::Pipes::CreatePipe(
                    &mut read,
                    &mut write,
                    std::ptr::null(),
                    4096,
                )
            },
            0
        );
        let handle = unsafe { OwnedHandle::from_raw_handle(read) };
        let _peer = unsafe { OwnedHandle::from_raw_handle(write) };
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut native = Native::new();
        native.input = Some(Arc::new(handle));
        native.settled = false;
        native.query_pending = true;
        native.query = Some(Query {
            iosb: Box::default(),
            output: QueryOutput::Name(Box::default()),
            _witness: Some(QueryWitness(dropped.clone())),
        });
        let owner = crate::stdio::Stdio {
            native: Some(Box::new(native)),
            _not_sync: std::marker::PhantomData,
        };
        let mut failure = crate::stdio::AcquireFailure {
            error: io::Error::other("synthetic pending query"),
            owner,
        };
        assert!(!failure.poll_stop_or_restore().unwrap());
        assert!(!failure.is_settled());
        drop(failure);
        assert!(!dropped.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(unsafe { GetFileType(read) }, FILE_TYPE_PIPE);
    }
}
