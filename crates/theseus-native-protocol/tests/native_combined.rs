//! Pure combined native DTO/codec contracts; no SDK, service or child is used.
use theseus_native_protocol::v2;
#[path = "support/linux_gio_qualification.rs"]
mod support;
fn combined() -> v2::IntegrationManifest {
    let (native, _, _) = support::fixture();
    let mut value = serde_json::to_value(native).unwrap();
    value["service_authority"] = serde_json::json!({"kind":"systemd","slot":"fixture_service","scope":"user","bus_endpoint":{"kind":"unix_path","path":"/fixture/private-bus"},"qualification":{"manager":"systemd","version":"fixture-v1","api_profile":"systemd_dbus_private_v1","features":["unit_properties","start_stop","manager_reload"],"receipt_sha256":"88".repeat(32)},"namespace":{"kind":"exclusive_owned_registration","slot":"fixture_service","unit_load_roots":["/fixture/units"],"dropins":"forbidden","provider_context":"parser-only-owned-fixture"},"manager_context":{"kind":"whole_private","slot":"fixture_service","sut_allowed_actions":["daemon_reload"],"rehearsal_allowed_actions":[],"provider_receipt_required":true},"expected_peer_uid":1000,"expected_manager_uid":1000,"definition_root":"/fixture/units","unit":"fixture.service","managed_by":"installer","sut_effects":["registration","configuration","removal","daemon_reload"],"allowed_actions":["inspect","start","stop"]});
    value["definitions"] = serde_json::json!([{"role":"configured","artifact":"target","executable":"/fixture/service","arguments":["/fixture/service"],"environment":[],"working_directory":"/fixture","definition_sha256":"aa".repeat(32)}]);
    serde_json::from_value(value).unwrap()
}
#[test]
fn native_combined_keeps_closed_manifest_and_actual_description_wire() {
    let manifest = combined();
    manifest
        .validate()
        .expect("native and service share one committed manifest");
    let capabilities = v2::IntegrationCapabilities::for_manifest(&manifest).unwrap();
    assert_eq!(
        capabilities.names(),
        &[
            "inspect_service",
            "service_start",
            "service_stop",
            "inspect_desktop",
            "native_activation",
            "request_close"
        ]
    );
    assert_eq!(
        v2::IntegrationCapabilities::Combined.names(),
        &[
            "inspect_service",
            "service_start",
            "service_stop",
            "inspect_desktop",
            "owned_executable",
            "request_close"
        ]
    );
    let core = v2::Digest::parse(&manifest.core_manifest_digest).unwrap();
    let integration = manifest.digest().unwrap();
    let native = v2::NativeDescription {
        binding: v2::BindingId::from_bytes([1; 16]),
        manifest_digest: v2::integration_manifest_digest(core, integration),
        workspace_marker: v2::WorkspaceMarker::from_bytes([2; 16]),
        os: v2::Os::Linux,
        helper_principal: v2::Principal::Posix {
            effective_uid: 1000,
            effective_gid: 1000,
        },
        programs: v2::ProgramList::new(
            [
                "check_base",
                "check_data",
                "check_target",
                "configure",
                "exercise",
                "install_base",
                "install_target",
                "uninstall",
            ]
            .iter()
            .map(|p| v2::ProgramId::new(p).unwrap())
            .collect(),
        )
        .unwrap(),
        capabilities: v2::NativeCapabilities,
        provider_epoch: v2::Digest::from_bytes([3; 32]),
        artifact_commitment: v2::Digest::from_bytes([4; 32]),
        build: v2::Provenance::Unavailable {
            reason: "pure codec fixture".into(),
        },
        os_build: v2::Provenance::Unavailable {
            reason: "pure codec fixture".into(),
        },
    };
    let envelope = v2::Envelope::new(
        v2::SessionId::from_bytes([1; 16]),
        1,
        v2::Message::Response {
            reply_to: 1,
            result: v2::Reply::IntegrationDescription {
                description: Box::new(v2::IntegrationDescription {
                    manifest: Box::new(manifest.clone()),
                    native,
                    core_manifest_digest: core,
                    integration_manifest_digest: integration,
                    capabilities,
                    namespace_receipt_digest: v2::Digest::from_bytes([5; 32]),
                    context_digest: v2::Digest::from_bytes([6; 32]),
                    ownership_id: "pure-combined".into(),
                }),
            },
        },
    )
    .unwrap();
    let encoded = v2::encode(&envelope).unwrap();
    assert_eq!(
        v2::Decoder::new().feed(&encoded).unwrap().frame,
        Some(envelope)
    );
    let mut different = manifest.clone();
    different.definitions[0].definition_sha256 = "bb".repeat(32);
    assert_ne!(different.digest().unwrap(), integration);
    let mut overlap = manifest.clone();
    overlap.desktop.as_mut().unwrap().slot =
        overlap.service_authority.as_ref().unwrap().slot().clone();
    overlap.artifact_slots[0].slot = overlap.desktop.as_ref().unwrap().slot.clone();
    assert!(overlap.validate().is_err(), "combined slots cannot alias");
    let mut wrong = manifest;
    wrong
        .desktop
        .as_mut()
        .unwrap()
        .allowed_modes
        .push(v2::broker::DesktopMode::OwnedExecutable);
    assert!(
        wrong.validate().is_err(),
        "combined cannot widen native mode ceiling"
    );
}
