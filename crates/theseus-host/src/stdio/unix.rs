#[cfg(any(target_os = "macos", test))]
#[path = "macos_pipe.rs"]
mod macos_pipe;
#[cfg(feature = "test-support")]
use super::test_support::{Control, Event, Trace};
use super::{
    MAX_OPERATION, ReadStep, WriteStep, invalid, stopped, unix_flags::restored_pipe_flags_match,
};
#[cfg(feature = "test-support")]
use serde_json::json;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

struct Endpoint {
    fd: OwnedFd,
    saved: Option<i32>,
}
pub(super) struct Native {
    input: Option<Endpoint>,
    output: Option<Endpoint>,
    stopping: bool,
    #[cfg(feature = "test-support")]
    pub(super) trace: Trace,
    #[cfg(feature = "test-support")]
    pub(super) control: Control,
    #[cfg(feature = "test-support")]
    barrier: bool,
}
impl Native {
    pub(super) fn new() -> Self {
        Self {
            input: None,
            output: None,
            stopping: false,
            #[cfg(feature = "test-support")]
            trace: Trace::default(),
            #[cfg(feature = "test-support")]
            control: Control::Normal,
            #[cfg(feature = "test-support")]
            barrier: false,
        }
    }
    pub(super) fn acquire(&mut self) -> io::Result<()> {
        self.input = Some(self.duplicate(0)?);
        self.output = Some(self.duplicate(1)?);
        let input_identity = self.qualify(true)?;
        let output_identity = self.qualify(false)?;
        #[cfg(not(target_os = "macos"))]
        let aliased = input_identity == output_identity;
        #[cfg(target_os = "macos")]
        let aliased = {
            let input = macos_pipe::identity(
                self.endpoint(true).fd.as_raw_fd(),
                input_identity,
                #[cfg(feature = "test-support")]
                &mut self.trace,
                #[cfg(feature = "test-support")]
                "stdin",
            )?;
            let output = macos_pipe::identity(
                self.endpoint(false).fd.as_raw_fd(),
                output_identity,
                #[cfg(feature = "test-support")]
                &mut self.trace,
                #[cfg(feature = "test-support")]
                "stdout",
            )?;
            let disjoint = macos_pipe::disjoint(input, output);
            #[cfg(feature = "test-support")]
            self.trace.push(Event {
                phase: "pipe_identity",
                endpoint: "control",
                api: "proc_pidfdinfo",
                ok: disjoint,
                status: None,
                error: None,
                observed: json!({"input":input,"output":output,"disjoint":disjoint}),
            });
            !disjoint
        };
        if aliased {
            #[cfg(feature = "test-support")]
            self.trace.push(Event {
                phase: "alias_rejected",
                endpoint: "control",
                api: if cfg!(target_os = "macos") {
                    "proc_pidfdinfo pipe identity"
                } else {
                    "fstat device/inode identity"
                },
                ok: true,
                status: None,
                error: None,
                observed: json!({"same_underlying_pipe":true}),
            });
            return Err(invalid("stdin and stdout alias one pipe"));
        }
        self.nowait(true)?;
        #[cfg(feature = "test-support")]
        if self.control == Control::PartialAcquire {
            self.trace.push(Event { phase: "rollback_injection", endpoint: "control", api: "synthetic acquisition failure", ok: true, status: None, error: None, observed: json!({"obligation":"mutated_input_mode","acquired_endpoints":2,"mutated_endpoints":1,"synthetic":true}) });
            return Err(io::Error::other("injected partial acquisition"));
        }
        self.nowait(false)
    }
    fn duplicate(&mut self, fd: i32) -> io::Result<Endpoint> {
        let raw = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
        let error = (raw < 0).then(io::Error::last_os_error);
        // Establish ownership before any observer allocation can unwind.
        let owned = (raw >= 0).then(|| unsafe { OwnedFd::from_raw_fd(raw) });
        #[cfg(feature = "test-support")]
        self.trace.push(Event {
            phase: "duplicate",
            endpoint: if fd == 0 { "stdin" } else { "stdout" },
            api: "fcntl(F_DUPFD_CLOEXEC)",
            ok: raw >= 0,
            status: Some(raw as i64),
            error: error.as_ref().and_then(io::Error::raw_os_error),
            observed: json!({"same_access":true}),
        });
        match owned {
            Some(fd) => Ok(Endpoint { fd, saved: None }),
            None => Err(error.unwrap()),
        }
    }
    fn endpoint(&self, reading: bool) -> &Endpoint {
        if reading {
            self.input.as_ref()
        } else {
            self.output.as_ref()
        }
        .unwrap()
    }
    fn qualify(&mut self, reading: bool) -> io::Result<(libc::dev_t, libc::ino_t)> {
        let fd = self.endpoint(reading).fd.as_raw_fd();
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let stat = unsafe { stat.assume_init() };
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        let pipe = stat.st_mode & libc::S_IFMT == libc::S_IFIFO;
        let access = flags & libc::O_ACCMODE;
        let ok = pipe
            && if reading {
                access != libc::O_WRONLY
            } else {
                access != libc::O_RDONLY
            };
        #[cfg(feature = "test-support")]
        self.trace.push(Event { phase: "qualified", endpoint: label(reading), api: "fstat/F_GETFL", ok, status: Some(flags as i64), error: None, observed: json!({"pipe":pipe,"access":access,"file_type":stat.st_mode & libc::S_IFMT,"local":true}) });
        if !ok {
            return Err(invalid(
                "stdio requires pipe endpoints with matching access",
            ));
        }
        Ok((stat.st_dev, stat.st_ino))
    }
    fn nowait(&mut self, reading: bool) -> io::Result<()> {
        let endpoint = if reading {
            self.input.as_mut()
        } else {
            self.output.as_mut()
        }
        .unwrap();
        let fd = endpoint.fd.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        let status = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
        let error = (status < 0).then(io::Error::last_os_error);
        if status == 0 {
            endpoint.saved = Some(flags);
        }
        #[cfg(feature = "test-support")]
        self.trace.push(Event {
            phase: "set_nowait",
            endpoint: label(reading),
            api: "fcntl(F_SETFL)",
            ok: status == 0,
            status: Some(status as i64),
            error: error.as_ref().and_then(io::Error::raw_os_error),
            observed: json!({"before":flags,"requested": flags | libc::O_NONBLOCK}),
        });
        if let Some(error) = error {
            return Err(error);
        }
        let actual = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if actual != flags | libc::O_NONBLOCK {
            return Err(io::Error::other("nonblocking mode verification failed"));
        }
        #[cfg(feature = "test-support")]
        self.trace.push(Event {
            phase: "mode_sharing",
            endpoint: label(reading),
            api: "fcntl(F_GETFL inherited alias)",
            ok: unsafe { libc::fcntl(if reading { 0 } else { 1 }, libc::F_GETFL) } == actual,
            status: Some(actual as i64),
            error: None,
            observed: json!({"nowait":true,"dedicated_same_end_required":true}),
        });
        Ok(())
    }
    pub(super) fn try_read(&mut self, bytes: &mut [u8]) -> io::Result<ReadStep> {
        if self.stopping {
            return Err(stopped());
        }
        if bytes.is_empty() {
            return Ok(ReadStep::Pending);
        }
        #[cfg(feature = "test-support")]
        if matches!(self.control, Control::BeforeCall | Control::HoldBeforeCall) {
            self.barrier = true;
            return Ok(ReadStep::Pending);
        }
        let len = bytes.len().min(MAX_OPERATION);
        let status = unsafe {
            libc::read(
                self.endpoint(true).fd.as_raw_fd(),
                bytes.as_mut_ptr().cast(),
                len,
            )
        };
        let result = progress(status);
        #[cfg(feature = "test-support")]
        self.record_io(true, len, status, result.is_ok());
        let result = result?;
        Ok(match result {
            Some(0) => ReadStep::Eof,
            Some(n) => ReadStep::Bytes(n),
            None => ReadStep::Pending,
        })
    }
    pub(super) fn try_write(&mut self, bytes: &[u8]) -> io::Result<WriteStep> {
        if self.stopping {
            return Err(stopped());
        }
        if bytes.is_empty() {
            return Ok(WriteStep::Pending);
        }
        let len = bytes.len().min(MAX_OPERATION);
        let status = unsafe {
            libc::write(
                self.endpoint(false).fd.as_raw_fd(),
                bytes.as_ptr().cast(),
                len,
            )
        };
        let result = progress(status);
        #[cfg(feature = "test-support")]
        self.record_io(false, len, status, result.is_ok());
        let result = result?;
        Ok(match result {
            Some(n) if n > 0 => WriteStep::Bytes(n),
            _ => WriteStep::Pending,
        })
    }
    pub(super) fn write_in_flight(&self) -> bool {
        false
    }
    pub(super) fn request_stop(&mut self) {
        self.stopping = true;
    }
    pub(super) fn poll_stop_or_restore(&mut self) -> io::Result<bool> {
        self.request_stop();
        #[cfg(feature = "test-support")]
        if self.control == Control::HoldBeforeCall {
            return Ok(false);
        }
        // Reverse mutation order. Never clear saved flags before verified restore.
        for reading in [false, true] {
            let endpoint = if reading {
                self.input.as_mut()
            } else {
                self.output.as_mut()
            };
            let Some(endpoint) = endpoint else { continue };
            if let Some(flags) = endpoint.saved {
                #[cfg(feature = "test-support")]
                if self.control == Control::HoldRestore {
                    return Err(io::Error::other("synthetic retained restoration failure"));
                }
                let status = unsafe { libc::fcntl(endpoint.fd.as_raw_fd(), libc::F_SETFL, flags) };
                let error = (status < 0).then(io::Error::last_os_error);
                let actual = unsafe { libc::fcntl(endpoint.fd.as_raw_fd(), libc::F_GETFL) };
                #[cfg(feature = "test-support")]
                self.trace.push(Event {
                    phase: "restore",
                    endpoint: label(reading),
                    api: "fcntl(F_SETFL/F_GETFL)",
                    ok: status == 0 && restored_pipe_flags_match(flags, actual),
                    status: Some(status as i64),
                    error: error.as_ref().and_then(io::Error::raw_os_error),
                    observed: json!({"expected":flags,"actual":actual}),
                });
                if let Some(error) = error {
                    return Err(error);
                }
                if !restored_pipe_flags_match(flags, actual) {
                    return Err(io::Error::other("restored pipe flags differ"));
                }
                endpoint.saved = None;
            }
        }
        self.output.take();
        self.input.take();
        Ok(true)
    }
    pub(super) fn is_settled(&self) -> bool {
        self.input.is_none() && self.output.is_none()
    }
    #[cfg(feature = "test-support")]
    pub(super) fn before_call(&self) -> bool {
        self.barrier
    }
    #[cfg(feature = "test-support")]
    pub(super) fn release_control(&mut self) {
        self.control = Control::Normal;
    }
    #[cfg(feature = "test-support")]
    pub(super) fn ownership(&self) -> serde_json::Value {
        json!({"owned_endpoints":usize::from(self.input.is_some())+usize::from(self.output.is_some()),"saved_modes":usize::from(self.input.as_ref().is_some_and(|e|e.saved.is_some()))+usize::from(self.output.as_ref().is_some_and(|e|e.saved.is_some())),"actual_input_flags":self.input.as_ref().map(|e|unsafe {libc::fcntl(e.fd.as_raw_fd(),libc::F_GETFL)}),"actual_output_flags":self.output.as_ref().map(|e|unsafe {libc::fcntl(e.fd.as_raw_fd(),libc::F_GETFL)}),"native_workers":0})
    }
    #[cfg(feature = "test-support")]
    fn record_io(&mut self, reading: bool, len: usize, status: isize, ok: bool) {
        let errno = (status < 0)
            .then(|| io::Error::last_os_error().raw_os_error())
            .flatten();
        self.trace.push(Event { phase: "io", endpoint: label(reading), api: if reading {"read"} else {"write"}, ok, status: Some(status as i64), error: errno, observed: json!({"requested":len,"result":if !ok {"Error"} else if status > 0 {"Bytes"} else if status == 0 && reading {"Eof"} else {"Pending"},"count":(status > 0).then_some(status as usize),"errno":errno}) });
    }
}
#[cfg(feature = "test-support")]
fn label(reading: bool) -> &'static str {
    if reading { "stdin" } else { "stdout" }
}
fn progress(status: isize) -> io::Result<Option<usize>> {
    if status >= 0 {
        return Ok(Some(status as usize));
    }
    let error = io::Error::last_os_error();
    if matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    ) {
        Ok(None)
    } else {
        Err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stdio::Stdio;
    fn pipe() -> (OwnedFd, OwnedFd) {
        let mut pair = [-1; 2];
        assert_eq!(unsafe { libc::pipe(pair.as_mut_ptr()) }, 0);
        unsafe { (OwnedFd::from_raw_fd(pair[0]), OwnedFd::from_raw_fd(pair[1])) }
    }
    fn transport() -> (Stdio, OwnedFd, OwnedFd, OwnedFd, OwnedFd) {
        let (input, peer_input) = pipe();
        let (peer_output, output) = pipe();
        let input_alias = input.try_clone().unwrap();
        let output_alias = output.try_clone().unwrap();
        let mut native = Native::new();
        native.input = Some(Endpoint {
            fd: input,
            saved: None,
        });
        native.output = Some(Endpoint {
            fd: output,
            saved: None,
        });
        assert_ne!(
            native.qualify(true).unwrap(),
            native.qualify(false).unwrap()
        );
        native.nowait(true).unwrap();
        native.nowait(false).unwrap();
        (
            Stdio {
                native: Some(Box::new(native)),
                _not_sync: std::marker::PhantomData,
            },
            peer_input,
            peer_output,
            input_alias,
            output_alias,
        )
    }
    fn flags(fd: &OwnedFd) -> i32 {
        unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) }
    }
    #[test]
    fn native_operations_are_bounded_and_shared_modes_restore_after_stop() {
        let (mut owner, peer_input, peer_output, input_alias, output_alias) = transport();
        let mut read = [0; 8192];
        assert_eq!(owner.try_read(&mut read).unwrap(), ReadStep::Pending);
        assert_eq!(
            unsafe { libc::write(peer_input.as_raw_fd(), [b'I'; 5000].as_ptr().cast(), 5000) },
            5000
        );
        assert_eq!(owner.try_read(&mut read).unwrap(), ReadStep::Bytes(4096));
        assert_eq!(&read[..4096], &[b'I'; 4096]);
        assert_eq!(owner.try_read(&mut read).unwrap(), ReadStep::Bytes(904));
        drop(peer_input);
        assert_eq!(owner.try_read(&mut read).unwrap(), ReadStep::Eof);
        assert_eq!(
            owner.try_write(&[b'O'; 5000]).unwrap(),
            WriteStep::Bytes(4096)
        );
        assert_eq!(
            unsafe {
                libc::read(
                    peer_output.as_raw_fd(),
                    read.as_mut_ptr().cast(),
                    read.len(),
                )
            },
            4096
        );
        assert_eq!(&read[..4096], &[b'O'; 4096]);
        assert_ne!(flags(&input_alias) & libc::O_NONBLOCK, 0);
        assert_ne!(flags(&output_alias) & libc::O_NONBLOCK, 0);
        owner.request_stop();
        assert!(owner.try_write(b"forbidden").is_err());
        assert!(owner.try_read(&mut read).is_err());
        assert!(owner.poll_stop_or_restore().unwrap());
        assert!(owner.is_settled());
        assert_eq!(flags(&input_alias) & libc::O_NONBLOCK, 0);
        assert_eq!(flags(&output_alias) & libc::O_NONBLOCK, 0);
    }
    #[cfg(feature = "test-support")]
    #[test]
    fn failed_restore_keeps_saved_flags_and_authority_for_retry() {
        let (mut owner, _peer_input, _peer_output, input_alias, output_alias) = transport();
        owner.inner().control = Control::HoldRestore;
        assert!(owner.poll_stop_or_restore().is_err());
        assert!(!owner.is_settled());
        assert_ne!(flags(&input_alias) & libc::O_NONBLOCK, 0);
        assert_ne!(flags(&output_alias) & libc::O_NONBLOCK, 0);
        assert!(owner.inner().input.as_ref().unwrap().saved.is_some());
        assert!(owner.inner().output.as_ref().unwrap().saved.is_some());
        owner.inner().release_control();
        assert!(owner.poll_stop_or_restore().unwrap());
        assert_eq!(flags(&input_alias) & libc::O_NONBLOCK, 0);
        assert_eq!(flags(&output_alias) & libc::O_NONBLOCK, 0);
    }
    #[test]
    fn erroneous_drop_retains_native_authority_instead_of_silent_close() {
        let (mut owner, _peer_input, _peer_output, input_alias, output_alias) = transport();
        let input = owner.inner().input.as_ref().unwrap().fd.as_raw_fd();
        let output = owner.inner().output.as_ref().unwrap().fd.as_raw_fd();
        drop(owner);
        assert!(unsafe { libc::fcntl(input, libc::F_GETFD) } >= 0);
        assert!(unsafe { libc::fcntl(output, libc::F_GETFD) } >= 0);
        assert_ne!(flags(&input_alias) & libc::O_NONBLOCK, 0);
        assert_ne!(flags(&output_alias) & libc::O_NONBLOCK, 0);
        // This deliberately leaked owner remains until this test binary exits.
    }
}
