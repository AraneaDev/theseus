//! Local-only first-instance named pipes. Every OVERLAPPED and byte buffer stays
//! owned until terminal GetOverlappedResult; cancellation is not completion.
use super::*;
use std::{ffi::c_void, mem, ptr};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_BROKEN_PIPE, ERROR_INSUFFICIENT_BUFFER, ERROR_IO_INCOMPLETE,
        ERROR_IO_PENDING, ERROR_NOT_FOUND, ERROR_OPERATION_ABORTED, ERROR_PIPE_CONNECTED,
        GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
    },
    Security::{GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser},
    Storage::FileSystem::{
        CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
        PIPE_ACCESS_DUPLEX, ReadFile, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, WriteFile,
    },
    System::{
        IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
            GetNamedPipeClientSessionId, GetNamedPipeServerProcessId, GetNamedPipeServerSessionId,
            PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
        },
        Threading::{
            CreateEventW, GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId, OpenProcess,
            OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    },
};
#[link(name = "kernel32")]
unsafe extern "system" {
    fn ProcessIdToSessionId(process: u32, session: *mut u32) -> i32;
}
#[link(name = "advapi32")]
unsafe extern "system" {
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        text: *const u16,
        revision: u32,
        descriptor: *mut *mut c_void,
        size: *mut u32,
    ) -> i32;
    fn ConvertSidToStringSidW(sid: *const c_void, text: *mut *mut u16) -> i32;
}
#[link(name = "user32")]
unsafe extern "system" {
    fn GetProcessWindowStation() -> HANDLE;
    fn GetThreadDesktop(thread: u32) -> HANDLE;
    fn GetUserObjectInformationW(
        object: HANDLE,
        index: i32,
        info: *mut c_void,
        length: u32,
        needed: *mut u32,
    ) -> i32;
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
#[derive(Debug)]
struct NativeError {
    api: &'static str,
    error: io::Error,
}
impl std::fmt::Display for NativeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.api, self.error)
    }
}
impl std::error::Error for NativeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}
fn native(api: &'static str, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), NativeError { api, error })
}
fn last(api: &'static str) -> io::Error {
    native(api, io::Error::last_os_error())
}
fn code(error: &io::Error) -> Option<i32> {
    error
        .get_ref()
        .and_then(|e| e.downcast_ref::<NativeError>())
        .and_then(|e| e.error.raw_os_error())
        .or(error.raw_os_error())
}
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}
struct Allocation(*mut c_void);
impl Drop for Allocation {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
fn sid_from_process(process: HANDLE) -> io::Result<(String, Handle)> {
    let mut raw = ptr::null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut raw) } == 0 {
        return Err(last("OpenProcessToken"));
    }
    let token = Handle(raw);
    let mut length = 0;
    unsafe { GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut length) };
    if unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER
        || length < mem::size_of::<TOKEN_USER>() as u32
        || length > 4096
    {
        return Err(last("GetTokenInformation(TokenUser size)"));
    }
    let mut storage = [0usize; 4096 / mem::size_of::<usize>()];
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            storage.as_mut_ptr().cast(),
            length,
            &mut length,
        )
    } == 0
    {
        return Err(last("GetTokenInformation(TokenUser)"));
    }
    let sid = unsafe { (*storage.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let start = sid as usize;
    let base = storage.as_ptr() as usize;
    let end = base
        .checked_add(length as usize)
        .ok_or_else(|| invalid("token storage overflow"))?;
    if start < base + mem::size_of::<TOKEN_USER>()
        || start.checked_add(8).is_none_or(|v| v > end)
        || !start.is_multiple_of(4)
    {
        return Err(invalid("native token SID outside query storage"));
    }
    let count = unsafe { *sid.cast::<u8>().add(1) } as usize;
    if count > 15 || start.checked_add(8 + count * 4).is_none_or(|v| v > end) {
        return Err(invalid("native token SID exceeds bound"));
    }
    let mut output = ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut output) } == 0 {
        return Err(last("ConvertSidToStringSidW"));
    }
    let _allocation = Allocation(output.cast());
    let mut units = 0;
    while units <= 256 && unsafe { *output.add(units) } != 0 {
        units += 1;
    }
    if units > 256 {
        return Err(invalid("unbounded native SID"));
    }
    let text = String::from_utf16(unsafe { std::slice::from_raw_parts(output, units) })
        .map_err(|_| invalid("invalid native SID"))?;
    crate::services::readiness::Sid::new(text.clone())?;
    Ok((text, token))
}
fn object_name(object: HANDLE) -> io::Result<String> {
    if object.is_null() {
        return Err(last("GetProcessWindowStation/GetThreadDesktop"));
    }
    let mut needed = 0;
    let mut bytes = [0u16; 512];
    if unsafe {
        GetUserObjectInformationW(
            object,
            2,
            bytes.as_mut_ptr().cast(),
            mem::size_of_val(&bytes) as u32,
            &mut needed,
        )
    } == 0
    {
        return Err(last("GetUserObjectInformationW(UOI_NAME)"));
    }
    if needed < 2 || needed as usize > mem::size_of_val(&bytes) || needed % 2 != 0 {
        return Err(invalid("unbounded native desktop/station name"));
    }
    let units = needed as usize / 2;
    if bytes[units - 1] != 0 || bytes[..units - 1].contains(&0) {
        return Err(invalid("invalid native desktop/station name"));
    }
    String::from_utf16(&bytes[..units - 1])
        .map_err(|_| invalid("non-UTF16 native desktop/station name"))
}
pub(super) fn measure_local_identity() -> io::Result<MeasuredIdentity> {
    let (sid, _token) = sid_from_process(unsafe { GetCurrentProcess() })?;
    let principal = CurrentPrincipal::Windows {
        token_user_sid: sid,
    };
    if crate::principal::current_principal()? != principal {
        return Err(invalid(
            "broker transport forbids a different effective thread token",
        ));
    }
    let pid = unsafe { GetCurrentProcessId() };
    let mut session = 0;
    if unsafe { ProcessIdToSessionId(pid, &mut session) } == 0 {
        return Err(last("ProcessIdToSessionId(current process)"));
    }
    Ok(MeasuredIdentity {
        principal,
        diagnostic_pid: Some(pid),
        session_id: Some(session),
        posix_process_session: None,
        window_station: Some(object_name(unsafe { GetProcessWindowStation() })?),
        desktop: Some(object_name(unsafe {
            GetThreadDesktop(GetCurrentThreadId())
        })?),
    })
}
fn ceiling(value: &PeerCeiling) -> io::Result<()> {
    let CurrentPrincipal::Windows { token_user_sid } = &value.principal else {
        return Err(invalid("non-Windows peer ceiling"));
    };
    crate::services::readiness::Sid::new(token_user_sid.clone())?;
    Ok(())
}
fn endpoint(value: BrokerEndpoint) -> io::Result<Vec<u16>> {
    match value {
        BrokerEndpoint::Pipe(path) => Ok(wide(path.as_str())),
        _ => Err(invalid("non-named-pipe broker endpoint")),
    }
}
struct PeerOwners {
    _process: Handle,
    _token: Handle,
}
fn peer(pipe: HANDLE, server: bool) -> io::Result<(MeasuredIdentity, PeerOwners)> {
    let (mut pid, mut session) = (0, 0);
    let pid_call = if server {
        GetNamedPipeClientProcessId
    } else {
        GetNamedPipeServerProcessId
    };
    let session_call = if server {
        GetNamedPipeClientSessionId
    } else {
        GetNamedPipeServerSessionId
    };
    if unsafe { pid_call(pipe, &mut pid) } == 0 {
        return Err(last("GetNamedPipePeerProcessId"));
    }
    if unsafe { session_call(pipe, &mut session) } == 0 {
        return Err(last("GetNamedPipePeerSessionId"));
    }
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if raw.is_null() {
        return Err(last("OpenProcess(query-only named-pipe peer metadata)"));
    }
    let process = Handle(raw);
    let (sid, token) = sid_from_process(process.0)?;
    let (mut after_pid, mut after_session) = (0, 0);
    if unsafe { pid_call(pipe, &mut after_pid) } == 0
        || unsafe { session_call(pipe, &mut after_session) } == 0
    {
        return Err(last("GetNamedPipePeer identity recheck"));
    }
    if pid == 0 || pid != after_pid || session != after_session {
        return Err(invalid(
            "original named-pipe peer changed during token query",
        ));
    }
    Ok((
        MeasuredIdentity {
            principal: CurrentPrincipal::Windows {
                token_user_sid: sid,
            },
            diagnostic_pid: Some(pid),
            session_id: Some(session),
            posix_process_session: None,
            window_station: None,
            desktop: None,
        },
        PeerOwners {
            _process: process,
            _token: token,
        },
    ))
}
#[derive(Clone, Copy)]
enum Kind {
    Connect,
    Read,
    Write,
}
struct Slot {
    overlapped: Box<OVERLAPPED>,
    _event: Handle,
    buffer: Vec<u8>,
    pending: bool,
    inline: usize,
    kind: Kind,
}
impl Slot {
    fn new(kind: Kind, buffer: Vec<u8>) -> io::Result<Self> {
        let event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
        if event.is_null() {
            return Err(last("CreateEventW"));
        }
        let mut overlapped = Box::<OVERLAPPED>::default();
        overlapped.hEvent = event;
        Ok(Self {
            overlapped,
            _event: Handle(event),
            buffer,
            pending: false,
            inline: 0,
            kind,
        })
    }
    fn issue(&mut self, pipe: HANDLE) -> io::Result<()> {
        let ok = unsafe {
            match self.kind {
                Kind::Connect => ConnectNamedPipe(pipe, &mut *self.overlapped),
                Kind::Read => ReadFile(
                    pipe,
                    self.buffer.as_mut_ptr(),
                    self.buffer.len() as u32,
                    ptr::null_mut(),
                    &mut *self.overlapped,
                ),
                Kind::Write => WriteFile(
                    pipe,
                    self.buffer.as_ptr(),
                    self.buffer.len() as u32,
                    ptr::null_mut(),
                    &mut *self.overlapped,
                ),
            }
        };
        if ok != 0 {
            self.pending = !matches!(self.kind, Kind::Connect);
            return Ok(());
        }
        let error = unsafe { GetLastError() };
        if error == ERROR_IO_PENDING {
            self.pending = true;
            return Ok(());
        }
        if matches!(self.kind, Kind::Connect) && error == ERROR_PIPE_CONNECTED {
            return Ok(());
        }
        Err(native(
            "named-pipe native I/O submission",
            io::Error::from_raw_os_error(error as i32),
        ))
    }
    fn result(&mut self, pipe: HANDLE) -> io::Result<Option<usize>> {
        if !self.pending {
            return Ok(Some(self.inline));
        }
        let mut transferred = 0;
        if unsafe { GetOverlappedResult(pipe, &*self.overlapped, &mut transferred, 0) } != 0 {
            self.pending = false;
            return Ok(Some(transferred as usize));
        }
        let error = unsafe { GetLastError() };
        if error == ERROR_IO_INCOMPLETE {
            return Ok(None);
        }
        self.pending = false;
        Err(native(
            "GetOverlappedResult",
            io::Error::from_raw_os_error(error as i32),
        ))
    }
    fn stop(&mut self, pipe: HANDLE) -> io::Result<bool> {
        if !self.pending {
            return Ok(true);
        }
        if unsafe { CancelIoEx(pipe, &*self.overlapped) } == 0
            && unsafe { GetLastError() } != ERROR_NOT_FOUND
        {
            return Err(last("CancelIoEx"));
        }
        match self.result(pipe) {
            Ok(None) => Ok(false),
            Ok(Some(_)) => Ok(true),
            Err(e) if code(&e) == Some(ERROR_OPERATION_ABORTED as i32) => Ok(true),
            Err(e) => Err(e),
        }
    }
}
pub(super) struct Stream {
    pipe: Option<Handle>,
    local: MeasuredIdentity,
    measured: Option<MeasuredIdentity>,
    expected: PeerCeiling,
    peer_owners: Option<PeerOwners>,
    read: Option<Slot>,
    write: Option<Slot>,
    stopping: bool,
    server: bool,
}
impl Stream {
    pub fn connect(
        endpoint_value: BrokerEndpoint,
        local: PeerCeiling,
        expected: PeerCeiling,
    ) -> io::Result<Self> {
        ceiling(&local)?;
        ceiling(&expected)?;
        let measured = measure_local_identity()?;
        admit(&measured, &local)?;
        let name = endpoint(endpoint_value)?;
        let raw = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(last(
                "CreateFileW(explicit local broker, identification-only SQOS)",
            ));
        }
        Ok(Self {
            pipe: Some(Handle(raw)),
            local: measured,
            measured: None,
            expected,
            peer_owners: None,
            read: None,
            write: None,
            stopping: false,
            server: false,
        })
    }
    pub fn local_identity(&self) -> &MeasuredIdentity {
        &self.local
    }
    pub fn identity(&self) -> Option<&MeasuredIdentity> {
        self.measured.as_ref()
    }
    pub fn poll_connected(&mut self) -> io::Result<bool> {
        if self.measured.is_none() {
            let (identity, owners) = peer(
                self.pipe.as_ref().ok_or_else(|| invalid("pipe settled"))?.0,
                self.server,
            )?;
            admit(&identity, &self.expected)?;
            self.measured = Some(identity);
            self.peer_owners = Some(owners);
        }
        Ok(true)
    }
    pub fn try_read(&mut self, bytes: &mut [u8]) -> io::Result<ReadStep> {
        let pipe = self.pipe.as_ref().ok_or_else(|| invalid("pipe settled"))?.0;
        if self.read.is_none() {
            let mut slot = Slot::new(Kind::Read, vec![0; 4096])?;
            let result = slot.issue(pipe);
            self.read = Some(slot);
            if let Err(e) = result {
                if code(&e) == Some(ERROR_BROKEN_PIPE as i32) {
                    self.read = None;
                    return Ok(ReadStep::Eof);
                }
                return Err(e);
            }
        }
        let result = self.read.as_mut().expect("original read").result(pipe);
        match result {
            Ok(None) => Ok(ReadStep::Pending),
            Ok(Some(n)) => {
                let slot = self.read.take().expect("terminal original read");
                if n > bytes.len() {
                    self.read = Some(slot);
                    return Err(invalid(
                        "caller read buffer cannot replace pending native buffer",
                    ));
                }
                bytes[..n].copy_from_slice(&slot.buffer[..n]);
                if n == 0 {
                    Ok(ReadStep::Eof)
                } else {
                    Ok(ReadStep::Bytes(n))
                }
            }
            Err(e) if code(&e) == Some(ERROR_BROKEN_PIPE as i32) => {
                self.read = None;
                Ok(ReadStep::Eof)
            }
            Err(e) => Err(e),
        }
    }
    pub fn try_write(&mut self, bytes: &[u8]) -> io::Result<WriteStep> {
        let pipe = self.pipe.as_ref().ok_or_else(|| invalid("pipe settled"))?.0;
        if let Some(slot) = &self.write
            && slot.buffer != bytes
        {
            return Err(invalid("pending original pipe write bytes cannot change"));
        }
        if self.write.is_none() {
            let mut slot = Slot::new(Kind::Write, bytes.to_vec())?;
            let result = slot.issue(pipe);
            self.write = Some(slot);
            result?;
        }
        match self.write.as_mut().expect("original write").result(pipe)? {
            None => Ok(WriteStep::Pending),
            Some(n) => {
                if n == 0 || n > bytes.len() {
                    return Err(invalid("invalid native pipe write progress"));
                }
                self.write = None;
                Ok(WriteStep::Bytes(n))
            }
        }
    }
    pub fn write_in_flight(&self) -> bool {
        self.write.as_ref().is_some_and(|slot| slot.pending)
    }
    pub fn request_stop(&mut self) {
        self.stopping = true;
    }
    pub fn poll_settle(&mut self) -> io::Result<bool> {
        if !self.stopping {
            return Ok(false);
        }
        let Some(pipe) = self.pipe.as_ref().map(|p| p.0) else {
            return Ok(true);
        };
        let mut done = true;
        for slot in [&mut self.read, &mut self.write] {
            if let Some(io) = slot {
                if io.stop(pipe)? {
                    *slot = None;
                } else {
                    done = false;
                }
            }
        }
        if !done {
            return Ok(false);
        }
        if self.server && unsafe { DisconnectNamedPipe(pipe) } == 0 {
            let error = unsafe { GetLastError() };
            if error != windows_sys::Win32::Foundation::ERROR_PIPE_NOT_CONNECTED {
                return Err(native(
                    "DisconnectNamedPipe",
                    io::Error::from_raw_os_error(error as i32),
                ));
            }
        }
        self.peer_owners = None;
        self.pipe = None;
        Ok(true)
    }
}
pub(super) struct Listener {
    name: Vec<u16>,
    local: MeasuredIdentity,
    peer: PeerCeiling,
    descriptor: Option<Allocation>,
    pipe: Option<Handle>,
    connect: Option<Slot>,
    stopping: bool,
}
impl Listener {
    pub fn prepare(
        endpoint_value: BrokerEndpoint,
        local: PeerCeiling,
        expected: PeerCeiling,
    ) -> io::Result<Self> {
        ceiling(&local)?;
        ceiling(&expected)?;
        let measured = measure_local_identity()?;
        admit(&measured, &local)?;
        let CurrentPrincipal::Windows {
            token_user_sid: server_sid,
        } = &measured.principal
        else {
            return Err(invalid("non-Windows local identity"));
        };
        let CurrentPrincipal::Windows {
            token_user_sid: client_sid,
        } = &expected.principal
        else {
            return Err(invalid("non-Windows peer identity"));
        };
        let text = wide(&format!(
            "D:P(A;;GRGW;;;{server_sid})(A;;GRGW;;;{client_sid})"
        ));
        let mut descriptor = ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                1,
                &mut descriptor,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(last("ConvertStringSecurityDescriptorToSecurityDescriptorW"));
        }
        Ok(Self {
            name: endpoint(endpoint_value)?,
            local: measured,
            peer: expected,
            descriptor: Some(Allocation(descriptor)),
            pipe: None,
            connect: None,
            stopping: false,
        })
    }
    pub fn start_bind(&mut self) -> io::Result<()> {
        let descriptor = self
            .descriptor
            .as_ref()
            .ok_or_else(|| invalid("original pipe descriptor unavailable"))?
            .0;
        let security = SECURITY_ATTRIBUTES {
            nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let raw = unsafe {
            CreateNamedPipeW(
                self.name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                4096,
                4096,
                0,
                &security,
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(last(
                "CreateNamedPipeW(local first instance, protected explicit principals)",
            ));
        }
        self.pipe = Some(Handle(raw));
        Ok(())
    }
    pub fn local_identity(&self) -> &MeasuredIdentity {
        &self.local
    }
    pub fn try_accept(&mut self) -> io::Result<Option<Stream>> {
        if self.pipe.is_none() {
            self.start_bind()?;
        }
        let pipe = self.pipe.as_ref().expect("original pipe").0;
        if self.connect.is_none() {
            let mut slot = Slot::new(Kind::Connect, Vec::new())?;
            let result = slot.issue(pipe);
            self.connect = Some(slot);
            result?;
        }
        if self
            .connect
            .as_mut()
            .expect("original connect")
            .result(pipe)?
            .is_none()
        {
            return Ok(None);
        }
        self.connect = None;
        let (identity, owners) = peer(pipe, true)?;
        if let Err(error) = admit(&identity, &self.peer) {
            if unsafe { DisconnectNamedPipe(pipe) } == 0 {
                return Err(last("DisconnectNamedPipe(rejected original peer)"));
            }
            return Err(error);
        }
        Ok(Some(Stream {
            pipe: self.pipe.take(),
            local: self.local.clone(),
            measured: Some(identity),
            expected: self.peer.clone(),
            peer_owners: Some(owners),
            read: None,
            write: None,
            stopping: false,
            server: true,
        }))
    }
    pub fn request_stop(&mut self) {
        self.stopping = true;
    }
    pub fn poll_settle(&mut self) -> io::Result<bool> {
        if !self.stopping {
            return Ok(false);
        }
        if let Some(pipe) = &self.pipe
            && let Some(slot) = &mut self.connect
        {
            if !slot.stop(pipe.0)? {
                return Ok(false);
            }
            self.connect = None;
        }
        self.pipe = None;
        self.descriptor = None;
        Ok(true)
    }
}
