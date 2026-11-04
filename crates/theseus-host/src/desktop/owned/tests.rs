use super::*;
#[cfg(unix)]
use crate::services::readiness::UnixPath;
fn expected_unsettled_descendants() -> DescendantScope {
    if OwnedExecutable::owned_process_tree_available() {
        DescendantScope::Pending
    } else {
        DescendantScope::Unknown
    }
}
fn expected_settled_descendants() -> DescendantScope {
    if OwnedExecutable::owned_process_tree_available() {
        DescendantScope::Joined
    } else {
        DescendantScope::Unknown
    }
}
#[test]
fn required_owned_tree_capability_is_unavailable_on_macos_before_any_spawn() {
    assert_eq!(
        OwnedExecutable::owned_process_tree_available(),
        cfg!(any(target_os = "linux", windows))
    );
    #[cfg(target_os = "macos")]
    assert!(!OwnedExecutable::owned_process_tree_available());
}
use crate::{
    desktop::session::{BrokerEndpoint, SessionAuthority},
    services::readiness::{ArtifactRole, ReadyProtocol},
};

fn binding() -> ExecutableBinding {
    let ready = ReadinessChallenge {
        protocol: ReadyProtocol::TheseusFixtureReadyV1,
        lease: "lease-original".into(),
        slot: "desktop".into(),
        resource_binding_digest: [1; 32],
        start_or_activation_operation: "activate-original".into(),
        action_digest: [2; 32],
        nonce: [3; 16],
        expectation_digest: [4; 32],
    };
    let mut close = ready.clone();
    close.start_or_activation_operation = "close-original".into();
    close.action_digest = [5; 32];
    close.nonce = [6; 16];
    ExecutableBinding {
        ready,
        close,
        close_original_activation_operation: "activate-original".into(),
        expectation: ReadinessExpectation {
            artifact: ArtifactRole::Target,
            artifact_length: 42,
            artifact_sha256: [7; 32],
            version_literal: "source-v1".into(),
            data_sha256: [8; 32],
            configuration_digest: [9; 32],
        },
        cooperative_instance: "instance-original".into(),
        instance_binding_digest: [10; 32],
    }
}
fn next_probe() -> CommittedReadinessProbe {
    let b = binding();
    let mut challenge = b.ready;
    challenge.nonce = [11; 16];
    challenge.expectation_digest = [12; 32];
    let mut expectation = b.expectation;
    expectation.version_literal = "source-v2".into();
    expectation.data_sha256 = [13; 32];
    CommittedReadinessProbe {
        challenge,
        expectation,
    }
}
#[test]
fn fresh_probe_preserves_original_action_and_never_reuses_nonce_or_exceeds_bound() {
    let b = binding();
    let next = next_probe();
    let used = [b.ready.nonce];
    assert!(validate_fresh_probe(&b, &next, &used).is_ok());
    for mutate in [
        (|c: &mut ReadinessChallenge| c.lease = "replacement".into())
            as fn(&mut ReadinessChallenge),
        |c| c.slot = "replacement".into(),
        |c| c.resource_binding_digest = [99; 32],
        |c| c.start_or_activation_operation = "new-activation".into(),
        |c| c.action_digest = [99; 32],
        |c| c.nonce = [0; 16],
        |c| c.nonce = binding().ready.nonce,
        |c| c.nonce = binding().close.nonce,
    ] {
        let mut changed = next.clone();
        mutate(&mut changed.challenge);
        assert!(validate_fresh_probe(&b, &changed, &used).is_err());
    }
    assert!(validate_fresh_probe(&b, &next, &[next.challenge.nonce]).is_err());
    assert!(validate_fresh_probe(&b, &next, &[[33; 16]; 64]).is_err());
}
fn session() -> BoundSession {
    let principal = current_principal().unwrap();
    let kind = if cfg!(target_os = "linux") {
        DesktopKind::Linux
    } else if cfg!(target_os = "macos") {
        DesktopKind::MacOs
    } else {
        DesktopKind::Windows
    };
    #[cfg(unix)]
    let endpoint =
        BrokerEndpoint::Unix(UnixPath::new("/tmp/theseus-source/broker.sock".into()).unwrap());
    #[cfg(windows)]
    let endpoint = BrokerEndpoint::Pipe(
        crate::services::readiness::NamedPipe::new(r"\\.\pipe\theseus.source-owned-broker".into())
            .unwrap(),
    );
    let binding = SessionBinding {
        target_id: "source-target".into(),
        ownership_id: [1; 16],
        provider_epoch: [2; 32],
        session_id: "source-only".into(),
        session_epoch: [3; 32],
        broker_header_digest: [4; 32],
        manifest_digest: [5; 32],
        broker_endpoint: endpoint,
        broker_principal: principal.clone(),
        helper_principal: principal,
        display: SuppliedDisplay::SourceOwnedHeadless,
    };
    BoundSession::admit(
        SessionAuthority {
            kind,
            binding: binding.clone(),
            allowed_modes: vec![DesktopMode::OwnedExecutable],
        },
        &binding,
    )
    .unwrap()
}
#[test]
fn close_requires_distinct_durably_committed_action_and_same_original_activation() {
    let b = binding();
    assert!(b.validate().is_ok());
    let mut b = binding();
    b.close.nonce = b.ready.nonce;
    assert!(b.validate().is_err());
    let mut b = binding();
    b.close = b.ready.clone();
    assert!(b.validate().is_err());
    let mut b = binding();
    b.close_original_activation_operation = "replacement-activation".into();
    assert!(b.validate().is_err());
    let mut b = binding();
    b.close.resource_binding_digest = [99; 32];
    assert!(b.validate().is_err());
    let mut b = binding();
    b.close.lease = "replacement-lease".into();
    assert!(b.validate().is_err());
}
#[test]
fn no_default_environment_or_display_material_can_enter_source_owned_execution() {
    let session = session();
    let mut command = NativeCommand::new(std::env::current_exe().unwrap());
    assert!(validate_command(&session, &command).is_err());
    command.environment = ChildEnvironment::Replace(vec![]);
    assert!(validate_command(&session, &command).is_ok());
    command.environment = ChildEnvironment::Replace(vec![("DISPLAY".into(), ":0".into())]);
    assert!(validate_command(&session, &command).is_err());
    command.environment = ChildEnvironment::Replace(vec![(
        "DBUS_SESSION_BUS_ADDRESS".into(),
        "unix:path=/tmp/unadmitted.sock".into(),
    )]);
    assert!(validate_command(&session, &command).is_err());
    command = NativeCommand::new("unadmitted-relative-executable");
    command.environment = ChildEnvironment::Replace(vec![]);
    assert!(validate_command(&session, &command).is_err());
}

