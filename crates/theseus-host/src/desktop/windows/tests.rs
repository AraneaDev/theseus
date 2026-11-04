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
        flags: 0xb2,
    };
    assert!(p.admit_metadata(&good).is_ok());
    for bad in [
        ShortcutMetadata {
            target: "https://example.test".into(),
            ..good.clone()
        },
        ShortcutMetadata {
            arguments: "--instance other".into(),
            ..good.clone()
        },
        ShortcutMetadata {
            working_directory: r"C:\other".into(),
            ..good.clone()
        },
        ShortcutMetadata {
            flags: good.flags | 0x2000,
            ..good.clone()
        },
        ShortcutMetadata {
            flags: good.flags | 0x200,
            ..good.clone()
        },
        ShortcutMetadata {
            flags: good.flags | 1,
            ..good.clone()
        },
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
#[test]
fn windows_shortcut_structural_policy_refuses_opaque_launcher_extensions() {
    let mut bytes = vec![0u8; 108];
    bytes[..4].copy_from_slice(&76u32.to_le_bytes());
    bytes[4..20].copy_from_slice(&[1, 20, 2, 0, 0, 0, 0, 0, 192, 0, 0, 0, 0, 0, 0, 70]);
    bytes[20..24].copy_from_slice(&0x82u32.to_le_bytes());
    bytes[60..64].copy_from_slice(&1u32.to_le_bytes());
    bytes[76..80].copy_from_slice(&28u32.to_le_bytes());
    bytes[84..88].copy_from_slice(&1u32.to_le_bytes());
    assert_eq!(validate_link_bytes(&bytes).unwrap(), 0x82);
    for flag in [1, 4, 8, 0x40, 0x100, 0x200, 0x2000, 0x40000] {
        let mut bad = bytes.clone();
        bad[20..24].copy_from_slice(&(0x82u32 | flag).to_le_bytes());
        assert!(validate_link_bytes(&bad).is_err());
    }
    let mut extra = bytes.clone();
    extra.extend([0; 4]);
    assert!(validate_link_bytes(&extra).is_err());
    let mut network = bytes;
    network[84..88].copy_from_slice(&2u32.to_le_bytes());
    assert!(validate_link_bytes(&network).is_err());
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
