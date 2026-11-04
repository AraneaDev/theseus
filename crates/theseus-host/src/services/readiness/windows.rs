//! Local-only first-instance pipe. All OVERLAPPED storage remains owned until
//! GetOverlappedResult(FALSE) reports terminal completion; cancellation is a request.
use super::*;
use std::{
    ffi::c_void,
    mem,
    os::windows::io::{AsRawHandle, BorrowedHandle},
    ptr,
    rc::Rc,
};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, CompareObjectHandles, DuplicateHandle, ERROR_INSUFFICIENT_BUFFER,
        ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED,
        GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
    },
    Security::{GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser},
    Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX, ReadFile,
        WriteFile,
    },
    System::{
        IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
            PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
        },
        Threading::{
            CreateEventW, GetCurrentProcess, GetProcessId, OpenProcess, OpenProcessToken,
            PROCESS_QUERY_LIMITED_INFORMATION,
        },
    },
};
// SDK authorization feature is intentionally not added to the shared manifest.
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipePeerExpectation {
    pub sid: Sid,
    /// Original accepted Start observation, never a later reconstructed owner.
    pub original_pid: u32,
    pub cooperative_instance: String,
    pub instance_binding_digest: [u8; 32],
}
// Keep the original io::Error (and native Win32 code) with its exact API.
// Public readiness classification stays bounded and does not own diagnostics.
#[derive(Debug)]
struct NativeError {
    api: &'static str,
    error: io::Error,
}
impl std::fmt::Display for NativeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (Win32): {}", self.api, self.error)
    }
}
impl std::error::Error for NativeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}
fn native_error(api: &'static str, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), NativeError { api, error })
}
fn last_native_error(api: &'static str) -> io::Error {
    native_error(api, io::Error::last_os_error())
}
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
struct LocalAllocation(*mut c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
fn token_sid(process: HANDLE) -> io::Result<(Sid, Handle)> {
    let mut token = ptr::null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(last_native_error("OpenProcessToken"));
    }
    let token = Handle(token);
    let mut needed = 0;
    let sized = unsafe { GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut needed) };
    if sized == 0 {
        let error = unsafe { GetLastError() };
        if error != ERROR_INSUFFICIENT_BUFFER {
            return Err(native_error(
                "GetTokenInformation(TokenUser size)",
                io::Error::from_raw_os_error(error as i32),
            ));
        }
    }
    if needed < mem::size_of::<TOKEN_USER>() as u32 || needed > 4096 {
        return Err(invalid("unbounded token user metadata"));
    }
    // Native TOKEN_USER contains pointers; ensure native alignment explicitly.
    let mut buffer = vec![0usize; (needed as usize).div_ceil(mem::size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(last_native_error("GetTokenInformation(TokenUser)"));
    }
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut string = ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut string) } == 0 {
        return Err(last_native_error("ConvertSidToStringSidW"));
    }
    let allocation = LocalAllocation(string.cast());
    let mut length = 0;
    while length <= 184 && unsafe { *string.add(length) } != 0 {
        length += 1;
    }
    if length > 184 {
        return Err(invalid("unbounded token SID"));
    }
    let sid = String::from_utf16(unsafe { std::slice::from_raw_parts(string, length) })
        .map_err(|_| invalid("invalid native SID"))?;
    drop(allocation);
    Ok((Sid::new(sid)?, token))
}
#[derive(Clone, Copy)]
enum Kind {
    Connect,
    Write,
    Prefix,
    Body,
}
struct IoSlot {
    overlapped: Box<OVERLAPPED>,
    _event: Handle,
    buffer: Vec<u8>,
    kind: Kind,
    pending: bool,
    immediate: u32,
}
impl IoSlot {
    fn new(kind: Kind, buffer: Vec<u8>) -> io::Result<Self> {
        let event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
        if event.is_null() {
            return Err(last_native_error("CreateEventW"));
        }
        let mut overlapped = Box::<OVERLAPPED>::default();
        overlapped.hEvent = event;
        Ok(Self {
            overlapped,
            _event: Handle(event),
            buffer,
            kind,
            pending: false,
            immediate: 0,
        })
    }
    fn issue(&mut self, pipe: HANDLE) -> io::Result<()> {
        let ok = unsafe {
            match self.kind {
                Kind::Connect => ConnectNamedPipe(pipe, &mut *self.overlapped),
                Kind::Write => WriteFile(
                    pipe,
                    self.buffer.as_ptr(),
                    self.buffer.len() as u32,
                    ptr::null_mut(),
                    &mut *self.overlapped,
                ),
                Kind::Prefix | Kind::Body => ReadFile(
                    pipe,
                    self.buffer.as_mut_ptr(),
                    self.buffer.len() as u32,
                    ptr::null_mut(),
                    &mut *self.overlapped,
                ),
            }
        };
        if ok != 0 {
            // Obtain byte counts from the original OVERLAPPED even for inline
            // completion; never expose a movable stack byte-count pointer.
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
        let api = match self.kind {
            Kind::Connect => "ConnectNamedPipe",
            Kind::Write => "WriteFile",
            Kind::Prefix | Kind::Body => "ReadFile",
        };
        Err(native_error(
            api,
            io::Error::from_raw_os_error(error as i32),
        ))
    }
    fn result(&mut self, pipe: HANDLE) -> io::Result<Option<usize>> {
        if !self.pending {
            return Ok(Some(self.immediate as usize));
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
        self.pending = false; // Any other completion result is terminal, including ABORTED.
        Err(native_error(
            "GetOverlappedResult",
            io::Error::from_raw_os_error(error as i32),
        ))
    }
    fn cancel(&self, pipe: HANDLE) -> io::Result<()> {
        if self.pending && unsafe { CancelIoEx(pipe, &*self.overlapped) } == 0 {
            return Err(last_native_error("CancelIoEx"));
        }
        Ok(())
    }
}
struct ProbeState {
    challenge: ReadinessChallenge,
    expectation: ReadinessExpectation,
    peer: PipePeerExpectation,
    request: Vec<u8>,
    written: usize,
    prefix: Vec<u8>,
    body: Vec<u8>,
    connected: bool,
    authenticated: bool,
    measured_sid: Option<Sid>,
    // Query-only metadata owners, never process-control/lifetime authorities.
    process: Option<Handle>,
    original_process: Option<Handle>,
    token: Option<Handle>,
    slot: Option<IoSlot>,
    last: Option<ReadinessObservation>,
    terminal: bool,
}
/// Single native owner; !Send/!Sync so native buffers/events stay on its worker.
pub struct LocalChallenge {
    endpoint: NamedPipe,
    helper_sid: Sid,
    pipe: Option<Handle>,
    active: Option<ProbeState>,
    revoked: bool,
    // Channel lifetime, rather than probe lifetime: take/abandon/cleanup cannot
    // discard the original terminal native failures. At most 16 original errors.
    retained_errors: Vec<io::Error>,
    _worker: Rc<()>,
}
impl LocalChallenge {
    pub fn bind(endpoint: NamedPipe, helper_sid: Sid) -> io::Result<Self> {
        let (measured, _token) = token_sid(unsafe { GetCurrentProcess() })?;
        if measured != helper_sid {
            return Err(invalid("declared pipe helper SID differs from token user"));
        }
        // Protected DACL contains precisely these two token users, no inherited ACEs.
        // 0x12019f = file generic read/write + synchronize, no change-owner/ACL grants.
        let sddl = wide(&format!(
            "D:P(A;;0x12019f;;;{})(A;;0x12019f;;;S-1-5-18)",
            helper_sid.as_str()
        ));
        let mut descriptor = ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(last_native_error(
                "ConvertStringSecurityDescriptorToSecurityDescriptorW",
            ));
        }
        let _descriptor = LocalAllocation(descriptor);
        let attributes = SECURITY_ATTRIBUTES {
            nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let name = wide(endpoint.as_str());
        let pipe = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                (MAX_FRAME_BYTES + 4) as u32,
                (MAX_FRAME_BYTES + 4) as u32,
                0,
                &attributes,
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            return Err(last_native_error("CreateNamedPipeW"));
        }
        Ok(Self {
            endpoint,
            helper_sid,
            pipe: Some(Handle(pipe)),
            active: None,
            revoked: false,
            retained_errors: Vec::new(),
            _worker: Rc::new(()),
        })
    }
    pub fn endpoint(&self) -> &NamedPipe {
        &self.endpoint
    }
    pub fn is_revoked(&self) -> bool {
        self.revoked
    }
    /// Re-arm the SAME first-instance server handle after an accepted SUT
    /// transition. A pending OVERLAPPED/nonce is retained, never canceled here.
    pub fn renew_after_sut_transition(&mut self) -> io::Result<()> {
        if self.revoked || self.pipe.is_none() || self.active.is_some() {
            return Err(invalid(
                "original pipe probe must retire before SUT transition renewal",
            ));
        }
        self.disconnect_original()
    }
    pub fn pending_observation(&self) -> Option<&ReadinessObservation> {
        self.active.as_ref().and_then(|a| a.last.as_ref())
    }
    pub fn probe(
        &mut self,
        challenge: &ReadinessChallenge,
        expectation: &ReadinessExpectation,
        peer: &PipePeerExpectation,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<ReadinessObservation> {
        self.probe_inner(challenge, expectation, peer, None, deadline, cancel, false)
            .and_then(completed_probe)
    }
    /// Uses only the supplied original process object. A query-only duplicate
    /// stays owned during pending I/O and is compared by object on every resume.
    #[allow(clippy::too_many_arguments)]
    pub fn probe_original_process(
        &mut self,
        challenge: &ReadinessChallenge,
        expectation: &ReadinessExpectation,
        peer: &PipePeerExpectation,
        original: BorrowedHandle<'_>,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<ReadinessObservation> {
        self.original_process_probe(
            challenge,
            expectation,
            peer,
            original,
            deadline,
            cancel,
            false,
        )
        .and_then(completed_probe)
    }
    /// One nonblocking advance retaining and comparing the SAME original process
    /// object on every resume. None yields without changing its semantic deadline.
    #[allow(clippy::too_many_arguments)]
    pub fn poll_original_process(
        &mut self,
        challenge: &ReadinessChallenge,
        expectation: &ReadinessExpectation,
        peer: &PipePeerExpectation,
        original: BorrowedHandle<'_>,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Option<ReadinessObservation>> {
        self.original_process_probe(
            challenge,
            expectation,
            peer,
            original,
            deadline,
            cancel,
            true,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn original_process_probe(
        &mut self,
        challenge: &ReadinessChallenge,
        expectation: &ReadinessExpectation,
        peer: &PipePeerExpectation,
        original: BorrowedHandle<'_>,
        deadline: Instant,
        cancel: &AtomicBool,
        poll_only: bool,
    ) -> io::Result<Option<ReadinessObservation>> {
        let (pid, sid) = Self::original_process_metadata(original)?;
        if pid != peer.original_pid || sid != peer.sid {
            return Err(invalid("original process differs from bound pipe peer"));
        }
        let mut duplicate = ptr::null_mut();
        if unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                original.as_raw_handle(),
                GetCurrentProcess(),
                &mut duplicate,
                PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                0,
            )
        } == 0
        {
            return Err(last_native_error(
                "DuplicateHandle(original query metadata)",
            ));
        }
        self.probe_inner(
            challenge,
            expectation,
            peer,
            Some(Handle(duplicate)),
            deadline,
            cancel,
            poll_only,
        )
    }
    pub fn original_process_metadata(original: BorrowedHandle<'_>) -> io::Result<(u32, Sid)> {
        let pid = unsafe { GetProcessId(original.as_raw_handle()) };
        if pid == 0 {
            return Err(last_native_error("GetProcessId(original)"));
        }
        let (sid, _token) = token_sid(original.as_raw_handle())?;
        Ok((pid, sid))
    }
    #[allow(clippy::too_many_arguments)]
    /// One bounded nonblocking advance using the original semantic deadline.
    /// None preserves all original OVERLAPPED/buffer/channel owners for polling.
    pub fn poll(
        &mut self,
        challenge: &ReadinessChallenge,
        expectation: &ReadinessExpectation,
        peer: &PipePeerExpectation,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Option<ReadinessObservation>> {
        self.probe_inner(challenge, expectation, peer, None, deadline, cancel, true)
    }
    #[allow(clippy::too_many_arguments)]
    fn probe_inner(
        &mut self,
        challenge: &ReadinessChallenge,
        expectation: &ReadinessExpectation,
        peer: &PipePeerExpectation,
        original_process: Option<Handle>,
        deadline: Instant,
        cancel: &AtomicBool,
        poll_only: bool,
    ) -> io::Result<Option<ReadinessObservation>> {
        if self.revoked || self.pipe.is_none() {
            return Err(invalid("original pipe authority revoked or closed"));
        }
        validate_challenge(challenge)?;
        // Validate ceiling before admitting native work, including a pending resume.
        deadline_failure(deadline, cancel)?;
        if !literal(&expectation.version_literal)
            || !literal(&peer.cooperative_instance)
            || peer.instance_binding_digest == [0; 32]
            || peer.original_pid == 0
            || !admitted_peer_sid(&self.helper_sid, &peer.sid)
        {
            return Err(invalid(
                "missing original Start/principal/cooperative readiness binding",
            ));
        }
        if let Some(active) = &self.active {
            let original_matches = match (&active.original_process, &original_process) {
                (None, None) => true,
                (Some(held), Some(now)) => (unsafe { CompareObjectHandles(held.0, now.0) }) != 0,
                _ => false,
            };
            if !original_matches {
                return Err(invalid(
                    "pending original process object cannot be replaced",
                ));
            }
            if &active.challenge != challenge
                || &active.expectation != expectation
                || &active.peer != peer
            {
                return Err(invalid("pending original probe context cannot be replaced"));
            }
            if active.terminal {
                return Ok(Some(active.last.clone().expect("terminal observation")));
            }
        } else {
            let body = encode_challenge(challenge)?;
            let mut request = (body.len() as u32).to_be_bytes().to_vec();
            request.extend(body);
            self.active = Some(ProbeState {
                challenge: challenge.clone(),
                expectation: expectation.clone(),
                peer: peer.clone(),
                request,
                written: 0,
                prefix: Vec::new(),
                body: Vec::new(),
                connected: false,
                authenticated: false,
                measured_sid: None,
                process: None,
                original_process,
                token: None,
                slot: None,
                last: None,
                terminal: false,
            });
        }
        loop {
            if let Some(failure) = deadline_failure(deadline, cancel)? {
                if failure == ReadinessFailure::Cancelled {
                    self.cancel_pending();
                }
                return Ok(Some(self.observe(
                    ReadinessState::Incomplete,
                    Some(failure),
                    None,
                    false,
                    poll_only,
                )));
            }
            match self.advance() {
                Ok(Some(result)) => {
                    if let Some(failure) = deadline_failure(deadline, cancel)? {
                        return Ok(Some(self.observe(
                            ReadinessState::Incomplete,
                            Some(failure),
                            result.response,
                            false,
                            true,
                        )));
                    }
                    return Ok(Some(result));
                }
                Ok(None) => {}
                Err(error) => {
                    self.retain_error(error);
                    return Ok(Some(self.observe(
                        ReadinessState::Incomplete,
                        Some(ReadinessFailure::PeerClosed),
                        None,
                        false,
                        true,
                    )));
                }
            }
            if poll_only {
                return Ok(None);
            }
            std::thread::sleep(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(25)),
            );
        }
    }
    fn retain_error(&mut self, error: io::Error) {
        if self.retained_errors.len() < 16 {
            self.retained_errors.push(error);
        }
    }
    fn observe(
        &mut self,
        state: ReadinessState,
        failure: Option<ReadinessFailure>,
        response: Option<ReadinessResponse>,
        authenticated_instance: bool,
        terminal: bool,
    ) -> ReadinessObservation {
        let a = self.active.as_mut().expect("original probe");
        let result = ReadinessObservation {
            state,
            peer: None,
            peer_sid: a.measured_sid.clone(),
            response,
            failure,
            authenticated_instance,
            instance_binding_digest: a.peer.instance_binding_digest,
        };
        a.last = Some(result.clone());
        a.terminal = terminal;
        result
    }
    fn advance(&mut self) -> io::Result<Option<ReadinessObservation>> {
        let pipe = self.pipe.as_ref().expect("original pipe").0;
        let a = self.active.as_mut().expect("original probe");
        if a.slot.is_none() {
            let (kind, buffer) = if !a.connected {
                (Kind::Connect, Vec::new())
            } else if !a.authenticated {
                let mut pid = 0;
                if unsafe { GetNamedPipeClientProcessId(pipe, &mut pid) } == 0 {
                    return Err(last_native_error("GetNamedPipeClientProcessId"));
                }
                if pid != a.peer.original_pid {
                    self.revoked = true;
                    return Ok(Some(self.observe(
                        ReadinessState::Failed,
                        Some(ReadinessFailure::WrongPeer),
                        None,
                        false,
                        true,
                    )));
                }
                // QUERY_LIMITED_INFORMATION only: never TERMINATE, VM_WRITE,
                // CREATE_THREAD, DUP_HANDLE, token impersonation or adjustment.
                let process = if let Some(original) = &a.original_process {
                    original.0
                } else {
                    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
                    if raw.is_null() {
                        return Err(last_native_error("OpenProcess"));
                    }
                    a.process = Some(Handle(raw));
                    raw
                };
                if unsafe { GetProcessId(process) } != pid {
                    return Err(invalid("original process no longer matches pipe peer"));
                }
                let (sid, token) = token_sid(process)?;
                a.measured_sid = Some(sid.clone());
                a.token = Some(token);
                if sid != a.peer.sid {
                    self.revoked = true;
                    return Ok(Some(self.observe(
                        ReadinessState::Failed,
                        Some(ReadinessFailure::WrongPeer),
                        None,
                        false,
                        true,
                    )));
                }
                // Verify the kernel connection is unchanged across token metadata.
                let mut after = 0;
                if unsafe { GetNamedPipeClientProcessId(pipe, &mut after) } == 0 {
                    return Err(last_native_error("GetNamedPipeClientProcessId"));
                }
                if after != pid {
                    return Err(invalid("pipe peer changed during metadata authentication"));
                }
                a.authenticated = true;
                (Kind::Write, a.request[a.written..].to_vec())
            } else if a.written < a.request.len() {
                (Kind::Write, a.request[a.written..].to_vec())
            } else if a.prefix.len() < 4 {
                (Kind::Prefix, vec![0; 4 - a.prefix.len()])
            } else {
                let length =
                    u32::from_be_bytes(a.prefix.as_slice().try_into().expect("four byte prefix"))
                        as usize;
                if length > MAX_FRAME_BYTES {
                    return Ok(Some(self.observe(
                        ReadinessState::Failed,
                        Some(ReadinessFailure::FrameTooLarge),
                        None,
                        false,
                        true,
                    )));
                }
                if a.body.len() == length {
                    return self.finish_response();
                }
                (Kind::Body, vec![0; length - a.body.len()])
            };
            let mut slot = IoSlot::new(kind, buffer)?;
            slot.issue(pipe)?;
            a.slot = Some(slot);
        }
        let slot = a.slot.as_mut().expect("owned slot");
        let Some(count) = slot.result(pipe)? else {
            return Ok(None);
        };
        let slot = a.slot.take().expect("terminal slot");
        if count > slot.buffer.len() {
            return Err(invalid("native IPC length exceeded owned buffer"));
        }
        if !matches!(slot.kind, Kind::Connect) && count == 0 {
            return Err(invalid("pipe peer closed before response"));
        }
        match slot.kind {
            Kind::Connect => a.connected = true,
            Kind::Write => a.written += count,
            Kind::Prefix => a.prefix.extend_from_slice(&slot.buffer[..count]),
            Kind::Body => a.body.extend_from_slice(&slot.buffer[..count]),
        }
        Ok(None)
    }
    fn finish_response(&mut self) -> io::Result<Option<ReadinessObservation>> {
        let a = self.active.as_ref().expect("original probe");
        let response = match decode_response(&a.body) {
            Ok(r) => r,
            Err(_) => {
                let failure = if decode_fields(&a.body, 15)
                    .is_ok_and(|f| f[0] != "theseus_fixture_ready_v1")
                {
                    ReadinessFailure::WrongProtocol
                } else {
                    ReadinessFailure::MalformedResponse
                };
                return Ok(Some(self.observe(
                    ReadinessState::Failed,
                    Some(failure),
                    None,
                    false,
                    true,
                )));
            }
        };
        let failure = compare(
            &a.challenge,
            &a.expectation,
            &a.peer.cooperative_instance,
            &response,
        );
        let authenticated = correlation_authenticated(failure);
        Ok(Some(self.observe(
            if failure.is_none() {
                ReadinessState::Ready
            } else {
                ReadinessState::Failed
            },
            failure,
            Some(response),
            authenticated,
            true,
        )))
    }
    fn cancel_pending(&mut self) {
        let result = match (
            &self.pipe,
            self.active.as_ref().and_then(|a| a.slot.as_ref()),
        ) {
            (Some(pipe), Some(slot)) => slot.cancel(pipe.0),
            _ => Ok(()),
        };
        if let Err(error) = result {
            self.retain_error(error);
        }
    }
    fn settle_cancelled(&mut self) -> bool {
        self.cancel_pending();
        if let (Some(pipe), Some(slot)) = (
            &self.pipe,
            self.active.as_mut().and_then(|a| a.slot.as_mut()),
        ) {
            // FALSE is mandatory. CancelIoEx success alone never settles ownership.
            match slot.result(pipe.0) {
                Ok(None) => return false,
                Ok(Some(_)) => {}
                Err(error) => self.retain_error(error),
            }
        }
        true
    }
    pub fn take_terminal_probe(&mut self) -> io::Result<ReadinessObservation> {
        if !self
            .active
            .as_ref()
            .is_some_and(|a| a.terminal && a.slot.as_ref().is_none_or(|s| !s.pending))
        {
            return Err(invalid("original pipe probe remains pending"));
        }
        self.disconnect_original()?;
        Ok(self
            .active
            .take()
            .expect("terminal probe")
            .last
            .expect("terminal observation"))
    }
    /// Requests cancellation; an unresolved native operation remains owned.
    pub fn abandon_probe(&mut self) -> Option<ReadinessObservation> {
        if !self.settle_cancelled() {
            return None;
        }
        if self.disconnect_original().is_err() {
            self.revoked = true;
            return None;
        }
        self.active.take().and_then(|a| a.last)
    }
    fn disconnect_original(&mut self) -> io::Result<()> {
        if let Some(pipe) = &self.pipe
            && unsafe { DisconnectNamedPipe(pipe.0) } == 0
        {
            let error = unsafe { GetLastError() };
            if error != ERROR_PIPE_NOT_CONNECTED {
                self.revoked = true;
                let error = native_error(
                    "DisconnectNamedPipe",
                    io::Error::from_raw_os_error(error as i32),
                );
                let reason = error.to_string();
                self.retain_error(error);
                return Err(io::Error::other(reason));
            }
        }
        // Re-arm only this still-open first-instance pipe, never reopen a name.
        Ok(())
    }
    pub fn cleanup(&mut self) -> io::Result<()> {
        if !self.settle_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "original pipe cancellation remains pending",
            ));
        }
        self.active.take();
        self.pipe.take();
        Ok(())
    }
}
// Authentication cannot widen the protected helper+SYSTEM endpoint DACL.
// Service manifests retain their separate LocalSystem-only account policy.
fn admitted_peer_sid(helper: &Sid, peer: &Sid) -> bool {
    peer == helper || peer.as_str() == "S-1-5-18"
}

