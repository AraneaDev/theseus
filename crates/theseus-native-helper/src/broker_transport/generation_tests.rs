use super::*;
use theseus_native_protocol::v2::{
    self,
    broker::*,
    ledger::{Entry, Record},
};
// Pure serialized declarations for generation selection: no runtime/session admission.
// These Linux DTOs intentionally do not claim a platform parser on the test host.
fn declaration(value: serde_json::Value) -> BrokerServerConfig {
    let config: BrokerServerConfig = serde_json::from_value(value).unwrap();
    config.manifest.validate().unwrap();
    config.header.validate().unwrap();
    assert_eq!(
        config.header.manifest_digest,
        v2::integration_manifest_digest(
            v2::Digest::parse(&config.manifest.core_manifest_digest).unwrap(),
            config.manifest.digest().unwrap(),
        )
        .to_hex(),
    );
    config
}
fn config() -> BrokerServerConfig {
    let mut value = super::native_routing_tests::native_config();
    value["manifest"]
        .as_object_mut()
        .unwrap()
        .remove("desktop_native");
    value["manifest"]["desktop"]["allowed_modes"] = serde_json::json!(["owned_executable"]);
    value["manifest"]["desktop_capabilities"] =
        serde_json::json!(["inspect_desktop", "owned_executable", "request_close"]);
    value["header"]["allowed_modes"] = serde_json::json!(["owned_executable"]);
    let manifest: v2::IntegrationManifest =
        serde_json::from_value(value["manifest"].clone()).unwrap();
    let full = v2::integration_manifest_digest(
        v2::Digest::parse(&manifest.core_manifest_digest).unwrap(),
        manifest.digest().unwrap(),
    );
    value["header"]["manifest_digest"] = serde_json::json!(full.to_hex());
    declaration(value)
}
fn witness(program: &str, n: u8) -> DesktopSutRunWitness {
    let lease = v2::LeaseId::from_bytes([1; 16]);
    let operation = v2::OperationId::from_bytes([n; 16]);
    let program = v2::ProgramId::new(program).unwrap();
    let request = v2::Request::Run {
        lease,
        operation,
        program: program.clone(),
    };
    DesktopSutRunWitness {
        lease: lease.to_hex(),
        operation: operation.to_hex(),
        request_digest: v2::request_digest(&request).unwrap().to_hex(),
        program,
    }
}
fn request(
    config: &BrokerServerConfig,
    n: u8,
    transition: Option<DesktopActivationTransition>,
) -> DesktopRequest {
    let d = config.manifest.desktop.as_ref().unwrap();
    let mut request = DesktopRequest {
        helper_lease: "01".repeat(16),
        helper_operation: format!("{n:02x}").repeat(16),
        broker_lease: "03".repeat(16),
        binding_digest: config.header.digest().unwrap().to_hex(),
        activation_nonce: format!("{:02x}", n + 1).repeat(16),
        slot: d.slot.clone(),
        prior_binding_digest: "05".repeat(32),
        action: DesktopAction::Activate {
            owned_lifetime: None,
            transition,
            mode: DesktopMode::OwnedExecutable,
            entry_digest: d.entry_digest.clone(),
            expectation_digest: d.expectation_digest.clone(),
            close_binding: CooperativeCloseBinding {
                operation: format!("{:02x}", n + 2).repeat(16),
                action_digest: "99".repeat(32),
                nonce: format!("{:02x}", n + 3).repeat(16),
            },
        },
    };
    let digest = request.close_request().unwrap().digest().unwrap().to_hex();
    if let DesktopAction::Activate { close_binding, .. } = &mut request.action {
        close_binding.action_digest = digest;
    }
    request.validate().unwrap();
    request
}
fn history(config: &BrokerServerConfig, retired: bool) -> (BrokerLedgerView, DesktopRequest) {
    let tuple = config
        .manifest
        .desktop_definition(&v2::DesktopDefinitionKey::CONFIGURED_TARGET)
        .unwrap();
    let original = request(
        config,
        20,
        Some(DesktopActivationTransition {
            definition: tuple.key,
            tuple_digest: tuple.digest().unwrap().to_hex(),
            sut: witness("configure", 10),
            previous_activation: None,
        }),
    );
    let mut entries = vec![
        Entry::IntegrationBrokerHeader {
            header: config.header.clone(),
        },
        Entry::LeaseReserved {
            intent: v2::IntentId::from_bytes([2; 16]),
            lease: v2::LeaseId::from_bytes([3; 16]),
        },
        Entry::Broker {
            event: BrokerEvent::Accepted {
                request: original.clone(),
                request_digest: original.digest().unwrap().to_hex(),
            },
        },
        Entry::Broker {
            event: BrokerEvent::EffectRecorded {
                operation: original.helper_operation.clone(),
                receipt: BrokerEffectReceipt {
                    evidence: DesktopEvidence::OwnedExecutable,
                    acknowledged: true,
                    receipt_digest: original.digest().unwrap().to_hex(),
                },
            },
        },
    ];
    if retired {
        entries.push(Entry::Broker {
            event: BrokerEvent::LifetimeRecorded {
                operation: original.helper_operation.clone(),
                primary: BrokerPrimary::Exited,
                descendants: BrokerDescendants::Joined,
                cleanup_complete: true,
            },
        });
    }
    let records = entries
        .into_iter()
        .enumerate()
        .map(|(i, e)| Record::new(i as u16, e).unwrap())
        .collect::<Vec<_>>();
    (BrokerLedgerView::replay(&records).unwrap(), original)
}
fn next(config: &BrokerServerConfig, old: &DesktopRequest) -> DesktopActivationTransition {
    let tuple = config
        .manifest
        .desktop_definition(&v2::DesktopDefinitionKey::CONFIGURED_TARGET)
        .unwrap();
    DesktopActivationTransition {
        definition: tuple.key,
        tuple_digest: tuple.digest().unwrap().to_hex(),
        sut: witness("install_target", 30),
        previous_activation: Some(old.identity().unwrap()),
    }
}
#[test]
fn inspection_is_read_only_and_does_not_consume_next_generation() {
    let config = config();
    let (view, old) = history(&config, true);
    let transition = next(&config, &old);
    let count = view.accepted_count();
    let continuity = Some(old.identity().unwrap());
    for _ in 0..2 {
        let selected =
            generation::inspection(&config, &view, Some(&transition), continuity.as_ref()).unwrap();
        assert_eq!(selected.key, transition.definition);
    }
    assert_eq!(view.accepted_count(), count);
    let request = request(&config, 40, Some(transition));
    assert!(generation::activation(&config, &view, &request, continuity.as_ref()).is_ok());
}
#[test]
fn new_generation_refuses_unknown_lost_or_missing_original_continuity() {
    let config = config();
    let (view, old) = history(&config, false);
    let transition = next(&config, &old);
    assert!(
        generation::inspection(
            &config,
            &view,
            Some(&transition),
            Some(&old.identity().unwrap())
        )
        .is_err()
    );
    let (view, old) = history(&config, true);
    let transition = next(&config, &old);
    assert!(generation::inspection(&config, &view, Some(&transition), None).is_err());
    let request = request(&config, 40, Some(transition));
    assert!(generation::activation(&config, &view, &request, None).is_err());
}
#[test]
fn generation_selection_requires_exact_tuple_and_previous_identity() {
    let config = config();
    let (view, old) = history(&config, true);
    let mut transition = next(&config, &old);
    let previous = old.identity().unwrap();
    transition.tuple_digest = "ff".repeat(32);
    assert!(generation::inspection(&config, &view, Some(&transition), Some(&previous)).is_err());
    let mut transition = next(&config, &old);
    transition
        .previous_activation
        .as_mut()
        .unwrap()
        .helper_operation = "ee".repeat(16);
    assert!(generation::inspection(&config, &view, Some(&transition), Some(&previous)).is_err());
}
pub(crate) fn combined_manifest_fixture() -> v2::IntegrationManifest {
    // Serial parser commitments only: no manager qualification capability minted.
    let owned = config();
    let mut manifest = serde_json::to_value(&owned.manifest).unwrap();
    manifest["service_authority"] = serde_json::json!({"kind":"systemd","slot":"fixture_service","scope":"user","bus_endpoint":{"kind":"unix_path","path":"/fixture/private-bus"},"qualification":{"manager":"systemd","version":"fixture-v1","api_profile":"systemd_dbus_private_v1","features":["unit_properties","start_stop","manager_reload"],"receipt_sha256":"88".repeat(32)},"namespace":{"kind":"exclusive_owned_registration","slot":"fixture_service","unit_load_roots":["/fixture/units"],"dropins":"forbidden","provider_context":"parser-only-owned-fixture"},"manager_context":{"kind":"whole_private","slot":"fixture_service","sut_allowed_actions":["daemon_reload"],"rehearsal_allowed_actions":[],"provider_receipt_required":true},"expected_peer_uid":1000,"expected_manager_uid":1000,"definition_root":"/fixture/units","unit":"fixture.service","managed_by":"installer","sut_effects":["registration","configuration","removal","daemon_reload"],"allowed_actions":["inspect","start","stop"]});
    manifest["definitions"] = serde_json::json!([{"role":"configured","artifact":"target","executable":"/fixture/service","arguments":["/fixture/service"],"environment":[],"working_directory":"/fixture","definition_sha256":"aa".repeat(32)}]);
    let manifest: v2::IntegrationManifest = serde_json::from_value(manifest).unwrap();
    manifest.validate().unwrap();
    manifest
}
#[test]
fn combined_owned_declaration_preserves_complete_manifest_and_header() {
    let owned = config();
    let manifest = combined_manifest_fixture();
    assert_eq!(
        v2::IntegrationCapabilities::for_manifest(&manifest).unwrap(),
        v2::IntegrationCapabilities::Combined
    );
    let mut header = owned.header.clone();
    header.manifest_digest = v2::integration_manifest_digest(
        v2::Digest::parse(&manifest.core_manifest_digest).unwrap(),
        manifest.digest().unwrap(),
    )
    .to_hex();
    let value = serde_json::json!({"kind":"theseus_native_desktop_broker","schema":1,"profile":"native_integration_broker_v1","ledger_root":"/fixture/ledger","workspace_root":"/fixture/workspace","manifest":manifest,"header":header});
    let decoded = declaration(value);
    assert_eq!(decoded.manifest, manifest);
    assert_eq!(decoded.header, header);
}
#[test]
fn generation_binding_changes_with_successful_sut_witness_and_keeps_legacy_bytes() {
    let config = config();
    let (view, old) = history(&config, true);
    let _ = view;
    let tuple = config
        .manifest
        .desktop_definition(&v2::DesktopDefinitionKey::CONFIGURED_TARGET)
        .unwrap();
    let a = old.transition().unwrap();
    let b = next(&config, &old);
    let base = "ab".repeat(32);
    assert_eq!(
        generation::inspection_binding(&base, &tuple, None).unwrap(),
        base
    );
    assert_ne!(
        generation::inspection_binding(&base, &tuple, Some(a)).unwrap(),
        generation::inspection_binding(&base, &tuple, Some(&b)).unwrap()
    );
}
#[test]
fn consumed_sut_reference_cannot_mint_a_new_activation() {
    let config = config();
    let (view, old) = history(&config, true);
    let mut transition = next(&config, &old);
    transition.sut = old.transition().unwrap().sut.clone();
    let request = request(&config, 40, Some(transition));
    assert!(
        generation::activation(&config, &view, &request, Some(&old.identity().unwrap())).is_err()
    );
}
#[test]
fn explicit_base_tuple_selection_never_changes_full_manifest_or_falls_back_to_target() {
    let mut config = config();
    let mut base = config
        .manifest
        .desktop_definition(&v2::DesktopDefinitionKey::CONFIGURED_TARGET)
        .unwrap();
    base.key = v2::DesktopDefinitionKey {
        role: v2::DesktopDefinitionRole::Base,
        artifact: v2::DesktopArtifactRole::Base,
    };
    base.readiness.role = "base".into();
    base.readiness.artifact = "base".into();
    base.readiness.version_literal = "base-version".into();
    base.readiness.artifact_sha256 = "bb".repeat(32);
    base.executable.sha256 = base.readiness.artifact_sha256.clone();
    base.desktop.executable_digest = base.executable.sha256.clone();
    base.desktop.entry_digest = "cc".repeat(32);
    base.desktop.expectation_digest = base.readiness.digest().unwrap().to_hex();
    base.validate().unwrap();
    config.manifest.desktop_definitions.push(base.clone());
    config.manifest.validate().unwrap();
    config.header.manifest_digest = v2::integration_manifest_digest(
        v2::Digest::parse(&config.manifest.core_manifest_digest).unwrap(),
        config.manifest.digest().unwrap(),
    )
    .to_hex();
    let before = config.manifest.digest().unwrap();
    let transition = DesktopActivationTransition {
        definition: base.key,
        tuple_digest: base.digest().unwrap().to_hex(),
        sut: witness("install_base", 10),
        previous_activation: None,
    };
    let view = BrokerLedgerView::replay(&[]).unwrap();
    let selected = generation::inspection(&config, &view, Some(&transition), None).unwrap();
    assert_eq!(selected, base);
    assert_eq!(config.manifest.digest().unwrap(), before);
    let request = request(&config, 40, Some(transition));
    assert!(
        generation::activation(&config, &view, &request, None).is_err(),
        "configured-target hashes must not activate base tuple"
    );
}
#[cfg(target_os = "linux")]
#[test]
fn native_combined_config_still_requires_original_linux_qualification() {
    let service = combined_manifest_fixture();
    let mut value = super::native_routing_tests::native_config();
    value["manifest"]["service_authority"] =
        serde_json::to_value(service.service_authority).unwrap();
    value["manifest"]["definitions"] = serde_json::to_value(service.definitions).unwrap();
    let manifest: v2::IntegrationManifest =
        serde_json::from_value(value["manifest"].clone()).unwrap();
    manifest.validate().unwrap();
    value["header"]["manifest_digest"] = serde_json::json!(
        v2::integration_manifest_digest(
            v2::Digest::parse(&manifest.core_manifest_digest).unwrap(),
            manifest.digest().unwrap()
        )
        .to_hex()
    );
    let error = BrokerServerConfig::from_bytes(&serde_json::to_vec(&value).unwrap())
        .err()
        .expect("original Linux qualification remains mandatory");
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    assert!(
        error
            .to_string()
            .contains("authenticated original SDK/application qualification bridge")
    );
}
