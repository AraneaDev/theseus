//! Pure precommitted generation/SUT-boundary contracts; no native or process APIs.
use serde_json::{Value, json};
use theseus_native_protocol::v2::{self, broker::*};
fn manifest() -> Value {
    let ready = json!({"role":"configured","artifact":"target","artifact_length":123,"artifact_sha256":"33".repeat(32),"version_literal":"target","data_sha256":"44".repeat(32),"configuration_digest":"55".repeat(32)});
    let expectation: v2::ReadinessDefinitionExpectation =
        serde_json::from_value(ready.clone()).unwrap();
    json!({"schema":1,"core_manifest_digest":"11".repeat(32),"desktop":{"schema":1,"core_manifest_digest":"11".repeat(32),"slot":"desktop","entry_root":"/fixture/entries","namespace_policy":"exclusive_quiescent","entry":"fixture.desktop","entry_digest":"22".repeat(32),"executable_digest":"33".repeat(32),"allowed_modes":["owned_executable"],"ready_endpoint":{"kind":"unix_path","path":"/fixture/ready.sock"},"close_endpoint":{"kind":"unix_path","path":"/fixture/close.sock"},"expectation_digest":expectation.digest().unwrap().to_hex()},"desktop_executable":{"root":"/fixture","relative_path":"app","length":123,"sha256":"33".repeat(32),"arguments":["/fixture/app"],"environment":[],"working_directory":"/fixture"},"desktop_readiness":ready,"definitions":[],"artifact_slots":[{"slot":"desktop","relative_path":"fixture.desktop","length":128,"sha256":"22".repeat(32)}],"readiness_protocol":"theseus_fixture_ready_v1","desktop_capabilities":["inspect_desktop","owned_executable","request_close"]})
}
fn key(
    role: v2::DesktopDefinitionRole,
    artifact: v2::DesktopArtifactRole,
) -> v2::DesktopDefinitionKey {
    v2::DesktopDefinitionKey { role, artifact }
}
fn extra() -> v2::DesktopDefinition {
    let m: v2::IntegrationManifest = serde_json::from_value(manifest()).unwrap();
    let mut d = m
        .desktop_definition(&v2::DesktopDefinitionKey::CONFIGURED_TARGET)
        .unwrap();
    d.key = key(
        v2::DesktopDefinitionRole::Base,
        v2::DesktopArtifactRole::Base,
    );
    d.readiness.role = "base".into();
    d.readiness.artifact = "base".into();
    d.desktop.expectation_digest = d.readiness.digest().unwrap().to_hex();
    d
}
#[test]
fn all_definitions_are_committed_before_acquire_without_changing_legacy_bytes() {
    let mut m: v2::IntegrationManifest = serde_json::from_value(manifest()).unwrap();
    m.validate().unwrap();
    assert!(
        serde_json::to_value(&m)
            .unwrap()
            .get("desktop_definitions")
            .is_none()
    );
    let old = m.digest().unwrap();
    let d = extra();
    m.desktop_definitions.push(d.clone());
    m.validate().unwrap();
    assert_ne!(m.digest().unwrap(), old);
    assert_eq!(m.desktop_definition(&d.key).unwrap(), d);
    m.desktop_definitions.push(d);
    assert!(m.validate().is_err());
}
#[test]
fn generation_tuple_cannot_change_static_roots_channels_modes_or_claim_an_uncommitted_key() {
    let mut m: v2::IntegrationManifest = serde_json::from_value(manifest()).unwrap();
    m.desktop_definitions.push(extra());
    assert!(
        m.desktop_definition(&key(
            v2::DesktopDefinitionRole::Configured,
            v2::DesktopArtifactRole::Base
        ))
        .is_err()
    );
    for field in ["root", "channel", "mode", "length", "role"] {
        let mut bad = m.clone();
        let d = &mut bad.desktop_definitions[0];
        match field {
            "root" => d.executable.root = "/foreign".into(),
            "channel" => {
                d.desktop.close_endpoint = v2::LocalEndpoint::UnixPath {
                    path: "/foreign/close".into(),
                }
            }
            "mode" => d.desktop.allowed_modes = vec![DesktopMode::NativeActivation],
            "length" => d.entry_length = 0,
            _ => d.readiness.role = "configured".into(),
        };
        assert!(bad.validate().is_err(), "{field}");
    }
}
fn sut(program: &str, operation: u8) -> DesktopSutRunWitness {
    let lease = v2::LeaseId::from_bytes([7; 16]);
    let operation = v2::OperationId::from_bytes([operation; 16]);
    let program = v2::ProgramId::new(program).unwrap();
    let request = v2::Request::Run {
        lease,
        operation,
        program: program.clone(),
    };
    DesktopSutRunWitness {
        lease: lease.to_hex(),
        operation: operation.to_hex(),
        program,
        request_digest: v2::request_digest(&request).unwrap().to_hex(),
    }
}
#[test]
fn transition_witness_binds_exact_original_sut_run_and_role_program_mapping() {
    use v2::{DesktopArtifactRole as A, DesktopDefinitionKey as K, DesktopDefinitionRole as R};
    let base = sut("install_base", 8);
    base.validate().unwrap();
    assert_eq!(
        base.definition_after(None).unwrap(),
        Some(key(R::Base, A::Base))
    );
    let configured = sut("configure", 9);
    assert_eq!(
        configured
            .definition_after(Some(key(R::Base, A::Base)))
            .unwrap(),
        Some(key(R::Configured, A::Base))
    );
    let upgrade = sut("install_target", 10);
    assert_eq!(
        upgrade
            .definition_after(Some(key(R::Configured, A::Base)))
            .unwrap(),
        Some(K::CONFIGURED_TARGET)
    );
    let mut forged = base;
    forged.program = v2::ProgramId::new("configure").unwrap();
    assert!(forged.validate().is_err());
    assert!(sut("check_data", 11).validate().is_err());
}
#[test]
fn generation_inspection_is_additive_on_existing_wire_and_binds_observation_tuple() {
    let m: v2::IntegrationManifest = serde_json::from_value(manifest()).unwrap();
    let d = m
        .desktop_definition(&v2::DesktopDefinitionKey::CONFIGURED_TARGET)
        .unwrap();
    let generation = DesktopActivationTransition {
        definition: d.key,
        tuple_digest: d.digest().unwrap().to_hex(),
        sut: sut("configure", 9),
        previous_activation: None,
    };
    generation.validate().unwrap();
    let request = v2::Request::InspectDesktop {
        lease: v2::LeaseId::from_bytes([7; 16]),
        slot: v2::ResourceSlot::new("desktop").unwrap(),
        generation: Some(generation.clone()),
    };
    let frame = v2::Envelope::new(
        v2::SessionId::from_bytes([1; 16]),
        1,
        v2::Message::Request { request },
    )
    .unwrap();
    let bytes = v2::encode(&frame).unwrap();
    assert_eq!(v2::Decoder::new().feed(&bytes).unwrap().frame, Some(frame));
    let legacy = v2::Envelope::new(
        v2::SessionId::from_bytes([1; 16]),
        1,
        v2::Message::Request {
            request: v2::Request::InspectDesktop {
                lease: v2::LeaseId::from_bytes([7; 16]),
                slot: v2::ResourceSlot::new("desktop").unwrap(),
                generation: None,
            },
        },
    )
    .unwrap();
    assert!(!String::from_utf8_lossy(&v2::encode(&legacy).unwrap()[4..]).contains("generation"));
}
fn records_for_sut(
    witness: &DesktopSutRunWitness,
    outcome: v2::Outcome,
) -> Vec<v2::ledger::Record> {
    use v2::ledger::{Entry, Record};
    let lease = v2::LeaseId::parse(&witness.lease).unwrap();
    let operation = v2::OperationId::parse(&witness.operation).unwrap();
    vec![
        Record::new(
            0,
            Entry::Header {
                binding: v2::BindingId::from_bytes([1; 16]),
                manifest_digest: v2::Digest::from_bytes([2; 32]),
                workspace_marker: v2::WorkspaceMarker::from_bytes([3; 16]),
                os: v2::Os::Linux,
            },
        )
        .unwrap(),
        Record::new(
            1,
            Entry::LeaseReserved {
                intent: v2::IntentId::from_bytes([4; 16]),
                lease,
            },
        )
        .unwrap(),
        Record::new(
            2,
            Entry::OperationAccepted {
                lease,
                operation,
                request_digest: v2::Digest::parse(&witness.request_digest).unwrap(),
                operation_kind: v2::OperationKind::Run,
            },
        )
        .unwrap(),
        Record::new(
            3,
            Entry::OperationFinished {
                lease,
                operation,
                outcome,
            },
        )
        .unwrap(),
    ]
}
fn success() -> v2::Outcome {
    v2::Outcome::RunCompleted {
        reason: v2::RunReason::Natural,
        status: v2::NativeStatus::PosixExit { code: 0 },
        cleanup: v2::Cleanup::PosixLeaderReapedGroupTerminationRequested,
        stdout_bytes: 0,
        stderr_bytes: 0,
        truncated: false,
    }
}
#[test]
fn sut_witness_requires_own_exact_accepted_run_and_natural_zero_terminal() {
    let witness = sut("configure", 8);
    let records = records_for_sut(&witness, success());
    witness.matches_successful_terminal(&records).unwrap();
    assert!(witness.matches_successful_terminal(&records[..3]).is_err());
    for (reason, code) in [
        (v2::RunReason::CancelRequested, 0),
        (v2::RunReason::Deadline, 0),
        (v2::RunReason::Natural, 1),
    ] {
        let mut outcome = success();
        if let v2::Outcome::RunCompleted {
            reason: r, status, ..
        } = &mut outcome
        {
            *r = reason;
            *status = v2::NativeStatus::PosixExit { code };
        }
        assert!(
            witness
                .matches_successful_terminal(&records_for_sut(&witness, outcome))
                .is_err()
        );
    }
    let changed = sut("configure", 9);
    assert!(changed.matches_successful_terminal(&records).is_err());
}
#[test]
fn serialized_successful_sut_boundary_cannot_precede_own_terminal_or_be_reused() {
    let witness = sut("configure", 8);
    let mut records = records_for_sut(&witness, success());
    // Classic ledgers grant no desktop authority even for an otherwise successful Run.
    records.push(
        v2::ledger::Record::new(
            4,
            v2::ledger::Entry::Broker {
                event: BrokerEvent::HelperSutRunCompleted {
                    witness: witness.clone(),
                    definition: Some(v2::DesktopDefinitionKey::CONFIGURED_TARGET),
                },
            },
        )
        .unwrap(),
    );
    assert!(BrokerLedgerView::replay(&records).is_err());
}
fn append(records: &mut Vec<v2::ledger::Record>, entry: v2::ledger::Entry) {
    records.push(v2::ledger::Record::new(records.len() as u16, entry).unwrap());
}
fn event(records: &mut Vec<v2::ledger::Record>, event: BrokerEvent) {
    append(records, v2::ledger::Entry::Broker { event });
}
fn helper_graph() -> (
    v2::IntegrationManifest,
    IntegrationBrokerHeader,
    Vec<v2::ledger::Record>,
) {
    use v2::ledger::Entry;
    let m: v2::IntegrationManifest = serde_json::from_value(manifest()).unwrap();
    let integration = m.digest().unwrap();
    let core = v2::Digest::parse(&m.core_manifest_digest).unwrap();
    let full = v2::integration_manifest_digest(core, integration);
    let h:IntegrationBrokerHeader=serde_json::from_value(json!({"profile":"native_integration_broker_v1","binding":"11".repeat(16),"manifest_digest":full.to_hex(),"workspace_marker":"33".repeat(16),"os":"linux","target_id":"fixture","ownership_id":"44".repeat(16),"provider_epoch":"55".repeat(32),"session_id":"session","session_epoch":"66".repeat(32),"broker_endpoint":{"kind":"unix_path","path":"/fixture/broker.sock"},"broker_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"helper_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"display":{"kind":"x11","display":":92","authority_file":"/fixture/auth","bus":"/fixture/bus.sock"},"allowed_modes":["owned_executable"]})).unwrap();
    let context:v2::IntegrationRuntimeContext=serde_json::from_value(json!({"kind":"theseus_native_integration_context","schema":1,"ownership_id":h.ownership_id,"binding":h.binding,"manifest_digest":h.manifest_digest,"workspace_marker":h.workspace_marker,"provider_epoch":h.provider_epoch,"integration_manifest_digest":integration.to_hex(),"desktop_broker_header":h})).unwrap();
    let original = BrokerBind {
        ownership_id: h.ownership_id.clone(),
        helper_lease: v2::LeaseId::from_bytes([7; 16]).to_hex(),
        helper_operation: "ab".repeat(16),
        slot: v2::ResourceSlot::new("desktop").unwrap(),
        broker_intent: "ac".repeat(16),
        expected_header: h.clone(),
    };
    let receipt = BrokerBindingReceipt {
        request_digest: original.digest().unwrap().to_hex(),
        original: original.clone(),
        publication_scope: IntentPublicationScope::FileAndDirectorySynced,
        broker_lease: "99".repeat(16),
    };
    let mut r = Vec::new();
    append(
        &mut r,
        Entry::IntegrationHeader {
            artifact_commitment: v2::Digest::from_bytes([1; 32]),
            binding: v2::BindingId::parse(&h.binding).unwrap(),
            manifest_digest: full,
            workspace_marker: v2::WorkspaceMarker::parse(&h.workspace_marker).unwrap(),
            os: v2::Os::Linux,
            provider_epoch: v2::Digest::parse(&h.provider_epoch).unwrap(),
            core_manifest_digest: core,
            integration_manifest_digest: integration,
            context_digest: context.digest().unwrap(),
            ownership_id: h.ownership_id.clone(),
        },
    );
    append(
        &mut r,
        Entry::DesktopAuthorityDeclared {
            manifest: Box::new(m.clone()),
            context: Box::new(context),
        },
    );
    append(
        &mut r,
        Entry::LeaseReserved {
            intent: v2::IntentId::from_bytes([5; 16]),
            lease: v2::LeaseId::from_bytes([7; 16]),
        },
    );
    event(
        &mut r,
        BrokerEvent::BindAccepted {
            request_digest: original.digest().unwrap().to_hex(),
            original: original.clone(),
        },
    );
    event(
        &mut r,
        BrokerEvent::BindIntentPublished {
            operation: original.helper_operation,
            scope: receipt.publication_scope,
        },
    );
    event(&mut r, BrokerEvent::BindingCommitted { receipt });
    (m, h, r)
}
fn complete_sut(records: &mut Vec<v2::ledger::Record>, witness: &DesktopSutRunWitness) {
    use v2::ledger::Entry;
    append(
        records,
        Entry::OperationAccepted {
            lease: v2::LeaseId::parse(&witness.lease).unwrap(),
            operation: v2::OperationId::parse(&witness.operation).unwrap(),
            request_digest: v2::Digest::parse(&witness.request_digest).unwrap(),
            operation_kind: v2::OperationKind::Run,
        },
    );
    append(
        records,
        Entry::OperationFinished {
            lease: v2::LeaseId::parse(&witness.lease).unwrap(),
            operation: v2::OperationId::parse(&witness.operation).unwrap(),
            outcome: success(),
        },
    );
    event(
        records,
        BrokerEvent::HelperSutRunCompleted {
            witness: witness.clone(),
            definition: Some(v2::DesktopDefinitionKey::CONFIGURED_TARGET),
        },
    );
}
fn activate(
    m: &v2::IntegrationManifest,
    h: &IntegrationBrokerHeader,
    witness: DesktopSutRunWitness,
    op: u8,
    previous_activation: Option<BrokerOperationIdentity>,
) -> DesktopRequest {
    let tuple = m
        .desktop_definition(&v2::DesktopDefinitionKey::CONFIGURED_TARGET)
        .unwrap();
    let transition = DesktopActivationTransition {
        definition: tuple.key,
        tuple_digest: tuple.digest().unwrap().to_hex(),
        sut: witness,
        previous_activation,
    };
    let mut request = DesktopRequest {
        helper_lease: v2::LeaseId::from_bytes([7; 16]).to_hex(),
        helper_operation: v2::OperationId::from_bytes([op; 16]).to_hex(),
        broker_lease: "99".repeat(16),
        binding_digest: h.digest().unwrap().to_hex(),
        activation_nonce: v2::BindingId::from_bytes([op + 1; 16]).to_hex(),
        slot: tuple.desktop.slot.clone(),
        prior_binding_digest: "bb".repeat(32),
        action: DesktopAction::Activate {
            transition: Some(transition),
            mode: DesktopMode::OwnedExecutable,
            entry_digest: tuple.desktop.entry_digest,
            expectation_digest: tuple.desktop.expectation_digest,
            close_binding: CooperativeCloseBinding {
                operation: v2::OperationId::from_bytes([op + 2; 16]).to_hex(),
                action_digest: "00".repeat(32),
                nonce: v2::BindingId::from_bytes([op + 3; 16]).to_hex(),
            },
        },
    };
    let close = request.close_request().unwrap().digest().unwrap().to_hex();
    if let DesktopAction::Activate { close_binding, .. } = &mut request.action {
        close_binding.action_digest = close;
    }
    request
}
fn accept(records: &mut Vec<v2::ledger::Record>, request: &DesktopRequest) {
    event(
        records,
        BrokerEvent::HelperActionAccepted {
            request: request.clone(),
            request_digest: request.digest().unwrap().to_hex(),
        },
    );
    event(
        records,
        BrokerEvent::HelperTransitionAccepted {
            identity: request.identity().unwrap(),
            transition: request.transition().unwrap().clone(),
        },
    );
}
fn retired(request: &DesktopRequest) -> BrokerState {
    BrokerState {
        native_lifetime: None,
        effect: BrokerEffect::Receipt {
            receipt: BrokerEffectReceipt {
                evidence: DesktopEvidence::OwnedExecutable,
                acknowledged: true,
                receipt_digest: request.digest().unwrap().to_hex(),
            },
        },
        owner: BrokerOwner::Settled,
        primary: BrokerPrimary::Exited,
        descendants: BrokerDescendants::Joined,
        cleanup_complete: true,
        quarantined: false,
    }
}
#[test]
fn same_bytes_reinstall_needs_a_new_exact_sut_boundary_after_previous_original_retirement() {
    let (m, h, mut records) = helper_graph();
    let first_sut = sut("configure", 8);
    complete_sut(&mut records, &first_sut);
    let first = activate(&m, &h, first_sut, 20, None);
    accept(&mut records, &first);
    v2::ledger::LedgerView::replay(&records).unwrap();
    let next_sut = sut("install_target", 9);
    complete_sut(&mut records, &next_sut); // wrong order: original was not retired when this Run began
    event(
        &mut records,
        BrokerEvent::HelperStateObserved {
            identity: first.identity().unwrap(),
            state: retired(&first),
        },
    );
    let next = activate(&m, &h, next_sut, 30, Some(first.identity().unwrap()));
    accept(&mut records, &next);
    assert!(v2::ledger::LedgerView::replay(&records).is_err());
}
#[test]
fn helper_preserves_retired_original_and_accepts_fresh_same_tuple_after_new_sut_run() {
    let (m, h, mut records) = helper_graph();
    let first_sut = sut("configure", 8);
    complete_sut(&mut records, &first_sut);
    let first = activate(&m, &h, first_sut, 20, None);
    accept(&mut records, &first);
    event(
        &mut records,
        BrokerEvent::HelperStateObserved {
            identity: first.identity().unwrap(),
            state: retired(&first),
        },
    );
    let next_sut = sut("install_target", 9);
    complete_sut(&mut records, &next_sut);
    let next = activate(&m, &h, next_sut, 30, Some(first.identity().unwrap()));
    accept(&mut records, &next);
    v2::ledger::LedgerView::replay(&records).unwrap();
    let view = BrokerLedgerView::replay(&records).unwrap();
    assert_eq!(
        view.helper_state(&first.identity().unwrap()).unwrap(),
        Some(&retired(&first))
    );
    assert!(view.helper_transition(&next.identity().unwrap()).is_some());
}
#[test]
fn generation_state_cannot_cross_an_unpublished_helper_transition_boundary() {
    let (m, h, mut records) = helper_graph();
    let witness = sut("configure", 8);
    complete_sut(&mut records, &witness);
    let request = activate(&m, &h, witness, 20, None);
    event(
        &mut records,
        BrokerEvent::HelperActionAccepted {
            request: request.clone(),
            request_digest: request.digest().unwrap().to_hex(),
        },
    );
    event(
        &mut records,
        BrokerEvent::HelperStateObserved {
            identity: request.identity().unwrap(),
            state: retired(&request),
        },
    );
    assert!(v2::ledger::LedgerView::replay(&records).is_err());
}
fn wire_roundtrip(message: v2::Message) {
    let envelope = v2::Envelope::new(v2::SessionId::from_bytes([1; 16]), 1, message).unwrap();
    let bytes = v2::encode(&envelope).unwrap();
    assert_eq!(
        v2::Decoder::new().feed(&bytes).unwrap().frame,
        Some(envelope)
    );
}
#[test]
fn first_and_later_generation_envelopes_omit_absent_nested_native_and_previous_fields() {
    let (mut m, h, _) = helper_graph();
    m.desktop_definitions.push(extra());
    let core = v2::Digest::parse(&m.core_manifest_digest).unwrap();
    let integration = m.digest().unwrap();
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
            reason: "pure wire fixture".into(),
        },
        os_build: v2::Provenance::Unavailable {
            reason: "pure wire fixture".into(),
        },
    };
    let capabilities = v2::IntegrationCapabilities::for_manifest(&m).unwrap();
    wire_roundtrip(v2::Message::Response {
        reply_to: 1,
        result: v2::Reply::IntegrationDescription {
            description: Box::new(v2::IntegrationDescription {
                namespace_receipt_digest: v2::Digest::from_bytes([5; 32]),
                manifest: Box::new(m.clone()),
                context_digest: v2::Digest::from_bytes([6; 32]),
                ownership_id: "fixture-owned".into(),
                native,
                core_manifest_digest: core,
                integration_manifest_digest: integration,
                capabilities,
            }),
        },
    });
    let first = activate(&m, &h, sut("configure", 8), 20, None);
    assert!(first.transition().unwrap().previous_activation.is_none());
    wire_roundtrip(v2::Message::Request {
        request: v2::Request::DesktopAction {
            request: Box::new(first.clone()),
        },
    });
    let next = activate(
        &m,
        &h,
        sut("install_target", 9),
        30,
        Some(first.identity().unwrap()),
    );
    assert!(next.transition().unwrap().previous_activation.is_some());
    wire_roundtrip(v2::Message::Request {
        request: v2::Request::DesktopAction {
            request: Box::new(next),
        },
    });
    wire_roundtrip(v2::Message::Response {
        reply_to: 1,
        result: v2::Reply::DesktopObservation {
            lease: v2::LeaseId::from_bytes([7; 16]),
            observation: Box::new(DesktopObservation {
                generation: first.transition().cloned(),
                slot: first.slot.clone(),
                entry_present: true,
                entry_valid: true,
                entry_digest: Some("22".repeat(32)),
                executable_digest: Some("33".repeat(32)),
                session_header_digest: h.digest().unwrap().to_hex(),
                binding_digest: first.prior_binding_digest.clone(),
                complete: true,
            }),
        },
    });
}
#[test]
fn uninstall_sut_boundary_omits_absent_definition_in_actual_ledger_codec() {
    let record = v2::ledger::Record::new(
        1,
        v2::ledger::Entry::Broker {
            event: BrokerEvent::HelperSutRunCompleted {
                witness: sut("uninstall", 8),
                definition: None,
            },
        },
    )
    .unwrap();
    let bytes = v2::ledger::encode(&record).unwrap();
    let mut decoder = v2::ledger::Decoder::new();
    assert_eq!(decoder.feed(&bytes).unwrap().record, Some(record));
    assert!(
        !bytes
            .windows(b"\"definition\":null".len())
            .any(|window| window == b"\"definition\":null")
    );
}
#[test]
fn native_generations_keep_one_broker_startup_environment_and_qualified_sdk_namespace() {
    let mut m: v2::IntegrationManifest = serde_json::from_value(manifest()).unwrap();
    let native = v2::NativeDesktopDefinition::LinuxGio {
        bootstrap: v2::NativeBootstrap::TheseusFixtureReadyBootstrapV1,
        lifetime: NativeLifetimeRequirement::OwnedPrimaryExited,
        sdk: v2::LinuxGioQualification {
            glib_version: [2, 80, 0],
            native_abi: v2::NativeLinuxAbi::LinuxX86_64,
            helper_root: "/fixture/sdk".into(),
            helper_relative_path: "gio-launch-desktop".into(),
            helper: v2::NativeArtifactCommitment {
                length: 456,
                sha256: "66".repeat(32),
            },
            sdk_receipt_sha256: "77".repeat(32),
            helper_receipt_sha256: "88".repeat(32),
            application_receipt_sha256: "99".repeat(32),
            provenance: v2::NativeQualificationProvenance::ProviderBuildAttested,
        },
    };
    m.desktop_native = Some(native);
    m.desktop.as_mut().unwrap().allowed_modes = vec![DesktopMode::NativeActivation];
    m.desktop_capabilities = vec![
        "inspect_desktop".into(),
        "native_activation".into(),
        "request_close".into(),
    ];
    let mut additional = extra();
    additional.native = m.desktop_native.clone();
    additional.desktop.allowed_modes = vec![DesktopMode::NativeActivation];
    m.desktop_definitions.push(additional);
    m.validate().unwrap();
    for field in ["environment", "sdk_root"] {
        let mut bad = m.clone();
        if field == "environment" {
            bad.desktop_definitions[0].executable.environment = vec![v2::DesktopEnvironment {
                name: "A".into(),
                value: "foreign".into(),
            }];
        } else if let Some(v2::NativeDesktopDefinition::LinuxGio { sdk, .. }) =
            &mut bad.desktop_definitions[0].native
        {
            sdk.helper_root = "/foreign/sdk".into();
        }
        assert!(bad.validate().is_err(), "{field}");
    }
}
#[test]
fn corrupt_typed_generation_record_index_returns_error_instead_of_panicking() {
    let (_, _, mut records) = helper_graph();
    complete_sut(&mut records, &sut("configure", 8));
    let offset = records
        .iter()
        .position(|record| {
            matches!(
                record.entry(),
                v2::ledger::Entry::Broker {
                    event: BrokerEvent::HelperSutRunCompleted { .. }
                }
            )
        })
        .unwrap();
    // Record::new accepts this locally bounded index, but the public replay
    // must also validate its position before trusting it as a slice boundary.
    records[offset] = v2::ledger::Record::new(
        (v2::ledger::MAX_RECORDS - 1) as u16,
        records[offset].entry().clone(),
    )
    .unwrap();
    assert!(BrokerLedgerView::replay(&records).is_err());
}