#[cfg(unix)]
fn channels() -> (tempfile::TempDir, LocalChallenge, LocalChallenge) {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::Builder::new()
        .prefix("th-desk-")
        .tempdir_in(if cfg!(target_os = "macos") {
            "/private/tmp"
        } else {
            "/tmp"
        })
        .unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let ready = LocalChallenge::bind(
        UnixPath::new(dir.path().join("ready.sock").to_str().unwrap().into()).unwrap(),
    )
    .unwrap();
    let close = LocalChallenge::bind(
        UnixPath::new(dir.path().join("close.sock").to_str().unwrap().into()).unwrap(),
    )
    .unwrap();
    (dir, ready, close)
}
#[cfg(windows)]
fn channels() -> (tempfile::TempDir, LocalChallenge, LocalChallenge) {
    use crate::services::readiness::{NamedPipe, Sid};
    let dir = tempfile::tempdir().unwrap();
    let CurrentPrincipal::Windows { token_user_sid } = current_principal().unwrap() else {
        panic!("Windows fixture principal")
    };
    let sid = Sid::new(token_user_sid).unwrap();
    let mut random = [0; 16];
    crate::random_bytes(&mut random).unwrap();
    let nonce = random
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect::<String>();
    let ready = LocalChallenge::bind(
        NamedPipe::new(format!(r"\\.\pipe\theseus.desktop-ready-{nonce}")).unwrap(),
        sid.clone(),
    )
    .unwrap();
    let close = LocalChallenge::bind(
        NamedPipe::new(format!(r"\\.\pipe\theseus.desktop-close-{nonce}")).unwrap(),
        sid,
    )
    .unwrap();
    (dir, ready, close)
}
#[test]
fn failed_pre_effect_validation_retains_and_explicitly_settles_original_channels() {
    let (_dir, ready, close) = channels();
    let command = NativeCommand::new(std::env::current_exe().unwrap()); // Inherit must refuse.
    let mut failure =
        OwnedExecutable::spawn(session(), &command, binding(), ready, close).unwrap_err();
    assert!(failure.owner.child.is_none());
    assert!(failure.owner.ready_channel.is_some());
    assert!(failure.owner.close_channel.is_some());
    assert!(failure.owner.try_cleanup().unwrap());
    assert!(failure.owner.observation().local_cleanup_complete);
    assert!(!failure.owner.observation().native_activation_available);
    assert_eq!(
        failure.owner.observation().evidence,
        ExecutableEvidence::SourceOwnedExecutable
    );
}