#[cfg(test)]
#[test]
fn explicit_peer_account_stays_within_original_helper_and_system_dacl() {
    let helper = Sid::new("S-1-5-21-1001".into()).unwrap();
    assert!(admitted_peer_sid(&helper, &helper));
    assert!(admitted_peer_sid(
        &helper,
        &Sid::new("S-1-5-18".into()).unwrap()
    ));
    assert!(!admitted_peer_sid(
        &helper,
        &Sid::new("S-1-5-21-1002".into()).unwrap()
    ));
}
impl Drop for LocalChallenge {
    fn drop(&mut self) {
        if self.cleanup().is_err() {
            // Never free OVERLAPPED/event/buffers underneath the kernel. This
            // bounded original owner is quarantined if cancellation is unresolved.
            // No infinite wait and no assertion of service/application exit.
            if let Some(active) = self.active.take() {
                mem::forget(active);
            }
            if let Some(pipe) = self.pipe.take() {
                mem::forget(pipe);
            }
            mem::forget(mem::take(&mut self.retained_errors));
        }
    }
}

#[cfg(test)]
mod native_pipe_tests {
    use super::*;
    // These tests touch only private process-named test pipes, never SCM/services.
    // They require native Windows execution; cross-compilation is supplemental.
    #[test]
    fn original_process_probe_keeps_same_object_proof_across_cancelled_resume() {
        let original = unsafe { BorrowedHandle::borrow_raw(GetCurrentProcess()) };
        let (pid, sid) = LocalChallenge::original_process_metadata(original).unwrap();
        let endpoint = NamedPipe::new(format!(
            r"\\.\pipe\theseus.original-shell-object-{}",
            std::process::id()
        ))
        .unwrap();
        let mut channel = LocalChallenge::bind(endpoint, sid.clone()).unwrap();
        let challenge = super::super::tests::challenge();
        let expectation = super::super::tests::expectation();
        let peer = PipePeerExpectation {
            sid,
            original_pid: pid,
            cooperative_instance: "original".into(),
            instance_binding_digest: [8; 32],
        };
        let cancelled = AtomicBool::new(true);
        let deadline = Instant::now() + Duration::from_secs(2);
        let observed = channel
            .probe_original_process(
                &challenge,
                &expectation,
                &peer,
                original,
                deadline,
                &cancelled,
            )
            .unwrap();
        assert_eq!(observed.state, ReadinessState::Incomplete);
        let retained = channel
            .active
            .as_ref()
            .unwrap()
            .original_process
            .as_ref()
            .unwrap()
            .0;
        assert_ne!(retained, unsafe { GetCurrentProcess() });
        assert_ne!(
            unsafe { CompareObjectHandles(retained, GetCurrentProcess()) },
            0
        );
        assert!(
            channel
                .probe_original_process(
                    &challenge,
                    &expectation,
                    &peer,
                    original,
                    deadline,
                    &cancelled
                )
                .is_ok()
        );
        assert_eq!(
            channel
                .active
                .as_ref()
                .unwrap()
                .original_process
                .as_ref()
                .unwrap()
                .0,
            retained
        );
        // The new scheduling-yield API resumes the same original object proof;
        // accidentally routing it through generic poll would fail this assertion.
        let polled = channel
            .poll_original_process(
                &challenge,
                &expectation,
                &peer,
                original,
                deadline,
                &cancelled,
            )
            .unwrap()
            .unwrap();
        assert_eq!(polled.state, ReadinessState::Incomplete);
        assert_eq!(
            channel
                .active
                .as_ref()
                .unwrap()
                .original_process
                .as_ref()
                .unwrap()
                .0,
            retained
        );
        assert!(
            channel
                .poll(&challenge, &expectation, &peer, deadline, &cancelled)
                .is_err(),
            "generic nonblocking metadata may not replace original-object authority"
        );
        // The legacy PID metadata path cannot replace a pending original-object proof.
        assert!(
            channel
                .probe(&challenge, &expectation, &peer, deadline, &cancelled)
                .is_err()
        );
        let mut wrong = peer.clone();
        wrong.original_pid = wrong.original_pid.wrapping_add(1);
        assert!(
            channel
                .probe_original_process(
                    &challenge,
                    &expectation,
                    &wrong,
                    original,
                    deadline,
                    &cancelled
                )
                .is_err()
        );
        channel.cleanup().unwrap();
    }
    #[test]
    fn original_first_instance_pipe_is_retained_until_explicit_cleanup() {
        let (sid, _token) = token_sid(unsafe { GetCurrentProcess() }).unwrap();
        let endpoint = NamedPipe::new(format!(
            r"\\.\pipe\theseus.host-ready-owned-{}",
            std::process::id()
        ))
        .unwrap();
        let mut original = LocalChallenge::bind(endpoint.clone(), sid.clone()).unwrap();
        assert!(LocalChallenge::bind(endpoint.clone(), sid.clone()).is_err());
        let original_handle = original.pipe.as_ref().unwrap().0;
        original.renew_after_sut_transition().unwrap();
        assert_eq!(original.pipe.as_ref().unwrap().0, original_handle);
        assert!(
            LocalChallenge::bind(endpoint.clone(), sid.clone()).is_err(),
            "renewal retains first-instance exclusion"
        );
        original.cleanup().unwrap();
        let mut fresh = LocalChallenge::bind(endpoint, sid).unwrap();
        fresh.cleanup().unwrap();
    }
    #[test]
    fn wrong_declared_helper_sid_refuses_before_creating_pipe() {
        let (actual, _token) = token_sid(unsafe { GetCurrentProcess() }).unwrap();
        let other = Sid::new(
            if actual.as_str() == "S-1-5-18" {
                "S-1-5-19"
            } else {
                "S-1-5-18"
            }
            .into(),
        )
        .unwrap();
        let endpoint = NamedPipe::new(format!(
            r"\\.\pipe\theseus.host-ready-sid-{}",
            std::process::id()
        ))
        .unwrap();
        assert!(LocalChallenge::bind(endpoint.clone(), other).is_err());
        let mut owned = LocalChallenge::bind(endpoint, actual).unwrap();
        owned.cleanup().unwrap();
    }
}

