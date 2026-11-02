use super::{Evidence, Observation, Progress, failure};
use serde_json::json;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_ACCESS_INFORMATION, FILE_MODE_INFORMATION, FILE_PIPE_LOCAL_INFORMATION,
    FileAccessInformation, FileIsRemoteDeviceInformation, FileModeInformation,
    FilePipeLocalInformation, NtQueryInformationFile,
};
use windows_sys::Wdk::System::SystemServices::FILE_IS_REMOTE_DEVICE_INFORMATION;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::{Console::*, IO::*, Pipes::*, Threading::*};

pub(super) struct Endpoint {
    handle: OwnedHandle,
    label: &'static str,
    saved: Option<u32>,
}
fn duplicate(handle: HANDLE, access: u32, options: u32) -> io::Result<OwnedHandle> {
    let mut result = null_mut();
    if unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            handle,
            GetCurrentProcess(),
            &mut result,
            access,
            0,
            options,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(result) })
}
fn query<T: Default>(
    handle: HANDLE,
    class: i32,
    label: &str,
    evidence: &mut Evidence,
) -> io::Result<T> {
    // Stable allocations remain owned if the kernel unexpectedly queues a query.
    let mut output = Box::<T>::default();
    let mut iosb = Box::<IO_STATUS_BLOCK>::default();
    let status = unsafe {
        NtQueryInformationFile(
            handle,
            &mut *iosb,
            (&mut *output as *mut T).cast(),
            std::mem::size_of::<T>() as u32,
            class,
        )
    };
    if status == STATUS_PENDING {
        let _ = evidence.event(
            "query_pending",
            label,
            "NtQueryInformationFile",
            false,
            (Some(status as i64), None),
            json!({"class":class,"storage_retained":true}),
        );
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_secs(5) {
            super::tick();
        }
        // Do not read, move, free or reborrow pending query storage.
        std::process::exit(2);
    }
    let returned = iosb.Information;
    let completion = unsafe { iosb.Anonymous.Status };
    let ok = status >= 0 && completion >= 0 && returned == std::mem::size_of::<T>();
    evidence.event("query", label, "NtQueryInformationFile", ok, (Some(status as i64), None), json!({"class":class,"completion_status":completion,"returned_bytes":returned,"expected_bytes":std::mem::size_of::<T>()}))?;
    if !ok {
        return Err(failure("native query failed or returned wrong size"));
    }
    Ok(*output)
}
fn qualify(handle: HANDLE, label: &str, evidence: &mut Evidence) -> io::Result<()> {
    let kind = unsafe { GetFileType(handle) };
    let error = (kind == FILE_TYPE_UNKNOWN).then(|| unsafe { GetLastError() } as i32);
    evidence.event(
        "query",
        label,
        "GetFileType",
        kind == FILE_TYPE_PIPE,
        (Some(kind as i64), error),
        json!({"type":kind}),
    )?;
    if kind != FILE_TYPE_PIPE {
        return Err(failure("not a pipe"));
    }
    let access: FILE_ACCESS_INFORMATION = query(handle, FileAccessInformation, label, evidence)?;
    let mode: FILE_MODE_INFORMATION = query(handle, FileModeInformation, label, evidence)?;
    let local: FILE_PIPE_LOCAL_INFORMATION =
        query(handle, FilePipeLocalInformation, label, evidence)?;
    let remote: FILE_IS_REMOTE_DEVICE_INFORMATION =
        query(handle, FileIsRemoteDeviceInformation, label, evidence)?;
    let required = if label == "stdin" {
        FILE_READ_DATA
    } else {
        FILE_WRITE_DATA
    };
    // FILE_SYNCHRONOUS_IO_ALERT/NONALERT are native create options 0x10/0x20.
    let ok = access.AccessFlags & required != 0
        && mode.Mode & 0x30 != 0
        && local.NamedPipeType == 0
        && !remote.IsRemote;
    evidence.event("qualified", label, "native pipe profile", ok, (None, None), json!({"access":access.AccessFlags,"mode":mode.Mode,"pipe_type":local.NamedPipeType,"remote":remote.IsRemote,"pipe_end":local.NamedPipeEnd}))?;
    if !ok {
        return Err(failure(
            "requires local synchronous byte pipe with required data access",
        ));
    }
    Ok(())
}
impl Endpoint {
    pub(super) fn expect_read_access(&mut self) {
        self.label = "stdin";
    }
    pub(super) fn mode_changed(&self) -> bool {
        self.saved.is_some()
    }

