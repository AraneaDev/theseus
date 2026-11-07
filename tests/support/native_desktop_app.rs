//! Compiled source desktop fixture. No service-manager or display environment.
use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::Path,
    sync::{Arc, OnceLock, mpsc},
    time::{Duration, Instant},
};
use theseus_host::services::readiness::{
    MAX_FRAME_BYTES, ReadinessChallenge, ReadinessResponse, decode_challenge, encode_response,
};

trait Stream: Read + Write {}
impl<T: Read + Write> Stream for T {}

fn connect(endpoint: &str) -> std::io::Result<Box<dyn Stream>> {
    #[cfg(unix)]
    {
        let stream = std::os::unix::net::UnixStream::connect(endpoint)?;
        // Keep the SAME established connection until its original challenge or
        // controller close/cancel. The process already has the bounded90s lifetime
        // below; a perconnection timeout leaves stale peers in the broker backlog.
        Ok(Box::new(stream))
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // This fixture may run under a more privileged explicitly supplied
        // account. Its endpoint must not acquire impersonation authority.
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(0x0010_0000 | 0x0001_0000)
            .open(endpoint)
            .map(|stream| Box::new(stream) as Box<dyn Stream>)
    }
}

fn response(
    c: ReadinessChallenge,
    version: &str,
    artifact: &[u8],
    data: &[u8],
    instance: &str,
) -> ReadinessResponse {
    let data_sha256 = Sha256::digest(data).into();
    ReadinessResponse {
        protocol: c.protocol,
        lease: c.lease,
        slot: c.slot,
        resource_binding_digest: c.resource_binding_digest,
        start_or_activation_operation: c.start_or_activation_operation,
        action_digest: c.action_digest,
        nonce: c.nonce,
        artifact_length: artifact.len() as u64,
        artifact_sha256: Sha256::digest(artifact).into(),
        version_literal: format!("{version}\n"),
        data_sha256,
        configuration_digest: data_sha256,
        instance: instance.into(),
        initialized: data == b"theseus fixture data\n",
    }
}

fn instance_for_probe(
    action_digest: [u8; 32],
    close: bool,
    original: &OnceLock<String>,
) -> std::io::Result<String> {
    let digest = action_digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if !close {
        let prior = original.get_or_init(|| digest.clone());
        if prior != &digest {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "readiness cannot replace the original activation instance",
            ));
        }
    }
    original.get().cloned().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "no original readiness instance for cooperative close",
        )
    })
}

enum ExchangeAttempt {
    NotConnected,
    Completed,
}

fn exchange(
    endpoint: &str,
    data: &Path,
    artifact: &[u8],
    version: &str,
    original: &OnceLock<String>,
    close: bool,
) -> Result<ExchangeAttempt> {
    let mut stream = match connect(endpoint) {
        Ok(stream) => stream,
        Err(_) => return Ok(ExchangeAttempt::NotConnected),
    };
    let mut prefix = [0; 4];
    stream.read_exact(&mut prefix)?;
    let length = u32::from_be_bytes(prefix) as usize;
    ensure!(
        (1..=MAX_FRAME_BYTES).contains(&length),
        "desktop challenge exceeds bound"
    );
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    let challenge = decode_challenge(&bytes)?;
    let instance = instance_for_probe(challenge.action_digest, close, original)?;
    let current_data = std::fs::read(data)?;
    let bytes = encode_response(&response(
        challenge,
        version,
        artifact,
        &current_data,
        &instance,
    ))?;
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(&bytes)?;
    stream.flush()?;
    Ok(ExchangeAttempt::Completed)
}

