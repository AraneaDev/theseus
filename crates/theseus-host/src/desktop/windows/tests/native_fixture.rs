//! SUT-side inert shortcut installer fixture. It never activates the shortcut.
use super::*;
use sha2::{Digest, Sha256};
use std::{fs, path::Path};
/// Author `entry` under `root` exactly as a real installer does: Shell COM
/// IShellLinkW setters plus IPersistFile::Save (which writes the target
/// IDList that IShellLinkW::GetPath reports). Returns the saved bytes.
fn author_link(root: &Path, entry: &str, target: &str, cwd: &str, args: &str) -> Vec<u8> {
    let apartment = ComApartment::initialize_original_thread().unwrap();
    let path = root.join(entry);
    super::super::com::author_shortcut_for_test(
        &apartment,
        path.to_str().unwrap(),
        target,
        args,
        cwd,
    )
    .unwrap();
    fs::read(path).unwrap()
}
/// Bounded structural description of saved shortcut bytes, used only when
/// admission refuses, so the real Shell-authored layout is visible in CI.
fn describe_link_bytes(bytes: &[u8]) -> String {
    let u16at = |i: usize| {
        bytes
            .get(i..i + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]) as usize)
    };
    let u32at = |i: usize| {
        bytes
            .get(i..i + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let mut out = format!(
        "len={} flags={:#x?} attributes={:#x?} show={:?} hotkey={:?} validator={:?}",
        bytes.len(),
        u32at(20),
        u32at(24),
        u32at(60),
        u16at(64),
        validate_link_bytes(bytes).map_err(|e| e.to_string()),
    );
    let flags = u32at(20).unwrap_or(0);
    let mut offset = 76;
    if flags & 1 != 0 {
        let size = u16at(offset).unwrap_or(0);
        out += &format!(" idlist={size}");
        offset += 2 + size;
    }
    if flags & 2 != 0 {
        let size = u32at(offset).unwrap_or(0) as usize;
        out += &format!(
            " linkinfo(size={size} header={:?} flags={:?})",
            u32at(offset + 4),
            u32at(offset + 8)
        );
        offset += size.max(4);
    }
    for (flag, name) in [
        (4, "name"),
        (8, "relative"),
        (0x10, "cwd"),
        (0x20, "args"),
        (0x40, "icon"),
    ] {
        if flags & flag != 0 {
            let count = u16at(offset).unwrap_or(0);
            out += &format!(" {name}={count}");
            offset += 2 + count * 2;
        }
    }
    out += " extra=[";
    for _ in 0..32 {
        match u32at(offset) {
            Some(size) if size >= 8 => {
                out += &format!("{:#x}:{size} ", u32at(offset + 4).unwrap_or(0));
                offset += size as usize;
            }
            terminal => {
                out += &format!("end:{terminal:?} at {offset}");
                break;
            }
        }
    }
    out += "] hex=";
    for byte in bytes.iter().take(8192) {
        out += &format!("{byte:02x}");
    }
    out
}
fn commitment(data: &[u8]) -> ArtifactCommitment {
    ArtifactCommitment {
        length: data.len() as u64,
        sha256: Sha256::digest(data).into(),
    }
}
/// Mandatory native source input: compiled desktop fixture with an embedded
/// asInvoker/uiAccess=false manifest. Absence is failure, never skipped proof.
fn fixture(root: &Path) -> ShortcutProfile {
    let supplied=std::env::var_os("THESEUS_WINDOWS_DESKTOP_FIXTURE_EXE").expect("native Windows source CI must supply THESEUS_WINDOWS_DESKTOP_FIXTURE_EXE with embedded asInvoker/no-uiAccess manifest");
    let executable = fs::read(supplied).unwrap();
    fs::write(root.join("fixture.exe"), &executable).unwrap();
    let root = root.to_str().unwrap().to_owned();
    let args = vec!["--theseus-inert-fixture".into(), "original nonce".into()];
    let bytes = author_link(
        Path::new(&root),
        "app.lnk",
        &format!("{root}\\fixture.exe"),
        &root,
        &command_line(&args),
    );
    ShortcutProfile {
        root,
        entry: "app.lnk".into(),
        executable: "fixture.exe".into(),
        arguments: args,
        entry_artifact: commitment(&bytes),
        executable_artifact: commitment(&executable),
    }
}
/// The closed profile admits only canonical long-name drive roots. Runner
/// `%TEMP%` may be an 8.3 alias (for example `C:\Users\RUNNER~1\...`),
/// which the profile correctly refuses, so the fixture root is created under
/// the filesystem-resolved long form of the temporary directory.
fn long_name_tempdir() -> tempfile::TempDir {
    let resolved = fs::canonicalize(std::env::temp_dir()).unwrap();
    let resolved = resolved.to_str().expect("temporary directory is not UTF-8");
    let base = resolved
        .strip_prefix(r"\\?\")
        .expect("canonical temporary directory is not a verbatim drive path");
    tempfile::tempdir_in(base).unwrap()
}
/// Native COM admission; a refusal panics with a bounded diagnostic naming
/// the refused field, the native COM view and the saved byte layout.
fn admit_or_explain(root: &Path, profile: ShortcutProfile) -> AdmittedShortcut {
    let expected = format!(
        "target={:?} arguments={:?} cwd={:?}",
        format!("{}\\{}", profile.root, profile.executable),
        command_line(&profile.arguments),
        profile.root
    );
    let profile_entry = profile.entry.clone();
    let apartment = ComApartment::initialize_original_thread().unwrap();
    match AdmittedShortcut::parse(apartment, RetainedShortcut::open(profile).unwrap()) {
        Ok(admitted) => admitted,
        Err(error) => {
            // Bounded diagnostic only; the failure itself is preserved.
            let entry = root.join(&profile_entry);
            let observed = match ComApartment::initialize_original_thread() {
                Ok(apartment) => {
                    super::super::com::diagnose_native_metadata(&apartment, entry.to_str().unwrap())
                }
                Err(apartment_error) => {
                    format!("diagnostic apartment unavailable: {apartment_error}")
                }
            };
            let layout = match fs::read(&entry) {
                Ok(bytes) => describe_link_bytes(&bytes),
                Err(read_error) => format!("unreadable: {read_error}"),
            };
            panic!(
                "native parse refused: {error}\nexpected: {expected}\nobserved: {observed}\nlayout: {layout}"
            );
        }
    }
}
#[test]
fn windows_shortcut_inert_encoder_bytes_are_admitted_by_native_com() {
    // The deterministic inert encoder (used by the SUT desktop entry fixture)
    // must produce bytes native Shell COM reads back as the exact target.
    let temp = long_name_tempdir();
    let mut profile = fixture(temp.path());
    let bytes = super::super::fixture::encode_link(
        &format!("{}\\{}", profile.root, profile.executable),
        &profile.root,
        &command_line(&profile.arguments),
    )
    .unwrap();
    fs::write(temp.path().join("app.lnk"), &bytes).unwrap();
    profile.entry_artifact = commitment(&bytes);
    let admitted = admit_or_explain(temp.path(), profile);
    admitted.revalidate().unwrap();
    if admitted.retire_original().is_err() {
        panic!("original thread parser retirement failed");
    }
}
#[test]
fn windows_shortcut_native_com_parser_and_retained_pins_are_actual() {
    let temp = long_name_tempdir();
    let profile = fixture(temp.path());
    let admitted = admit_or_explain(temp.path(), profile);
    admitted.revalidate().unwrap();
    // Actual Windows sharing prevents both replacement and in-place mutation
    // while Shell/COM could consume the original admitted pathname.
    assert!(fs::write(temp.path().join("app.lnk"), b"replaced").is_err());
    assert!(fs::remove_file(temp.path().join("fixture.exe")).is_err());
    assert!(fs::rename(temp.path(), temp.path().with_extension("moved")).is_err());
    let apartment = match admitted.retire_original() {
        Ok(apartment) => apartment,
        Err(_) => panic!("original thread parser retirement failed"),
    };
    fs::write(temp.path().join("app.lnk"), b"authorized replacement").unwrap();
    fs::remove_file(temp.path().join("app.lnk")).unwrap();
    fs::remove_file(temp.path().join("fixture.exe")).unwrap();
    apartment.pump_once().unwrap();
}
#[test]
fn windows_shortcut_native_com_rejects_changed_target_before_activation() {
    let temp = long_name_tempdir();
    let mut profile = fixture(temp.path());
    // A real, existing, different executable in the same root, authored by
    // Shell COM, so only the COM-reported target differs from the profile.
    fs::copy(
        temp.path().join("fixture.exe"),
        temp.path().join("wrong.exe"),
    )
    .unwrap();
    let bad = author_link(
        temp.path(),
        "app.lnk",
        &format!("{}\\wrong.exe", profile.root),
        &profile.root,
        &command_line(&profile.arguments),
    );
    profile.entry_artifact = commitment(&bad);
    let apartment = ComApartment::initialize_original_thread().unwrap();
    let error = match AdmittedShortcut::parse(apartment, RetainedShortcut::open(profile).unwrap()) {
        Ok(_) => panic!("changed shortcut target admitted"),
        Err(error) => error,
    };
    // Once the byte profile admits the Shell-authored layout, the refusal
    // must be the exact target comparison, not an incidental earlier check.
    if validate_link_bytes(&bad).is_ok() {
        assert_eq!(
            error.to_string(),
            "native shortcut target differs from exact admitted target",
            "{}",
            describe_link_bytes(&bad)
        );
    }
}

// This is original-handle source IPC, not a Shell/native-activation receipt.
// Only the exact current test binary is started; no shortcut is activated.
const IPC_FIXTURE_TEST: &str =
    "desktop::windows::tests::native_fixture::windows_original_process_ready_close_fixture_child";
fn ipc_fixture_response(endpoint: &str, mode: &str, close: bool) -> std::io::Result<()> {
    use crate::services::readiness::{ReadinessResponse, decode_challenge, encode_response};
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};
    let until = Instant::now() + Duration::from_secs(2);
    let mut stream = loop {
        match fs::OpenOptions::new().read(true).write(true).open(endpoint) {
            Ok(stream) => break stream,
            Err(error) if error.raw_os_error() == Some(231) && Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(2))
            }
            Err(error) => return Err(error),
        }
    };
    let mut prefix = [0; 4];
    stream.read_exact(&mut prefix)?;
    let size = u32::from_be_bytes(prefix) as usize;
    assert!(size <= 4096);
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes)?;
    let c = decode_challenge(&bytes)?;
    let mut response = ReadinessResponse {
        protocol: c.protocol,
        lease: c.lease,
        slot: c.slot,
        resource_binding_digest: c.resource_binding_digest,
        start_or_activation_operation: c.start_or_activation_operation,
        action_digest: c.action_digest,
        nonce: c.nonce,
        artifact_length: 1,
        artifact_sha256: [5; 32],
        version_literal: "v1".into(),
        data_sha256: [6; 32],
        configuration_digest: [7; 32],
        instance: "original-fixture".into(),
        initialized: true,
    };
    if !close {
        match mode {
            "wrong-data" => response.data_sha256 = [99; 32],
            "wrong-version" => response.version_literal = "wrong".into(),
            "wrong-nonce" => response.nonce = [99; 16],
            "wrong-instance" => response.instance = "different-instance".into(),
            _ => {}
        }
    }
    let body = encode_response(&response)?;
    stream.write_all(&(body.len() as u32).to_be_bytes())?;
    stream.write_all(&body)?;
    Ok(())
}
#[test]
fn windows_original_process_ready_close_fixture_child() {
    let Ok(mode) = std::env::var("THESEUS_WINDOWS_ORIGINAL_IPC_MODE") else {
        return;
    };
    if mode == "idle" {
        std::thread::sleep(std::time::Duration::from_secs(5));
        return;
    }
    let ready = std::env::var("THESEUS_WINDOWS_ORIGINAL_IPC_READY").unwrap();
    // A deliberately wrong original peer is rejected before receiving a frame;
    // its retained local endpoint is later closed by the test's own cleanup.
    if ipc_fixture_response(&ready, &mode, false).is_err() {
        return;
    }
    ipc_fixture_response(
        &std::env::var("THESEUS_WINDOWS_ORIGINAL_IPC_CLOSE").unwrap(),
        &mode,
        true,
    )
    .unwrap();
}
struct OriginalIpcChild(std::process::Child);
impl Drop for OriginalIpcChild {
    fn drop(&mut self) {
        // Original fixture child object only; never reopen/control a PID.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn ipc_child(mode: &str, ready: &str, close: &str) -> OriginalIpcChild {
    OriginalIpcChild(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", IPC_FIXTURE_TEST, "--nocapture"])
            .env_clear()
            .env("THESEUS_WINDOWS_ORIGINAL_IPC_MODE", mode)
            .env("THESEUS_WINDOWS_ORIGINAL_IPC_READY", ready)
            .env("THESEUS_WINDOWS_ORIGINAL_IPC_CLOSE", close)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    )
}
#[test]
fn windows_original_handle_ipc_failed_facts_allow_close_but_failed_identity_does_not() {
    use crate::{
        principal::{CurrentPrincipal, current_principal},
        services::readiness::{
            ArtifactRole, LocalChallenge, NamedPipe, PipePeerExpectation, ReadinessChallenge,
            ReadinessExpectation, ReadinessFailure, ReadinessState, ReadyProtocol, Sid,
        },
    };
    use std::{
        os::windows::io::AsHandle,
        sync::atomic::AtomicBool,
        time::{Duration, Instant},
    };
    let CurrentPrincipal::Windows { token_user_sid } = current_principal().unwrap() else {
        panic!("Windows token principal required")
    };
    let sid = Sid::new(token_user_sid).unwrap();
    let expectation = ReadinessExpectation {
        artifact: ArtifactRole::Target,
        artifact_length: 1,
        artifact_sha256: [5; 32],
        version_literal: "v1".into(),
        data_sha256: [6; 32],
        configuration_digest: [7; 32],
    };
    for (mode, failure, authenticated) in [
        ("wrong-data", ReadinessFailure::WrongData, true),
        ("wrong-version", ReadinessFailure::WrongVersion, true),
        ("wrong-nonce", ReadinessFailure::WrongNonce, false),
        ("wrong-instance", ReadinessFailure::WrongInstance, false),
        ("wrong-peer", ReadinessFailure::WrongPeer, false),
    ] {
        let mut random = [0; 16];
        crate::random_bytes(&mut random).unwrap();
        let unique = random
            .iter()
            .map(|v| format!("{v:02x}"))
            .collect::<String>();
        let ready_endpoint =
            NamedPipe::new(format!(r"\\.\pipe\theseus.original-ready-{unique}")).unwrap();
        let close_endpoint =
            NamedPipe::new(format!(r"\\.\pipe\theseus.original-close-{unique}")).unwrap();
        let mut ready = LocalChallenge::bind(ready_endpoint.clone(), sid.clone()).unwrap();
        let mut close = LocalChallenge::bind(close_endpoint.clone(), sid.clone()).unwrap();
        let mut child = ipc_child(mode, ready_endpoint.as_str(), close_endpoint.as_str());
        // Negative peer case supplies a different genuinely held child object;
        // kernel pipe credentials must reject the connecting application.
        let wrong_original = (mode == "wrong-peer")
            .then(|| ipc_child("idle", ready_endpoint.as_str(), close_endpoint.as_str()));
        let original = wrong_original.as_ref().unwrap_or(&child);
        let peer = PipePeerExpectation {
            sid: sid.clone(),
            original_pid: original.0.id(),
            cooperative_instance: "original-fixture".into(),
            instance_binding_digest: [8; 32],
        };
        let challenge = ReadinessChallenge {
            protocol: ReadyProtocol::TheseusFixtureReadyV1,
            lease: "original-lease".into(),
            slot: "desktop".into(),
            resource_binding_digest: [1; 32],
            start_or_activation_operation: "original-activation".into(),
            action_digest: [2; 32],
            nonce: [3; 16],
            expectation_digest: [4; 32],
        };
        let cancel = AtomicBool::new(false);
        let observed = ready
            .probe_original_process(
                &challenge,
                &expectation,
                &peer,
                original.0.as_handle(),
                Instant::now() + Duration::from_secs(2),
                &cancel,
            )
            .unwrap();
        assert_eq!(observed.state, ReadinessState::Failed);
        assert_eq!(observed.failure, Some(failure));
        assert_eq!(observed.authenticated_instance, authenticated);
        let before = observed.clone();
        let mut progress = ActivationProgress::pending();
        progress.native_return(true).unwrap();
        super::super::probe::authenticate_observed_instance(&mut progress, &observed).unwrap();
        assert_eq!(observed, before);
        assert_eq!(progress.new_instance_proved(), authenticated);
        assert!(!progress.primary_exited());
        assert!(!progress.descendants_owned());
        if authenticated {
            assert!(child.0.try_wait().unwrap().is_none());
            let mut close_challenge = challenge.clone();
            close_challenge.start_or_activation_operation = "original-close".into();
            close_challenge.action_digest = [9; 32];
            close_challenge.nonce = [10; 16];
            let receipt = close
                .probe_original_process(
                    &close_challenge,
                    &expectation,
                    &peer,
                    child.0.as_handle(),
                    Instant::now() + Duration::from_secs(2),
                    &cancel,
                )
                .unwrap();
            assert_eq!(receipt.state, ReadinessState::Ready);
            assert!(receipt.authenticated_instance);
            assert!(!progress.primary_exited());
            let until = Instant::now() + Duration::from_secs(2);
            loop {
                if let Some(status) = child.0.try_wait().unwrap() {
                    assert!(status.success());
                    break;
                }
                assert!(Instant::now() < until);
                std::thread::sleep(Duration::from_millis(2));
            }
            progress.observe_primary_exit();
            assert!(progress.primary_exited());
            assert_eq!(observed.failure, Some(failure));
        } else {
            assert!(!progress.new_instance_proved());
            // Adapter close guard remains false. This source fixture cleanup
            // uses only its independently held original std::process::Child.
        }
        let until = Instant::now() + Duration::from_secs(2);
        loop {
            let r = ready.cleanup();
            let c = close.cleanup();
            if r.is_ok() && c.is_ok() {
                break;
            }
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
