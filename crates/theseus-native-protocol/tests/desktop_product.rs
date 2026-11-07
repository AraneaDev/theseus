use theseus_native_protocol::v2::{self, broker::DesktopManifest};

fn desktop() -> serde_json::Value {
    serde_json::json!({
        "schema":1,"core_manifest_digest":"11".repeat(32),"slot":"desktop",
        "entry_root":"/fixture/entries","entry":"fixture.desktop",
        "namespace_policy":"exclusive_quiescent",
        "entry_digest":"22".repeat(32),"executable_digest":"33".repeat(32),
        "allowed_modes":["owned_executable"],
        "ready_endpoint":{"kind":"unix_path","path":"/fixture/ready.sock"},
        "close_endpoint":{"kind":"unix_path","path":"/fixture/close.sock"},
        "expectation_digest":readiness().digest().unwrap().to_hex()
    })
}
fn readiness() -> v2::ReadinessDefinitionExpectation {
    v2::ReadinessDefinitionExpectation {
        role: "configured".into(),
        artifact: "target".into(),
        artifact_length: 123,
        artifact_sha256: "33".repeat(32),
        version_literal: "source-v1".into(),
        data_sha256: "55".repeat(32),
        configuration_digest: "66".repeat(32),
    }
}
fn executable() -> serde_json::Value {
    serde_json::json!({"root":"/fixture","relative_path":"app","length":123,"sha256":"33".repeat(32),"arguments":["/fixture/app","desktop"],"environment":[],"working_directory":"/fixture"})
}
#[test]
fn desktop_only_integration_manifest_needs_no_service_or_manager() {
    let raw = serde_json::json!({
        "schema":1,"core_manifest_digest":"11".repeat(32),
        "desktop":desktop(),"desktop_executable":executable(),"desktop_readiness":readiness(),"definitions":[],"artifact_slots":[{"slot":"desktop","relative_path":"fixture.desktop","length":128,"sha256":"22".repeat(32)}],
        "readiness_protocol":"theseus_fixture_ready_v1","desktop_capabilities":["inspect_desktop","owned_executable","request_close"]
    });
    let manifest: v2::IntegrationManifest =
        serde_json::from_value(raw.clone()).expect("declared desktop-only profile");
    manifest
        .validate()
        .expect("desktop-only authority is complete");
    assert_eq!(serde_json::to_value(&manifest).unwrap(), raw);
    let mut missing = raw.clone();
    missing.as_object_mut().unwrap().remove("desktop");
    assert!(
        serde_json::from_value::<v2::IntegrationManifest>(missing)
            .map_or(true, |v| v.validate().is_err())
    );
    let mut fake = raw;
    fake["definitions"] = serde_json::json!([{}]);
    assert!(serde_json::from_value::<v2::IntegrationManifest>(fake).is_err());
}
#[test]
fn desktop_manifest_core_commitment_is_the_actual_integration_core() {
    let mut raw = serde_json::json!({
        "schema":1,"core_manifest_digest":"11".repeat(32),
        "desktop":desktop(),"desktop_executable":executable(),"desktop_readiness":readiness(),"definitions":[],"artifact_slots":[{"slot":"desktop","relative_path":"fixture.desktop","length":128,"sha256":"22".repeat(32)}],
        "readiness_protocol":"theseus_fixture_ready_v1","desktop_capabilities":["inspect_desktop","owned_executable","request_close"]
    });
    let desktop: DesktopManifest = serde_json::from_value(desktop()).unwrap();
    desktop.validate().unwrap();
    raw["desktop"]["core_manifest_digest"] = serde_json::json!("55".repeat(32));
    assert!(
        serde_json::from_value::<v2::IntegrationManifest>(raw)
            .unwrap()
            .validate()
            .is_err()
    );
}
#[test]
fn desktop_context_omits_every_manager_field_and_pins_explicit_original_session() {
    let manifest: v2::IntegrationManifest = serde_json::from_value(serde_json::json!({
        "schema":1,"core_manifest_digest":"11".repeat(32),"desktop":desktop(),"desktop_executable":executable(),"desktop_readiness":readiness(),"definitions":[],"artifact_slots":[{"slot":"desktop","relative_path":"fixture.desktop","length":128,"sha256":"22".repeat(32)}],
        "readiness_protocol":"theseus_fixture_ready_v1","desktop_capabilities":["inspect_desktop","owned_executable","request_close"]
    })).unwrap();
    let integration = manifest.digest().unwrap();
    let full =
        v2::integration_manifest_digest(v2::Digest::parse(&"11".repeat(32)).unwrap(), integration);
    let header = serde_json::json!({
        "profile":"native_integration_broker_v1","binding":"11".repeat(16),"manifest_digest":full.to_hex(),
        "workspace_marker":"33".repeat(16),"os":"linux","target_id":"fixture","ownership_id":"44".repeat(16),
        "provider_epoch":"55".repeat(32),"session_id":"supplied-session","session_epoch":"66".repeat(32),
        "broker_endpoint":{"kind":"unix_path","path":"/fixture/broker.sock"},
        "broker_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},
        "helper_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},
        "display":{"kind":"x11","display":":92","authority_file":"/fixture/auth","bus":"/fixture/bus.sock"},"allowed_modes":["owned_executable"]
    });
    let raw = serde_json::json!({
        "kind":"theseus_native_integration_context","schema":1,"ownership_id":"44".repeat(16),
        "binding":"11".repeat(16),"manifest_digest":full.to_hex(),"workspace_marker":"33".repeat(16),
        "provider_epoch":"55".repeat(32),"integration_manifest_digest":integration.to_hex(),"desktop_broker_header":header
    });
    let context: v2::IntegrationRuntimeContext =
        serde_json::from_value(raw.clone()).expect("desktop context without fabricated manager");
    manifest.validate_context(&context, v2::Os::Linux).unwrap();
    assert_eq!(serde_json::to_value(&context).unwrap(), raw);
    let mut partial = raw.clone();
    partial["namespace_receipt"] = serde_json::json!("fake-manager");
    assert!(
        serde_json::from_value::<v2::IntegrationRuntimeContext>(partial)
            .map_or(true, |v| v.validate().is_err())
    );
    let mut changed = raw;
    changed["desktop_broker_header"]["provider_epoch"] = serde_json::json!("77".repeat(32));
    assert!(
        serde_json::from_value::<v2::IntegrationRuntimeContext>(changed).map_or(true, |v| manifest
            .validate_context(&v, v2::Os::Linux)
            .is_err())
    );
}
#[test]
fn service_only_helper_header_cannot_accept_an_undeclared_desktop_bind() {
    use v2::{broker::*, ledger::*};
    let core = v2::Digest::from_bytes([1; 32]);
    let integration = v2::Digest::from_bytes([2; 32]);
    let full = v2::integration_manifest_digest(core, integration);
    let header: IntegrationBrokerHeader = serde_json::from_value(serde_json::json!({
        "profile":"native_integration_broker_v1","binding":"11".repeat(16),"manifest_digest":full.to_hex(),
        "workspace_marker":"33".repeat(16),"os":"linux","target_id":"fixture","ownership_id":"44".repeat(16),
        "provider_epoch":"55".repeat(32),"session_id":"supplied-session","session_epoch":"66".repeat(32),
        "broker_endpoint":{"kind":"unix_path","path":"/fixture/broker.sock"},
        "broker_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},
        "helper_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},
        "display":{"kind":"x11","display":":92","authority_file":"/fixture/auth","bus":"/fixture/bus.sock"},"allowed_modes":["owned_executable"]
    })).unwrap();
    let original = BrokerBind {
        ownership_id: header.ownership_id.clone(),
        helper_lease: "77".repeat(16),
        helper_operation: "88".repeat(16),
        slot: v2::ResourceSlot::new("desktop").unwrap(),
        broker_intent: "99".repeat(16),
        expected_header: header.clone(),
    };
    let records = vec![
        Record::new(
            0,
            Entry::IntegrationHeader {
                artifact_commitment: v2::Digest::from_bytes([3; 32]),
                binding: v2::BindingId::parse(&header.binding).unwrap(),
                manifest_digest: full,
                workspace_marker: v2::WorkspaceMarker::parse(&header.workspace_marker).unwrap(),
                os: v2::Os::Linux,
                provider_epoch: v2::Digest::parse(&header.provider_epoch).unwrap(),
                core_manifest_digest: core,
                integration_manifest_digest: integration,
                context_digest: v2::Digest::from_bytes([4; 32]),
                ownership_id: header.ownership_id.clone(),
            },
        )
        .unwrap(),
        Record::new(
            1,
            Entry::LeaseReserved {
                intent: v2::IntentId::from_bytes([5; 16]),
                lease: v2::LeaseId::parse(&original.helper_lease).unwrap(),
            },
        )
        .unwrap(),
        Record::new(
            2,
            Entry::Broker {
                event: BrokerEvent::BindAccepted {
                    request_digest: original.digest().unwrap().to_hex(),
                    original,
                },
            },
        )
        .unwrap(),
    ];
    assert!(
        LedgerView::replay(&records).is_err(),
        "a service-only helper must not gain an undeclared session capability"
    );
}

