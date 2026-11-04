//! Compiled cooperative fixture application; reports its actual image and data.
use anyhow::{Result, ensure};
#[cfg(unix)]
use std::path::Path;
pub fn run(version: &str) -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    #[cfg(windows)]
    if args.is_empty() {
        return windows::dispatch(version);
    }
    match args.first().map(String::as_str) {
        #[cfg(target_os = "linux")]
        Some("service") => {
            ensure!(
                args.len() == 4,
                "expected service endpoint, data file and explicit fixture fault"
            );
            service(version, Path::new(&args[1]), Path::new(&args[2]), &args[3])
        }
        #[cfg(target_os = "macos")]
        Some("service") => {
            ensure!(
                args.len() == 5,
                "expected endpoint/data/fault/immutable instance"
            );
            service(
                version,
                Path::new(&args[1]),
                Path::new(&args[2]),
                &args[3],
                &args[4],
            )
        }
        Some("check_base") if version == "base" => {
            println!("base");
            Ok(())
        }
        Some("check_target" | "exercise") if version == "target" => {
            println!("target");
            Ok(())
        }
        Some("check_data") => {
            ensure!(
                args.len() == 2 && std::fs::read(&args[1])? == b"theseus fixture data\n",
                "configured data differs"
            );
            println!("data");
            Ok(())
        }
        _ => Err(anyhow::anyhow!("fixture version or action differs")),
    }
}
#[cfg(target_os = "linux")]
fn service(version: &str, endpoint: &Path, data: &Path, fault: &str) -> Result<()> {
    ensure!(
        [
            "none",
            "missing_service",
            "leaked_service",
            "wrong_version",
            "wrong_data",
            "wrong_nonce",
            "wrong_peer"
        ]
        .contains(&fault),
        "unknown explicit fixture fault"
    );
    use sha2::{Digest as _, Sha256};
    use std::{
        io::{Read, Write},
        os::unix::net::UnixStream,
        time::{Duration, Instant},
    };
    use theseus_host::services::readiness::{
        MAX_FRAME_BYTES, ReadinessResponse, decode_challenge, encode_response,
    };
    ensure!(
        endpoint.is_absolute() && data.is_absolute(),
        "explicit fixture paths required"
    );
    let instance = std::env::var("INVOCATION_ID")?;
    ensure!(
        instance.len() == 32 && instance.bytes().all(|b| b.is_ascii_hexdigit()),
        "original systemd invocation required"
    );
    let executable = std::fs::read(std::env::current_exe()?)?;
    let artifact_length = executable.len() as u64;
    let artifact_sha256 = Sha256::digest(executable).into();
    let startup = Instant::now();
    loop {
        let mut stream = match UnixStream::connect(endpoint) {
            Ok(stream) => stream,
            Err(error) if startup.elapsed() < Duration::from_secs(30) => {
                let _ = error;
                std::thread::sleep(Duration::from_millis(25));
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        stream.set_write_timeout(Some(Duration::from_secs(30)))?;
        let mut prefix = [0; 4];
        if stream.read_exact(&mut prefix).is_err() {
            std::thread::sleep(Duration::from_millis(25));
            continue;
        }
        let length = u32::from_be_bytes(prefix) as usize;
        ensure!(
            (1..=MAX_FRAME_BYTES).contains(&length),
            "fixture challenge exceeds bound"
        );
        let mut body = vec![0; length];
        stream.read_exact(&mut body)?;
        let challenge = decode_challenge(&body)?;
        let bytes = std::fs::read(data)?;
        let data_sha256 = Sha256::digest(&bytes).into();
        let mut response = ReadinessResponse {
            protocol: challenge.protocol,
            lease: challenge.lease,
            slot: challenge.slot,
            resource_binding_digest: challenge.resource_binding_digest,
            start_or_activation_operation: challenge.start_or_activation_operation,
            action_digest: challenge.action_digest,
            nonce: challenge.nonce,
            artifact_length,
            artifact_sha256,
            version_literal: format!("{version}\n"),
            data_sha256,
            configuration_digest: data_sha256,
            instance: instance.clone(),
            initialized: bytes == b"theseus fixture data\n",
        };
        match fault {
            "wrong_version" => response.version_literal = "wrong-version\n".into(),
            "wrong_nonce" => response.nonce[0] ^= 1,
            "wrong_data" => response.data_sha256[0] ^= 1,
            _ => {}
        }
        let response = encode_response(&response)?;
        stream.write_all(&(response.len() as u32).to_be_bytes())?;
        stream.write_all(&response)?;
        // Remain manager-owned and running. Each fresh challenge requires a
        // fresh authenticated connection; EOF never starts another process.
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(target_os = "macos")]
fn service(version: &str, endpoint: &Path, data: &Path, fault: &str, instance: &str) -> Result<()> {
    use std::{
        io::{self, Read, Write},
        os::unix::net::UnixStream,
        time::{Duration, Instant},
    };
    fn read(stream: &mut UnixStream, mut bytes: &mut [u8], deadline: Instant) -> io::Result<()> {
        while !bytes.is_empty() {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "fixture frame deadline",
                ));
            }
            match stream.read(bytes) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "fixture peer disconnected",
                    ));
                }
                Ok(count) => {
                    bytes = &mut bytes[count..];
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2))
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
    fn write(stream: &mut UnixStream, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
        while !bytes.is_empty() {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "fixture frame deadline",
                ));
            }
            match stream.write(bytes) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "fixture peer disconnected",
                    ));
                }
                Ok(count) => {
                    bytes = &bytes[count..];
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2))
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
    use theseus_host::services::readiness::{MAX_FRAME_BYTES, decode_challenge};
    ensure!(
        endpoint.is_absolute() && data.is_absolute(),
        "explicit fixture paths required"
    );
    validate_instance(instance)?;
    validate_fault(fault)?;
    let startup = Instant::now();
    let artifact = std::fs::read(std::env::current_exe()?)?;
    loop {
        let mut stream = match UnixStream::connect(endpoint) {
            Ok(stream) => stream,
            Err(_) if startup.elapsed() < Duration::from_secs(30) => {
                std::thread::sleep(Duration::from_millis(25));
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        stream.set_nonblocking(true)?;
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut prefix = [0; 4];
        // EOF is a completed probe connection. Reconnect to the same original
        // helper listener; no challenge can select a different endpoint.
        if read(&mut stream, &mut prefix, deadline).is_err() {
            std::thread::sleep(Duration::from_millis(25));
            continue;
        }
        let length = u32::from_be_bytes(prefix) as usize;
        ensure!(
            (1..=MAX_FRAME_BYTES).contains(&length),
            "fixture challenge exceeds bound"
        );
        let mut body = vec![0; length];
        read(&mut stream, &mut body, deadline)?;
        let challenge = decode_challenge(&body)?;
        let response = cooperative_response(
            version,
            &artifact,
            &std::fs::read(data)?,
            instance,
            fault,
            challenge,
        )?;
        write(
            &mut stream,
            &(response.len() as u32).to_be_bytes(),
            deadline,
        )?;
        write(&mut stream, &response, deadline)?;
        std::thread::sleep(Duration::from_millis(25));
    }
}
#[cfg(any(target_os = "macos", windows))]
fn validate_instance(instance: &str) -> Result<()> {
    ensure!(
        !instance.is_empty() && instance.len() <= 128 && !instance.chars().any(char::is_control),
        "explicit immutable installation instance required"
    );
    Ok(())
}
#[cfg(any(target_os = "macos", windows))]
fn validate_fault(fault: &str) -> Result<()> {
    ensure!(
        [
            "none",
            "missing_service",
            "leaked_service",
            "wrong_version",
            "wrong_data",
            "wrong_nonce",
            "wrong_peer"
        ]
        .contains(&fault),
        "unknown explicit fixture fault"
    );
    Ok(())
}
#[cfg(any(target_os = "macos", windows))]
fn cooperative_response(
    version: &str,
    artifact: &[u8],
    data: &[u8],
    instance: &str,
    fault: &str,
    challenge: theseus_host::services::readiness::ReadinessChallenge,
) -> Result<Vec<u8>> {
    use sha2::{Digest as _, Sha256};
    use theseus_host::services::readiness::{ReadinessResponse, encode_response};
    let data_sha256 = Sha256::digest(data).into();
    let mut response = ReadinessResponse {
        protocol: challenge.protocol,
        lease: challenge.lease,
        slot: challenge.slot,
        resource_binding_digest: challenge.resource_binding_digest,
        start_or_activation_operation: challenge.start_or_activation_operation,
        action_digest: challenge.action_digest,
        nonce: challenge.nonce,
        artifact_length: artifact.len() as u64,
        artifact_sha256: Sha256::digest(artifact).into(),
        version_literal: format!("{version}\n"),
        data_sha256,
        configuration_digest: data_sha256,
        instance: instance.into(),
        initialized: data == b"theseus fixture data\n",
    };
    match fault {
        "wrong_version" => response.version_literal = "wrong-version\n".into(),
        "wrong_nonce" => response.nonce[0] ^= 1,
        "wrong_data" => response.data_sha256[0] ^= 1,
        _ => {}
    }
    Ok(encode_response(&response)?)
}

#[cfg(windows)]
#[path = "native_service_definition.rs"]
mod definition;
#[cfg(windows)]
mod windows {
    use super::{definition::ScmReaderConfig, *};
    use std::{
        ffi::c_void,
        io, ptr,
        sync::{
            OnceLock,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::{Duration, Instant},
    };
    type Handle = *mut c_void;
    #[repr(C)]
    struct ServiceTable {
        name: *mut u16,
        entry: Option<unsafe extern "system" fn(u32, *mut *mut u16)>,
    }
    #[repr(C)]
    struct Status {
        kind: u32,
        state: u32,
        accepted: u32,
        exit: u32,
        specific: u32,
        checkpoint: u32,
        wait_ms: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct Overlapped {
        internal: usize,
        internal_high: usize,
        offset: u32,
        offset_high: u32,
        event: Handle,
    }
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn StartServiceCtrlDispatcherW(table: *const ServiceTable) -> i32;
        fn RegisterServiceCtrlHandlerExW(
            name: *const u16,
            handler: Option<unsafe extern "system" fn(u32, u32, *mut c_void, *mut c_void) -> u32>,
            context: *mut c_void,
        ) -> Handle;
        fn SetServiceStatus(handle: Handle, status: *const Status) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            security: *const c_void,
            disposition: u32,
            flags: u32,
            template: Handle,
        ) -> Handle;
        fn WaitNamedPipeW(name: *const u16, timeout_ms: u32) -> i32;
        fn CreateEventW(
            security: *const c_void,
            manual: i32,
            initial: i32,
            name: *const u16,
        ) -> Handle;
        fn ReadFile(
            file: Handle,
            buffer: *mut u8,
            count: u32,
            done: *mut u32,
            overlapped: *mut Overlapped,
        ) -> i32;
        fn WriteFile(
            file: Handle,
            buffer: *const u8,
            count: u32,
            done: *mut u32,
            overlapped: *mut Overlapped,
        ) -> i32;
        fn GetOverlappedResult(
            file: Handle,
            overlapped: *mut Overlapped,
            done: *mut u32,
            wait: i32,
        ) -> i32;
        fn CancelIoEx(file: Handle, overlapped: *mut Overlapped) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
    }
    static INPUT: OnceLock<(ScmReaderConfig, String)> = OnceLock::new();
    static STOP: AtomicBool = AtomicBool::new(false);
    static FAILED: AtomicBool = AtomicBool::new(false);
    static STATUS_HANDLE: AtomicUsize = AtomicUsize::new(0);
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }
    fn invalid(s: &str) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, s)
    }
    pub(super) fn dispatch(version: &str) -> Result<()> {
        let path = std::env::current_exe()?.with_extension("service.json");
        // No alternate path, environment search, SCM enumeration or fallback.
        // OPEN_REPARSE_POINT plus original Host file primitives reject aliases.
        let parent = theseus_host::fs::HostDir::open(
            path.parent().ok_or_else(|| invalid("sidecar parent"))?,
            theseus_host::fs::RootPolicy::AllComponentsNoFollow,
        )?;
        let mut file = match parent
            .open_leaf_classified(path.file_name().ok_or_else(|| invalid("sidecar name"))?)
        {
            theseus_host::fs::LeafOpenResult::Opened(file) => file,
            other => anyhow::bail!("original private SCM sidecar unavailable: {other:?}"),
        };
        let before = theseus_host::fs::fingerprint(&file)?;
        ensure!(
            before.len > 0 && before.len <= 4096 && file.metadata()?.permissions().readonly(),
            "immutable bounded SCM sidecar required"
        );
        use std::io::Read;
        let mut bytes = Vec::new();
        (&mut file).take(4097).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= 4096 && theseus_host::fs::fingerprint(&file)? == before,
            "SCM sidecar changed"
        );
        let config: ScmReaderConfig = serde_json::from_slice(&bytes)?;
        config.validate()?;
        super::definition::verify_current_sid(&config.service_sid)?;
        super::validate_instance(&config.cooperative_instance)?;
        super::validate_fault(&config.fault)?;
        let mut name = wide(&config.service_name);
        INPUT
            .set((config, version.to_owned()))
            .map_err(|_| invalid("SCM dispatcher already initialized"))?;
        let table = [
            ServiceTable {
                name: name.as_mut_ptr(),
                entry: Some(service_main),
            },
            ServiceTable {
                name: ptr::null_mut(),
                entry: None,
            },
        ];
        // This process is the real SCM service; running the binary directly does
        // not produce readiness or substitute a console implementation.
        if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        ensure!(
            !FAILED.load(Ordering::Acquire),
            "SCM fixture ServiceMain failed"
        );
        drop(file); // original immutable input stays retained through service exit
        Ok(())
    }
    fn report(state: u32, exit: u32) -> io::Result<()> {
        let status = Status {
            kind: 0x10,
            state,
            accepted: if state == 4 { 1 } else { 0 },
            exit,
            specific: 0,
            checkpoint: u32::from(state == 2 || state == 3),
            wait_ms: if state == 2 || state == 3 { 30000 } else { 0 },
        };
        if unsafe { SetServiceStatus(STATUS_HANDLE.load(Ordering::Acquire) as Handle, &status) }
            == 0
        {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    unsafe extern "system" fn control(code: u32, _: u32, _: *mut c_void, _: *mut c_void) -> u32 {
        match code {
            1 => {
                STOP.store(true, Ordering::Release);
                0
            }
            4 => 0, // interrogate: SCM retains the last reported status
            _ => 120,
        }
    }
    unsafe extern "system" fn service_main(_: u32, _: *mut *mut u16) {
        let Some((config, version)) = INPUT.get() else {
            FAILED.store(true, Ordering::Release);
            return;
        };
        let name = wide(&config.service_name);
        let handle =
            unsafe { RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(control), ptr::null_mut()) };
        if handle.is_null() {
            FAILED.store(true, Ordering::Release);
            return;
        }
        STATUS_HANDLE.store(handle as usize, Ordering::Release);
        let result = (|| -> Result<()> {
            report(2, 0)?;
            let artifact = std::fs::read(std::env::current_exe()?)?;
            report(4, 0)?;
            serve(config, version, &artifact)
        })();
        if result.is_err() {
            FAILED.store(true, Ordering::Release);
        }
        if report(1, if result.is_ok() { 0 } else { 1064 }).is_err() {
            FAILED.store(true, Ordering::Release);
        }
    }
    struct NativeHandle(Handle);
    impl Drop for NativeHandle {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }
    struct IoOperation {
        overlapped: Overlapped,
        bytes: Vec<u8>,
        _event: NativeHandle,
    }
    struct Pipe {
        handle: Option<NativeHandle>,
        pending: Option<Box<IoOperation>>,
    }
    impl Drop for Pipe {
        fn drop(&mut self) {
            if let (Some(handle), Some(mut op)) = (&self.handle, self.pending.take()) {
                unsafe { CancelIoEx(handle.0, &mut op.overlapped) };
                let mut count = 0;
                if unsafe { GetOverlappedResult(handle.0, &mut op.overlapped, &mut count, 0) } == 0
                    && io::Error::last_os_error().raw_os_error() == Some(996)
                {
                    // Kernel still owns the OVERLAPPED/buffer: preserve both
                    // handles/storage. No blocking cancellation or premature free.
                    std::mem::forget(op);
                    std::mem::forget(self.handle.take());
                }
            }
        }
    }
    fn budget(deadline: Instant) -> io::Result<()> {
        if STOP.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "SCM stop requested",
            ));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "SCM readiness deadline",
            ));
        }
        Ok(())
    }
    impl Pipe {
        fn connect(endpoint: &str, deadline: Instant) -> io::Result<Self> {
            let name = wide(endpoint);
            loop {
                budget(deadline)?;
                // Fixed local name, overlapped I/O, no impersonation capability.
                let handle = unsafe {
                    CreateFileW(
                        name.as_ptr(),
                        0xc0000000,
                        0,
                        ptr::null(),
                        3,
                        0x40000000 | 0x00100000,
                        ptr::null_mut(),
                    )
                };
                if handle as isize != -1 {
                    return Ok(Self {
                        handle: Some(NativeHandle(handle)),
                        pending: None,
                    });
                }
                let error = io::Error::last_os_error();
                if !matches!(error.raw_os_error(), Some(2 | 231)) {
                    return Err(error);
                }
                unsafe { WaitNamedPipeW(name.as_ptr(), 25) };
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        fn transfer(
            &mut self,
            input: &[u8],
            read_length: Option<usize>,
            deadline: Instant,
        ) -> io::Result<Vec<u8>> {
            if self.pending.is_some() {
                return Err(invalid("original pipe operation retained"));
            }
            let length = read_length.unwrap_or(input.len());
            if length == 0 || length > 4100 {
                return Err(invalid("pipe frame bound"));
            }
            let mut output = Vec::with_capacity(length);
            while output.len() < length {
                budget(deadline)?;
                let event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
                if event.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let bytes = if read_length.is_some() {
                    vec![0; length - output.len()]
                } else {
                    input[output.len()..].to_vec()
                };
                self.pending = Some(Box::new(IoOperation {
                    overlapped: Overlapped {
                        event,
                        ..Default::default()
                    },
                    bytes,
                    _event: NativeHandle(event),
                }));
                let handle = self
                    .handle
                    .as_ref()
                    .ok_or_else(|| invalid("original pipe closed"))?
                    .0;
                let op = self.pending.as_mut().unwrap();
                let mut count = 0;
                let completed = if read_length.is_some() {
                    unsafe {
                        ReadFile(
                            handle,
                            op.bytes.as_mut_ptr(),
                            op.bytes.len() as u32,
                            &mut count,
                            &mut op.overlapped,
                        )
                    }
                } else {
                    unsafe {
                        WriteFile(
                            handle,
                            op.bytes.as_ptr(),
                            op.bytes.len() as u32,
                            &mut count,
                            &mut op.overlapped,
                        )
                    }
                };
                if completed == 0 {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(997) {
                        self.pending.take();
                        return Err(error);
                    }
                    loop {
                        budget(deadline)?;
                        let op = self.pending.as_mut().unwrap();
                        if unsafe { GetOverlappedResult(handle, &mut op.overlapped, &mut count, 0) }
                            != 0
                        {
                            break;
                        }
                        let error = io::Error::last_os_error();
                        if error.raw_os_error() != Some(996) {
                            self.pending.take();
                            return Err(error);
                        }
                        std::thread::sleep(Duration::from_millis(2));
                    }
                }
                let op = self.pending.take().unwrap();
                if count == 0 || count as usize > op.bytes.len() {
                    return Err(invalid("pipe disconnected or invalid transfer count"));
                }
                output.extend_from_slice(&op.bytes[..count as usize]);
            }
            Ok(output)
        }
    }
    fn serve(config: &ScmReaderConfig, version: &str, artifact: &[u8]) -> Result<()> {
        use theseus_host::services::readiness::{MAX_FRAME_BYTES, decode_challenge};
        loop {
            if STOP.load(Ordering::Acquire) {
                report(3, 0)?;
                return Ok(());
            }
            let result = (|| -> Result<()> {
                let deadline = Instant::now() + Duration::from_secs(30);
                let mut pipe = Pipe::connect(&config.endpoint, deadline)?;
                let prefix = pipe.transfer(&[], Some(4), deadline)?;
                let length =
                    u32::from_be_bytes(prefix.try_into().map_err(|_| invalid("challenge prefix"))?)
                        as usize;
                ensure!(
                    (1..=MAX_FRAME_BYTES).contains(&length),
                    "fixture challenge exceeds bound"
                );
                let body = pipe.transfer(&[], Some(length), deadline)?;
                let response = super::cooperative_response(
                    version,
                    artifact,
                    &std::fs::read(&config.data)?,
                    &config.cooperative_instance,
                    &config.fault,
                    decode_challenge(&body)?,
                )?;
                let mut frame = (response.len() as u32).to_be_bytes().to_vec();
                frame.extend_from_slice(&response);
                pipe.transfer(&frame, None, deadline)?;
                Ok(())
            })();
            if STOP.load(Ordering::Acquire) {
                report(3, 0)?;
                return Ok(());
            }
            // Every probe has its own bounded connection on the same endpoint.
            // Broken/disconnected probes reconnect; malformed codecs fail service.
            if let Err(error) = result {
                let retry = error
                    .downcast_ref::<io::Error>()
                    .is_some_and(|e| matches!(e.raw_os_error(), Some(109 | 232 | 233)));
                if !retry {
                    return Err(error);
                }
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}
