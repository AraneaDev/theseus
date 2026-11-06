use theseus_native_protocol::v2::{IntentId, LeaseId, broker::*, ledger::*};

fn header() -> IntegrationBrokerHeader {
    serde_json::from_value(serde_json::json!({
        "profile":"native_integration_broker_v1", "binding":"11".repeat(16),
        "manifest_digest":"22".repeat(32), "workspace_marker":"33".repeat(16),
        "os":"linux", "target_id":"owned-target", "ownership_id":"44".repeat(16),
        "provider_epoch":"55".repeat(32), "session_id":"supplied-session",
        "session_epoch":"66".repeat(32),
        "broker_endpoint":{"kind":"unix_path","path":"/fixture/broker.sock"},
        "broker_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},
        "helper_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},
        "display":{"kind":"x11","display":":92","authority_file":"/fixture/auth","bus":"/fixture/bus.sock"},
        "allowed_modes":["owned_executable"]
    })).unwrap()
}
fn activation(h: &IntegrationBrokerHeader) -> DesktopRequest {
    let mut request: DesktopRequest = serde_json::from_value(serde_json::json!({
        "helper_lease":"77".repeat(16), "helper_operation":"88".repeat(16),
        "broker_lease":"99".repeat(16), "binding_digest":h.digest().unwrap().to_hex(),
        "activation_nonce":"aa".repeat(16), "slot":"desktop", "prior_binding_digest":"bb".repeat(32),
        "action":{"kind":"activate","mode":"owned_executable","entry_digest":"cc".repeat(32),"expectation_digest":"dd".repeat(32),"close_binding":{"operation":"ee".repeat(16),"action_digest":"00".repeat(32),"nonce":"fe".repeat(16)}}
    })).unwrap();
    let close_digest = request.close_request().unwrap().digest().unwrap().to_hex();
    if let DesktopAction::Activate { close_binding, .. } = &mut request.action {
        close_binding.action_digest = close_digest;
    }
    request
}
fn records() -> (IntegrationBrokerHeader, DesktopRequest, Vec<Record>) {
    let h = header();
    let request = activation(&h);
    let records = vec![
        Record::new(0, Entry::IntegrationBrokerHeader { header: h.clone() }).unwrap(),
        Record::new(
            1,
            Entry::LeaseReserved {
                intent: IntentId::from_bytes([1; 16]),
                lease: LeaseId::parse(&request.broker_lease).unwrap(),
            },
        )
        .unwrap(),
        Record::new(
            2,
            Entry::Broker {
                event: BrokerEvent::Accepted {
                    request: request.clone(),
                    request_digest: request.digest().unwrap().to_hex(),
                },
            },
        )
        .unwrap(),
    ];
    (h, request, records)
}
#[test]
fn broker_header_and_original_request_are_canonical_strict_and_bound() {
    let (h, request, records) = records();
    h.validate().unwrap();
    request.validate().unwrap();
    for record in &records {
        let bytes = encode(record).unwrap();
        let mut decoder = Decoder::new();
        assert_eq!(decoder.feed(&bytes).unwrap().record.as_ref(), Some(record));
        decoder.finish().unwrap();
    }
    let mut changed = request.clone();
    changed.activation_nonce = "ef".repeat(16);
    assert!(changed.digest().is_err());
    let mut bad = serde_json::to_value(&h).unwrap();
    bad["broker_endpoint"]["path"] = serde_json::json!("unixexec:/bin/sh");
    assert!(
        serde_json::from_value::<IntegrationBrokerHeader>(bad)
            .unwrap()
            .validate()
            .is_err()
    );
}
#[test]
fn accepted_broker_restart_is_unknown_and_never_fresh_activation() {
    let (_, request, mut records) = records();
    LedgerView::replay(&records).unwrap();
    let view = BrokerLedgerView::replay(&records).unwrap();
    assert_eq!(view.query(&request).unwrap().effect, BrokerEffect::Unknown);
    assert_eq!(view.query(&request).unwrap().owner, BrokerOwner::Lost);
    let mut changed = request.clone();
    changed.prior_binding_digest = "ee".repeat(32);
    assert!(view.query(&changed).is_err());
    records.push(
        Record::new(
            3,
            Entry::Broker {
                event: BrokerEvent::Quarantined {
                    operation: request.helper_operation.clone(),
                },
            },
        )
        .unwrap(),
    );
    let view = BrokerLedgerView::replay(&records).unwrap();
    assert_eq!(view.query(&request).unwrap().effect, BrokerEffect::Unknown);
    assert_eq!(view.accepted_count(), 1);
}
#[test]
fn terminal_effect_receipt_is_independent_of_lost_primary_and_tree() {
    let (_, request, mut records) = records();
    records.push(
        Record::new(
            3,
            Entry::Broker {
                event: BrokerEvent::EffectRecorded {
                    operation: request.helper_operation.clone(),
                    receipt: BrokerEffectReceipt {
                        evidence: DesktopEvidence::OwnedExecutable,
                        acknowledged: true,
                        receipt_digest: "ff".repeat(32),
                    },
                },
            },
        )
        .unwrap(),
    );
    let view = BrokerLedgerView::replay(&records).unwrap();
    let state = view.query(&request).unwrap();
    assert!(matches!(state.effect, BrokerEffect::Receipt { .. }));
    assert_eq!(state.owner, BrokerOwner::Lost);
    assert_eq!(state.primary, BrokerPrimary::Unknown);
    assert_eq!(state.descendants, BrokerDescendants::Unknown);
    assert!(!state.cleanup_complete);
    records.push(
        Record::new(
            4,
            Entry::LeaseClosed {
                lease: LeaseId::parse(&request.broker_lease).unwrap(),
                disposition: Disposition::AwaitingExternalReset,
            },
        )
        .unwrap(),
    );
    assert!(LedgerView::replay(&records).is_err());
}
#[test]
fn close_policy_is_committed_before_activation_and_changes_refuse() {
    let (_, request, records) = records();
    let close = request.close_request().unwrap();
    close.validate().unwrap();
    let DesktopAction::Activate { close_binding, .. } = &request.action else {
        unreachable!()
    };
    assert_eq!(
        close.digest().unwrap().to_hex(),
        close_binding.action_digest
    );
    let mut changed = request.clone();
    if let DesktopAction::Activate { close_binding, .. } = &mut changed.action {
        close_binding.nonce = "12".repeat(16);
    }
    assert!(changed.validate().is_err());
    let mut valid = records.clone();
    valid.push(
        Record::new(
            3,
            Entry::Broker {
                event: BrokerEvent::Accepted {
                    request_digest: close.digest().unwrap().to_hex(),
                    request: close.clone(),
                },
            },
        )
        .unwrap(),
    );
    LedgerView::replay(&valid).unwrap();
    let mut changed = close.clone();
    changed.prior_binding_digest = "12".repeat(32);
    let mut invalid = records;
    invalid.push(
        Record::new(
            3,
            Entry::Broker {
                event: BrokerEvent::Accepted {
                    request_digest: changed.digest().unwrap().to_hex(),
                    request: changed,
                },
            },
        )
        .unwrap(),
    );
    assert!(LedgerView::replay(&invalid).is_err());
}
#[test]
fn classic_profiles_refuse_broker_events_without_widening_native_authority() {
    let (_, _, mut records) = records();
    records[0] = Record::new(
        0,
        Entry::Header {
            binding: theseus_native_protocol::v2::BindingId::from_bytes([1; 16]),
            manifest_digest: theseus_native_protocol::v2::Digest::from_bytes([2; 32]),
            workspace_marker: theseus_native_protocol::v2::WorkspaceMarker::from_bytes([3; 16]),
            os: theseus_native_protocol::v2::Os::Linux,
        },
    )
    .unwrap();
    assert!(LedgerView::replay(&records).is_err());
}
#[test]
fn helper_binding_mapping_requires_original_intent_and_durable_publication_scope() {
    use theseus_native_protocol::v2::{BindingId, Digest, Os, WorkspaceMarker};
    let mut h = header();
    let core = Digest::from_bytes([1; 32]);
    let readiness:theseus_native_protocol::v2::ReadinessDefinitionExpectation=serde_json::from_value(serde_json::json!({"role":"configured","artifact":"target","artifact_length":123,"artifact_sha256":"33".repeat(32),"version_literal":"source-v1","data_sha256":"55".repeat(32),"configuration_digest":"66".repeat(32)})).unwrap();
    let manifest: theseus_native_protocol::v2::IntegrationManifest = serde_json::from_value(serde_json::json!({
        "schema":1,"core_manifest_digest":core.to_hex(),"desktop":{
            "schema":1,"core_manifest_digest":core.to_hex(),"slot":"desktop","entry_root":"/fixture/entries","namespace_policy":"exclusive_quiescent","entry":"fixture.desktop","entry_digest":"22".repeat(32),"executable_digest":"33".repeat(32),"allowed_modes":["owned_executable"],"ready_endpoint":{"kind":"unix_path","path":"/fixture/ready.sock"},"close_endpoint":{"kind":"unix_path","path":"/fixture/close.sock"},"expectation_digest":readiness.digest().unwrap().to_hex()},
        "desktop_readiness":readiness,"desktop_executable":{"root":"/fixture","relative_path":"app","length":123,"sha256":"33".repeat(32),"arguments":["/fixture/app"],"environment":[],"working_directory":"/fixture"},
        "definitions":[],"artifact_slots":[{"slot":"desktop","relative_path":"fixture.desktop","length":128,"sha256":"22".repeat(32)}],"readiness_protocol":"theseus_fixture_ready_v1","desktop_capabilities":["inspect_desktop","owned_executable","request_close"]
    })).unwrap();
    let integration = manifest.digest().unwrap();
    h.manifest_digest =
        theseus_native_protocol::v2::integration_manifest_digest(core, integration).to_hex();
    let context: theseus_native_protocol::v2::IntegrationRuntimeContext = serde_json::from_value(serde_json::json!({
        "kind":"theseus_native_integration_context","schema":1,"ownership_id":h.ownership_id,"binding":h.binding,"manifest_digest":h.manifest_digest,"workspace_marker":h.workspace_marker,"provider_epoch":h.provider_epoch,"integration_manifest_digest":integration.to_hex(),"desktop_broker_header":h
    })).unwrap();
    let original = BrokerBind {
        ownership_id: h.ownership_id.clone(),
        helper_lease: "77".repeat(16),
        helper_operation: "88".repeat(16),
        slot: theseus_native_protocol::v2::ResourceSlot::new("desktop").unwrap(),
        broker_intent: "99".repeat(16),
        expected_header: h.clone(),
    };
    let mut records = vec![
        Record::new(
            0,
            Entry::IntegrationHeader {
                artifact_commitment: Digest::from_bytes([3; 32]),
                binding: BindingId::parse(&h.binding).unwrap(),
                manifest_digest: Digest::parse(&h.manifest_digest).unwrap(),
                workspace_marker: WorkspaceMarker::parse(&h.workspace_marker).unwrap(),
                os: Os::Linux,
                provider_epoch: Digest::parse(&h.provider_epoch).unwrap(),
                core_manifest_digest: core,
                integration_manifest_digest: integration,
                context_digest: context.digest().unwrap(),
                ownership_id: h.ownership_id.clone(),
            },
        )
        .unwrap(),
        Record::new(
            1,
            Entry::DesktopAuthorityDeclared {
                manifest: Box::new(manifest),
                context: Box::new(context),
            },
        )
        .unwrap(),
        Record::new(
            2,
            Entry::LeaseReserved {
                intent: IntentId::from_bytes([7; 16]),
                lease: LeaseId::parse(&original.helper_lease).unwrap(),
            },
        )
        .unwrap(),
        Record::new(
            3,
            Entry::Broker {
                event: BrokerEvent::BindAccepted {
                    request_digest: original.digest().unwrap().to_hex(),
                    original: original.clone(),
                },
            },
        )
        .unwrap(),
    ];
    let receipt = BrokerBindingReceipt {
        original: original.clone(),
        request_digest: original.digest().unwrap().to_hex(),
        publication_scope: IntentPublicationScope::FileAndDirectorySynced,
        broker_lease: "aa".repeat(16),
    };
    let mut missing = records.clone();
    missing.push(
        Record::new(
            4,
            Entry::Broker {
                event: BrokerEvent::BindingCommitted {
                    receipt: receipt.clone(),
                },
            },
        )
        .unwrap(),
    );
    assert!(LedgerView::replay(&missing).is_err());
    records.push(
        Record::new(
            4,
            Entry::Broker {
                event: BrokerEvent::BindIntentPublished {
                    operation: original.helper_operation.clone(),
                    scope: receipt.publication_scope,
                },
            },
        )
        .unwrap(),
    );
    records.push(
        Record::new(
            5,
            Entry::Broker {
                event: BrokerEvent::BindingCommitted {
                    receipt: receipt.clone(),
                },
            },
        )
        .unwrap(),
    );
    LedgerView::replay(&records).unwrap();
    assert_eq!(
        BrokerLedgerView::replay(&records)
            .unwrap()
            .binding(&original)
            .unwrap(),
        Some(&receipt)
    );
    let mut changed = original.clone();
    changed.broker_intent = "bb".repeat(16);
    assert!(
        BrokerLedgerView::replay(&records)
            .unwrap()
            .binding(&changed)
            .is_err()
    );
}
#[test]
fn desktop_only_manifest_has_platform_matched_closed_channels_without_service_authority() {
    use theseus_native_protocol::v2::{LocalEndpoint, ResourceSlot};
    let mut manifest = DesktopManifest {
        schema: 1,
        core_manifest_digest: "11".repeat(32),
        slot: ResourceSlot::new("desktop").unwrap(),
        entry_root: "/fixture/entries".into(),
        namespace_policy: "exclusive_quiescent".into(),
        entry: "fixture.desktop".into(),
        entry_digest: "22".repeat(32),
        executable_digest: "33".repeat(32),
        allowed_modes: vec![DesktopMode::OwnedExecutable],
        ready_endpoint: LocalEndpoint::UnixPath {
            path: "/fixture/ready.sock".into(),
        },
        close_endpoint: LocalEndpoint::UnixPath {
            path: "/fixture/close.sock".into(),
        },
        expectation_digest: "44".repeat(32),
    };
    manifest.validate().unwrap();
    manifest.ready_endpoint = LocalEndpoint::NamedPipe {
        path: r"\\.\pipe\theseus.ready".into(),
    };
    assert!(manifest.validate().is_err());
    manifest.close_endpoint = LocalEndpoint::NamedPipe {
        path: r"\\.\pipe\theseus.close".into(),
    };
    manifest.entry_root = r"C:\fixture\entries".into();
    manifest.entry = "fixture.lnk".into();
    manifest.validate().unwrap();
    manifest.entry_root = r"\\remote\share".into();
    assert!(manifest.validate().is_err());
    manifest.entry_root = r"C:\fixture\..\escape".into();
    assert!(manifest.validate().is_err());
}
#[test]
fn broker_quarantine_is_sticky_when_original_retained_owner_later_settles() {
    let (_, request, mut records) = records();
    records.push(
        Record::new(
            3,
            Entry::Broker {
                event: BrokerEvent::EffectRecorded {
                    operation: request.helper_operation.clone(),
                    receipt: BrokerEffectReceipt {
                        evidence: DesktopEvidence::OwnedExecutable,
                        acknowledged: true,
                        receipt_digest: request.digest().unwrap().to_hex(),
                    },
                },
            },
        )
        .unwrap(),
    );
    records.push(
        Record::new(
            4,
            Entry::Broker {
                event: BrokerEvent::Quarantined {
                    operation: request.helper_operation.clone(),
                },
            },
        )
        .unwrap(),
    );
    records.push(
        Record::new(
            5,
            Entry::Broker {
                event: BrokerEvent::LifetimeRecorded {
                    operation: request.helper_operation.clone(),
                    primary: BrokerPrimary::Exited,
                    descendants: BrokerDescendants::Joined,
                    cleanup_complete: true,
                },
            },
        )
        .unwrap(),
    );
    LedgerView::replay(&records).unwrap();
    let state = BrokerLedgerView::replay(&records)
        .unwrap()
        .query(&request)
        .unwrap();
    assert!(state.quarantined);
    assert!(state.cleanup_complete);
    assert_eq!(state.owner, BrokerOwner::Settled);
    assert!(matches!(state.effect, BrokerEffect::Receipt { .. }));
    records.push(
        Record::new(
            6,
            Entry::LeaseClosed {
                lease: LeaseId::parse(&request.broker_lease).unwrap(),
                disposition: Disposition::AwaitingExternalReset,
            },
        )
        .unwrap(),
    );
    assert!(LedgerView::replay(&records).is_err());
}

