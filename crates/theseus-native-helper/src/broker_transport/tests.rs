use super::*;
use theseus_native_protocol::v2;
fn config_value() -> serde_json::Value {
    let expectation = v2::ReadinessDefinitionExpectation {
        role: "configured".into(),
        artifact: "target".into(),
        artifact_length: 3,
        artifact_sha256: "22".repeat(32),
        version_literal: "target".into(),
        data_sha256: "44".repeat(32),
        configuration_digest: "55".repeat(32),
    };
    let manifest:v2::IntegrationManifest=serde_json::from_value(serde_json::json!({"schema":1,"core_manifest_digest":"11".repeat(32),"desktop":{"schema":1,"namespace_policy":"exclusive_quiescent","core_manifest_digest":"11".repeat(32),"slot":"fixture_desktop","entry_root":"/fixture","entry":"fixture.desktop","entry_digest":"33".repeat(32),"executable_digest":"22".repeat(32),"allowed_modes":["owned_executable"],"ready_endpoint":{"kind":"unix_path","path":"/fixture/ready.sock"},"close_endpoint":{"kind":"unix_path","path":"/fixture/close.sock"},"expectation_digest":expectation.digest().unwrap().to_hex()},"desktop_executable":{"root":"/fixture","relative_path":"app","length":3,"sha256":"22".repeat(32),"arguments":["/fixture/app"],"environment":[],"working_directory":"/fixture"},"desktop_readiness":expectation,"definitions":[],"artifact_slots":[{"slot":"fixture_desktop","relative_path":"fixture.desktop","length":128,"sha256":"33".repeat(32)}],"readiness_protocol":"theseus_fixture_ready_v1","desktop_capabilities":["inspect_desktop","owned_executable","request_close"]})).unwrap();
    manifest.validate().unwrap();
    let full = v2::integration_manifest_digest(
        v2::Digest::parse(&manifest.core_manifest_digest).unwrap(),
        manifest.digest().unwrap(),
    );
    let mut config = serde_json::json!({"kind":"theseus_native_desktop_broker","schema":1,"profile":"native_integration_broker_v1","ledger_root":"/fixture/ledger","workspace_root":"/fixture/workspace","manifest":manifest,"header":{"profile":"native_integration_broker_v1","binding":"11".repeat(16),"manifest_digest":full.to_hex(),"workspace_marker":"33".repeat(16),"os":"linux","target_id":"fixture","ownership_id":"44".repeat(16),"provider_epoch":"55".repeat(32),"session_id":"owned-headless","session_epoch":"66".repeat(32),"broker_endpoint":{"kind":"unix_path","path":"/fixture/broker.sock"},"broker_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"helper_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"display":{"kind":"source_owned_headless"},"allowed_modes":["owned_executable"]}});
    if cfg!(windows) {
        config["ledger_root"] = serde_json::json!("C:/fixture/ledger");
        config["workspace_root"] = serde_json::json!("C:/fixture/workspace");
    }
    config
}
// Catches accepting catalog/default profiles, changed fullM or extra authority.
#[test]
fn standalone_broker_config_is_explicit_strict_and_binds_complete_manifest() {
    let value = config_value();
    assert!(BrokerServerConfig::from_bytes(&serde_json::to_vec(&value).unwrap()).is_ok());
    let mut missing_policy = value.clone();
    missing_policy["manifest"]["desktop"]
        .as_object_mut()
        .unwrap()
        .remove("namespace_policy");
    assert!(BrokerServerConfig::from_bytes(&serde_json::to_vec(&missing_policy).unwrap()).is_err());
    for (field, replacement) in [
        ("profile", serde_json::json!("native_catalog_v1")),
        ("schema", serde_json::json!(2)),
        ("kind", serde_json::json!("helper")),
        ("extra", serde_json::json!(true)),
    ] {
        let mut changed = value.clone();
        changed[field] = replacement;
        assert!(
            BrokerServerConfig::from_bytes(&serde_json::to_vec(&changed).unwrap()).is_err(),
            "accepted {field}"
        );
    }
    let mut changed = value.clone();
    changed["header"]["manifest_digest"] = serde_json::json!("ff".repeat(32));
    assert!(BrokerServerConfig::from_bytes(&serde_json::to_vec(&changed).unwrap()).is_err());
    let mut changed = value;
    changed["manifest"]["artifact_slots"] = serde_json::json!([]);
    assert!(BrokerServerConfig::from_bytes(&serde_json::to_vec(&changed).unwrap()).is_err());
    assert!(
        BrokerServerConfig::from_bytes(
            br#"{"kind":"theseus_native_desktop_broker","schema":1,"schema":1}"#
        )
        .is_err()
    );
}

