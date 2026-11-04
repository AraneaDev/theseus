use super::*;
use crate::fs::{HostDir, RootPolicy};
#[cfg(target_os = "linux")]
use std::{
    fs::File,
    os::{fd::FromRawFd, unix::fs::FileTypeExt},
};
use std::{
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::MetadataExt,
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
};

struct ProbeState {
    challenge: ReadinessChallenge,
    expectation: ReadinessExpectation,
    expected_peer: PeerExpectation,
    expected_pid: Option<u32>,
    stream: Option<UnixStream>,
    peer: Option<PeerCredentials>,
    request: Vec<u8>,
    written: usize,
    prefix: [u8; 4],
    prefix_read: usize,
    body: Vec<u8>,
    body_read: usize,
    last: Option<ReadinessObservation>,
    terminal: bool,
}
/// Original helper-owned endpoint and bounded in-progress challenge storage.
pub struct LocalChallenge {
    endpoint: UnixPath,
    root_path: PathBuf,
    leaf: std::ffi::CString,
    root: HostDir,
    endpoint_owner: SocketOwner,
    listener: UnixListener,
    active: Option<ProbeState>,
    revoked: bool,
    removed: bool,
    retained_errors: Vec<io::Error>,
}
impl LocalChallenge {
    /// Parent must already be an explicit private directory. Never unlinks or
    /// adopts a preexisting socket, and never creates parent directories.
    pub fn bind(endpoint: UnixPath) -> io::Result<Self> {
        let path = Path::new(endpoint.as_str());
        let root_path = path
            .parent()
            .ok_or_else(|| invalid("missing socket parent"))?
            .to_owned();
        let leaf = std::ffi::CString::new(
            path.file_name()
                .ok_or_else(|| invalid("missing socket leaf"))?
                .as_encoded_bytes(),
        )
        .map_err(|_| invalid("NUL endpoint leaf"))?;
        let root = HostDir::open(&root_path, RootPolicy::AllComponentsNoFollow)?;
        if root.directory.metadata()?.mode() & 0o077 != 0 {
            return Err(invalid(
                "readiness socket requires an explicitly private parent",
            ));
        }
        let listener = UnixListener::bind(path)?;
        listener.set_nonblocking(true)?;
        let endpoint_owner = open_socket(&root, &leaf)?;
        let mut channel = Self {
            endpoint,
            root_path,
            leaf,
            root,
            endpoint_owner,
            listener,
            active: None,
            revoked: false,
            removed: false,
            retained_errors: Vec::new(),
        };
        channel.revalidate()?;
        Ok(channel)
    }
    pub fn endpoint(&self) -> &UnixPath {
        &self.endpoint
    }
    pub fn is_revoked(&self) -> bool {
        self.revoked
    }
    fn revalidate(&mut self) -> io::Result<()> {
        if self.removed {
            return Err(invalid("original readiness endpoint revoked or removed"));
        }
        let result = (|| {
            let current = HostDir::open(&self.root_path, RootPolicy::AllComponentsNoFollow)?;
            if !self.root.same_object(&current)? {
                return Err(invalid("readiness root replaced"));
            }
            if !same_socket(&self.endpoint_owner, &open_socket(&self.root, &self.leaf)?)? {
                return Err(invalid("readiness endpoint replaced"));
            }
            Ok(())
        })();
        if result.is_err() {
            self.revoked = true;
        }
        result
    }
    /// A timed-out/cancelled probe retains the original stream and partial frame.
    /// Repeat only these exact inputs; a new nonce cannot replace pending work.
    pub fn probe(
        &mut self,
        challenge: &ReadinessChallenge,
        expectation: &ReadinessExpectation,
        peer: &PeerExpectation,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<ReadinessObservation> {
        self.probe_inner(challenge, expectation, peer, None, deadline, cancel)
    }
    /// Adds a kernel peer PID check from the caller's original accepted Start.
    /// The PID is never opened, signalled, or used to reconstruct an owner.
    pub fn probe_original(
        &mut self,
        challenge: &ReadinessChallenge,
        expectation: &ReadinessExpectation,
        peer: &PeerExpectation,
        original_pid: u32,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<ReadinessObservation> {
        if original_pid == 0 {
            return Err(invalid("missing original Start peer PID"));
        }
        self.probe_inner(
            challenge,
            expectation,
            peer,
            Some(original_pid),
            deadline,
            cancel,
        )
    }
    fn probe_inner(
        &mut self,
        challenge: &ReadinessChallenge,
        expectation: &ReadinessExpectation,
        peer: &PeerExpectation,
        original_pid: Option<u32>,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<ReadinessObservation> {
        #[cfg(target_os = "macos")]
        if original_pid.is_none() {
            return Err(invalid(
                "Darwin readiness requires original accepted Start PID",
            ));
        }
        if self.revoked {
            return Err(invalid("readiness endpoint authority permanently revoked"));
        }
        self.revalidate()?;
        validate_challenge(challenge)?;
        if !literal(&expectation.version_literal)
            || !literal(&peer.cooperative_instance)
            || peer.instance_binding_digest == [0; 32]
        {
            return Err(invalid(
                "missing admitted readiness expectation or instance binding",
            ));
        }
        if let Some(active) = &self.active {
            if &active.challenge != challenge
                || &active.expectation != expectation
                || &active.expected_peer != peer
                || active.expected_pid != original_pid
            {
                return Err(invalid("pending original probe context cannot be replaced"));
            }
            if active.terminal {
                return Ok(active.last.clone().expect("terminal observation retained"));
            }
        } else {
            let body = encode_challenge(challenge)?;
            let mut request = Vec::with_capacity(body.len() + 4);
            request.extend_from_slice(&(body.len() as u32).to_be_bytes());
            request.extend_from_slice(&body);
            self.active = Some(ProbeState {
                challenge: challenge.clone(),
                expectation: expectation.clone(),
                expected_peer: peer.clone(),
                expected_pid: original_pid,
                stream: None,
                peer: None,
                request,
                written: 0,
                prefix: [0; 4],
                prefix_read: 0,
                body: Vec::new(),
                body_read: 0,
                last: None,
                terminal: false,
            });
        }
        loop {
            if let Some(failure) = deadline_failure(deadline, cancel)? {
                return Ok(self.observe(
                    ReadinessState::Incomplete,
                    Some(failure),
                    None,
                    false,
                    false,
                ));
            }
            self.revalidate()?;
            match self.advance() {
                Ok(Some(observation)) => {
                    self.revalidate()?;
                    if let Some(failure) = deadline_failure(deadline, cancel)? {
                        return Ok(self.observe(
                            ReadinessState::Incomplete,
                            Some(failure),
                            observation.response,
                            false,
                            true,
                        ));
                    }
                    return Ok(observation);
                }
                Ok(None) => {}
                Err(error) => {
                    if self.retained_errors.len() < 16 {
                        self.retained_errors.push(error);
                    }
                    return Ok(self.observe(
                        ReadinessState::Incomplete,
                        Some(ReadinessFailure::PeerClosed),
                        None,
                        false,
                        true,
                    ));
                }
            }
            let active = self.active.as_ref().expect("original probe retained");
            let (fd, events) = match &active.stream {
                None => (self.listener.as_raw_fd(), libc::POLLIN),
                Some(stream) => (
                    stream.as_raw_fd(),
                    if active.written < active.request.len() {
                        libc::POLLOUT
                    } else {
                        libc::POLLIN
                    },
                ),
            };
            let mut pollfd = libc::pollfd {
                fd,
                events,
                revents: 0,
            };
            let timeout = deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .min(25) as i32;
            if unsafe { libc::poll(&mut pollfd, 1, timeout) } < 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    if self.retained_errors.len() < 16 {
                        self.retained_errors.push(error);
                    }
                    return Ok(self.observe(
                        ReadinessState::Incomplete,
                        Some(ReadinessFailure::PeerClosed),
                        None,
                        false,
                        true,
                    ));
                }
            }
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
        let active = self.active.as_mut().expect("original probe retained");
        let observation = ReadinessObservation {
            state,
            peer: active.peer,
            peer_sid: None,
            response,
            failure,
            authenticated_instance,
            instance_binding_digest: active.expected_peer.instance_binding_digest,
        };
        active.last = Some(observation.clone());
        active.terminal = terminal;
        if terminal {
            active.stream.take();
        }
        observation
    }
    fn advance(&mut self) -> io::Result<Option<ReadinessObservation>> {
        let active = self.active.as_mut().expect("original probe retained");
        if active.stream.is_none() {
            let (stream, _) = match self.listener.accept() {
                Ok(pair) => pair,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
                Err(error) => return Err(error),
            };
            stream.set_nonblocking(true)?;
            let actual = peer_credentials(&stream)?;
            active.peer = Some(actual);
            if actual.uid != active.expected_peer.uid
                || actual.gid != active.expected_peer.gid
                || active.expected_pid.is_some_and(|pid| pid != actual.pid)
            {
                self.revoked = true;
                return Ok(Some(self.observe(
                    ReadinessState::Failed,
                    Some(ReadinessFailure::WrongPeer),
                    None,
                    false,
                    true,
                )));
            }
            active.stream = Some(stream);
        }
        let stream = active.stream.as_mut().expect("accepted original stream");
        while active.written < active.request.len() {
            match stream.write(&active.request[active.written..]) {
                Ok(0) => {
                    return Ok(Some(self.observe(
                        ReadinessState::Incomplete,
                        Some(ReadinessFailure::PeerClosed),
                        None,
                        false,
                        true,
                    )));
                }
                Ok(n) => active.written += n,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        while active.prefix_read < 4 {
            match stream.read(&mut active.prefix[active.prefix_read..]) {
                Ok(0) => {
                    return Ok(Some(self.observe(
                        ReadinessState::Incomplete,
                        Some(ReadinessFailure::PeerClosed),
                        None,
                        false,
                        true,
                    )));
                }
                Ok(n) => active.prefix_read += n,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        let length = u32::from_be_bytes(active.prefix) as usize;
        if length > MAX_FRAME_BYTES {
            return Ok(Some(self.observe(
                ReadinessState::Failed,
                Some(ReadinessFailure::FrameTooLarge),
                None,
                false,
                true,
            )));
        }
        if active.body.is_empty() {
            active.body.resize(length, 0);
        }
        while active.body_read < length {
            match stream.read(&mut active.body[active.body_read..]) {
                Ok(0) => {
                    return Ok(Some(self.observe(
                        ReadinessState::Incomplete,
                        Some(ReadinessFailure::PeerClosed),
                        None,
                        false,
                        true,
                    )));
                }
                Ok(n) => active.body_read += n,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        let response = match decode_response(&active.body) {
            Ok(response) => response,
            Err(_) => {
                let failure = if decode_fields(&active.body, 15)
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
            &active.challenge,
            &active.expectation,
            &active.expected_peer.cooperative_instance,
            &response,
        );
        let authenticated_instance = correlation_authenticated(failure);
        Ok(Some(self.observe(
            if failure.is_none() {
                ReadinessState::Ready
            } else {
                ReadinessState::Failed
            },
            failure,
            Some(response),
            authenticated_instance,
            true,
        )))
    }
    pub fn pending_observation(&self) -> Option<&ReadinessObservation> {
        self.active.as_ref().and_then(|active| active.last.as_ref())
    }
    pub fn take_terminal_probe(&mut self) -> io::Result<ReadinessObservation> {
        if !self.active.as_ref().is_some_and(|active| active.terminal) {
            return Err(invalid("original probe remains pending"));
        }
        Ok(self
            .active
            .take()
            .expect("terminal probe retained")
            .last
            .expect("terminal observation retained"))
    }
    /// Explicitly settle this read-only IPC by closing its original local stream.
    /// This never claims application readiness, instance exit or manager cleanup.
    pub fn abandon_probe(&mut self) -> Option<ReadinessObservation> {
        self.active.take().and_then(|active| active.last)
    }
    pub fn cleanup(&mut self) -> io::Result<()> {
        if self.removed {
            return Ok(());
        }
        self.revalidate()?;
        if unsafe { libc::unlinkat(self.root.directory.as_raw_fd(), self.leaf.as_ptr(), 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        self.removed = true;
        self.active.take();
        Ok(())
    }
}
impl Drop for LocalChallenge {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}
#[cfg(target_os = "linux")]
type SocketOwner = File;
#[cfg(target_os = "linux")]
fn open_socket(root: &HostDir, leaf: &std::ffi::CStr) -> io::Result<SocketOwner> {
    let fd = unsafe {
        libc::openat(
            root.directory.as_raw_fd(),
            leaf.as_ptr(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    if !file.metadata()?.file_type().is_socket() {
        return Err(invalid("readiness endpoint is not a no-follow socket"));
    }
    Ok(file)
}
#[cfg(target_os = "linux")]
fn same_socket(a: &SocketOwner, b: &SocketOwner) -> io::Result<bool> {
    let a = a.metadata()?;
    let b = b.metadata()?;
    Ok(a.dev() == b.dev() && a.ino() == b.ino())
}
#[cfg(target_os = "linux")]
fn peer_credentials(stream: &UnixStream) -> io::Result<PeerCredentials> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    if length as usize != std::mem::size_of::<libc::ucred>() || credentials.pid <= 0 {
        return Err(invalid("incomplete socket peer credentials"));
    }
    Ok(PeerCredentials {
        pid: credentials.pid as u32,
        uid: credentials.uid,
        gid: credentials.gid,
    })
}
// Darwin does not expose Linux O_PATH or SO_PEERCRED. Retain the original
// listener and no-follow directory-relative socket identity instead.
#[cfg(target_os = "macos")]
struct SocketOwner {
    dev: libc::dev_t,
    ino: libc::ino_t,
}
#[cfg(target_os = "macos")]
fn open_socket(root: &HostDir, leaf: &std::ffi::CStr) -> io::Result<SocketOwner> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            root.directory.as_raw_fd(),
            leaf.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFSOCK {
        return Err(invalid("readiness endpoint is not a no-follow socket"));
    }
    Ok(SocketOwner {
        dev: stat.st_dev,
        ino: stat.st_ino,
    })
}
#[cfg(target_os = "macos")]
fn same_socket(a: &SocketOwner, b: &SocketOwner) -> io::Result<bool> {
    Ok(a.dev == b.dev && a.ino == b.ino)
}
#[cfg(target_os = "macos")]
fn peer_credentials(stream: &UnixStream) -> io::Result<PeerCredentials> {
    let (mut uid, mut gid) = (0, 0);
    if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // LOCAL_PEERPID is a Darwin SOL_LOCAL option (not Linux SO_PEERCRED).
    let mut pid: libc::pid_t = 0;
    let mut length = std::mem::size_of_val(&pid) as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&mut pid as *mut libc::pid_t).cast(),
            &mut length,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    if length as usize != std::mem::size_of_val(&pid) || pid <= 0 {
        return Err(invalid("missing Darwin kernel peer PID"));
    }
    Ok(PeerCredentials {
        pid: pid as u32,
        uid,
        gid,
    })
}

#[cfg(test)]
mod socket_tests {
    use super::tests::{challenge, expectation, response};
    use super::*;
    fn peer() -> PeerExpectation {
        PeerExpectation {
            uid: unsafe { libc::geteuid() },
            gid: unsafe { libc::getegid() },
            cooperative_instance: "original-invocation".into(),
            instance_binding_digest: [8; 32],
        }
    }
    impl LocalChallenge {
        fn probe_fixture(
            &mut self,
            challenge: &ReadinessChallenge,
            expectation: &ReadinessExpectation,
            peer: &PeerExpectation,
            deadline: Instant,
            cancel: &AtomicBool,
        ) -> io::Result<ReadinessObservation> {
            #[cfg(target_os = "linux")]
            {
                self.probe(challenge, expectation, peer, deadline, cancel)
            }
            #[cfg(target_os = "macos")]
            {
                self.probe_original(
                    challenge,
                    expectation,
                    peer,
                    std::process::id(),
                    deadline,
                    cancel,
                )
            }
        }
    }
    fn fixture() -> (tempfile::TempDir, UnixPath, LocalChallenge) {
        let dir = tempfile::Builder::new()
            .prefix("th-ready-")
            .tempdir_in(if cfg!(target_os = "macos") {
                "/private/tmp"
            } else {
                "/tmp"
            })
            .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let endpoint =
            UnixPath::new(dir.path().join("ready.sock").to_str().unwrap().into()).unwrap();
        let channel = LocalChallenge::bind(endpoint.clone()).unwrap();
        (dir, endpoint, channel)
    }
    fn application(endpoint: UnixPath, response: ReadinessResponse) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let mut stream = UnixStream::connect(endpoint.as_str()).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut length = [0; 4];
            if stream.read_exact(&mut length).is_err() {
                return;
            }
            let mut body = vec![0; u32::from_be_bytes(length) as usize];
            stream.read_exact(&mut body).unwrap();
            assert_eq!(decode_challenge(&body).unwrap(), challenge());
            let bytes = encode_response(&response).unwrap();
            if stream
                .write_all(&(bytes.len() as u32).to_be_bytes())
                .is_ok()
            {
                let _ = stream.write_all(&bytes);
            }
        })
    }
    #[test]
    fn owned_socket_challenge_authenticates_peer_separately_from_claimed_instance() {
        let (_dir, endpoint, mut channel) = fixture();
        let app = application(endpoint, response());
        let observed = channel
            .probe_fixture(
                &challenge(),
                &expectation(),
                &peer(),
                Instant::now() + Duration::from_secs(2),
                &AtomicBool::new(false),
            )
            .unwrap();
        app.join().unwrap();
        assert_eq!(observed.state, ReadinessState::Ready);
        assert!(observed.authenticated_instance);
        assert_eq!(
            observed.peer.unwrap(),
            PeerCredentials {
                pid: std::process::id(),
                uid: peer().uid,
                gid: peer().gid
            }
        );
        assert_eq!(observed.response, Some(response()));
    }
    #[test]
    fn original_start_pid_is_checked_before_disclosing_challenge() {
        for (pid, ready) in [
            (std::process::id(), true),
            (std::process::id().wrapping_add(1), false),
        ] {
            let (_dir, endpoint, mut channel) = fixture();
            let app = application(endpoint, response());
            let observed = channel
                .probe_original(
                    &challenge(),
                    &expectation(),
                    &peer(),
                    pid,
                    Instant::now() + Duration::from_secs(2),
                    &AtomicBool::new(false),
                )
                .unwrap();
            app.join().unwrap();
            assert_eq!(observed.authenticated_instance, ready);
            assert_eq!(
                observed.state,
                if ready {
                    ReadinessState::Ready
                } else {
                    ReadinessState::Failed
                }
            );
            if !ready {
                assert_eq!(observed.failure, Some(ReadinessFailure::WrongPeer));
                assert!(observed.response.is_none());
                assert!(channel.is_revoked());
            }
        }
    }
    #[test]
    fn cancelled_original_start_correlation_cannot_be_replaced_on_resume() {
        let (_dir, _endpoint, mut channel) = fixture();
        let pid = std::process::id();
        let cancelled = AtomicBool::new(true);
        let deadline = Instant::now() + Duration::from_secs(2);
        assert_eq!(
            channel
                .probe_original(
                    &challenge(),
                    &expectation(),
                    &peer(),
                    pid,
                    deadline,
                    &cancelled
                )
                .unwrap()
                .failure,
            Some(ReadinessFailure::Cancelled)
        );
        assert!(
            channel
                .probe_original(
                    &challenge(),
                    &expectation(),
                    &peer(),
                    pid.wrapping_add(1),
                    deadline,
                    &cancelled
                )
                .is_err()
        );
        assert!(
            channel
                .probe(&challenge(), &expectation(), &peer(), deadline, &cancelled)
                .is_err()
        );
        assert!(channel.take_terminal_probe().is_err());
        assert!(channel.abandon_probe().is_some());
    }
    #[test]
    fn wrong_peer_is_failed_before_challenge_data_is_disclosed() {
        let (_dir, endpoint, mut channel) = fixture();
        let app = application(endpoint, response());
        let mut wrong = peer();
        wrong.uid = wrong.uid.wrapping_add(1);
        let observed = channel
            .probe_fixture(
                &challenge(),
                &expectation(),
                &wrong,
                Instant::now() + Duration::from_secs(2),
                &AtomicBool::new(false),
            )
            .unwrap();
        app.join().unwrap();
        assert_eq!(observed.state, ReadinessState::Failed);
        assert_eq!(observed.failure, Some(ReadinessFailure::WrongPeer));
        assert!(observed.response.is_none());
        assert!(!observed.authenticated_instance);
    }
    #[test]
    fn actual_owned_socket_refuses_each_wrong_claim_without_calling_it_ready() {
        type MismatchCase = (ReadinessFailure, fn(&mut ReadinessResponse));
        let cases: [MismatchCase; 12] = [
            (ReadinessFailure::WrongLease, |r| {
                r.lease = "wrong-lease".into()
            }),
            (ReadinessFailure::WrongSlot, |r| r.slot = "other".into()),
            (ReadinessFailure::WrongResourceBinding, |r| {
                r.resource_binding_digest = [99; 32]
            }),
            (ReadinessFailure::WrongAction, |r| {
                r.action_digest = [99; 32]
            }),
            (ReadinessFailure::WrongAction, |r| {
                r.start_or_activation_operation = "other-action".into()
            }),
            (ReadinessFailure::WrongNonce, |r| r.nonce = [99; 16]),
            (ReadinessFailure::WrongArtifact, |r| {
                r.artifact_sha256 = [99; 32]
            }),
            (ReadinessFailure::WrongArtifact, |r| r.artifact_length += 1),
            (ReadinessFailure::WrongVersion, |r| {
                r.version_literal = "wrong".into()
            }),
            (ReadinessFailure::WrongData, |r| r.data_sha256 = [99; 32]),
            (ReadinessFailure::WrongConfiguration, |r| {
                r.configuration_digest = [99; 32]
            }),
            (ReadinessFailure::WrongInstance, |r| {
                r.instance = "other-invocation".into()
            }),
        ];
        for (failure, mutate) in cases {
            let (_dir, endpoint, mut channel) = fixture();
            let mut wrong = response();
            mutate(&mut wrong);
            let app = application(endpoint, wrong.clone());
            let observation = channel
                .probe_fixture(
                    &challenge(),
                    &expectation(),
                    &peer(),
                    Instant::now() + Duration::from_secs(2),
                    &AtomicBool::new(false),
                )
                .unwrap();
            app.join().unwrap();
            assert_eq!(observation.state, ReadinessState::Failed, "{failure:?}");
            assert_eq!(observation.failure, Some(failure));
            assert_eq!(observation.response, Some(wrong));
            assert!(observation.peer.is_some());
        }
    }
    #[test]
    fn timeout_retains_original_partial_frame_and_exact_nonce_for_read_only_resume() {
        let (_dir, endpoint, mut channel) = fixture();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let app = std::thread::spawn(move || {
            let mut stream = UnixStream::connect(endpoint.as_str()).unwrap();
            let mut prefix = [0; 4];
            stream.read_exact(&mut prefix).unwrap();
            let mut request = vec![0; u32::from_be_bytes(prefix) as usize];
            stream.read_exact(&mut request).unwrap();
            let body = encode_response(&response()).unwrap();
            stream
                .write_all(&(body.len() as u32).to_be_bytes())
                .unwrap();
            stream.write_all(&body[..15]).unwrap();
            resume_rx.recv().unwrap();
            stream.write_all(&body[15..]).unwrap();
        });
        let observation = channel
            .probe_fixture(
                &challenge(),
                &expectation(),
                &peer(),
                Instant::now() + Duration::from_millis(40),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(observation.state, ReadinessState::Incomplete);
        assert_eq!(observation.failure, Some(ReadinessFailure::Deadline));
        assert!(channel.active.as_ref().unwrap().body_read > 0);
        assert!(channel.take_terminal_probe().is_err());
        let mut replacement = challenge();
        replacement.nonce = [55; 16];
        assert!(
            channel
                .probe_fixture(
                    &replacement,
                    &expectation(),
                    &peer(),
                    Instant::now() + Duration::from_secs(1),
                    &AtomicBool::new(false)
                )
                .is_err()
        );
        resume_tx.send(()).unwrap();
        let observation = channel
            .probe_fixture(
                &challenge(),
                &expectation(),
                &peer(),
                Instant::now() + Duration::from_secs(2),
                &AtomicBool::new(false),
            )
            .unwrap();
        app.join().unwrap();
        assert_eq!(observation.state, ReadinessState::Ready);
        assert!(channel.take_terminal_probe().is_ok());
        assert!(channel.pending_observation().is_none());
    }
    #[test]
    fn cancelled_probe_is_incomplete_and_explicit_abandon_only_settles_local_ipc() {
        let (_dir, endpoint, mut channel) = fixture();
        let observed = channel
            .probe_fixture(
                &challenge(),
                &expectation(),
                &peer(),
                Instant::now() + Duration::from_secs(1),
                &AtomicBool::new(true),
            )
            .unwrap();
        assert_eq!(observed.failure, Some(ReadinessFailure::Cancelled));
        assert_eq!(observed.state, ReadinessState::Incomplete);
        assert!(observed.peer.is_none());
        assert!(channel.abandon_probe().is_some());
        channel.cleanup().unwrap();
        assert!(!Path::new(endpoint.as_str()).exists());
    }
    #[test]
    fn replaced_endpoint_and_preexisting_leaf_are_never_unlinked() {
        let (dir, endpoint, mut channel) = fixture();
        let original = dir.path().join("retained.sock");
        std::fs::rename(endpoint.as_str(), &original).unwrap();
        let replacement = UnixListener::bind(endpoint.as_str()).unwrap();
        assert!(channel.cleanup().is_err());
        assert!(channel.is_revoked());
        drop(channel);
        assert!(Path::new(endpoint.as_str()).exists());
        drop(replacement);
        let dir = tempfile::Builder::new()
            .prefix("th-ready-")
            .tempdir_in(if cfg!(target_os = "macos") {
                "/private/tmp"
            } else {
                "/tmp"
            })
            .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let file = dir.path().join("preexisting");
        std::fs::write(&file, b"untouched").unwrap();
        assert!(
            LocalChallenge::bind(UnixPath::new(file.to_str().unwrap().into()).unwrap()).is_err()
        );
        assert_eq!(std::fs::read(file).unwrap(), b"untouched");
    }
    #[test]
    fn oversized_response_prefix_is_failed_before_allocating_response_body() {
        let (_dir, endpoint, mut channel) = fixture();
        let app = std::thread::spawn(move || {
            let mut stream = UnixStream::connect(endpoint.as_str()).unwrap();
            let mut prefix = [0; 4];
            stream.read_exact(&mut prefix).unwrap();
            let mut request = vec![0; u32::from_be_bytes(prefix) as usize];
            stream.read_exact(&mut request).unwrap();
            stream.write_all(&4097u32.to_be_bytes()).unwrap();
        });
        let observed = channel
            .probe_fixture(
                &challenge(),
                &expectation(),
                &peer(),
                Instant::now() + Duration::from_secs(2),
                &AtomicBool::new(false),
            )
            .unwrap();
        app.join().unwrap();
        assert_eq!(observed.failure, Some(ReadinessFailure::FrameTooLarge));
        assert_eq!(observed.state, ReadinessState::Failed);
        assert!(channel.active.as_ref().unwrap().body.is_empty());
    }
    #[test]
    fn unsupported_protocol_is_a_failed_typed_response() {
        let (_dir, endpoint, mut channel) = fixture();
        let app = std::thread::spawn(move || {
            let mut stream = UnixStream::connect(endpoint.as_str()).unwrap();
            let mut prefix = [0; 4];
            stream.read_exact(&mut prefix).unwrap();
            let mut request = vec![0; u32::from_be_bytes(prefix) as usize];
            stream.read_exact(&mut request).unwrap();
            let response = encode_response(&response()).unwrap();
            let fields = decode_fields(&response, 15).unwrap();
            let mut fields: Vec<String> = fields.into_iter().map(String::from).collect();
            fields[0] = "wrong_protocol".into();
            let bytes = encode_fields(&fields).unwrap();
            stream
                .write_all(&(bytes.len() as u32).to_be_bytes())
                .unwrap();
            stream.write_all(&bytes).unwrap();
        });
        let observed = channel
            .probe_fixture(
                &challenge(),
                &expectation(),
                &peer(),
                Instant::now() + Duration::from_secs(2),
                &AtomicBool::new(false),
            )
            .unwrap();
        app.join().unwrap();
        assert_eq!(observed.state, ReadinessState::Failed);
        assert_eq!(observed.failure, Some(ReadinessFailure::WrongProtocol));
    }
}