#[test]
fn desktop_authority_declaration_has_a_typed_ledger_record() {
    // This record must preserve the complete original manifest/context, rather
    // than laundering broker metadata through a service header.
    let _: fn(
        Box<v2::IntegrationManifest>,
        Box<v2::IntegrationRuntimeContext>,
    ) -> v2::ledger::Entry =
        |manifest, context| v2::ledger::Entry::DesktopAuthorityDeclared { manifest, context };
}

#[test]
fn desktop_owned_command_must_be_independently_committed_and_match_executable() {
    let raw = serde_json::json!({"schema":1,"core_manifest_digest":"11".repeat(32),"desktop":desktop(),"desktop_executable":executable(),"desktop_readiness":readiness(),"definitions":[],"artifact_slots":[{"slot":"desktop","relative_path":"fixture.desktop","length":128,"sha256":"22".repeat(32)}],"readiness_protocol":"theseus_fixture_ready_v1","desktop_capabilities":["inspect_desktop","owned_executable","request_close"]});
    let manifest: v2::IntegrationManifest = serde_json::from_value(raw.clone()).unwrap();
    manifest.validate().unwrap();
    for bad in ["../app", "/app", "nested/../../app"] {
        let mut changed = raw.clone();
        changed["desktop_executable"]["relative_path"] = serde_json::json!(bad);
        assert!(
            serde_json::from_value::<v2::IntegrationManifest>(changed)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    let mut changed = raw.clone();
    changed["desktop_executable"]["sha256"] = serde_json::json!("99".repeat(32));
    assert!(
        serde_json::from_value::<v2::IntegrationManifest>(changed)
            .unwrap()
            .validate()
            .is_err()
    );
    let mut missing = raw;
    missing
        .as_object_mut()
        .unwrap()
        .remove("desktop_executable");
    assert!(
        serde_json::from_value::<v2::IntegrationManifest>(missing)
            .unwrap()
            .validate()
            .is_err()
    );
}

#[test]
fn desktop_namespace_policy_is_required_exact_and_committed() {
    let mut missing = desktop();
    missing.as_object_mut().unwrap().remove("namespace_policy");
    assert!(
        serde_json::from_value::<DesktopManifest>(missing).is_err(),
        "missing namespace contract must refuse before effects"
    );
    for policy in [
        "",
        "immutable",
        "exclusive",
        "Exclusive_Quiescent",
        "exclusive_quiescent ",
    ] {
        let mut raw = desktop();
        raw["namespace_policy"] = policy.into();
        assert!(
            serde_json::from_value::<DesktopManifest>(raw).map_or(true, |m| m.validate().is_err()),
            "unsupported namespace contract {policy:?}"
        );
    }
    let manifest: DesktopManifest = serde_json::from_value(desktop()).unwrap();
    manifest.validate().unwrap();
    assert_eq!(
        serde_json::to_value(&manifest).unwrap()["namespace_policy"],
        "exclusive_quiescent"
    );
    assert!(manifest.digest().is_ok());
}
