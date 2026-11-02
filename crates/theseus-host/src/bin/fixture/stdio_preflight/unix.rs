#[path = "../../../stdio/unix_flags.rs"]
mod unix_flags;
use super::{Evidence, Observation, Progress, failure};
use serde_json::json;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use unix_flags::restored_pipe_flags_match;

pub(super) struct Endpoint {
    fd: OwnedFd,
    label: &'static str,
    saved: Option<i32>,
}
impl Endpoint {
    pub(super) fn expect_read_access(&mut self) {
        self.label = "stdin";
    }
    pub(super) fn mode_changed(&self) -> bool {
        self.saved.is_some()
    }

    pub(super) fn standard(fd: i32, evidence: &mut Evidence) -> io::Result<Self> {
        let label = if fd == 0 { "stdin" } else { "stdout" };
        let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
        let error = (duplicate < 0).then(io::Error::last_os_error);
        evidence.event(
            "duplicate",
            label,
            "fcntl(F_DUPFD_CLOEXEC)",
            duplicate >= 0,
            (Some(duplicate as i64), None),
            json!({"errno":error.as_ref().and_then(io::Error::raw_os_error),"same_access":true}),
        )?;
        if let Some(error) = error {
            return Err(error);
        }
        Ok(Self {
            fd: unsafe { OwnedFd::from_raw_fd(duplicate) },
            label,
            saved: None,
        })
    }
    pub(super) fn qualify(&self, evidence: &mut Evidence) -> io::Result<()> {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        let status = unsafe { libc::fstat(self.fd.as_raw_fd(), stat.as_mut_ptr()) };
        let error = (status != 0).then(io::Error::last_os_error);
        evidence.event(
            "query",
            self.label,
            "fstat",
            status == 0,
            (Some(status as i64), None),
            json!({"errno":error.as_ref().and_then(io::Error::raw_os_error)}),
        )?;
        if let Some(error) = error {
            return Err(error);
        }
        let stat = unsafe { stat.assume_init() };
        let mode = unsafe { libc::fcntl(self.fd.as_raw_fd(), libc::F_GETFL) };
        if mode < 0 {
            return Err(io::Error::last_os_error());
        }
        let pipe = stat.st_mode & libc::S_IFMT == libc::S_IFIFO;
        let access = mode & libc::O_ACCMODE;
        let rights = if self.label == "stdin" {
            access != libc::O_WRONLY
        } else {
            access != libc::O_RDONLY
        };
        evidence.event("qualified", self.label, "fstat/F_GETFL", pipe && rights, (Some(mode as i64), None), json!({"file_type":stat.st_mode & libc::S_IFMT,"pipe":pipe,"access":access,"local":true}))?;
        if !pipe || !rights {
            return Err(failure("endpoint is not a pipe with required access"));
        }
        Ok(())
    }
    pub(super) fn nowait(&mut self, evidence: &mut Evidence) -> io::Result<()> {
        let mode = unsafe { libc::fcntl(self.fd.as_raw_fd(), libc::F_GETFL) };
        if mode < 0 {
            return Err(io::Error::last_os_error());
        }
        let status =
            unsafe { libc::fcntl(self.fd.as_raw_fd(), libc::F_SETFL, mode | libc::O_NONBLOCK) };
        if status == 0 {
            self.saved = Some(mode);
        }
        let error = (status < 0).then(io::Error::last_os_error);
        evidence.event(
            "set_nowait",
            self.label,
            "fcntl(F_SETFL)",
            status == 0,
            (Some(status as i64), None),
            json!({"before":mode,"errno":error.as_ref().and_then(io::Error::raw_os_error)}),
        )?;
        if let Some(error) = error {
            return Err(error);
        }
        self.verify_sharing(evidence)
    }
    pub(super) fn verify_sharing(&self, evidence: &mut Evidence) -> io::Result<()> {
        let duplicate = unsafe { libc::fcntl(self.fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
        if duplicate < 0 {
            return Err(io::Error::last_os_error());
        }
        let duplicate = unsafe { OwnedFd::from_raw_fd(duplicate) };
        let actual = unsafe { libc::fcntl(duplicate.as_raw_fd(), libc::F_GETFL) };
        let ok = actual >= 0 && actual & libc::O_NONBLOCK != 0;
        evidence.event(
            "mode_sharing",
            self.label,
            "fcntl(F_GETFL duplicate)",
            ok,
            (Some(actual as i64), None),
            json!({"nowait":actual & libc::O_NONBLOCK != 0}),
        )?;
        if !ok {
            return Err(failure("nonblocking mode was not shared"));
        }
        Ok(())
    }
    pub(super) fn restore(&mut self, evidence: &mut Evidence, fault: bool) -> io::Result<()> {
        let mut injected = None;
        if fault {
            let result = unsafe { libc::fcntl(-1, libc::F_SETFL, 0) };
            let error = io::Error::last_os_error();
            evidence.event(
                "restore_fault",
                self.label,
                "fcntl(invalid descriptor)",
                false,
                (Some(result as i64), None),
                json!({"errno":error.raw_os_error(),"injected":true}),
            )?;
            injected = Some(error);
        }
        if let Some(mode) = self.saved.take() {
            let status = unsafe { libc::fcntl(self.fd.as_raw_fd(), libc::F_SETFL, mode) };
            let error = (status < 0).then(io::Error::last_os_error);
            let actual = unsafe { libc::fcntl(self.fd.as_raw_fd(), libc::F_GETFL) };
            evidence.event("restore", self.label, "fcntl(F_SETFL/F_GETFL)", status == 0 && restored_pipe_flags_match(mode, actual), (Some(status as i64), None), json!({"expected":mode,"actual":actual,"errno":error.as_ref().and_then(io::Error::raw_os_error)}))?;
            if let Some(error) = error {
                return Err(error);
            }
            if !restored_pipe_flags_match(mode, actual) {
                return Err(failure("restored mode differs"));
            }
        }
        injected.map_or(Ok(()), Err)
    }
}
fn observation(status: isize, api: &'static str, reading: bool) -> Observation {
    let error = (status < 0).then(io::Error::last_os_error);
    let code = error.as_ref().and_then(io::Error::raw_os_error);
    let progress = if status > 0 {
        Ok(Progress::Bytes(status as usize))
    } else if status == 0 {
        Ok(if reading {
            Progress::Eof
        } else {
            Progress::Pending
        })
    } else if error.as_ref().is_some_and(|e| {
        matches!(
            e.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
        )
    }) {
        Ok(Progress::Pending)
    } else {
        Err(error.unwrap())
    };
    Observation {
        progress,
        api,
        status: status as i64,
        error: code,
    }
}
pub(super) fn read(endpoint: &Endpoint, bytes: &mut [u8]) -> Observation {
    observation(
        unsafe {
            libc::read(
                endpoint.fd.as_raw_fd(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        },
        "read",
        true,
    )
}
pub(super) fn write(endpoint: &Endpoint, bytes: &[u8]) -> Observation {
    observation(
        unsafe { libc::write(endpoint.fd.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) },
        "write",
        false,
    )
}
pub(super) fn control(case: &str, candidate: &str, evidence: &mut Evidence) -> io::Result<bool> {
    if matches!(case, "message-pipe" | "overlapped-pipe") {
        evidence.mark("platform_scope",json!({"applicable":false,"reason":"Windows-only pipe creation modes; Unix descriptors have no such modes"}))?;
        return Ok(true);
    }
    if case == "stderr-alias" {
        if unsafe { libc::dup2(1, 2) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut out = std::mem::MaybeUninit::<libc::stat>::uninit();
        let mut err = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(1, out.as_mut_ptr()) } != 0
            || unsafe { libc::fstat(2, err.as_mut_ptr()) } != 0
        {
            return Err(io::Error::last_os_error());
        }
        let (out, err) = unsafe { (out.assume_init(), err.assume_init()) };
        evidence.mark("stderr_policy",json!({"same_object":out.st_dev == err.st_dev && out.st_ino == err.st_ino,"stderr_selected":false}))?;
    }
    let mut keepers = Vec::<OwnedFd>::new();
    let endpoint = match case {
        "file" => Some(Endpoint {
            fd: tempfile::tempfile()?.into(),
            label: "stdin",
            saved: None,
        }),
        "socket" => {
            let (first, second) = std::os::unix::net::UnixStream::pair()?;
            keepers.push(second.into());
            Some(Endpoint {
                fd: first.into(),
                label: "stdin",
                saved: None,
            })
        }
        "terminal" => {
            let (mut master, mut slave) = (-1, -1);
            let status = unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if status != 0 {
                return Err(io::Error::last_os_error());
            }
            keepers.push(unsafe { OwnedFd::from_raw_fd(master) });
            Some(Endpoint {
                fd: unsafe { OwnedFd::from_raw_fd(slave) },
                label: "stdin",
                saved: None,
            })
        }
        "reduced-access" => {
            let mut pair = [-1, -1];
            if unsafe { libc::pipe(pair.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            keepers.push(unsafe { OwnedFd::from_raw_fd(pair[0]) });
            Some(Endpoint {
                fd: unsafe { OwnedFd::from_raw_fd(pair[1]) },
                label: "stdin",
                saved: None,
            })
        }
        "named-byte" => {
            use std::os::unix::ffi::OsStrExt;
            let temp = tempfile::tempdir()?;
            let path = temp.path().join("byte-pipe");
            let name = std::ffi::CString::new(path.as_os_str().as_bytes())?;
            if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let raw = unsafe {
                libc::open(
                    name.as_ptr(),
                    libc::O_RDWR | libc::O_NONBLOCK | libc::O_CLOEXEC,
                )
            };
            if raw < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut pipe = Endpoint {
                fd: unsafe { OwnedFd::from_raw_fd(raw) },
                label: "stdin",
                saved: None,
            };
            if candidate == "owned_io_experiment" {
                let write = write(&pipe, b"positive");
                write.record(evidence, "control_pipe")?;
                if !matches!(write.progress?, Progress::Bytes(8)) {
                    return Err(failure("named pipe peer write"));
                }
                if !super::owned::probe(&pipe, evidence, true)? {
                    return Err(failure("production named pipe rejected"));
                }
                return Ok(true);
            }
            pipe.qualify(evidence)?;
            pipe.nowait(evidence)?;
            let write = write(&pipe, b"positive");
            write.record(evidence, "control_pipe")?;
            if !matches!(write.progress?, Progress::Bytes(8)) {
                return Err(failure("named pipe write"));
            }
            let mut buffer = [0; 8];
            let read = read(&pipe, &mut buffer);
            read.record(evidence, "control_pipe")?;
            if !matches!(read.progress?, Progress::Bytes(8)) || buffer != *b"positive" {
                return Err(failure("named pipe read"));
            }
            pipe.restore(evidence, false)?;
            evidence.mark("restored", json!({"positive_control":true}))?;
            return Ok(true);
        }
        _ => None,
    };
    if let Some(endpoint) = endpoint {
        let accepted = if candidate == "owned_io_experiment" {
            super::owned::probe(&endpoint, evidence, false)?
        } else {
            endpoint.qualify(evidence).is_ok()
        };
        if accepted {
            return Err(failure("unsupported endpoint was accepted"));
        }
        evidence.mark("rejected", json!({"case":case,"actual_endpoint":true}))?;
        return Ok(true);
    }
    Ok(false)
}

pub(super) fn restore_fault(evidence: &mut Evidence) -> io::Result<()> {
    let status = unsafe { libc::fcntl(-1, libc::F_SETFL, 0) };
    let error = io::Error::last_os_error();
    evidence.event(
        "restore_fault",
        "control",
        "fcntl(invalid descriptor)",
        false,
        (Some(status as i64), None),
        json!({"errno":error.raw_os_error(),"injected":true}),
    )
}

pub(super) struct InputOverride {
    saved: OwnedFd,
    index: i32,
}
impl Drop for InputOverride {
    fn drop(&mut self) {
        if unsafe { libc::dup2(self.saved.as_raw_fd(), self.index) } < 0 {
            std::process::exit(2);
        }
    }
}
impl Endpoint {
    pub(super) fn install_input(&self) -> io::Result<InputOverride> {
        let raw = unsafe { libc::fcntl(0, libc::F_DUPFD_CLOEXEC, 3) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let saved = unsafe { OwnedFd::from_raw_fd(raw) };
        if unsafe { libc::dup2(self.fd.as_raw_fd(), 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(InputOverride { saved, index: 0 })
    }
}

pub(super) fn self_loop(evidence: &mut Evidence) -> io::Result<()> {
    let mut pair = [-1; 2];
    if unsafe { libc::pipe(pair.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let input = Endpoint {
        fd: unsafe { OwnedFd::from_raw_fd(pair[0]) },
        label: "stdin",
        saved: None,
    };
    let output = unsafe { OwnedFd::from_raw_fd(pair[1]) };
    let saved = unsafe { libc::fcntl(1, libc::F_DUPFD_CLOEXEC, 3) };
    if saved < 0 {
        return Err(io::Error::last_os_error());
    }
    let restore = InputOverride {
        saved: unsafe { OwnedFd::from_raw_fd(saved) },
        index: 1,
    };
    if unsafe { libc::dup2(output.as_raw_fd(), 1) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let accepted = super::owned::probe(&input, evidence, false)?;
    drop(restore);
    if accepted {
        return Err(failure("production accepted one pipe self-loop"));
    }
    evidence.mark(
        "rejected",
        json!({"actual_self_loop":true,"production_transport":true}),
    )
}