#[test]
fn desktop_requests_use_the_existing_strict_v2_frame_and_original_identity() {
    use theseus_native_protocol::v2::{self, Envelope, Message, Reply, Request, SessionId};
    let header = header();
    let request = activation(&header);
    let identity = request.identity().unwrap();
    let requests = vec![
        Request::DesktopAction {
            request: Box::new(request.clone()),
        },
        Request::QueryDesktop {
            identity: Box::new(identity.clone()),
        },
        Request::CancelDesktop {
            identity: Box::new(identity.clone()),
        },
        Request::ProbeDesktopReadiness {
            identity: Box::new(identity.clone()),
            nonce: v2::BindingId::from_bytes([0xab; 16]),
            deadline_ms: 1000,
        },
    ];
    for request in requests {
        let frame = Envelope::new(
            SessionId::from_bytes([1; 16]),
            1,
            Message::Request { request },
        )
        .unwrap();
        let bytes = v2::encode(&frame).unwrap();
        assert_eq!(v2::Decoder::new().feed(&bytes).unwrap().frame, Some(frame));
    }
    let frame = Envelope::new(
        SessionId::from_bytes([1; 16]),
        2,
        Message::Response {
            reply_to: 1,
            result: Reply::DesktopState {
                identity: Box::new(identity),
                state: Box::new(BrokerState::unknown()),
            },
        },
    )
    .unwrap();
    let bytes = v2::encode(&frame).unwrap();
    assert_eq!(v2::Decoder::new().feed(&bytes).unwrap().frame, Some(frame));
    let invalid = Request::ProbeDesktopReadiness {
        identity: Box::new(request.identity().unwrap()),
        nonce: v2::BindingId::from_bytes([0; 16]),
        deadline_ms: 0,
    };
    assert!(
        Envelope::new(
            SessionId::from_bytes([1; 16]),
            1,
            Message::Request { request: invalid }
        )
        .is_err()
    );
}