fn validate_arguments(args: &[String]) -> Result<()> {
    ensure!(
        matches!(args.len(), 4 | 7),
        "desktop requires legacy ready/close/data/marker or exact static bootstrap tokens"
    );
    if args.len() == 7 {
        ensure!(
            args[4] == "--theseus-bootstrap=theseus_fixture_ready_bootstrap_v1"
                && args[5] == format!("--theseus-ready={}", args[0])
                && args[6] == format!("--theseus-close={}", args[1]),
            "native bootstrap argv differs from exact committed endpoints/closed protocol"
        );
    }
    // The fourth legacy value is an inert static marker, never instance authority.
    // Only the first authenticated READY action digest fixes the original instance.
    ensure!(
        args[0] != args[1] && !args[3].is_empty(),
        "distinct channels and nonempty static marker required"
    );
    ensure!(
        Path::new(&args[2]).is_absolute(),
        "absolute data leaf required"
    );
    #[cfg(unix)]
    ensure!(
        Path::new(&args[0]).is_absolute() && Path::new(&args[1]).is_absolute(),
        "absolute desktop endpoints required"
    );
    #[cfg(windows)]
    ensure!(
        args[0].starts_with(r"\\.\pipe\") && args[1].starts_with(r"\\.\pipe\"),
        "local named-pipe endpoints required"
    );
    Ok(())
}

pub fn run(version: &str, args: &[String]) -> Result<()> {
    validate_arguments(args)?;
    let artifact = std::fs::read(std::env::current_exe()?)?;
    let (finished, outcome) = mpsc::channel();
    let original_instance = Arc::new(OnceLock::new());
    // Threads own only this process's original channel handles. The bounded
    // fixture lifetime exits the process on timeout; no detached child exists.
    for (endpoint, close) in [(args[0].clone(), false), (args[1].clone(), true)] {
        let data = args[2].clone();
        let original_instance = Arc::clone(&original_instance);
        let artifact = artifact.clone();
        let version = version.to_owned();
        let finished = finished.clone();
        std::thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(90);
            loop {
                let result = exchange(
                    &endpoint,
                    Path::new(&data),
                    &artifact,
                    &version,
                    &original_instance,
                    close,
                );
                match result {
                    Ok(ExchangeAttempt::Completed) if close => {
                        let _ = finished.send(());
                        return;
                    }
                    // Once connected, an ended original exchange is never
                    // retried on a replacement peer/nonce. READY success permits
                    // only the next separately committed readiness challenge.
                    Err(_) => return,
                    _ => {}
                }
                if Instant::now() >= until {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
    }
    drop(finished);
    outcome.recv_timeout(Duration::from_secs(90))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn static_arguments() -> Vec<String> {
        #[cfg(unix)]
        let args = [
            "/fixture/ready.sock",
            "/fixture/close.sock",
            "/fixture/data",
            "static-marker",
        ];
        #[cfg(windows)]
        let args = [
            r"\\.\pipe\ready",
            r"\\.\pipe\close",
            r"C:\fixture\data",
            "static-marker",
        ];
        args.into_iter().map(str::to_owned).collect()
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn desktop_close_keeps_original_connection_during_idle_before_challenge() {
        use std::sync::atomic::AtomicBool;
        use theseus_host::{
            principal::{CurrentPrincipal, current_principal},
            services::readiness::{
                ArtifactRole, LocalChallenge, PeerExpectation, ReadinessExpectation,
                ReadinessState, ReadyProtocol, UnixPath,
            },
        };
        let temp = tempfile::Builder::new()
            .prefix("th-close-idle-")
            .tempdir_in("/tmp")
            .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let endpoint =
            UnixPath::new(temp.path().join("close.sock").to_str().unwrap().into()).unwrap();
        let mut channel = LocalChallenge::bind(endpoint.clone()).unwrap();
        let artifact = b"compiled-test-artifact";
        let data = b"theseus fixture data\n";
        let expectation = ReadinessExpectation {
            artifact: ArtifactRole::Target,
            artifact_length: artifact.len() as u64,
            artifact_sha256: Sha256::digest(artifact).into(),
            version_literal: "target\n".into(),
            data_sha256: Sha256::digest(data).into(),
            configuration_digest: Sha256::digest(data).into(),
        };
        let challenge = ReadinessChallenge {
            protocol: ReadyProtocol::TheseusFixtureReadyV1,
            lease: "11".repeat(16),
            slot: "desktop".into(),
            resource_binding_digest: [2; 32],
            start_or_activation_operation: "original-close".into(),
            action_digest: [3; 32],
            nonce: [4; 16],
            expectation_digest: [5; 32],
        };
        let CurrentPrincipal::Posix {
            effective_uid,
            effective_gid,
        } = current_principal().unwrap()
        else {
            panic!("POSIX test principal required")
        };
        let peer = PeerExpectation {
            uid: effective_uid,
            gid: effective_gid,
            cooperative_instance: "01".repeat(32),
            instance_binding_digest: [6; 32],
        };
        let (connected_tx, connected_rx) = mpsc::channel();
        let (outcome_tx, outcome_rx) = mpsc::channel();
        let path = endpoint.as_str().to_owned();
        let app = std::thread::spawn(move || {
            let result = (|| -> Result<()> {
                let mut stream = connect(&path)?;
                connected_tx.send(())?;
                let mut prefix = [0; 4];
                stream.read_exact(&mut prefix)?;
                let mut request = vec![0; u32::from_be_bytes(prefix) as usize];
                stream.read_exact(&mut request)?;
                let reply = encode_response(&response(
                    decode_challenge(&request)?,
                    "target",
                    artifact,
                    data,
                    &"01".repeat(32),
                ))?;
                stream.write_all(&(reply.len() as u32).to_be_bytes())?;
                stream.write_all(&reply)?;
                stream.flush()?;
                Ok(())
            })();
            outcome_tx
                .send(result.map_err(|error| format!("{error:#}")))
                .unwrap();
        });
        connected_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let original_deadline = Instant::now() + Duration::from_secs(5);
        let cancel = AtomicBool::new(false);
        // The app may precede combined service stop/work by more than two seconds.
        // It must keep this SAME connection; no new probe/peer/nonce/deadline retry.
        if let Ok(early) = outcome_rx.recv_timeout(Duration::from_millis(2300)) {
            let observed = channel
                .poll_original(
                    &challenge,
                    &expectation,
                    &peer,
                    std::process::id(),
                    original_deadline,
                    &cancel,
                )
                .unwrap();
            app.join().unwrap();
            panic!(
                "fixture abandoned original close before challenge: {early:?}; original channel {observed:?}"
            );
        }
        let observed = loop {
            if let Some(observed) = channel
                .poll_original(
                    &challenge,
                    &expectation,
                    &peer,
                    std::process::id(),
                    original_deadline,
                    &cancel,
                )
                .unwrap()
            {
                break observed;
            }
            assert!(Instant::now() < original_deadline);
            std::thread::yield_now();
        };
        app.join().unwrap();
        assert!(outcome_rx.recv().unwrap().is_ok());
        assert_eq!(observed.state, ReadinessState::Ready);
        assert!(observed.authenticated_instance);
        assert_eq!(observed.peer.unwrap().pid, std::process::id());
    }
    #[test]
    fn desktop_static_arguments_accept_legacy_or_exact_closed_bootstrap() {
        let mut args = static_arguments();
        validate_arguments(&args).unwrap();
        args.extend([
            "--theseus-bootstrap=theseus_fixture_ready_bootstrap_v1".into(),
            format!("--theseus-ready={}", args[0]),
            format!("--theseus-close={}", args[1]),
        ]);
        validate_arguments(&args).unwrap();
        for extra in [
            "--unknown=value",
            "--theseus-nonce=per-action",
            "--theseus-instance=per-action",
            "--theseus-ready=/different",
        ] {
            let mut wrong = args.clone();
            wrong[4] = extra.into();
            assert!(validate_arguments(&wrong).is_err());
            let mut duplicate = args.clone();
            duplicate.push(extra.into());
            assert!(validate_arguments(&duplicate).is_err());
        }
        let mut swapped = args.clone();
        swapped.swap(5, 6);
        assert!(validate_arguments(&swapped).is_err());
        let mut mismatch = args;
        mismatch[5] = "--theseus-ready=/different".into();
        assert!(validate_arguments(&mismatch).is_err());
    }
    #[test]
    fn desktop_close_keeps_original_activation_instance_and_refuses_before_readiness() {
        let original = OnceLock::new();
        assert!(instance_for_probe([2; 32], true, &original).is_err());
        let activation = instance_for_probe([1; 32], false, &original).unwrap();
        assert_eq!(activation, "01".repeat(32));
        assert_eq!(
            instance_for_probe([2; 32], true, &original).unwrap(),
            activation
        );
        assert_eq!(
            instance_for_probe([1; 32], false, &original).unwrap(),
            activation
        );
        assert!(instance_for_probe([3; 32], false, &original).is_err());
        assert_eq!(original.get().unwrap(), &activation);
    }
    #[test]
    fn desktop_response_measures_image_and_data_instead_of_echoing_expectations() {
        use theseus_host::services::readiness::ReadyProtocol;
        let challenge = ReadinessChallenge {
            protocol: ReadyProtocol::TheseusFixtureReadyV1,
            lease: "lease".into(),
            slot: "desktop".into(),
            resource_binding_digest: [1; 32],
            start_or_activation_operation: "activation".into(),
            action_digest: [2; 32],
            nonce: [3; 16],
            expectation_digest: [4; 32],
        };
        let observed = response(
            challenge.clone(),
            "target",
            b"actual-image",
            b"wrong-data",
            "original",
        );
        let base = response(
            challenge.clone(),
            "base",
            b"actual-base-image",
            b"theseus fixture data\n",
            "original-base",
        );
        assert_eq!(base.version_literal, "base\n");
        assert_eq!(
            base.artifact_sha256,
            <[u8; 32]>::from(Sha256::digest(b"actual-base-image"))
        );
        assert_eq!(base.instance, "original-base");
        assert_eq!(observed.nonce, challenge.nonce);
        assert_eq!(observed.artifact_length, 12);
        assert_eq!(
            observed.artifact_sha256,
            <[u8; 32]>::from(Sha256::digest(b"actual-image"))
        );
        assert_eq!(
            observed.data_sha256,
            <[u8; 32]>::from(Sha256::digest(b"wrong-data"))
        );
        assert!(!observed.initialized);
        assert!(
            response(
                challenge,
                "target",
                b"actual-image",
                b"theseus fixture data\n",
                "original"
            )
            .initialized
        );
    }
}
