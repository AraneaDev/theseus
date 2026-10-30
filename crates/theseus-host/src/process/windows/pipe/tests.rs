use super::*;
use std::io::{Read, Write};
use std::mem::{offset_of, size_of};
use std::time::{Duration, Instant};
use windows_sys::Win32::Security::{
    Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
    *,
};

fn raw_zero(peer: &File) {
    let mut written = u32::MAX;
    assert_ne!(
        unsafe {
            WriteFile(
                peer.as_raw_handle(),
                b"".as_ptr(),
                0,
                &mut written,
                null_mut(),
            )
        },
        0
    );
    assert_eq!(written, 0);
}
fn collect(reader: &mut PipeReader) -> Vec<u8> {
    let until = Instant::now() + Duration::from_secs(5);
    let mut bytes = Vec::new();
    loop {
        assert!(Instant::now() < until);
        let mut out = [0; 2];
        match reader.try_read(&mut out) {
            Ok(0) => return bytes,
            Ok(n) => bytes.extend_from_slice(&out[..n]),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::yield_now(),
            Err(e) => panic!("{e}"),
        }
    }
}
#[test]
fn raw_zero_byte_write_then_payload_then_peer_close_never_premature_eof() {
    for pending_first in [false, true] {
        let (mut reader, mut peer) = reader().unwrap();
        if pending_first {
            assert_eq!(
                reader.try_read(&mut [0; 8]).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
            assert!(
                reader
                    .endpoint
                    .operation
                    .as_ref()
                    .unwrap()
                    .get()
                    .pending
                    .get()
            );
        }
        raw_zero(&peer);
        let until = Instant::now() + Duration::from_millis(20);
        while Instant::now() < until {
            assert_eq!(
                reader.try_read(&mut [0; 8]).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
        }
        peer.write_all(b"later payload").unwrap();
        drop(peer);
        assert_eq!(collect(&mut reader), b"later payload");
    }
}
#[test]
fn immediate_and_pending_zero_completion_mapping_is_no_progress() {
    let (mut reader, _peer) = reader().unwrap();
    // Both initiation and pending completion funnel through the same mapper.
    assert_eq!(
        reader.finish_read(Ok(0)).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert!(!reader.eof);
    assert_eq!(
        reader.finish_read(Ok(0)).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert!(!reader.eof);
}
fn fill(writer: &mut PipeWriter, bytes: &[u8]) -> usize {
    let until = Instant::now() + Duration::from_secs(5);
    let mut accepted = 0;
    loop {
        assert!(Instant::now() < until);
        match writer.try_write(bytes) {
            Ok(n) => {
                accepted += n;
                assert!(accepted < 4 * 1024 * 1024);
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                let op = writer.endpoint.operation.as_ref().unwrap().get();
                assert!(op.pending.get());
                if unsafe { WaitForSingleObject(op.event.as_raw_handle(), 100) } == WAIT_TIMEOUT {
                    return accepted;
                }
            }
            Err(e) => panic!("{e}"),
        }
    }
}
#[test]
fn pending_write_owns_copy_and_validates_retry_before_acknowledgment() {
    let (mut writer, mut peer) = writer().unwrap();
    let mut bytes = vec![b'x'; BUFFER];
    let mut accepted = fill(&mut writer, &bytes);
    assert!(
        writer
            .endpoint
            .operation
            .as_ref()
            .unwrap()
            .get()
            .pending
            .get()
    );
    assert_eq!(
        writer.try_write(b"different").unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    // Mutating the caller's old allocation cannot change a native in-flight write
    // or the separate retry snapshot. Never inspect the native pending buffer.
    bytes.fill(b'y');
    assert_eq!(
        writer.try_write(&bytes).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    bytes.fill(b'x');
    let until = Instant::now() + Duration::from_secs(5);
    std::thread::scope(|scope| {
        let read = scope.spawn(move || {
            let mut received = Vec::new();
            peer.read_to_end(&mut received).unwrap();
            received
        });
        loop {
            match writer.try_write(&bytes) {
                Ok(n) => {
                    accepted += n;
                    break;
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < until);
                    std::thread::yield_now();
                }
                Err(e) => panic!("{e}"),
            }
        }
        writer.close().unwrap();
        assert_eq!(read.join().unwrap(), vec![b'x'; accepted]);
    });
}
#[test]
fn failed_pending_write_never_acknowledges_delivery() {
    let (mut writer, peer) = writer().unwrap();
    let bytes = vec![0; BUFFER];
    fill(&mut writer, &bytes);
    drop(peer);
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        match writer.try_write(&bytes) {
            Ok(n) => panic!("failed pending write acknowledged {n}"),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < until);
                std::thread::yield_now();
            }
            Err(_) => break,
        }
    }
    writer.close().unwrap();
}
#[test]
fn pending_connect_failure_guard_owns_setup_even_without_child() {
    let (owner, _name) = server(true).unwrap();
    let endpoint = Endpoint {
        operation: Some(owner),
        stopped: false,
    };
    let operation = endpoint.active().unwrap();
    operation.reset().unwrap();
    operation.pending.set(true);
    let success =
        unsafe { ConnectNamedPipe(operation.handle.as_raw_handle(), operation.overlapped.get()) };
    let error = io::Error::last_os_error();
    if success != 0 || !code(&error, ERROR_IO_PENDING) {
        operation.pending.set(false);
    }
    assert_eq!(success, 0);
    assert_eq!(error.raw_os_error(), Some(ERROR_IO_PENDING as i32));
    let mut failure = setup_failure(endpoint, true, io::Error::other("original setup failure"));
    assert_eq!(failure.launch, crate::process::LaunchPhase::NeverCreated);
    assert_eq!(failure.error.to_string(), "original setup failure");
    let HostCleanup::Pending(guard) = &mut failure.cleanup else {
        panic!("lost pending connect owner")
    };
    assert!(guard.child.is_none());
    assert!(
        guard.readers[0]
            .endpoint
            .operation
            .as_ref()
            .unwrap()
            .get()
            .pending
            .get()
    );
    let start = Instant::now();
    assert!(guard.try_complete().unwrap());
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(guard.readers.is_empty());
}
#[test]
fn completed_read_cancel_not_found_still_observes_completion() {
    let (mut reader, mut peer) = reader().unwrap();
    assert_eq!(
        reader.try_read(&mut [0; 1]).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    peer.write_all(b"done").unwrap();
    let operation = reader.endpoint.operation.as_ref().unwrap().get();
    assert_eq!(
        unsafe { WaitForSingleObject(operation.event.as_raw_handle(), 5000) },
        WAIT_OBJECT_0
    );
    assert_eq!(
        unsafe { CancelIoEx(operation.handle.as_raw_handle(), operation.overlapped.get()) },
        0
    );
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(ERROR_NOT_FOUND as i32)
    );
    reader.cancel_and_complete().unwrap();
    assert!(reader.endpoint.operation.is_none());
    assert_eq!(
        reader.try_read(&mut [0; 1]).unwrap_err().kind(),
        io::ErrorKind::Interrupted
    );
}
#[test]
fn pipe_objects_have_private_protected_acl_and_noninheritable_controllers() {
    for reading in [true, false] {
        let (owner, name) = server(reading).unwrap();
        let op = owner.get();
        assert_private(&op.handle).unwrap();
        let mut flags = 0;
        assert_ne!(
            unsafe { GetHandleInformation(op.handle.as_raw_handle(), &mut flags) },
            0
        );
        assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
        assert_ne!(
            unsafe { GetHandleInformation(op.event.as_raw_handle(), &mut flags) },
            0
        );
        assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
        let collision = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                BUFFER as u32,
                BUFFER as u32,
                1000,
                null(),
            )
        };
        assert_eq!(
            collision, INVALID_HANDLE_VALUE,
            "first-instance creation cannot join an existing named pipe"
        );
    }
}
fn checked(value: i32) -> io::Result<()> {
    if value == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
fn token_user(token: &OwnedHandle) -> io::Result<Vec<usize>> {
    unsafe {
        let mut bytes = 0;
        GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut bytes);
        let mut storage = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
        checked(GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            storage.as_mut_ptr().cast(),
            bytes,
            &mut bytes,
        ))?;
        Ok(storage)
    }
}
fn effective_token() -> io::Result<OwnedHandle> {
    unsafe {
        let mut raw = null_mut();
        if OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut raw) == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_NO_TOKEN as i32) {
                return Err(error);
            }
            checked(OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw))?;
        }
        Ok(OwnedHandle::from_raw_handle(raw))
    }
}
fn assert_private(file: &OwnedHandle) -> io::Result<()> {
    // Query the kernel's installed descriptor, not the constructor's input.
    unsafe {
        let mut descriptor = null_mut();
        let mut acl = null_mut();
        let status = GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut acl,
            null_mut(),
            &mut descriptor,
        );
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        struct FreeDescriptor(PSECURITY_DESCRIPTOR);
        impl Drop for FreeDescriptor {
            fn drop(&mut self) {
                unsafe {
                    LocalFree(self.0);
                }
            }
        }
        let _descriptor = FreeDescriptor(descriptor);
        let mut control = 0;
        let mut revision = 0;
        checked(GetSecurityDescriptorControl(
            descriptor,
            &mut control,
            &mut revision,
        ))?;
        assert_ne!(control & SE_DACL_PROTECTED, 0);
        assert!(!acl.is_null());
        assert_eq!((*acl).AceCount, 2);
        let user = token_user(&effective_token()?)?;
        let user_sid = (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid;
        let mut system = [0usize; 9];
        let mut system_bytes = size_of_val(&system) as u32;
        checked(CreateWellKnownSid(
            WinLocalSystemSid,
            null_mut(),
            system.as_mut_ptr().cast(),
            &mut system_bytes,
        ))?;
        let mut saw_user = false;
        let mut saw_system = false;
        for index in 0..2 {
            let mut raw_ace = null_mut();
            checked(GetAce(acl, index, &mut raw_ace))?;
            let ace = &*raw_ace.cast::<ACCESS_ALLOWED_ACE>();
            assert_eq!(ace.Header.AceType, 0); // ACCESS_ALLOWED_ACE_TYPE
            assert_eq!(ace.Header.AceFlags, 0); // neither inherited nor inherit-only
            assert_eq!(ace.Mask, FILE_ALL_ACCESS);
            let sid = (raw_ace.cast::<u8>())
                .add(offset_of!(ACCESS_ALLOWED_ACE, SidStart))
                .cast();
            if EqualSid(sid, user_sid) != 0 {
                saw_user = true;
            } else if EqualSid(sid, system.as_mut_ptr().cast()) != 0 {
                saw_system = true;
            } else {
                panic!("unexpected broad-grant SID in private DACL");
            }
        }
        assert!(saw_user && saw_system);
    }
    Ok(())
}

#[test]
fn pending_read_and_write_move_to_another_thread_without_moving_native_storage() {
    let (mut output, mut peer) = reader().unwrap();
    assert_eq!(
        output.try_read(&mut [0; 1]).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    std::thread::scope(|scope| {
        let read = scope.spawn(move || collect(&mut output));
        peer.write_all(b"moved pending reader").unwrap();
        drop(peer);
        assert_eq!(read.join().unwrap(), b"moved pending reader");
    });

    let (mut input, mut peer) = writer().unwrap();
    let bytes = vec![b'm'; BUFFER];
    let accepted = fill(&mut input, &bytes);
    std::thread::scope(|scope| {
        let write = scope.spawn(move || {
            let until = Instant::now() + Duration::from_secs(5);
            let written = loop {
                match input.try_write(&bytes) {
                    Ok(n) => break n,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < until);
                        std::thread::yield_now();
                    }
                    Err(e) => panic!("{e}"),
                }
            };
            input.close().unwrap();
            written
        });
        let mut received = Vec::new();
        peer.read_to_end(&mut received).unwrap();
        assert_eq!(received, vec![b'm'; accepted + write.join().unwrap()]);
    });
}