#[test]
fn desktop_helper_mutations_share_canonical_v2_request_digest() {
    use theseus_native_protocol::v2;
    let header = header();
    let action = activation(&header);
    let bind = BrokerBind {
        ownership_id: header.ownership_id.clone(),
        helper_lease: action.helper_lease.clone(),
        helper_operation: "bc".repeat(16),
        slot: action.slot.clone(),
        broker_intent: "bd".repeat(16),
        expected_header: header,
    };
    for request in [
        v2::Request::BrokerBind {
            original: Box::new(bind),
        },
        v2::Request::DesktopAction {
            request: Box::new(action),
        },
    ] {
        assert!(
            v2::request_digest(&request).is_ok(),
            "published desktop mutations require the existing canonical request commitment"
        );
    }
}

#[test]
fn desktop_absence_and_partial_artifacts_roundtrip_without_null_wire_fields() {
    use theseus_native_protocol::v2;
    for (entry_present, executable_digest) in
        [(false, None), (true, None), (true, Some("14".repeat(32)))]
    {
        let frame = v2::Envelope::new(
            v2::SessionId::from_bytes([1; 16]),
            2,
            v2::Message::Response {
                reply_to: 1,
                result: v2::Reply::DesktopObservation {
                    lease: v2::LeaseId::from_bytes([2; 16]),
                    observation: Box::new(DesktopObservation {
                        generation: None,
                        slot: v2::ResourceSlot::new("desktop").unwrap(),
                        entry_present,
                        entry_valid: executable_digest.is_some(),
                        entry_digest: entry_present.then(|| "15".repeat(32)),
                        executable_digest,
                        session_header_digest: "16".repeat(32),
                        binding_digest: "17".repeat(32),
                        complete: true,
                    }),
                },
            },
        )
        .unwrap();
        let bytes = v2::encode(&frame).unwrap();
        assert_eq!(v2::Decoder::new().feed(&bytes).unwrap().frame, Some(frame));
    }
}

