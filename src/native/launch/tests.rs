use super::*;
#[cfg(unix)]
fn integration_launch_json() -> serde_json::Value {
    let mut value: serde_json::Value =
        serde_json::from_str(include_str!("../../../examples/native/launch.example.json")).unwrap();
    value["schema"] = 2.into();
    value["helper_profile"] = "native_integration_v1".into();
    value["core_manifest_digest"] = "02".repeat(32).into();
    value["integration_manifest_digest"] = "11".repeat(32).into();
    value["manifest_digest"] =
        "cd64ea4df93a58dc89771b1f60cd2b829e3e45e0bdb78650ee6a672cd7971d3c".into();
    value["service_authority"] = serde_json::json!({
        "kind":"systemd","slot":"service","scope":"user","bus_endpoint":{"kind":"unix_path","path":"/fixture/bus"},
        "qualification":{"manager":"systemd","version":"262","api_profile":"systemd_dbus_private_v1","features":["unit_properties","start_stop","manager_reload"],"receipt_sha256":"03".repeat(32)},
        "namespace":{"kind":"exclusive_owned_registration","slot":"service","unit_load_roots":["/fixture/units"],"dropins":"forbidden","provider_context":"private-fixture"},
        "manager_context":{"kind":"whole_private","slot":"manager","sut_allowed_actions":["daemon_reload"],"rehearsal_allowed_actions":[],"provider_receipt_required":true},
        "expected_peer_uid":1000,"expected_manager_uid":1000,"definition_root":"/fixture/units","unit":"theseus-fixture.service","managed_by":"installer",
        "sut_effects":["registration","configuration","removal","daemon_reload"],"allowed_actions":["inspect","start","stop"]
    });
    value["desktop_authority"] = serde_json::Value::Null;
    value
}
#[cfg(all(target_os = "linux", feature = "native-integration-test-support"))]
#[test]
fn source_combined_fixture_keeps_exact_both_authorities_and_shipping_source_refusal() {
    let service: v2::ServiceAuthority =
        serde_json::from_value(integration_launch_json()["service_authority"].clone()).unwrap();
    let mut manifest = crate::native::integration::desktop::readiness_role_review_tests::manifest(
        "configured",
        "target",
    );
    manifest.service_authority = Some(service.clone());
    manifest.definitions.push(v2::ServiceDefinition {
        scm: None,
        role: "configured".into(),
        artifact: "target".into(),
        executable: "/fixture/reader".into(),
        arguments: vec!["/fixture/reader".into(), "service".into()],
        environment: vec![],
        working_directory: "/fixture".into(),
        definition_sha256: "44".repeat(32),
    });
    manifest.validate().unwrap();
    let desktop:v2::broker::DesktopAuthority=serde_json::from_value(serde_json::json!({"target_id":"source-combined","os":"linux","broker_endpoint":{"kind":"unix_path","path":"/fixture/broker.sock"},"broker_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"helper_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"session_id":"source-combined","session_epoch":"66".repeat(32),"display":{"kind":"source_owned_headless"},"allowed_modes":["owned_executable"]})).unwrap();
    let core = v2::Digest::parse(&manifest.core_manifest_digest).unwrap();
    let integration = manifest.digest().unwrap();
    let binding = StoreBinding {
        binding: v2::BindingId::from_bytes([1; 16]),
        workspace_marker: v2::WorkspaceMarker::from_bytes([2; 16]),
        manifest_digest: v2::integration_manifest_digest(core, integration),
        os: v2::Os::Linux,
    };
    let make = |manifest: &v2::IntegrationManifest, desktop: v2::broker::DesktopAuthority| {
        LaunchAuthority::fixture_combined(
            NativeCommand::new("/fixture/helper"),
            StoreBinding {
                binding: binding.binding,
                manifest_digest: binding.manifest_digest,
                workspace_marker: binding.workspace_marker,
                os: binding.os,
            },
            v2::Digest::from_bytes([3; 32]),
            core,
            integration,
            service.clone(),
            desktop,
            manifest,
        )
    };
    let authority = make(&manifest, desktop.clone()).unwrap();
    assert!(authority.integration_service_authority().is_some());
    assert!(authority.integration_desktop_authority().is_some());
    let mut native = desktop.clone();
    native.allowed_modes = vec![v2::broker::DesktopMode::NativeActivation];
    assert!(make(&manifest, native).is_err());
    let mut missing = manifest.clone();
    missing.service_authority = None;
    assert!(make(&missing, desktop.clone()).is_err());
    let mut missing = manifest.clone();
    missing.desktop = None;
    assert!(make(&missing, desktop.clone()).is_err());
    let mut raw = integration_launch_json();
    raw["desktop_authority"] = serde_json::to_value(desktop).unwrap();
    assert!(
        LaunchAuthority::decode(&serde_json::to_vec(&raw).unwrap()).is_err(),
        "feature union cannot unseal raw Source authority"
    );
}
#[cfg(unix)]
#[test]
fn desktop_launch_admits_only_explicit_supplied_broker_ceiling_without_service() {
    let mut value = integration_launch_json();
    value["service_authority"] = serde_json::Value::Null;
    value["desktop_authority"] = serde_json::json!({
        "target_id":value["target_id"],"os":"linux","broker_endpoint":{"kind":"unix_path","path":"/fixture/broker.sock"},
        "broker_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},
        "helper_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},
        "session_id":"supplied-session","session_epoch":"66".repeat(32),
        "display":{"kind":"x11","display":":92","authority_file":"/fixture/auth","bus":"/fixture/bus.sock"},"allowed_modes":["owned_executable"]
    });
    let bytes = serde_json::to_vec(&value).unwrap();
    let launch = LaunchAuthority::decode(&bytes).expect("desktop-only supplied broker authority");
    assert!(launch.integration_service_authority().is_none());
    assert_eq!(
        launch.view().fingerprint,
        format!("{:x}", Sha256::digest(&bytes))
    );
    for key in [
        "broker_endpoint",
        "broker_principal",
        "helper_principal",
        "session_id",
        "display",
    ] {
        let mut changed = value.clone();
        changed["desktop_authority"]
            .as_object_mut()
            .unwrap()
            .remove(key);
        assert!(LaunchAuthority::decode(&serde_json::to_vec(&changed).unwrap()).is_err());
    }
    let mut changed = value;
    changed["desktop_authority"]["broker_endpoint"]["path"] = "unixexec:/bin/sh".into();
    assert!(LaunchAuthority::decode(&serde_json::to_vec(&changed).unwrap()).is_err());
}
#[cfg(unix)]
#[test]
fn integration_launch_commits_full_source_and_both_manifest_components() {
    let value = integration_launch_json();
    let bytes = serde_json::to_vec(&value).unwrap();
    let launch = LaunchAuthority::decode(&bytes).unwrap();
    assert_eq!(
        launch.view().fingerprint,
        format!("{:x}", Sha256::digest(&bytes))
    );
    assert!(matches!(
        launch.expected().profile,
        v2::HelperProfile::Integration { .. }
    ));
    for key in [
        "core_manifest_digest",
        "integration_manifest_digest",
        "manifest_digest",
    ] {
        let mut wrong = value.clone();
        wrong[key] = "05".repeat(32).into();
        assert!(LaunchAuthority::decode(&serde_json::to_vec(&wrong).unwrap()).is_err());
    }
    for key in ["helper_profile", "service_authority", "desktop_authority"] {
        let mut missing = value.clone();
        missing.as_object_mut().unwrap().remove(key);
        assert!(LaunchAuthority::decode(&serde_json::to_vec(&missing).unwrap()).is_err());
    }
    let mut wrong = value;
    wrong["service_authority"]["bus_endpoint"]["path"] = "unixexec:/bin/false".into();
    assert!(LaunchAuthority::decode(&serde_json::to_vec(&wrong).unwrap()).is_err());
}
#[cfg(unix)]
#[test]
fn integration_context_cannot_replace_static_identity_or_add_unknown_fields() {
    let mut authority =
        LaunchAuthority::decode(&serde_json::to_vec(&integration_launch_json()).unwrap()).unwrap();
    let context = v2::IntegrationRuntimeContext {
        linux_gio_qualification: None,
        manager_platform: Some(v2::ManagerPlatform::Systemd),
        desktop_broker_header: None,
        installation_instance_digest: None,
        kind: "theseus_native_integration_context".into(),
        schema: 1,
        ownership_id: "original-owner".into(),
        binding: authority.binding.binding.to_hex(),
        manifest_digest: authority.binding.manifest_digest.to_hex(),
        workspace_marker: authority.binding.workspace_marker.to_hex(),
        provider_epoch: authority.epoch.to_hex(),
        integration_manifest_digest: authority.integration_components().unwrap().1.to_hex(),
        manager_unique_name: Some(":1.42".into()),
        namespace_receipt: Some("private-context-original".into()),
    };
    authority
        .bind_integration_context(&serde_json::to_string(&context).unwrap())
        .unwrap();
    assert_eq!(authority.integration_context(), Some(&context));
    let mut wrong = context.clone();
    wrong.binding = "09".repeat(16);
    assert!(
        authority
            .bind_integration_context(&serde_json::to_string(&wrong).unwrap())
            .is_err()
    );
    let mut unknown = serde_json::to_value(&context).unwrap();
    unknown["fallback_bus"] = "user".into();
    assert!(
        authority
            .bind_integration_context(&serde_json::to_string(&unknown).unwrap())
            .is_err()
    );
    assert_eq!(authority.integration_context(), Some(&context));
}
#[cfg(all(unix, feature = "native-integration-test-support"))]
#[test]
fn deferred_fixture_execution_rejects_replaced_helper_and_provider_before_effect() {
    use crate::target_authority::FixtureExecutablePins;
    use std::{os::unix::fs::PermissionsExt, sync::Arc};
    for replaced in ["helper", "provider"] {
        let root = tempfile::tempdir().unwrap();
        let artifacts = root.path().join("artifacts");
        std::fs::create_dir(&artifacts).unwrap();
        let marker = root.path().join("replacement-ran");
        let body = b"#!/bin/sh\necho replacement > \"$1\"\n";
        let helper = artifacts.join("helper");
        let provider = artifacts.join("provider");
        for path in [&helper, &provider] {
            std::fs::write(path, body).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o500)).unwrap();
        }
        std::fs::set_permissions(&artifacts, std::fs::Permissions::from_mode(0o500)).unwrap();
        let pins = Arc::new(
            FixtureExecutablePins::retain(
                [helper.clone(), provider.clone()]
                    .into_iter()
                    .map(|path| {
                        (
                            path,
                            body.len() as u64,
                            format!("{:x}", Sha256::digest(body)),
                        )
                    })
                    .collect(),
            )
            .unwrap(),
        );
        let mut provider_command = NativeCommand::new(&provider);
        provider_command.arg(&marker);
        provider_command.environment = ChildEnvironment::Replace(vec![]);
        let authority = LaunchAuthority::fixture_integration(
            NativeCommand::new(&helper),
            StoreBinding {
                binding: v2::BindingId::from_bytes([1; 16]),
                manifest_digest: v2::integration_manifest_digest(
                    v2::Digest::from_bytes([2; 32]),
                    v2::Digest::from_bytes([17; 32]),
                ),
                workspace_marker: v2::WorkspaceMarker::from_bytes([3; 16]),
                os: v2::Os::Linux,
            },
            v2::Digest::from_bytes([4; 32]),
            v2::Digest::from_bytes([2; 32]),
            v2::Digest::from_bytes([17; 32]),
            serde_json::from_value(integration_launch_json()["service_authority"].clone()).unwrap(),
        )
        .unwrap()
        .with_fixture_provider("pin-test".into(), provider_command)
        .unwrap()
        .with_fixture_executable_pins(pins)
        .unwrap();
        assert!(authority.provider_command().is_ok());
        assert!(authority.provider_factory().is_ok());
        std::fs::set_permissions(&artifacts, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::rename(
            artifacts.join(replaced),
            root.path().join("retained-original-image"),
        )
        .unwrap();
        std::fs::write(artifacts.join(replaced), body).unwrap();
        std::fs::set_permissions(
            artifacts.join(replaced),
            std::fs::Permissions::from_mode(0o500),
        )
        .unwrap();
        std::fs::set_permissions(&artifacts, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert!(
            authority.provider_factory().is_err(),
            "deferred provider setup must not consume replacement image"
        );
        let preparation = authority.prepare();
        assert!(
            preparation.error().is_some() && preparation.command.is_none(),
            "deferred helper startup must refuse changed image authority"
        );
        assert!(!marker.exists(), "replacement code never executes");
        std::fs::set_permissions(&artifacts, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}
#[test]
fn ssh_argv_has_one_literal_subsystem_and_no_config_or_agent_fallback() {
    let p = std::path::Path::new(if cfg!(windows) {
        r"\\?\C:\fixture"
    } else {
        "/fixture"
    });
    let command = openssh_subsystem_argv(
        p,
        "target.example",
        22,
        "helper",
        "theseus",
        SshMaterialPaths {
            identity: p,
            host_keys: p,
            empty_global: p,
        },
    )
    .unwrap();
    let args = command
        .args
        .iter()
        .map(|s| s.to_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(&args[..6], ["-F", "none", "-T", "-s", "-S", "none"]);
    assert_eq!(&args[args.len() - 2..], ["target.example", "theseus"]);
    assert!(args.contains(&"IdentityAgent=none"));
    let expected_identity = if cfg!(windows) {
        "C:/fixture"
    } else {
        "/fixture"
    };
    let identity_option = args.iter().position(|arg| *arg == "-i").unwrap();
    assert_eq!(args[identity_option + 1], expected_identity);
    assert!(args.contains(&format!("UserKnownHostsFile={expected_identity}").as_str()));
    assert!(
        openssh_subsystem_argv(
            p,
            "-oProxyCommand=x",
            22,
            "helper",
            "theseus",
            SshMaterialPaths {
                identity: p,
                host_keys: p,
                empty_global: p
            }
        )
        .is_err()
    );
    assert!(
        openssh_subsystem_argv(
            p,
            "target",
            22,
            "helper",
            "name;command",
            SshMaterialPaths {
                identity: p,
                host_keys: p,
                empty_global: p
            }
        )
        .is_err()
    );
    assert!(
        openssh_subsystem_argv(
            p,
            "target",
            22,
            "helper",
            "theseus",
            SshMaterialPaths {
                identity: std::path::Path::new("/tmp/%h"),
                host_keys: p,
                empty_global: p
            }
        )
        .is_err()
    );
}
