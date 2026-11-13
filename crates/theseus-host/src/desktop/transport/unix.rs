use super::*;
use crate::fs::{HostDir, RootPolicy};
use std::{
    ffi::CString,
    fs::File,
    mem,
    os::unix::{
        ffi::OsStrExt,
        fs::MetadataExt,
        io::{AsRawFd, FromRawFd},
    },
    path::PathBuf,
};
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
fn endpoint(value: BrokerEndpoint) -> io::Result<PathBuf> {
    match value {
        BrokerEndpoint::Unix(path) => Ok(path.as_str().into()),
        _ => Err(invalid("non-Unix broker endpoint")),
    }
}
fn ceiling(value: &PeerCeiling) -> io::Result<()> {
    if !matches!(value.principal, CurrentPrincipal::Posix { .. }) || value.session_id.is_some() {
        return Err(invalid("non-POSIX peer ceiling"));
    }
    Ok(())
}
pub(super) fn measure_local_identity() -> io::Result<MeasuredIdentity> {
    let session = unsafe { libc::getsid(0) };
    if session < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(MeasuredIdentity {
        principal: crate::principal::current_principal()?,
        diagnostic_pid: Some(unsafe { libc::getpid() } as u32),
        session_id: None,
        posix_process_session: Some(session),
        window_station: None,
        desktop: None,
    })
}
fn peer(fd: libc::c_int) -> io::Result<MeasuredIdentity> {
    #[cfg(target_os = "linux")]
    let (uid, gid, pid) = {
        let mut value = unsafe { mem::zeroed::<libc::ucred>() };
        let mut length = mem::size_of::<libc::ucred>() as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut value as *mut libc::ucred).cast(),
                &mut length,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        if length as usize != mem::size_of::<libc::ucred>() {
            return Err(invalid("kernel peer credentials length mismatch"));
        }
        (value.uid, value.gid, Some(value.pid as u32))
    };
    #[cfg(not(target_os = "linux"))]
    let (uid, gid, pid) = {
        let (mut uid, mut gid) = (0, 0);
        if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } != 0 {
            return Err(io::Error::last_os_error());
        }
        (uid, gid, None)
    };
    Ok(MeasuredIdentity {
        principal: CurrentPrincipal::Posix {
            effective_uid: uid,
            effective_gid: gid,
        },
        diagnostic_pid: pid,
        session_id: None,
        posix_process_session: None,
        window_station: None,
        desktop: None,
    })
}
fn address(path: &std::path::Path) -> io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    let mut address = unsafe { mem::zeroed::<libc::sockaddr_un>() };
    if bytes.len() >= address.sun_path.len() {
        return Err(invalid("broker socket endpoint exceeds native bound"));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (i, b) in bytes.iter().enumerate() {
        address.sun_path[i] = *b as libc::c_char;
    }
    let length =
        (mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as libc::socklen_t;
    #[cfg(target_os = "macos")]
    {
        address.sun_len = length as u8;
    }
    Ok((address, length))
}
fn socket() -> io::Result<File> {
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    #[cfg(target_os = "macos")]
    {
        let one: libc::c_int = 1;
        if unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_NOSIGPIPE,
                (&one as *const libc::c_int).cast(),
                mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0
        || unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}
pub(super) struct Stream {
    file: Option<File>,
    peer: Option<MeasuredIdentity>,
    ceiling: PeerCeiling,
    connecting: bool,
    stopping: bool,
}
impl Stream {
    pub fn connect(
        endpoint_value: BrokerEndpoint,
        local: PeerCeiling,
        expected: PeerCeiling,
    ) -> io::Result<Self> {
        ceiling(&local)?;
        ceiling(&expected)?;
        let observed = measure_local_identity()?;
        admit(&observed, &local)?;
        let path = endpoint(endpoint_value)?;
        let (address, length) = address(&path)?;
        let file = socket()?;
        let result = unsafe {
            libc::connect(
                file.as_raw_fd(),
                (&address as *const libc::sockaddr_un).cast(),
                length,
            )
        };
        if result != 0 {
            let error = io::Error::last_os_error();
            if !matches!(
                error.raw_os_error(),
                Some(libc::EINPROGRESS | libc::EALREADY)
            ) {
                return Err(error);
            }
        }
        Ok(Self {
            file: Some(file),
            peer: None,
            ceiling: expected,
            connecting: result != 0,
            stopping: false,
        })
    }
    pub fn identity(&self) -> Option<&MeasuredIdentity> {
        self.peer.as_ref()
    }
    pub fn poll_connected(&mut self) -> io::Result<bool> {
        let fd = self
            .file
            .as_ref()
            .ok_or_else(|| invalid("original socket closed"))?
            .as_raw_fd();
        if self.connecting {
            let mut p = libc::pollfd {
                fd,
                events: libc::POLLOUT,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut p, 1, 0) };
            if ready < 0 {
                return Err(io::Error::last_os_error());
            }
            if ready == 0 {
                return Ok(false);
            }
            let mut error: libc::c_int = 0;
            let mut len = mem::size_of::<libc::c_int>() as libc::socklen_t;
            if unsafe {
                libc::getsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_ERROR,
                    (&mut error as *mut libc::c_int).cast(),
                    &mut len,
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            if error != 0 {
                return Err(io::Error::from_raw_os_error(error));
            }
            self.connecting = false;
        }
        if self.peer.is_none() {
            let measured = peer(fd)?;
            admit(&measured, &self.ceiling)?;
            self.peer = Some(measured);
        }
        Ok(true)
    }
    pub fn try_read(&mut self, bytes: &mut [u8]) -> io::Result<ReadStep> {
        let n = unsafe {
            libc::recv(
                self.file
                    .as_ref()
                    .ok_or_else(|| invalid("socket closed"))?
                    .as_raw_fd(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
                0,
            )
        };
        if n < 0 {
            let error = io::Error::last_os_error();
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                return Ok(ReadStep::Pending);
            }
            return Err(error);
        }
        if n == 0 {
            Ok(ReadStep::Eof)
        } else {
            Ok(ReadStep::Bytes(n as usize))
        }
    }
    pub fn try_write(&mut self, bytes: &[u8]) -> io::Result<WriteStep> {
        #[cfg(target_os = "linux")]
        let flags = libc::MSG_NOSIGNAL;
        #[cfg(not(target_os = "linux"))]
        let flags = 0;
        let n = unsafe {
            libc::send(
                self.file
                    .as_ref()
                    .ok_or_else(|| invalid("socket closed"))?
                    .as_raw_fd(),
                bytes.as_ptr().cast(),
                bytes.len(),
                flags,
            )
        };
        if n < 0 {
            let error = io::Error::last_os_error();
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                return Ok(WriteStep::Pending);
            }
            return Err(error);
        }
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "socket write made no progress",
            ));
        }
        Ok(WriteStep::Bytes(n as usize))
    }
    pub fn write_in_flight(&self) -> bool {
        false
    }
    pub fn request_stop(&mut self) {
        self.stopping = true;
    }
    pub fn poll_settle(&mut self) -> io::Result<bool> {
        if !self.stopping {
            return Ok(false);
        }
        self.file = None;
        Ok(true)
    }
}
pub(super) struct Listener {
    file: Option<File>,
    parent: HostDir,
    parent_path: PathBuf,
    name: CString,
    identity: Option<(u64, u64)>,
    bound: bool,
    revoked: bool,
    peer: PeerCeiling,
    stopping: bool,
    removed: bool,
}
fn stat(parent: &HostDir, name: &CString) -> io::Result<libc::stat> {
    let mut value = unsafe { mem::zeroed::<libc::stat>() };
    if unsafe {
        libc::fstatat(
            parent.directory.as_raw_fd(),
            name.as_ptr(),
            &mut value,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(value)
}
impl Listener {
    pub fn prepare(
        endpoint_value: BrokerEndpoint,
        local: PeerCeiling,
        peer_ceiling: PeerCeiling,
    ) -> io::Result<Self> {
        ceiling(&local)?;
        ceiling(&peer_ceiling)?;
        let observed = measure_local_identity()?;
        admit(&observed, &local)?;
        let path = endpoint(endpoint_value)?;
        let parent_path = path
            .parent()
            .ok_or_else(|| invalid("socket parent missing"))?
            .to_owned();
        let parent = HostDir::open(&parent_path, RootPolicy::AllComponentsNoFollow)?;
        let metadata = parent.directory.metadata()?;
        let CurrentPrincipal::Posix { effective_uid, .. } = observed.principal else {
            return Err(invalid("non-POSIX local identity"));
        };
        if metadata.uid() != effective_uid || metadata.mode() & 0o077 != 0 {
            return Err(invalid(
                "broker endpoint parent must be owned private directory",
            ));
        }
        let name = CString::new(
            path.file_name()
                .ok_or_else(|| invalid("socket leaf missing"))?
                .as_encoded_bytes(),
        )
        .map_err(|_| invalid("invalid socket leaf"))?;
        Ok(Self {
            file: Some(socket()?),
            parent,
            parent_path,
            name,
            identity: None,
            bound: false,
            revoked: false,
            peer: peer_ceiling,
            stopping: false,
            removed: false,
        })
    }
    pub fn start_bind(&mut self) -> io::Result<()> {
        let path = self
            .parent_path
            .join(std::ffi::OsStr::from_bytes(self.name.as_bytes()));
        let (address, length) = address(&path)?;
        if unsafe {
            libc::bind(
                self.file.as_ref().expect("original listener").as_raw_fd(),
                (&address as *const libc::sockaddr_un).cast(),
                length,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        self.bound = true;
        let value = stat(&self.parent, &self.name)?;
        self.identity = Some((value.st_dev as u64, value.st_ino as u64));
        self.finish_bind()
    }
    fn finish_bind(&mut self) -> io::Result<()> {
        // The retained parent is admitted as private 0700. Do not chmod a
        // pathname: a substituted symlink must never mutate its target. Binding,
        // identity capture and unlink require the declared exclusive, quiescent
        // namespace; these checks detect observed replacement, not a hostile
        // same-UID writer racing between checks and native operations.
        self.revalidate()?;
        if unsafe {
            libc::listen(
                self.file.as_ref().expect("original listener").as_raw_fd(),
                1,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    fn revalidate(&mut self) -> io::Result<()> {
        if self.revoked {
            return Err(invalid("original endpoint authority permanently revoked"));
        }
        let result = (|| {
            let current = HostDir::open(&self.parent_path, RootPolicy::AllComponentsNoFollow)?;
            if !self.parent.same_object(&current)? {
                return Err(invalid("original socket parent replaced"));
            }
            let value = stat(&self.parent, &self.name)?;
            if Some((value.st_dev as u64, value.st_ino as u64)) != self.identity
                || value.st_mode & libc::S_IFMT != libc::S_IFSOCK
            {
                return Err(invalid("original socket mapping replaced"));
            }
            Ok(())
        })();
        if result.is_err() {
            self.revoked = true;
        }
        result
    }
    pub fn try_accept(&mut self) -> io::Result<Option<Stream>> {
        self.revalidate()?;
        let fd = unsafe {
            libc::accept(
                self.file
                    .as_ref()
                    .ok_or_else(|| invalid("listener closed"))?
                    .as_raw_fd(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if fd < 0 {
            let error = io::Error::last_os_error();
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                return Ok(None);
            }
            return Err(error);
        }
        let file = unsafe { File::from_raw_fd(fd) };
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0
            || unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) } < 0
        {
            return Err(io::Error::last_os_error());
        }
        #[cfg(target_os = "macos")]
        {
            let one: libc::c_int = 1;
            if unsafe {
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_NOSIGPIPE,
                    (&one as *const libc::c_int).cast(),
                    mem::size_of::<libc::c_int>() as libc::socklen_t,
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        let observed = peer(fd)?;
        admit(&observed, &self.peer)?;
        Ok(Some(Stream {
            file: Some(file),
            peer: Some(observed),
            ceiling: self.peer.clone(),
            connecting: false,
            stopping: false,
        }))
    }
    pub fn request_stop(&mut self) {
        self.stopping = true;
    }
    pub fn poll_settle(&mut self) -> io::Result<bool> {
        if !self.stopping {
            return Ok(false);
        }
        self.file = None;
        if self.bound && !self.removed {
            self.revalidate()?;
            if unsafe { libc::unlinkat(self.parent.directory.as_raw_fd(), self.name.as_ptr(), 0) }
                != 0
            {
                return Err(io::Error::last_os_error());
            }
            self.removed = true;
        }
        Ok(true)
    }
}

#[cfg(test)]
mod mapping_tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    // Run by the parent alongside real UDS tests. Re-enter the setup tail after
    // identity capture with a substituted link, exactly where pathname chmod
    // previously mutated a target before no-follow validation.
    #[test]
    fn setup_tail_never_chmods_symlink_target_and_revocation_survives_restoration() {
        let dir = tempfile::Builder::new()
            .prefix("tb-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().join("broker.sock");
        let target = dir.path().join("unrelated");
        std::fs::write(&target, b"unrelated original file").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o664)).unwrap();
        let principal = measure_local_identity().unwrap().principal;
        let ceiling = PeerCeiling {
            principal,
            session_id: None,
        };
        let endpoint = BrokerEndpoint::Unix(
            crate::services::readiness::UnixPath::new(path.to_str().unwrap().into()).unwrap(),
        );
        let mut listener = Listener::prepare(endpoint, ceiling.clone(), ceiling).unwrap();
        listener.start_bind().unwrap();
        let held = dir.path().join("held.sock");
        std::fs::rename(&path, &held).unwrap();
        symlink(&target, &path).unwrap();
        assert!(listener.finish_bind().is_err());
        assert_eq!(std::fs::metadata(&target).unwrap().mode() & 0o777, 0o664);
        std::fs::remove_file(&path).unwrap();
        std::fs::rename(&held, &path).unwrap();
        listener.request_stop();
        assert!(listener.poll_settle().is_err());
        assert!(
            path.exists(),
            "revoked mapping was unlinked after restoration"
        );
    }
}