fn original_activation() -> v2::broker::DesktopRequest {
    use v2::broker::*;
    let mut request = DesktopRequest {
        helper_lease: "11".repeat(16),
        helper_operation: "22".repeat(16),
        broker_lease: "33".repeat(16),
        binding_digest: "44".repeat(32),
        activation_nonce: "55".repeat(16),
        slot: v2::ResourceSlot::new("fixture_desktop").unwrap(),
        prior_binding_digest: "66".repeat(32),
        action: DesktopAction::Activate {
            owned_lifetime: None,
            transition: None,
            mode: DesktopMode::OwnedExecutable,
            entry_digest: "77".repeat(32),
            expectation_digest: "88".repeat(32),
            close_binding: CooperativeCloseBinding {
                operation: "99".repeat(16),
                action_digest: "aa".repeat(32),
                nonce: "bb".repeat(16),
            },
        },
    };
    let close_digest = request.close_request().unwrap().digest().unwrap().to_hex();
    let DesktopAction::Activate { close_binding, .. } = &mut request.action else {
        unreachable!()
    };
    close_binding.action_digest = close_digest;
    request.validate().unwrap();
    request
}
#[test]
fn post_activation_validation_returns_refreshed_quarantine_state() {
    use v2::broker::*;
    let stale = BrokerState {
        owned_lifetime: None,
        native_lifetime: None,
        effect: BrokerEffect::Receipt {
            receipt: BrokerEffectReceipt {
                evidence: DesktopEvidence::SourceOwnedExecutable,
                acknowledged: true,
                receipt_digest: "11".repeat(32),
            },
        },
        owner: BrokerOwner::Retained,
        primary: BrokerPrimary::Running,
        descendants: BrokerDescendants::Pending,
        cleanup_complete: false,
        quarantined: false,
    };
    let mut latest = stale.clone();
    latest.quarantined = true;
    let result = server::finish_activation_check(
        stale,
        Err(std::io::Error::other("observed replacement")),
        |_| Ok(latest.clone()),
    )
    .unwrap();
    assert!(result.quarantined);
    assert_eq!(result, latest);
}
#[test]
fn cancel_validation_preserves_unrelated_ready_and_close_and_resets_on_failure() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let original = original_activation();
    let identity = original.identity().unwrap();
    let pending_ready = server::Pending::Ready {
        reply_to: 7,
        identity: Box::new(identity.clone()),
        nonce: v2::BindingId::from_bytes([1; 16]),
        deadline: std::time::Instant::now(),
    };
    let pending_close = server::Pending::Close {
        reply_to: 8,
        request: Box::new(original.close_request().unwrap()),
        deadline: std::time::Instant::now(),
    };
    let flag = Arc::new(AtomicBool::new(false));
    for pending in [&pending_ready, &pending_close] {
        for bad in ["operation", "digest"] {
            let mut wrong = identity.clone();
            if bad == "operation" {
                wrong.helper_operation = "cc".repeat(16);
            } else {
                wrong.request_digest = "dd".repeat(32);
            }
            assert!(server::validate_cancel_identity(&original, Some(pending), &wrong).is_err());
            assert!(!flag.load(Ordering::Acquire));
        }
        assert!(server::validate_cancel_identity(&original, Some(pending), &identity).is_ok());
    }
    let mut unrelated = original.clone();
    unrelated.helper_operation = "ee".repeat(16);
    let close_digest = unrelated
        .close_request()
        .unwrap()
        .digest()
        .unwrap()
        .to_hex();
    let v2::broker::DesktopAction::Activate { close_binding, .. } = &mut unrelated.action else {
        unreachable!()
    };
    close_binding.action_digest = close_digest;
    let unrelated_ready = server::Pending::Ready {
        reply_to: 9,
        identity: Box::new(unrelated.identity().unwrap()),
        nonce: v2::BindingId::from_bytes([2; 16]),
        deadline: std::time::Instant::now(),
    };
    let unrelated_close = server::Pending::Close {
        reply_to: 10,
        request: Box::new(unrelated.close_request().unwrap()),
        deadline: std::time::Instant::now(),
    };
    for pending in [&unrelated_ready, &unrelated_close] {
        assert!(server::validate_cancel_identity(&original, Some(pending), &identity).is_err());
        assert!(!flag.load(Ordering::Acquire));
    }
    let result = server::with_cancel_flag(&flag, || {
        assert!(flag.load(Ordering::Acquire));
        Err::<(), _>(std::io::Error::other(
            "original runtime cancellation failure",
        ))
    });
    assert!(result.is_err());
    assert!(!flag.load(Ordering::Acquire));
    server::with_cancel_flag(&flag, || Ok(())).unwrap();
    assert!(!flag.load(Ordering::Acquire));
}
#[cfg(unix)]
fn local_roots_config() -> (tempfile::TempDir, BrokerServerConfig) {
    let dir = tempfile::tempdir().unwrap();
    let entry = dir.path().join("entries");
    let executable = dir.path().join("executables");
    std::fs::create_dir(&entry).unwrap();
    std::fs::create_dir(&executable).unwrap();
    let mut config =
        BrokerServerConfig::from_bytes(&serde_json::to_vec(&config_value()).unwrap()).unwrap();
    config.manifest.desktop.as_mut().unwrap().entry_root = entry.to_str().unwrap().into();
    let exe = config.manifest.desktop_executable.as_mut().unwrap();
    exe.root = executable.to_str().unwrap().into();
    exe.arguments = vec![format!("{}/app", exe.root)];
    exe.working_directory = exe.root.clone();
    (dir, config)
}
#[cfg(unix)]
#[test]
fn original_root_failure_is_sticky_after_each_root_is_restored() {
    for component in ["entries", "executables"] {
        let (dir, config) = local_roots_config();
        let roots = launch::DefinitionRoots::open(&config).unwrap();
        roots.revalidate().unwrap();
        let original = dir.path().join(component);
        let moved = dir.path().join("held-root");
        std::fs::rename(&original, &moved).unwrap();
        std::fs::create_dir(&original).unwrap();
        assert!(roots.revalidate().is_err());
        std::fs::remove_dir(&original).unwrap();
        std::fs::rename(&moved, &original).unwrap();
        assert!(
            roots.revalidate().is_err(),
            "restored {component} revived original authority"
        );
    }
}
#[cfg(unix)]
#[test]
fn nested_parent_absence_and_leaked_entry_are_independent_complete_observations() {
    let (_dir, mut config) = local_roots_config();
    config.manifest.desktop.as_mut().unwrap().entry = "applications/fixture.desktop".into();
    config.manifest.artifact_slots[0].relative_path = "applications/fixture.desktop".into();
    config
        .manifest
        .desktop_executable
        .as_mut()
        .unwrap()
        .relative_path = "bin/app".into();
    let roots = launch::DefinitionRoots::open(&config).unwrap();
    let empty = launch::inspect(
        &config,
        &roots,
        &config
            .manifest
            .desktop_definition(&v2::DesktopDefinitionKey::CONFIGURED_TARGET)
            .unwrap(),
    )
    .unwrap();
    assert!(!empty.entry_present);
    assert!(empty.executable_digest.is_none());
    let entries = std::path::Path::new(&config.manifest.desktop.as_ref().unwrap().entry_root)
        .join("applications");
    std::fs::create_dir(&entries).unwrap();
    std::fs::write(entries.join("fixture.desktop"), b"actual leaked entry").unwrap();
    let leaked = launch::inspect(
        &config,
        &roots,
        &config
            .manifest
            .desktop_definition(&v2::DesktopDefinitionKey::CONFIGURED_TARGET)
            .unwrap(),
    )
    .unwrap();
    assert!(leaked.entry_present);
    assert!(leaked.entry_digest.is_some());
    assert!(leaked.executable_digest.is_none());
    let exe = std::path::Path::new(&config.manifest.desktop_executable.as_ref().unwrap().root);
    std::fs::create_dir(exe.join("bin")).unwrap();
    std::fs::write(exe.join("bin/app"), b"app").unwrap();
    assert!(
        launch::inspect(
            &config,
            &roots,
            &config
                .manifest
                .desktop_definition(&v2::DesktopDefinitionKey::CONFIGURED_TARGET)
                .unwrap()
        )
        .unwrap()
        .executable_digest
        .is_some()
    );
    std::fs::remove_file(exe.join("bin/app")).unwrap();
    std::fs::remove_dir(exe.join("bin")).unwrap();
    let removed_executable_parent = launch::inspect(
        &config,
        &roots,
        &config
            .manifest
            .desktop_definition(&v2::DesktopDefinitionKey::CONFIGURED_TARGET)
            .unwrap(),
    )
    .unwrap();
    assert!(removed_executable_parent.entry_present);
    assert!(removed_executable_parent.executable_digest.is_none());
    std::fs::remove_file(entries.join("fixture.desktop")).unwrap();
    std::fs::remove_dir(entries).unwrap();
    let removed_both_parents = launch::inspect(
        &config,
        &roots,
        &config
            .manifest
            .desktop_definition(&v2::DesktopDefinitionKey::CONFIGURED_TARGET)
            .unwrap(),
    )
    .unwrap();
    assert!(!removed_both_parents.entry_present);
    assert!(removed_both_parents.executable_digest.is_none());
    std::fs::write(exe.join("bin"), b"wrong kind").unwrap();
    assert!(
        launch::inspect(
            &config,
            &roots,
            &config
                .manifest
                .desktop_definition(&v2::DesktopDefinitionKey::CONFIGURED_TARGET)
                .unwrap()
        )
        .is_err()
    );
    std::fs::remove_file(exe.join("bin")).unwrap();
    std::os::unix::fs::symlink(exe, exe.join("bin")).unwrap();
    assert!(
        launch::inspect(
            &config,
            &roots,
            &config
                .manifest
                .desktop_definition(&v2::DesktopDefinitionKey::CONFIGURED_TARGET)
                .unwrap()
        )
        .is_err()
    );
}

#[test]
fn scheduling_yield_preserves_original_ready_and_close_semantic_deadline() {
    let request = original_activation();
    let original = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let ready = server::Pending::Ready {
        reply_to: 1,
        identity: Box::new(request.identity().unwrap()),
        nonce: v2::BindingId::from_bytes([1; 16]),
        deadline: original,
    };
    let close = server::Pending::Close {
        reply_to: 2,
        request: Box::new(request.close_request().unwrap()),
        deadline: original,
    };
    for pending in [ready, close] {
        assert_eq!(
            pending.semantic_deadline(),
            original,
            "scheduling yield must not shorten original exchange lifetime"
        );
    }
}
