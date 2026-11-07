use super::*;
#[test]
fn authenticated_failed_application_facts_allow_only_original_instance_close() {
    use crate::services::readiness::{ReadinessFailure, ReadinessObservation, ReadinessState};
    for failure in [
        ReadinessFailure::WrongArtifact,
        ReadinessFailure::WrongVersion,
        ReadinessFailure::WrongData,
        ReadinessFailure::WrongConfiguration,
        ReadinessFailure::NotInitialized,
    ] {
        let observed = ReadinessObservation {
            state: ReadinessState::Failed,
            peer: None,
            peer_sid: None,
            response: None,
            failure: Some(failure),
            authenticated_instance: true,
            instance_binding_digest: [1; 32],
        };
        let original = observed.clone();
        let mut progress = ActivationProgress::pending();
        progress.native_return(true).unwrap();
        super::probe::authenticate_observed_instance(&mut progress, &observed).unwrap();
        assert!(progress.new_instance_proved());
        assert_eq!(progress.effect(), ActivationEffect::Acknowledged);
        assert!(!progress.primary_exited());
        assert!(!progress.descendants_owned());
        assert_eq!(observed, original);
    }
    for failure in [
        ReadinessFailure::WrongPeer,
        ReadinessFailure::WrongNonce,
        ReadinessFailure::WrongInstance,
    ] {
        let observed = ReadinessObservation {
            state: ReadinessState::Failed,
            peer: None,
            peer_sid: None,
            response: None,
            failure: Some(failure),
            authenticated_instance: false,
            instance_binding_digest: [1; 32],
        };
        let mut progress = ActivationProgress::pending();
        progress.native_return(true).unwrap();
        super::probe::authenticate_observed_instance(&mut progress, &observed).unwrap();
        assert!(!progress.new_instance_proved());
        assert_eq!(progress.effect(), ActivationEffect::Unknown);
        assert_eq!(observed.state, ReadinessState::Failed);
        assert_eq!(observed.failure, Some(failure));
    }
}
fn profile() -> ShortcutProfile {
    ShortcutProfile {
        root: r"C:\fixture".into(),
        entry: "app.lnk".into(),
        executable: "fixture.exe".into(),
        arguments: vec!["--instance".into(), "fresh nonce".into()],
        entry_artifact: ArtifactCommitment {
            length: 128,
            sha256: [1; 32],
        },
        executable_artifact: ArtifactCommitment {
            length: 1000,
            sha256: [2; 32],
        },
    }
}
#[test]
fn windows_shortcut_exact_target_arguments_cwd_and_closed_flags() {
    let p = profile();
    let good = ShortcutMetadata {
        target: r"C:\fixture\fixture.exe".into(),
        arguments: "--instance \"fresh nonce\"".into(),
        working_directory: r"C:\fixture".into(),
        layout: LinkLayout {
            flags: 0xbb,
            local_base_path: r"C:\fixture\fixture.exe".into(),
            relative_path: r".\fixture.exe".into(),
        },
    };
    assert!(p.admit_metadata(&good).is_ok());
    let with = |change: &dyn Fn(&mut ShortcutMetadata)| {
        let mut bad = good.clone();
        change(&mut bad);
        bad
    };
    for bad in [
        with(&|m| m.target = "https://example.test".into()),
        with(&|m| m.target = String::new()),
        with(&|m| m.arguments = "--instance other".into()),
        with(&|m| m.working_directory = r"C:\other".into()),
        // argv present requires HasArguments; any other flag refuses.
        with(&|m| m.layout.flags = 0x9b),
        with(&|m| m.layout.flags |= 0x2000),
        with(&|m| m.layout.flags |= 0x200),
        with(&|m| m.layout.flags |= 0x40),
        with(&|m| m.layout.flags |= 4),
        // Shell resolution fallbacks must name the same exact target.
        with(&|m| m.layout.local_base_path = r"C:\other\fixture.exe".into()),
        with(&|m| m.layout.relative_path = r"..\fixture.exe".into()),
        with(&|m| m.layout.relative_path = r".\other.exe".into()),
    ] {
        assert!(p.admit_metadata(&bad).is_err(), "{bad:?}");
    }
}
#[test]
fn windows_shortcut_refuses_alias_roots_and_unknown_elevation() {
    for root in [
        r"C:relative",
        r"\\server\share",
        r"\\?\C:\root",
        r"C:\root\..\other",
        r"C:\root.",
        r"C:\root:stream",
    ] {
        let mut p = profile();
        p.root = root.into();
        assert!(p.validate().is_err(), "{root}");
    }
    assert!(admit_run_level(1, false).is_ok());
    for level in [0, 2, 3, 4] {
        assert!(admit_run_level(level, false).is_err());
    }
    assert!(admit_run_level(1, true).is_err());
}
#[test]
fn windows_shell_activation_missing_handle_or_session_authority_never_becomes_owned_lifetime() {
    let mut progress = ActivationProgress::pending();
    progress.native_return(false).unwrap();
    assert_eq!(progress.effect(), ActivationEffect::Unavailable);
    assert!(!progress.owned_primary_available());
    assert!(progress.authenticate_new_instance(true).is_err());
    let mut progress = ActivationProgress::pending();
    progress.native_return(true).unwrap();
    assert_eq!(progress.effect(), ActivationEffect::Unavailable);
    assert!(!progress.new_instance_proved());
    progress.mark_deadline_or_cancel();
    assert!(progress.owned_primary_available());
    progress.authenticate_new_instance(false).unwrap();
    assert_eq!(progress.effect(), ActivationEffect::Unknown);
    assert!(!progress.descendants_owned());
}
#[test]
fn windows_original_handle_ack_primary_and_tree_are_independent() {
    let mut progress = ActivationProgress::pending();
    progress.native_return(true).unwrap();
    progress.authenticate_new_instance(true).unwrap();
    assert_eq!(progress.effect(), ActivationEffect::Acknowledged);
    assert!(!progress.primary_exited());
    progress.observe_primary_exit();
    assert!(progress.primary_exited());
    assert!(!progress.descendants_owned());
}
#[test]
fn windows_argument_commitment_quotes_backslashes_and_empty_values() {
    assert_eq!(command_line(&["".into(), "a b".into()]), r#""" "a b""#);
    assert_eq!(command_line(&["a\\\"b".into()]), r#""a\\\"b""#);
    assert_eq!(command_line(&["tail \\".into()]), r#""tail \\""#);
}
/// Exact bytes IShellLinkW + IPersistFile::Save wrote on windows-2025 (CI8,
/// run 42302044978) for the native fixture shortcut.
pub(super) fn shell_saved_link() -> Vec<u8> {
    let hex = include_str!("tests/shell_saved_windows_2025.lnk.hex").trim();
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}
const SHELL_SAVED_TARGET: &str = r"C:\Users\runneradmin\AppData\Local\Temp\.tmpNdQTs6\fixture.exe";
// Offsets inside the CI8 vector (asserted below before use).
const LINK_INFO: usize = 618;
const EXTRA: usize = 946;
const SPECIAL: usize = EXTRA;
const KNOWN: usize = EXTRA + 16;
const TRACKER: usize = KNOWN + 28;
const PROPERTIES: usize = TRACKER + 96;
fn put(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
#[test]
fn windows_shortcut_closed_profile_admits_exact_shell_saved_bytes() {
    let bytes = shell_saved_link();
    assert_eq!(bytes.len(), 1296);
    for (offset, signature) in [
        (SPECIAL, 0xa000_0005u32),
        (KNOWN, 0xa000_000b),
        (TRACKER, 0xa000_0003),
        (PROPERTIES, 0xa000_0009),
    ] {
        assert_eq!(bytes[offset + 4..offset + 8], signature.to_le_bytes());
    }
    assert_eq!(
        validate_link_bytes(&bytes).unwrap(),
        LinkLayout {
            flags: 0xbb,
            local_base_path: SHELL_SAVED_TARGET.into(),
            relative_path: r".\fixture.exe".into(),
        }
    );
}
#[test]
fn windows_shortcut_structural_policy_refuses_opaque_launcher_extensions() {
    let bytes = shell_saved_link();
    let refuse = |change: &dyn Fn(&mut Vec<u8>), why: &str| {
        let mut bad = bytes.clone();
        change(&mut bad);
        assert!(validate_link_bytes(&bad).is_err(), "{why}");
    };
    // Every LinkFlag outside the closed set: HasName, HasIconLocation,
    // HasExpString, RunInSeparateProcess, HasDarwinID, RunAsUser,
    // HasExpIcon, ForceNoLinkInfo, RunWithShimLayer, EnableTargetMetadata,
    // PreferEnvironmentPath, ...
    for flag in [
        4, 0x40, 0x100, 0x200, 0x400, 0x1000, 0x2000, 0x4000, 0x8000, 0x10000, 0x20000, 0x40000,
        0x80000, 0x2000000,
    ] {
        refuse(&|b| put(b, 20, 0xbb | flag), "extra flag");
    }
    // Required flags cannot be dropped (IDList, LinkInfo, relative, cwd).
    for flag in [1, 2, 8, 0x10, 0x80] {
        refuse(&|b| put(b, 20, 0xbb & !flag), "missing flag");
    }
    refuse(&|b| put(b, 24, 0x10), "directory target attributes");
    refuse(&|b| put(b, 56, 1), "icon index");
    refuse(&|b| put(b, 60, 3), "maximized show command");
    refuse(&|b| b[64] = 0x41, "hotkey");
    refuse(&|b| b[76] -= 2, "IDList size framing");
    refuse(&|b| b[78] = 2, "IDList undersized item");
    refuse(&|b| put(b, LINK_INFO + 8, 3), "network LinkInfo flags");
    refuse(&|b| put(b, LINK_INFO + 20, 40), "network LinkInfo offset");
    refuse(&|b| put(b, LINK_INFO + 4, 36), "unicode LinkInfo header");
    refuse(&|b| put(b, LINK_INFO + 28 + 4, 4), "remote drive type");
    // Unobserved or authority-bearing ExtraData blocks, by signature:
    // EnvironmentVariable, Console, ConsoleFE, Darwin, IconEnvironment,
    // Shim, VistaAndAboveIDList and an unknown signature.
    for signature in [
        0xa000_0001u32,
        0xa000_0002,
        0xa000_0004,
        0xa000_0006,
        0xa000_0007,
        0xa000_0008,
        0xa000_000c,
        0xa000_00ff,
    ] {
        refuse(&|b| put(b, SPECIAL + 4, signature), "ExtraData signature");
    }
    refuse(
        &|b| put(b, KNOWN + 4, 0xa000_0005),
        "duplicate/out-of-order block",
    );
    refuse(
        &|b| put(b, SPECIAL + 12, 540),
        "folder offset beyond IDList",
    );
    refuse(&|b| put(b, TRACKER + 8, 0x50), "tracker length");
    refuse(&|b| b[PROPERTIES + 8 + 8] ^= 1, "unknown property set");
    refuse(&|b| b[PROPERTIES + 8 + 41] = b'!', "owner SID text");
    refuse(&|b| put(b, 1292, 4), "non-terminal ExtraData end");
    refuse(&|b| b.extend([0; 4]), "trailing bytes");
}
#[test]
fn windows_shortcut_closed_profile_admits_each_observed_block_as_optional() {
    // Removing whole ExtraData blocks keeps a closed, still-ordered subset.
    let bytes = shell_saved_link();
    for (start, end) in [
        (SPECIAL, KNOWN),
        (KNOWN, TRACKER),
        (TRACKER, PROPERTIES),
        (EXTRA, 1292),
    ] {
        let mut subset = bytes[..start].to_vec();
        subset.extend(&bytes[end..]);
        assert!(validate_link_bytes(&subset).is_ok());
    }
}
#[cfg(windows)]
mod native_fixture;
#[test]
fn windows_fresh_probe_preserves_original_action_and_consumes_unique_nonce() {
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
        artifact: ArtifactRole::Target,
        artifact_length: 1,
        artifact_sha256: [5; 32],
        version_literal: "v1".into(),
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
        cooperative_instance: "fresh".into(),
        instance_binding_digest: [10; 32],
    };
    let mut next = CommittedReadinessProbe {
        challenge: ready,
        expectation,
    };
    next.challenge.nonce = [11; 16];
    assert!(super::probe::validate_fresh_probe(&binding, &next, &[[3; 16]]).is_ok());
    assert!(super::probe::validate_fresh_probe(&binding, &next, &[[11; 16]]).is_err());
    next.challenge.start_or_activation_operation = "replacement".into();
    assert!(super::probe::validate_fresh_probe(&binding, &next, &[[3; 16]]).is_err());
}