    pub(super) fn standard(index: i32, evidence: &mut Evidence) -> io::Result<Self> {
        let label = if index == 0 { "stdin" } else { "stdout" };
        let raw = unsafe {
            GetStdHandle(if index == 0 {
                STD_INPUT_HANDLE
            } else {
                STD_OUTPUT_HANDLE
            })
        };
        let copied = duplicate(raw, 0, DUPLICATE_SAME_ACCESS);
        evidence.event(
            "duplicate",
            label,
            "DuplicateHandle",
            copied.is_ok(),
            (
                None,
                copied.as_ref().err().and_then(io::Error::raw_os_error),
            ),
            json!({"same_access":true,"inheritable":false}),
        )?;
        Ok(Self {
            handle: copied?,
            label,
            saved: None,
        })
    }
    pub(super) fn qualify(&self, evidence: &mut Evidence) -> io::Result<()> {
        qualify(self.handle.as_raw_handle(), self.label, evidence)
    }
    fn mode(&self, evidence: &mut Evidence) -> io::Result<u32> {
        let mut mode = 0;
        let status = unsafe {
            GetNamedPipeHandleStateW(
                self.handle.as_raw_handle(),
                &mut mode,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
                0,
            )
        };
        let error = (status == 0).then(io::Error::last_os_error);
        evidence.event(
            "mode",
            self.label,
            "GetNamedPipeHandleStateW",
            status != 0,
            (
                Some(status as i64),
                error.as_ref().and_then(io::Error::raw_os_error),
            ),
            json!({"mode":if status != 0 {Some(mode)} else {None}}),
        )?;
        if let Some(error) = error {
            return Err(error);
        }
        Ok(mode)
    }
    pub(super) fn nowait(&mut self, evidence: &mut Evidence) -> io::Result<()> {
        let mode = self.mode(evidence)?;
        let desired = PIPE_NOWAIT;
        let status = unsafe {
            SetNamedPipeHandleState(self.handle.as_raw_handle(), &desired, null(), null())
        };
        if status != 0 {
            self.saved = Some(mode & (PIPE_READMODE_MESSAGE | PIPE_NOWAIT));
        }
        let error = (status == 0).then(io::Error::last_os_error);
        evidence.event(
            "set_nowait",
            self.label,
            "SetNamedPipeHandleState",
            status != 0,
            (
                Some(status as i64),
                error.as_ref().and_then(io::Error::raw_os_error),
            ),
            json!({"before":mode,"requested":desired}),
        )?;
        if let Some(error) = error {
            return Err(error);
        }
        self.verify_sharing(evidence)
    }
    pub(super) fn verify_sharing(&self, evidence: &mut Evidence) -> io::Result<()> {
        let original = self.mode(evidence)?;
        let copied = self.copy()?;
        let mode = copied.mode(evidence)?;
        evidence.event(
            "mode_sharing",
            self.label,
            "GetNamedPipeHandleStateW duplicate",
            mode == original,
            (None, None),
            json!({"mode":mode}),
        )?;
        if mode != original {
            return Err(failure("mode not shared by duplicate"));
        }
        Ok(())
    }
    pub(super) fn restore(&mut self, evidence: &mut Evidence, fault: bool) -> io::Result<()> {
        let mut injected = None;
        if fault {
            let mode = PIPE_WAIT;
            let status =
                unsafe { SetNamedPipeHandleState(INVALID_HANDLE_VALUE, &mode, null(), null()) };
            let error = io::Error::last_os_error();
            evidence.event(
                "restore_fault",
                self.label,
                "SetNamedPipeHandleState invalid handle",
                false,
                (Some(status as i64), error.raw_os_error()),
                json!({"injected":true}),
            )?;
            injected = Some(error);
        }
        if let Some(mode) = self.saved.take() {
            let status = unsafe {
                SetNamedPipeHandleState(self.handle.as_raw_handle(), &mode, null(), null())
            };
            let error = (status == 0).then(io::Error::last_os_error);
            evidence.event(
                "restore",
                self.label,
                "SetNamedPipeHandleState",
                status != 0,
                (
                    Some(status as i64),
                    error.as_ref().and_then(io::Error::raw_os_error),
                ),
                json!({"mode":mode}),
            )?;
            if let Some(error) = error {
                return Err(error);
            }
            if self.mode(evidence)? & (PIPE_NOWAIT | PIPE_READMODE_MESSAGE) != mode {
                return Err(failure("restoration verification differs"));
            }
        } else {
            evidence.mark(
                "restore",
                json!({"endpoint":self.label,"mode_mutation":false,"owned_blocking_profile":true}),
            )?;
        }
        injected.map_or(Ok(()), Err)
    }
    pub(super) fn copy(&self) -> io::Result<Self> {
        Ok(Self {
            handle: duplicate(self.handle.as_raw_handle(), 0, DUPLICATE_SAME_ACCESS)?,
            label: self.label,
            saved: None,
        })
    }
}
fn observation(
    status: i32,
    count: u32,
    api: &'static str,
    reading: bool,
    error: Option<i32>,
) -> Observation {
    let progress = if status != 0 && count > 0 {
        Ok(Progress::Bytes(count as usize))
    } else if status != 0 {
        Ok(Progress::Pending)
    }
    // TRUE/0 is never EOF proof.
    else if reading && error == Some(ERROR_BROKEN_PIPE as i32) {
        Ok(Progress::Eof)
    } else if error == Some(ERROR_NO_DATA as i32) || error == Some(ERROR_PIPE_BUSY as i32) {
        Ok(Progress::Pending)
    } else {
        Err(io::Error::from_raw_os_error(
            error.unwrap_or(ERROR_GEN_FAILURE as i32),
        ))
    };
    Observation {
        progress,
        api,
        status: status as i64,
        error,
    }
}
pub(super) fn read(endpoint: &Endpoint, bytes: &mut [u8]) -> Observation {
    let mut count = 0;
    let status = unsafe {
        ReadFile(
            endpoint.handle.as_raw_handle(),
            bytes.as_mut_ptr(),
            bytes.len() as u32,
            &mut count,
            null_mut(),
        )
    };
    let error = (status == 0).then(|| unsafe { GetLastError() } as i32);
    observation(status, count, "ReadFile(synchronous)", true, error)
}
pub(super) fn write(endpoint: &Endpoint, bytes: &[u8]) -> Observation {
    let mut count = 0;
    let status = unsafe {
        WriteFile(
            endpoint.handle.as_raw_handle(),
            bytes.as_ptr(),
            bytes.len() as u32,
            &mut count,
            null_mut(),
        )
    };
    let error = (status == 0).then(|| unsafe { GetLastError() } as i32);
    observation(status, count, "WriteFile(synchronous)", false, error)
}
pub(super) fn cancel(
    thread: &std::thread::JoinHandle<()>,
    evidence: &mut Evidence,
    record: bool,
) -> io::Result<()> {
    let status = unsafe { CancelSynchronousIo(thread.as_raw_handle()) };
    let error = (status == 0).then(|| unsafe { GetLastError() } as i32);
    if record {
        evidence.event(
            "cancel_request",
            "worker",
            "CancelSynchronousIo",
            status != 0,
            (Some(status as i64), error),
            json!({"real_thread_handle":true,"completion_proven":false}),
        )?;
    }
    Ok(())
}
fn named(message: bool, overlapped: bool) -> io::Result<(Endpoint, OwnedHandle)> {
    let name: Vec<u16> = format!(
        r"\\.\pipe\theseus-stdio-{}-{}",
        std::process::id(),
        if message {
            1
        } else if overlapped {
            2
        } else {
            0
        }
    )
    .encode_utf16()
    .chain(Some(0))
    .collect();
    let server = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX
                | FILE_FLAG_FIRST_PIPE_INSTANCE
                | if overlapped { FILE_FLAG_OVERLAPPED } else { 0 },
            if message {
                PIPE_TYPE_MESSAGE
            } else {
                PIPE_TYPE_BYTE
            } | PIPE_NOWAIT
                | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            4096,
            4096,
            0,
            null(),
        )
    };
    if server == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let handle = unsafe { OwnedHandle::from_raw_handle(server) };
    let client = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            null(),
            OPEN_EXISTING,
            0,
            null_mut(),
        )
    };
    if client == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok((
        Endpoint {
            handle,
            label: "stdin",
            saved: None,
        },
        unsafe { OwnedHandle::from_raw_handle(client) },
    ))
}
pub(super) fn control(case: &str, candidate: &str, evidence: &mut Evidence) -> io::Result<bool> {
    if case == "stderr-alias" {
        if unsafe { SetStdHandle(STD_ERROR_HANDLE, GetStdHandle(STD_OUTPUT_HANDLE)) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let out = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        let err = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
        // Optional stderr is never selected, even when duplicated handles have
        // different numeric values; no identity inference from unequal numbers.
        evidence.mark("stderr_policy",json!({"numeric_equal":out == err,"stderr_selected":false,"identity_if_different":"not_claimed"}))?;
    }
    if matches!(case, "named-byte" | "message-pipe" | "overlapped-pipe") {
        let (mut endpoint, peer) = named(case == "message-pipe", case == "overlapped-pipe")?;
        let result = endpoint.qualify(evidence);
        if case != "named-byte" {
            if result.is_ok() {
                return Err(failure("unsupported native pipe accepted"));
            }
            evidence.mark("rejected", json!({"actual_endpoint":true}))?;
            return Ok(true);
        }
        result?;
        if candidate == "direct_nowait" {
            endpoint.nowait(evidence)?;
        }
        let peer = Endpoint {
            handle: peer,
            label: "stdout",
            saved: None,
        };
        let written = write(&peer, b"positive");
        written.record(evidence, "control_peer")?;
        if !matches!(written.progress?, Progress::Bytes(8)) {
            return Err(failure("positive write failed"));
        }
        let mut buffer = [0; 8];
        let observed = read(&endpoint, &mut buffer);
        observed.record(evidence, "control_pipe")?;
        if !matches!(observed.progress?, Progress::Bytes(8)) || buffer != *b"positive" {
            return Err(failure("positive read failed"));
        }
        endpoint.restore(evidence, false)?;
        evidence.mark("restored", json!({"positive_control":true}))?;
        return Ok(true);
    }
    let endpoint = match case {
        "file" => Some(Endpoint {
            handle: tempfile::tempfile()?.into(),
            label: "stdin",
            saved: None,
        }),
        "terminal" => {
            let name: Vec<u16> = "CONIN$".encode_utf16().chain(Some(0)).collect();
            let raw = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    null(),
                    OPEN_EXISTING,
                    0,
                    null_mut(),
                )
            };
            if raw == INVALID_HANDLE_VALUE {
                // CI may have no console. Allocate a private console solely for
                // the rejection control; the ordinary std handles are untouched.
                if unsafe { AllocConsole() } == 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            let raw = if raw == INVALID_HANDLE_VALUE {
                unsafe {
                    CreateFileW(
                        name.as_ptr(),
                        GENERIC_READ,
                        FILE_SHARE_READ | FILE_SHARE_WRITE,
                        null(),
                        OPEN_EXISTING,
                        0,
                        null_mut(),
                    )
                }
            } else {
                raw
            };
            if raw == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            Some(Endpoint {
                handle: unsafe { OwnedHandle::from_raw_handle(raw) },
                label: "stdin",
                saved: None,
            })
        }
        "reduced-access" => {
            let raw = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
            Some(Endpoint {
                handle: duplicate(raw, FILE_READ_ATTRIBUTES | SYNCHRONIZE, 0)?,
                label: "stdin",
                saved: None,
            })
        }
        "socket" => {
            use std::os::windows::io::AsRawSocket;
            let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
            let client = std::net::TcpStream::connect(listener.local_addr()?)?;
            let (_server, _) = listener.accept()?;
            if qualify(client.as_raw_socket() as HANDLE, "stdin", evidence).is_ok() {
                return Err(failure("socket accepted"));
            }
            evidence.mark("rejected", json!({"actual_socket":true}))?;
            return Ok(true);
        }
        _ => None,
    };
    if let Some(endpoint) = endpoint {
        if endpoint.qualify(evidence).is_ok() {
            return Err(failure("unsupported endpoint accepted"));
        }
        evidence.mark("rejected", json!({"actual_endpoint":true}))?;
        return Ok(true);
    }
    Ok(false)
}