const FIXTURE_TEST: &str = "desktop::owned::tests::owned_fixture_process";
fn fixture_command(role: &str, ready: Option<&str>, close: Option<&str>) -> NativeCommand {
    let mut command = NativeCommand::new(std::env::current_exe().unwrap());
    command.args(["--exact", FIXTURE_TEST, "--nocapture"]);
    let mut entries = vec![("THESEUS_DESKTOP_SOURCE_CHILD".into(), role.into())];
    if let Some(endpoint) = ready {
        entries.push(("THESEUS_DESKTOP_READY".into(), endpoint.into()));
    }
    if let Some(endpoint) = close {
        entries.push(("THESEUS_DESKTOP_CLOSE".into(), endpoint.into()));
    }
    command.environment = ChildEnvironment::Replace(entries);
    command
}
trait FixtureStream: std::io::Read + std::io::Write {}
impl<T: std::io::Read + std::io::Write> FixtureStream for T {}
fn connect_fixture(endpoint: &str) -> std::io::Result<Box<dyn FixtureStream>> {
    #[cfg(unix)]
    return Ok(Box::new(std::os::unix::net::UnixStream::connect(endpoint)?));
    #[cfg(windows)]
    {
        let until = Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(endpoint)
            {
                Ok(file) => return Ok(Box::new(file)),
                Err(error) if error.raw_os_error() == Some(231) && Instant::now() < until => {
                    std::thread::sleep(std::time::Duration::from_millis(2))
                }
                Err(error) => return Err(error),
            }
        }
    }
}
fn fixture_exchange(endpoint: &str, close: bool, mode: &str) -> io::Result<()> {
    use crate::services::readiness::{ReadinessResponse, decode_challenge, encode_response};
    use std::io::{Read, Write};
    let mut stream = connect_fixture(endpoint)?;
    let mut prefix = [0; 4];
    stream.read_exact(&mut prefix)?;
    let size = u32::from_be_bytes(prefix) as usize;
    assert!(size <= 4096);
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes)?;
    let c = decode_challenge(&bytes)?;
    let b = binding();
    let fresh = next_probe();
    let expected = if close {
        &b.close
    } else if mode == "fresh-second" {
        &fresh.challenge
    } else {
        &b.ready
    };
    assert_eq!(&c, expected);
    if close && mode == "stalled-close" {
        std::thread::sleep(std::time::Duration::from_secs(5));
        return Ok(());
    }
    let e = if mode == "fresh-second" {
        &fresh.expectation
    } else {
        &b.expectation
    };
    let mut response = ReadinessResponse {
        protocol: c.protocol,
        lease: c.lease,
        slot: c.slot,
        resource_binding_digest: c.resource_binding_digest,
        start_or_activation_operation: c.start_or_activation_operation,
        action_digest: c.action_digest,
        nonce: c.nonce,
        artifact_length: e.artifact_length,
        artifact_sha256: e.artifact_sha256,
        version_literal: e.version_literal.clone(),
        data_sha256: e.data_sha256,
        configuration_digest: e.configuration_digest,
        instance: b.cooperative_instance,
        initialized: true,
    };
    if !close && mode == "wrong-nonce" {
        response.nonce = [99; 16]
    }
    if !close && mode == "wrong-data" {
        response.data_sha256 = [99; 32]
    }
    let bytes = encode_response(&response)?;
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(&bytes)?;
    stream.flush()?;
    Ok(())
}
// Intentionally let the fixture primary exit before this child. The isolated
// Linux broker subreaper consumes the actual adopted child wait; other native
// workers prove their original group/job joined. Waiting here would invalidate
// the primary-exit/lingering-descendant condition this fixture must exercise.
#[allow(clippy::zombie_processes)]
fn fixture_spawn_descendant(role: &str, ready: Option<&str>) {
    let command = fixture_command(role, ready, None);
    let ChildEnvironment::Replace(env) = command.environment else {
        unreachable!()
    };
    // Fixture-only descendant creation. It inherits the original owned group/job;
    // no PID is opened, signalled, or treated as a lifetime owner.
    let _descendant = std::process::Command::new(command.program)
        .args(command.args)
        .env_clear()
        .envs(env)
        .spawn()
        .unwrap();
}
#[cfg(unix)]
// This exact fixture deliberately orphans its escaped child to the isolated
// dedicated broker subreaper on Linux. On macOS the descendant remains Unknown
// and only a fixture sentinel requests its cooperative exit. Waiting here would
// remove the escaped-live-descendant condition that the regression requires.
#[allow(clippy::zombie_processes)]
fn fixture_spawn_escaped_descendant() {
    use std::io::BufRead;
    let mut command = fixture_command("escaped-descendant", None, None);
    let ChildEnvironment::Replace(ref mut env) = command.environment else {
        unreachable!()
    };
    env.push((
        "THESEUS_DESKTOP_RELEASE".into(),
        std::env::var_os("THESEUS_DESKTOP_RELEASE").unwrap(),
    ));
    let mut descendant = std::process::Command::new(command.program)
        .args(command.args)
        .env_clear()
        .envs(env.iter().cloned())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Wait for the actual setsid receipt before allowing primary close.
    let mut line = String::new();
    let mut output = std::io::BufReader::new(descendant.stdout.take().unwrap());
    loop {
        line.clear();
        assert!(output.read_line(&mut line).unwrap() > 0);
        if line.ends_with("escaped\n") {
            break;
        }
    }
}
#[test]
fn owned_fixture_process() {
    let Ok(role) = std::env::var("THESEUS_DESKTOP_SOURCE_CHILD") else {
        return;
    };
    if role == "descendant" {
        std::thread::sleep(std::time::Duration::from_secs(5));
        return;
    }
    #[cfg(unix)]
    if role == "escaped-descendant" {
        use std::io::Write;
        assert!(unsafe { libc::setsid() } > 0);
        std::io::stdout().write_all(b"escaped\n").unwrap();
        std::io::stdout().flush().unwrap();
        let release = std::env::var_os("THESEUS_DESKTOP_RELEASE").unwrap();
        let until = Instant::now() + std::time::Duration::from_secs(8);
        while !std::path::Path::new(&release).exists() {
            assert!(Instant::now() < until);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        // Fixture-reported cooperative completion, never native tree proof.
        std::fs::write(
            std::path::Path::new(&release).with_extension("exited"),
            b"fixture-exit",
        )
        .unwrap();
        std::process::exit(0);
    }
    if role == "wrong-peer-client" {
        let _ = fixture_exchange(
            &std::env::var("THESEUS_DESKTOP_READY").unwrap(),
            false,
            "normal",
        );
        return;
    }
    if let Some(mode) = role.strip_prefix("worker-") {
        actual_lifetime(mode);
        return;
    }
    let mode = role.strip_prefix("app-").unwrap();
    if mode == "backpressure" {
        use std::io::Write;
        // Deliberately broken fixture: exceed any supported action budget and
        // block on its original owned stdout before readiness can connect.
        std::io::stdout().write_all(&[b'x'; 128 * 1024]).unwrap();
        std::io::stdout().flush().unwrap();
    }
    let ready = std::env::var("THESEUS_DESKTOP_READY").unwrap();
    if mode == "wrong-peer" {
        fixture_spawn_descendant("wrong-peer-client", Some(&ready));
    } else {
        fixture_exchange(&ready, false, mode).unwrap();
    }
    if mode == "fresh-ready" {
        fixture_exchange(&ready, false, "fresh-second").unwrap();
    }
    if mode == "lingering-child" {
        fixture_spawn_descendant("descendant", None);
    }
    #[cfg(unix)]
    if mode == "escaped-child" {
        fixture_spawn_escaped_descendant();
    }
    fixture_exchange(&std::env::var("THESEUS_DESKTOP_CLOSE").unwrap(), true, mode).unwrap();
}
fn actual_lifetime(mode: &str) {
    use crate::services::readiness::{ReadinessFailure, ReadinessState};
    use std::time::Duration;
    #[cfg(target_os = "linux")]
    // SAFETY: This exact test entry is an original owned isolated subprocess,
    // has default SIGCHLD/no preexisting children, and runs one activation.
    let scope =
        unsafe { crate::desktop::scope::DedicatedBrokerScope::establish_for_exclusive_process() }
            .unwrap();
    let (_dir, ready, close) = channels();
    let mut command = fixture_command(
        &format!("app-{mode}"),
        Some(ready.endpoint().as_str()),
        Some(close.endpoint().as_str()),
    );
    let release = _dir.path().join("release");
    if mode == "escaped-child" {
        let ChildEnvironment::Replace(ref mut env) = command.environment else {
            unreachable!()
        };
        env.push((
            "THESEUS_DESKTOP_RELEASE".into(),
            release.as_os_str().to_owned(),
        ));
    }
    #[cfg(target_os = "linux")]
    let mut owner = OwnedExecutable::spawn_in_dedicated_broker(
        scope,
        session(),
        &command,
        binding(),
        ready,
        close,
    )
    .unwrap();
    #[cfg(not(target_os = "linux"))]
    let mut owner = OwnedExecutable::spawn(session(), &command, binding(), ready, close).unwrap();
    let cancel = AtomicBool::new(false);
    if mode == "backpressure" {
        let pending = owner
            .probe_ready(Instant::now() + Duration::from_millis(80), &cancel)
            .unwrap();
        assert_eq!(pending.state, ReadinessState::Incomplete);
        assert_eq!(pending.failure, Some(ReadinessFailure::Deadline));
        let mut drained = 0;
        let mut chunk = [0; 1024];
        let until = Instant::now() + Duration::from_secs(2);
        while drained < 16384 {
            let count = chunk.len().min(16384 - drained);
            match owner.try_stdout(&mut chunk[..count]) {
                Ok(n) => drained += n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => panic!("owned backpressure fixture: {e}"),
            }
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(drained, 16384);
        assert!(owner.stdout.is_some());
        assert!(owner.child.is_some());
        let cancelled = owner
            .probe_ready(
                Instant::now() + Duration::from_secs(1),
                &AtomicBool::new(true),
            )
            .unwrap();
        assert_eq!(cancelled.failure, Some(ReadinessFailure::Cancelled));
        settle_original(&mut owner);
        assert_eq!(
            owner.observation().descendants,
            expected_settled_descendants()
        );
        return;
    }
    if mode == "cancelled" {
        let pending = owner
            .probe_ready(
                Instant::now() + Duration::from_secs(1),
                &AtomicBool::new(true),
            )
            .unwrap();
        assert_eq!(pending.state, ReadinessState::Incomplete);
        assert_eq!(pending.failure, Some(ReadinessFailure::Cancelled));
        assert!(
            owner
                .ready_channel
                .as_ref()
                .unwrap()
                .pending_observation()
                .is_some()
        );
        let original = owner.current_ready_probe().clone();
        assert!(owner.begin_fresh_ready_probe(next_probe()).is_err());
        assert_eq!(owner.current_ready_probe(), &original);
        // The same owner/binding resumes the exact original nonce and stream.
    }
    let ready = owner
        .probe_ready(Instant::now() + Duration::from_secs(2), &cancel)
        .unwrap();
    match mode {
        "wrong-nonce" => assert_eq!(ready.failure, Some(ReadinessFailure::WrongNonce)),
        "wrong-data" => {
            assert_eq!(ready.failure, Some(ReadinessFailure::WrongData));
            assert!(ready.authenticated_instance)
        }
        "wrong-peer" => {
            assert_eq!(ready.failure, Some(ReadinessFailure::WrongPeer));
            assert!(!ready.authenticated_instance)
        }
        _ => {
            assert_eq!(ready.state, ReadinessState::Ready);
            assert!(ready.authenticated_instance)
        }
    }
    if mode == "fresh-ready" {
        let original = owner.current_ready_probe().clone();
        let mut wrong = next_probe();
        wrong.challenge.start_or_activation_operation = "replacement".into();
        assert!(owner.begin_fresh_ready_probe(wrong).is_err());
        assert_eq!(owner.current_ready_probe(), &original);
        let next = next_probe();
        let terminal = owner.begin_fresh_ready_probe(next.clone()).unwrap();
        assert_eq!(terminal.probe, original);
        assert_eq!(terminal.observation, ready);
        assert_eq!(owner.current_ready_probe(), &next);
        // Transition returns only the OLD sample. A real second IPC exchange
        // must independently supply readiness for the freshly published nonce.
        let fresh = owner
            .probe_ready(Instant::now() + Duration::from_secs(2), &cancel)
            .unwrap();
        assert_eq!(fresh.state, ReadinessState::Ready);
        assert!(fresh.authenticated_instance);
        let response = fresh.response.unwrap();
        assert_eq!(response.nonce, next.challenge.nonce);
        assert_eq!(response.version_literal, next.expectation.version_literal);
        assert_eq!(response.data_sha256, next.expectation.data_sha256);
        assert!(owner.begin_fresh_ready_probe(next).is_err());
    }
    let close = owner
        .request_close(
            Instant::now()
                + if mode == "stalled-close" {
                    Duration::from_millis(80)
                } else {
                    Duration::from_secs(2)
                },
            &cancel,
        )
        .unwrap();
    if mode == "stalled-close" {
        assert_eq!(close.state, ReadinessState::Incomplete);
        assert_eq!(close.failure, Some(ReadinessFailure::Deadline));
        assert!(
            owner
                .close_channel
                .as_ref()
                .unwrap()
                .pending_observation()
                .is_some()
        );
        assert!(!owner.observe_primary().unwrap());
    } else {
        assert_eq!(close.state, ReadinessState::Ready);
        let until = Instant::now() + Duration::from_secs(2);
        while !owner.observe_primary().unwrap() {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(owner.observation().primary_exited);
        assert_eq!(
            owner.observation().descendants,
            expected_unsettled_descendants()
        );
    }
    assert_eq!(
        owner.observation().evidence,
        ExecutableEvidence::SourceOwnedExecutable
    );
    assert!(!owner.observation().native_activation_available);
    #[cfg(target_os = "linux")]
    if mode == "escaped-child" {
        assert!(!owner.try_cleanup().unwrap());
        assert!(owner.observation().primary_exited);
        assert_eq!(owner.observation().descendants, DescendantScope::Pending);
        assert!(!owner.observation().local_cleanup_complete);
        std::fs::write(release, b"fixture-release").unwrap();
    }
    #[cfg(target_os = "macos")]
    if mode == "escaped-child" {
        assert!(owner.try_cleanup().unwrap());
        assert!(owner.observation().primary_exited);
        assert_eq!(owner.observation().descendants, DescendantScope::Unknown);
        assert!(owner.observation().local_cleanup_complete);
        assert!(!release.with_extension("exited").exists());
        std::fs::write(&release, b"fixture-release").unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        while !release.with_extension("exited").exists() {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(owner.observation().descendants, DescendantScope::Unknown);
    }
    settle_original(&mut owner);
    assert_eq!(
        owner.observation().descendants,
        expected_settled_descendants()
    );
    assert!(owner.observation().local_cleanup_complete);
    #[cfg(target_os = "linux")]
    if mode == "lingering-child" || mode == "escaped-child" {
        assert!(owner.scope.as_ref().unwrap().reaped_descendants() >= 1);
    }
}
fn settle_original(owner: &mut OwnedExecutable) {
    use std::time::Duration;
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        match owner.try_cleanup() {
            Ok(true) => break,
            Ok(false) => {}
            Err(_) => {
                // Native cancellation may still own OVERLAPPED buffers. A
                // retained error is independent of eventual original cleanup.
                assert!(!owner.retained_errors().is_empty());
                assert!(!owner.observation().local_cleanup_complete);
            }
        }
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn desktop_owned_executable_lifetime_is_actual_and_distinct_from_native_activation() {
    use std::time::Duration;
    let mut modes = vec![
        "normal",
        "wrong-nonce",
        "wrong-data",
        "wrong-peer",
        "stalled-close",
        "lingering-child",
        "cancelled",
        "backpressure",
        "fresh-ready",
    ];
    if cfg!(unix) {
        modes.push("escaped-child")
    }
    for mode in modes {
        let command = fixture_command(&format!("worker-{mode}"), None, None);
        let mut acquired = OwnedChild::spawn(&command, InputMode::Null).unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        let mut output = Vec::new();
        loop {
            let mut bytes = [0; 2048];
            for reader in [&mut acquired.stdout, &mut acquired.stderr] {
                match reader.try_read(&mut bytes) {
                    Ok(n) => {
                        if output.len() < 16384 {
                            output.extend_from_slice(&bytes[..n]);
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => panic!("{mode}: {e}"),
                }
            }
            if acquired.child.exited_reserved().unwrap() {
                break;
            }
            assert!(
                Instant::now() < until,
                "{mode}: {}",
                String::from_utf8_lossy(&output)
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let status = acquired.child.reap_and_release().unwrap();
        assert!(
            status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&output)
        );
    }
}