fn desktop_ready() -> (
    DesktopRequest,
    theseus_native_protocol::v2::ReadinessObservation,
) {
    use theseus_native_protocol::v2::*;
    let request = activation(&header());
    let expectation = ReadinessDefinitionExpectation {
        role: "configured".into(),
        artifact: "target".into(),
        artifact_length: 3,
        artifact_sha256: "12".repeat(32),
        version_literal: "target".into(),
        data_sha256: "13".repeat(32),
        configuration_digest: "14".repeat(32),
    };
    let observation = ReadinessObservation {
        peer_sid: None,
        expectation_digest: expectation.digest().unwrap().to_hex(),
        slot: request.slot.clone(),
        operation: request.helper_operation.clone(),
        action_digest: request.digest().unwrap().to_hex(),
        resource_binding_digest: request.prior_binding_digest.clone(),
        challenge_digest: "15".repeat(32),
        state: ReadinessState::Ready,
        authenticated_instance: true,
        instance_binding_digest: request.digest().unwrap().to_hex(),
        peer_uid: Some(1000),
        peer_gid: Some(1000),
        artifact_sha256: Some(expectation.artifact_sha256.clone()),
        version_literal: Some(expectation.version_literal.clone()),
        data_sha256: Some(expectation.data_sha256.clone()),
        configuration_digest: Some(expectation.configuration_digest.clone()),
        initialized: Some(true),
        response_digest: Some("16".repeat(32)),
        failure: None,
        expectation,
    };
    (request, observation)
}
#[test]
fn desktop_readiness_accepts_original_instance_and_user_principal_without_changing_service_policy()
{
    use theseus_native_protocol::v2::*;
    let (request, mut observation) = desktop_ready();
    let identity = request.identity().unwrap();
    assert_ne!(
        observation.instance_binding_digest,
        observation.resource_binding_digest
    );
    observation.validate_desktop(&identity).unwrap();
    observation
        .validate_desktop_peer(&header().broker_principal)
        .unwrap();
    assert!(
        observation.validate().is_err(),
        "service binding rule must stay unchanged"
    );
    observation.peer_uid = None;
    observation.peer_gid = None;
    observation.peer_sid = Some("S-1-5-21-1000".into());
    observation.validate_desktop(&identity).unwrap();
    observation
        .validate_desktop_peer(&BrokerPrincipal::Windows {
            token_user_sid: "S-1-5-21-1000".into(),
        })
        .unwrap();
    assert!(
        observation
            .validate_desktop_peer(&BrokerPrincipal::Windows {
                token_user_sid: "S-1-5-21-2000".into()
            })
            .is_err()
    );
    let desktop_frame = Envelope::new(
        SessionId::from_bytes([1; 16]),
        1,
        Message::Response {
            reply_to: 0,
            result: Reply::DesktopReadiness {
                identity: Box::new(identity.clone()),
                observation: Box::new(observation.clone()),
            },
        },
    )
    .unwrap();
    assert_eq!(
        Decoder::new()
            .feed(&encode(&desktop_frame).unwrap())
            .unwrap()
            .frame,
        Some(desktop_frame)
    );
    observation.instance_binding_digest = observation.resource_binding_digest.clone();
    assert!(
        observation.validate().is_err(),
        "service Windows peer remains SYSTEM-only"
    );
    observation.peer_sid = Some("S-1-5-18".into());
    observation.validate().unwrap();
    let frame = Envelope::new(
        SessionId::from_bytes([1; 16]),
        1,
        Message::Response {
            reply_to: 0,
            result: Reply::ReadinessObservation {
                lease: LeaseId::from_bytes([2; 16]),
                observation: Box::new(observation),
            },
        },
    )
    .unwrap();
    assert_eq!(
        Decoder::new().feed(&encode(&frame).unwrap()).unwrap().frame,
        Some(frame)
    );
}
#[test]
fn desktop_readiness_rejects_wrong_original_instance_and_peer() {
    let (request, mut observation) = desktop_ready();
    let identity = request.identity().unwrap();
    assert!(
        observation
            .validate_desktop_peer(&BrokerPrincipal::Posix {
                effective_uid: 1001,
                effective_gid: 1000
            })
            .is_err()
    );
    observation.instance_binding_digest = observation.resource_binding_digest.clone();
    assert!(observation.validate_desktop(&identity).is_err());
    let (_, mut observation) = desktop_ready();
    observation.action_digest = "ff".repeat(32);
    assert!(observation.validate_desktop(&identity).is_err());
}

