use super::*;
#[test]
fn intentional_callback_cycle_keeps_strong_owner_with_only_local_diagnostic_suppression() {
    let bridge = include_str!("bridge.m");
    let push = bridge.find("#pragma clang diagnostic push").unwrap();
    let ignore = bridge
        .find("#pragma clang diagnostic ignored \"-Warc-retain-cycles\"")
        .unwrap();
    let capture = bridge.find("state.completion = ^").unwrap();
    let clear = bridge[capture..].find("state.completion = nil").unwrap() + capture;
    let pop = bridge.find("#pragma clang diagnostic pop").unwrap();
    assert!(push < ignore && ignore < capture && capture < clear && clear < pop);
    assert!(!bridge[push..pop].contains("__weak"));
    assert!(!bridge[push..pop].contains("weakSelf"));
}

fn profile() -> BundleProfile {
    BundleProfile {
        app: "Fixture.app".into(),
        bundle_identifier: "dev.theseus.fixture".into(),
        executable: "fixture".into(),
        arguments: vec!["--cooperative".into()],
        info: ArtifactCommitment {
            length: 123,
            sha256: [1; 32],
        },
        executable_artifact: ArtifactCommitment {
            length: 456,
            sha256: [2; 32],
        },
    }
}
fn metadata() -> BundleMetadata {
    BundleMetadata {
        identifier: "dev.theseus.fixture".into(),
        executable: "fixture".into(),
        package_type: "APPL".into(),
        extra_keys: vec![],
    }
}

// Accepting any of these would admit a different executable/path or launch policy.
#[test]
fn closed_bundle_profile_rejects_paths_urls_environment_and_substitution() {
    let p = profile();
    assert!(p.validate().is_ok());
    for app in [
        "Other",
        "a/b.app",
        "../Fixture.app",
        "https://x.app",
        "Fixture.APP",
    ] {
        let mut bad = p.clone();
        bad.app = app.into();
        assert!(bad.validate().is_err(), "{app}");
    }
    for executable in ["../fixture", "a/b", "", "file:fixture"] {
        let mut bad = p.clone();
        bad.executable = executable.into();
        assert!(bad.validate().is_err());
    }
    for key in [
        "LSEnvironment",
        "CFBundleURLTypes",
        "CFBundleDocumentTypes",
        "LSBackgroundOnly",
        "LSUIElement",
        "Unknown",
    ] {
        let mut bad = metadata();
        bad.extra_keys.push(key.into());
        assert!(p.admit_metadata(&bad).is_err(), "{key}");
    }
    let mut bad = metadata();
    bad.identifier = "dev.other".into();
    assert!(p.admit_metadata(&bad).is_err());
    let mut bad = metadata();
    bad.executable = "other".into();
    assert!(p.admit_metadata(&bad).is_err());
    let mut bad = metadata();
    bad.package_type = "BNDL".into();
    assert!(p.admit_metadata(&bad).is_err());
    let mut bad = p.clone();
    bad.info.length = 65537;
    assert!(bad.validate().is_err());
    let mut bad = p.clone();
    bad.executable_artifact.length = 67108865;
    assert!(bad.validate().is_err());
    let mut bad = p.clone();
    bad.arguments = vec!["\0".into()];
    assert!(bad.validate().is_err());
}