#[cfg(test)]
mod error_retention_tests {
    use super::*;
    fn original() -> LocalChallenge {
        LocalChallenge {
            endpoint: NamedPipe::new(r"\\.\pipe\theseus.error-model".into()).unwrap(),
            helper_sid: Sid::new("S-1-5-18".into()).unwrap(),
            pipe: None,
            active: None,
            revoked: false,
            retained_errors: Vec::new(),
            _worker: Rc::new(()),
        }
    }
    fn assert_error(channel: &LocalChallenge, index: usize, api: &str, code: i32) {
        let record = channel.retained_errors[index]
            .get_ref()
            .unwrap()
            .downcast_ref::<NativeError>()
            .unwrap();
        assert_eq!(record.api, api);
        assert_eq!(record.error.raw_os_error(), Some(code));
    }
    #[test]
    fn windows_native_error_identity_survives_abandon_and_cleanup_with_bounded_storage() {
        let mut channel = original();
        let mut read = IoSlot::new(Kind::Body, vec![0; 4]).unwrap();
        channel.retain_error(read.issue(INVALID_HANDLE_VALUE).unwrap_err());
        let auth_error = match token_sid(INVALID_HANDLE_VALUE) {
            Err(error) => error,
            Ok(_) => panic!("invalid process handle authenticated"),
        };
        channel.retain_error(auth_error);
        channel.active = Some(probe_state(None));
        channel.observe(
            ReadinessState::Incomplete,
            Some(ReadinessFailure::PeerClosed),
            None,
            false,
            true,
        );
        channel.take_terminal_probe().unwrap();
        assert!(channel.abandon_probe().is_none());
        channel.cleanup().unwrap();
        assert_error(&channel, 0, "ReadFile", 6);
        assert_error(&channel, 1, "OpenProcessToken", 6);
        for _ in 0..32 {
            channel.retain_error(native_error("WriteFile", io::Error::from_raw_os_error(109)));
        }
        assert_eq!(channel.retained_errors.len(), 16);
        assert_error(&channel, 0, "ReadFile", 6);
    }
    fn probe_state(slot: Option<IoSlot>) -> ProbeState {
        ProbeState {
            challenge: ReadinessChallenge {
                protocol: ReadyProtocol::TheseusFixtureReadyV1,
                lease: "lease".into(),
                slot: "service".into(),
                resource_binding_digest: [1; 32],
                start_or_activation_operation: "start".into(),
                action_digest: [2; 32],
                nonce: [3; 16],
                expectation_digest: [4; 32],
            },
            expectation: ReadinessExpectation {
                artifact: ArtifactRole::Target,
                artifact_length: 1,
                artifact_sha256: [5; 32],
                version_literal: "v1".into(),
                data_sha256: [6; 32],
                configuration_digest: [7; 32],
            },
            peer: PipePeerExpectation {
                sid: Sid::new("S-1-5-18".into()).unwrap(),
                original_pid: 1,
                cooperative_instance: "original".into(),
                instance_binding_digest: [8; 32],
            },
            request: Vec::new(),
            written: 0,
            prefix: Vec::new(),
            body: Vec::new(),
            connected: false,
            authenticated: false,
            measured_sid: None,
            process: None,
            original_process: None,
            token: None,
            slot,
            last: None,
            terminal: false,
        }
    }
    #[test]
    fn terminal_native_cancellation_errors_remain_on_original_channel() {
        let mut channel = original();
        // Invalid native handles deterministically fail cancellation/completion;
        // no native operation owns the test slot's storage.
        channel.pipe = Some(Handle(INVALID_HANDLE_VALUE));
        let mut slot = IoSlot::new(Kind::Body, vec![0; 4]).unwrap();
        slot.pending = true;
        channel.active = Some(probe_state(Some(slot)));
        assert!(channel.settle_cancelled());
        assert!(
            !channel
                .active
                .as_ref()
                .unwrap()
                .slot
                .as_ref()
                .unwrap()
                .pending
        );
        assert!(channel.retained_errors.iter().any(|e| {
            e.get_ref()
                .unwrap()
                .downcast_ref::<NativeError>()
                .unwrap()
                .api
                == "CancelIoEx"
        }));
        assert!(channel.retained_errors.iter().any(|e| {
            e.get_ref()
                .unwrap()
                .downcast_ref::<NativeError>()
                .unwrap()
                .api
                == "GetOverlappedResult"
        }));
        channel.cleanup().unwrap();
        assert!(!channel.retained_errors.is_empty());
    }
    #[test]
    fn cancelled_connect_keeps_original_storage_until_terminal_completion() {
        let (sid, _token) = token_sid(unsafe { GetCurrentProcess() }).unwrap();
        let endpoint = NamedPipe::new(format!(
            r"\\.\pipe\theseus.cancel-storage-{}",
            std::process::id()
        ))
        .unwrap();
        let mut channel = LocalChallenge::bind(endpoint, sid).unwrap();
        let mut slot = IoSlot::new(Kind::Connect, Vec::new()).unwrap();
        slot.issue(channel.pipe.as_ref().unwrap().0).unwrap();
        assert!(slot.pending);
        let original = &*slot.overlapped as *const OVERLAPPED;
        channel.active = Some(probe_state(Some(slot)));
        channel.cancel_pending();
        let retained = channel.active.as_ref().unwrap().slot.as_ref().unwrap();
        assert!(retained.pending); // CancelIoEx is never native completion.
        assert_eq!(&*retained.overlapped as *const OVERLAPPED, original);
        assert!(channel.take_terminal_probe().is_err());
        let deadline = Instant::now() + Duration::from_secs(2);
        while !channel.settle_cancelled() {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(channel.retained_errors.iter().any(|error| {
            error
                .get_ref()
                .and_then(|e| e.downcast_ref::<NativeError>())
                .is_some_and(|e| {
                    e.api == "GetOverlappedResult" && e.error.raw_os_error() == Some(995)
                })
        }));
        channel.cleanup().unwrap();
    }
}