#[test]
fn desktop_readiness_guard_checks_described_peer_and_original_probe_challenge() {
    use theseus_native_protocol::v2::*;
    for wrong in ["none", "peer", "nonce", "binding"] {
        let header = header();
        let (request, mut observation) = desktop_ready();
        let mut identity = request.identity().unwrap();
        if wrong == "binding" {
            identity.binding_digest = "ff".repeat(32);
        }
        let nonce = BindingId::from_bytes([0x42; 16]);
        observation.challenge_digest = ReadinessChallengeFields {
            lease: identity.broker_lease.clone(),
            slot: observation.slot.as_str().into(),
            resource_binding_digest: observation.resource_binding_digest.clone(),
            operation: identity.helper_operation.clone(),
            action_digest: identity.request_digest.clone(),
            nonce: nonce.to_hex(),
            expectation_digest: observation.expectation_digest.clone(),
        }
        .digest()
        .unwrap()
        .to_hex();
        if wrong == "peer" {
            observation.peer_uid = Some(1001);
        }
        if wrong == "nonce" {
            observation.challenge_digest = "ff".repeat(32);
        }
        let sid = SessionId::from_bytes([1; 16]);
        let mut guard = SessionGuard::new(sid.clone(), Role::Controller);
        guard
            .accept_outbound(
                &Envelope::new(
                    sid.clone(),
                    0,
                    Message::Hello {
                        role: Role::Controller,
                    },
                )
                .unwrap(),
            )
            .unwrap();
        guard
            .accept_inbound(
                &Envelope::new(sid.clone(), 0, Message::Hello { role: Role::Helper }).unwrap(),
            )
            .unwrap();
        guard
            .accept_outbound(
                &Envelope::new(
                    sid.clone(),
                    1,
                    Message::Request {
                        request: Request::Describe,
                    },
                )
                .unwrap(),
            )
            .unwrap();
        guard
            .accept_inbound(
                &Envelope::new(
                    sid.clone(),
                    1,
                    Message::Response {
                        reply_to: 1,
                        result: Reply::BrokerDescription {
                            header: Box::new(header),
                        },
                    },
                )
                .unwrap(),
            )
            .unwrap();
        guard
            .accept_outbound(
                &Envelope::new(
                    sid.clone(),
                    2,
                    Message::Request {
                        request: Request::ProbeDesktopReadiness {
                            identity: Box::new(identity.clone()),
                            nonce,
                            deadline_ms: 1000,
                        },
                    },
                )
                .unwrap(),
            )
            .unwrap();
        let frame = Envelope::new(
            sid.clone(),
            2,
            Message::Response {
                reply_to: 2,
                result: Reply::DesktopReadiness {
                    identity: Box::new(identity),
                    observation: Box::new(observation),
                },
            },
        )
        .unwrap();
        assert_eq!(
            Decoder::new().feed(&encode(&frame).unwrap()).unwrap().frame,
            Some(frame.clone())
        );
        let result = guard.accept_inbound(&frame);
        assert_eq!(result.is_ok(), wrong == "none", "{wrong}: {result:?}");
    }
}