// A launcher policy request or PID alone must never become new-instance/tree proof.
#[test]
fn native_manager_receipt_needs_original_channel_and_never_owns_descendants() {
    let mut state = ActivationProgress::pending();
    assert_eq!(state.effect(), ActivationEffect::Pending);
    state
        .callback(CallbackResult::OriginalManagerInstance)
        .unwrap();
    assert_eq!(state.effect(), ActivationEffect::Unavailable);
    assert!(!state.descendants_owned());
    state.authenticate_instance(false).unwrap();
    assert_eq!(state.effect(), ActivationEffect::Unknown);

    let mut state = ActivationProgress::pending();
    state.mark_deadline_or_cancel();
    assert!(state.callback_pending());
    state
        .callback(CallbackResult::OriginalManagerInstance)
        .unwrap();
    state.authenticate_instance(true).unwrap();
    assert_eq!(state.effect(), ActivationEffect::Acknowledged);
    state.manager_terminated();
    assert!(state.manager_instance_terminated());
    assert!(!state.descendants_owned());
    assert!(
        state
            .callback(CallbackResult::OriginalManagerInstance)
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn retained_bundle_rejects_links_changed_bytes_missing_executable_and_replacement() {
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap().join("entries");
    let app = root.join("Fixture.app");
    fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    fs::write(app.join("Contents/Info.plist"), b"metadata").unwrap();
    fs::write(app.join("Contents/MacOS/fixture"), b"abc").unwrap();
    fs::set_permissions(
        app.join("Contents/MacOS/fixture"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let mut p = profile();
    p.info.length = 8;
    p.info.sha256 = [
        0x45, 0x44, 0x7b, 0x7a, 0xfb, 0xd5, 0xe5, 0x44, 0xf7, 0xd0, 0xf1, 0xdf, 0x0f, 0xcc, 0xd2,
        0x60, 0x14, 0xd9, 0x85, 0x01, 0x30, 0xab, 0xd3, 0xf0, 0x20, 0xb8, 0x9f, 0xf9, 0x6b, 0x82,
        0x07, 0x9f,
    ];
    p.executable_artifact.length = 3;
    p.executable_artifact.sha256 = [
        0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22,
        0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00,
        0x15, 0xad,
    ];
    let held = RetainedBundle::open(&root, p.clone()).unwrap();
    held.revalidate().unwrap();
    fs::write(app.join("Contents/MacOS/fixture"), b"abd").unwrap();
    assert!(held.revalidate().is_err());
    assert!(RetainedBundle::open(&root, p.clone()).is_err());
    fs::remove_file(app.join("Contents/MacOS/fixture")).unwrap();
    assert!(RetainedBundle::open(&root, p.clone()).is_err());
    fs::write(root.join("outside"), b"abc").unwrap();
    symlink(root.join("outside"), app.join("Contents/MacOS/fixture")).unwrap();
    assert!(RetainedBundle::open(&root, p.clone()).is_err());
    fs::remove_file(app.join("Contents/MacOS/fixture")).unwrap();
    fs::write(app.join("Contents/MacOS/fixture"), b"abc").unwrap();
    fs::set_permissions(
        app.join("Contents/MacOS/fixture"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let held = RetainedBundle::open(&root, p.clone()).unwrap();
    fs::rename(&root, temp.path().join("old-entries")).unwrap();
    fs::create_dir(&root).unwrap();
    assert!(held.revalidate().is_err());
    fs::remove_dir(&root).unwrap();
    symlink(temp.path().join("old-entries"), &root).unwrap();
    assert!(RetainedBundle::open(&root, p).is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn native_cfpropertylist_admits_exact_metadata_and_refuses_wrong_native_types() {
    let bytes = br#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>dev.theseus.fixture</string><key>CFBundleExecutable</key><string>fixture</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>"#;
    let parsed = native_metadata(bytes).unwrap();
    profile().admit_metadata(&parsed).unwrap();
    let wrong = String::from_utf8(bytes.to_vec()).unwrap().replace(
        "<string>fixture</string>",
        "<array><string>fixture</string></array>",
    );
    assert!(native_metadata(wrong.as_bytes()).is_err());
    assert!(native_metadata(b"not a property list").is_err());
    assert!(native_metadata(include_bytes!("embedded-nul.plist")).is_err());
    let document = String::from_utf8(bytes.to_vec())
        .unwrap()
        .replace("</dict>", "<key>CFBundleURLTypes</key><array/></dict>");
    assert!(native_metadata(document.as_bytes()).is_err());
}

#[test]
fn failed_or_ambiguous_native_callback_never_becomes_fresh_instance() {
    for result in [CallbackResult::Failed, CallbackResult::Ambiguous] {
        let mut state = ActivationProgress::pending();
        assert!(state.authenticate_instance(true).is_err());
        state.callback(result).unwrap();
        assert!(!state.callback_pending());
        assert!(state.authenticate_instance(true).is_err());
        assert!(!state.descendants_owned());
    }
}

#[test]
fn failed_application_facts_preserve_authenticated_original_activation() {
    let mut state = ActivationProgress::pending();
    state
        .callback(CallbackResult::OriginalManagerInstance)
        .unwrap();
    state.authenticate_instance(true).unwrap();
    assert_eq!(state.effect(), ActivationEffect::Acknowledged);
    assert!(!state.descendants_owned());
}

#[test]
fn fresh_probe_cannot_replace_initial_activation_identity_or_reuse_nonce() {
    use crate::{
        desktop::owned::{CommittedReadinessProbe, ExecutableBinding},
        services::readiness::{
            ArtifactRole, ReadinessChallenge, ReadinessExpectation, ReadyProtocol,
        },
    };
    let ready = ReadinessChallenge {
        protocol: ReadyProtocol::TheseusFixtureReadyV1,
        lease: "lease".into(),
        slot: "desktop".into(),
        resource_binding_digest: [1; 32],
        start_or_activation_operation: "activate".into(),
        action_digest: [2; 32],
        nonce: [3; 16],
        expectation_digest: [4; 32],
    };
    let expectation = ReadinessExpectation {
        artifact: ArtifactRole::Base,
        artifact_length: 3,
        artifact_sha256: [5; 32],
        version_literal: "base".into(),
        data_sha256: [6; 32],
        configuration_digest: [7; 32],
    };
    let mut close = ready.clone();
    close.start_or_activation_operation = "close".into();
    close.action_digest = [8; 32];
    close.nonce = [9; 16];
    let binding = ExecutableBinding {
        ready: ready.clone(),
        close,
        close_original_activation_operation: "activate".into(),
        expectation: expectation.clone(),
        cooperative_instance: "fresh-original".into(),
        instance_binding_digest: [10; 32],
    };
    binding.validate().unwrap();
    let mut fresh = CommittedReadinessProbe {
        challenge: ready,
        expectation,
    };
    fresh.challenge.nonce = [11; 16];
    fresh.challenge.expectation_digest = [12; 32];
    fresh.expectation.version_literal = "configured".into();
    let used = [binding.ready.nonce];
    probe::validate_fresh_probe(&binding, &fresh, &used).unwrap();
    assert_eq!(binding.ready.nonce, [3; 16]);
    for nonce in [[0; 16], [3; 16], [9; 16]] {
        let mut bad = fresh.clone();
        bad.challenge.nonce = nonce;
        assert!(probe::validate_fresh_probe(&binding, &bad, &used).is_err());
    }
    let mut bad = fresh.clone();
    bad.challenge.start_or_activation_operation = "another-activation".into();
    assert!(probe::validate_fresh_probe(&binding, &bad, &used).is_err());
    let mut bad = fresh.clone();
    bad.challenge.action_digest = [13; 32];
    assert!(probe::validate_fresh_probe(&binding, &bad, &used).is_err());
    assert!(probe::validate_fresh_probe(&binding, &fresh, &[[14; 16]; 64]).is_err());
}

// Native metadata/file source test only. The copied compiled test executable is
// never started, registered with LaunchServices, or presented as a GUI receipt.
#[cfg(target_os = "macos")]
#[test]
fn native_cf_admission_holds_exact_compiled_app_bundle_and_rejects_missing_executable() {
    use sha2::{Digest, Sha256};
    use std::{fs, io::Read, os::unix::fs::PermissionsExt};
    fn commitment(path: &std::path::Path) -> ArtifactCommitment {
        let mut file = fs::File::open(path).unwrap();
        let mut hash = Sha256::new();
        let mut bytes = [0; 16384];
        let mut length = 0;
        loop {
            let count = file.read(&mut bytes).unwrap();
            if count == 0 {
                break;
            }
            hash.update(&bytes[..count]);
            length += count as u64;
        }
        ArtifactCommitment {
            length,
            sha256: hash.finalize().into(),
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let app = root.join("Fixture.app/Contents");
    fs::create_dir_all(app.join("MacOS")).unwrap();
    let info = app.join("Info.plist");
    fs::write(&info, br#"<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>dev.theseus.fixture</string><key>CFBundleExecutable</key><string>fixture</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>"#).unwrap();
    let executable = app.join("MacOS/fixture");
    fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let mut p = profile();
    p.info = commitment(&info);
    p.executable_artifact = commitment(&executable);
    let admitted = AdmittedBundle::admit(RetainedBundle::open(&root, p.clone()).unwrap()).unwrap();
    admitted.revalidate().unwrap();
    fs::remove_file(&executable).unwrap();
    assert!(admitted.revalidate().is_err());
    assert!(RetainedBundle::open(&root, p).is_err());
}

#[test]
fn pending_native_storage_survives_drop_until_explicit_retirement() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct OriginalStorage {
        drops: Arc<AtomicUsize>,
        bytes: Vec<u8>,
    }
    impl Drop for OriginalStorage {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }
    let drops = Arc::new(AtomicUsize::new(0));
    let owner = retention::NativeRetention::new(OriginalStorage {
        drops: drops.clone(),
        bytes: vec![17; 4096],
    });
    let original = owner.bytes.as_ptr();
    let mut progress = ActivationProgress::pending();
    progress.mark_deadline_or_cancel();
    assert_eq!(owner.bytes.as_ptr(), original);
    drop(owner);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    let mut settled = retention::NativeRetention::new(OriginalStorage {
        drops: drops.clone(),
        bytes: vec![19; 4096],
    });
    settled.mark_settled();
    drop(settled);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn exceptional_native_return_retains_pending_callback_and_explicit_uncertainty() {
    let mut state = ActivationProgress::pending();
    state.mark_native_uncertain();
    assert_eq!(state.effect(), ActivationEffect::Unknown);
    assert!(state.callback_pending());
    state.mark_deadline_or_cancel();
    assert_eq!(state.effect(), ActivationEffect::Unknown);
    state
        .callback(CallbackResult::OriginalManagerInstance)
        .unwrap();
    assert_eq!(state.effect(), ActivationEffect::Unavailable);
    state.authenticate_instance(true).unwrap();
    assert_eq!(state.effect(), ActivationEffect::Acknowledged);
}
