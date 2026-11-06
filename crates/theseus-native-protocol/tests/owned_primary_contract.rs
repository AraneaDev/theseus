//! Pure original owned-primary contract tests; no SDK/session/process/IPC effects.
use serde_json::{Value, json};
use theseus_native_protocol::v2::{self, broker::*, ledger::*};
fn owned_records() -> (DesktopRequest, Vec<Record>) {
    let header:IntegrationBrokerHeader=serde_json::from_value(json!({"profile":"native_integration_broker_v1","binding":"11".repeat(16),"manifest_digest":"22".repeat(32),"workspace_marker":"33".repeat(16),"os":"macos","target_id":"fixture","ownership_id":"44".repeat(16),"provider_epoch":"55".repeat(32),"session_id":"gui-fixture","session_epoch":"66".repeat(32),"broker_endpoint":{"kind":"unix_path","path":"/fixture/broker.sock"},"broker_principal":{"kind":"posix","effective_uid":501,"effective_gid":20},"helper_principal":{"kind":"posix","effective_uid":501,"effective_gid":20},"display":{"kind":"mac_os_gui","domain":"gui/501"},"allowed_modes":["owned_executable"],"macos_security_session_id":123})).unwrap();
    header.validate().unwrap();
    let mut action:DesktopRequest=serde_json::from_value(json!({"helper_lease":"77".repeat(16),"helper_operation":"88".repeat(16),"broker_lease":"99".repeat(16),"binding_digest":header.digest().unwrap().to_hex(),"activation_nonce":"aa".repeat(16),"slot":"desktop","prior_binding_digest":"bb".repeat(32),"action":{"kind":"activate","owned_lifetime":{"kind":"macos_original_primary_v1"},"mode":"owned_executable","entry_digest":"cc".repeat(32),"expectation_digest":"dd".repeat(32),"close_binding":{"operation":"ee".repeat(16),"action_digest":"00".repeat(32),"nonce":"ff".repeat(16)}}})).unwrap();
    let digest = action.close_request().unwrap().digest().unwrap().to_hex();
    if let DesktopAction::Activate { close_binding, .. } = &mut action.action {
        close_binding.action_digest = digest;
    }
    let records = vec![
        Record::new(0, Entry::IntegrationBrokerHeader { header }).unwrap(),
        Record::new(
            1,
            Entry::LeaseReserved {
                intent: v2::IntentId::from_bytes([1; 16]),
                lease: v2::LeaseId::parse(&action.broker_lease).unwrap(),
            },
        )
        .unwrap(),
        Record::new(
            2,
            Entry::Broker {
                event: BrokerEvent::Accepted {
                    request: action.clone(),
                    request_digest: action.digest().unwrap().to_hex(),
                },
            },
        )
        .unwrap(),
    ];
    (action, records)
}
fn push(records: &mut Vec<Record>, event: BrokerEvent) {
    records.push(Record::new(records.len() as u16, Entry::Broker { event }).unwrap());
}
fn native_manifest() -> Value {
    json!({"schema":1,"core_manifest_digest":"11".repeat(32),"desktop":{"schema":1,"core_manifest_digest":"11".repeat(32),"slot":"desktop","entry_root":"/fixture/entries","namespace_policy":"exclusive_quiescent","entry":"fixture.desktop","entry_digest":"22".repeat(32),"executable_digest":"33".repeat(32),"allowed_modes":["native_activation"],"ready_endpoint":{"kind":"unix_path","path":"/fixture/ready.sock"},"close_endpoint":{"kind":"unix_path","path":"/fixture/close.sock"},"expectation_digest":"00".repeat(32)},"desktop_executable":{"root":"/fixture","relative_path":"app","length":123,"sha256":"33".repeat(32),"arguments":["/fixture/app","/fixture/ready.sock","/fixture/close.sock"],"environment":[],"working_directory":"/fixture"},"desktop_readiness":{"role":"configured","artifact":"target","artifact_length":123,"artifact_sha256":"33".repeat(32),"version_literal":"target","data_sha256":"44".repeat(32),"configuration_digest":"55".repeat(32)},"desktop_native":{"kind":"linux_gio","bootstrap":"theseus_fixture_ready_bootstrap_v1","lifetime":"owned_primary_exited","sdk":{"glib_version":[2,80,0],"native_abi":"linux_x86_64","helper_root":"/fixture/sdk","helper_relative_path":"gio-launch-desktop","helper":{"length":456,"sha256":"66".repeat(32)},"sdk_receipt_sha256":"77".repeat(32),"helper_receipt_sha256":"88".repeat(32),"application_receipt_sha256":"99".repeat(32),"provenance":"provider_build_attested"}},"definitions":[],"artifact_slots":[{"slot":"desktop","relative_path":"fixture.desktop","length":128,"sha256":"22".repeat(32)}],"readiness_protocol":"theseus_fixture_ready_v1","desktop_capabilities":["inspect_desktop","native_activation","request_close"]})
}
fn parse_manifest(mut value: Value) -> Result<v2::IntegrationManifest, v2::Error> {
    let ready: v2::ReadinessDefinitionExpectation =
        serde_json::from_value(value["desktop_readiness"].clone()).unwrap();
    value["desktop"]["expectation_digest"] = json!(ready.digest().unwrap().to_hex());
    let parsed: v2::IntegrationManifest =
        serde_json::from_value(value).map_err(|_| v2::Error::InvalidValue)?;
    parsed.validate()?;
    Ok(parsed)
}

fn owned_manifest() -> Value {
    let mut value = native_manifest();
    value.as_object_mut().unwrap().remove("desktop_native");
    value["desktop"]["allowed_modes"] = json!(["owned_executable"]);
    value["desktop_capabilities"] = json!(["inspect_desktop", "owned_executable", "request_close"]);
    value["desktop_owned_lifetime"] = json!({"kind":"macos_original_primary_v1"});
    value
}
#[test]
fn owned_primary_declaration_is_committed_and_cannot_be_native_or_change_tuple() {
    let explicit = parse_manifest(owned_manifest())
        .expect("explicit macOS owned-primary declaration must parse");
    let mut legacy = owned_manifest();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("desktop_owned_lifetime");
    assert_ne!(
        explicit.digest().unwrap(),
        parse_manifest(legacy).unwrap().digest().unwrap()
    );
    let tuple = explicit
        .desktop_definition(&v2::DesktopDefinitionKey::CONFIGURED_TARGET)
        .unwrap();
    let mut bad = serde_json::to_value(&explicit).unwrap();
    let mut other = serde_json::to_value(tuple).unwrap();
    other["key"] = json!({"role":"target","artifact":"target"});
    other["readiness"]["role"] = "target".into();
    let ready: v2::ReadinessDefinitionExpectation =
        serde_json::from_value(other["readiness"].clone()).unwrap();
    other["desktop"]["expectation_digest"] = ready.digest().unwrap().to_hex().into();
    other.as_object_mut().unwrap().remove("owned_lifetime");
    bad["desktop_definitions"] = json!([other]);
    assert!(parse_manifest(bad).is_err());
    let mut native = native_manifest();
    native["desktop_owned_lifetime"] = json!({"kind":"macos_original_primary_v1"});
    assert!(parse_manifest(native).is_err());
}
#[test]
fn owned_primary_accepted_is_atomic_and_legacy_event_cannot_override_it() {
    let (request, mut records) = owned_records();
    let view = BrokerLedgerView::replay(&records).unwrap();
    let state = view.query(&request).unwrap();
    let wire = serde_json::to_value(&state).unwrap();
    assert_eq!(
        wire["owned_lifetime"],
        json!({"definition":{"kind":"macos_original_primary_v1"},"primary":"pending","descendants":"unavailable","local_resources":"retained"})
    );
    assert_eq!(state.primary, BrokerPrimary::Unknown);
    assert!(!state.original_retired());
    assert!(!state.cleanup_complete);
    push(
        &mut records,
        BrokerEvent::LifetimeRecorded {
            operation: request.helper_operation.clone(),
            primary: BrokerPrimary::NotCreated,
            descendants: BrokerDescendants::NotCreated,
            cleanup_complete: true,
        },
    );
    assert!(BrokerLedgerView::replay(&records).is_err());
}
fn owned_event(
    request: &DesktopRequest,
    primary: &str,
    descendants: &str,
    local: &str,
) -> BrokerEvent {
    serde_json::from_value(json!({"kind":"owned_lifetime_recorded","operation":request.helper_operation,"state":{"definition":{"kind":"macos_original_primary_v1"},"primary":primary,"descendants":descendants,"local_resources":local}})).expect("closed owned lifetime event")
}
#[test]
fn owned_primary_replays_without_tree_claim_and_unknown_is_sticky() {
    let (request, mut records) = owned_records();
    push(
        &mut records,
        BrokerEvent::EffectRecorded {
            operation: request.helper_operation.clone(),
            receipt: BrokerEffectReceipt {
                evidence: DesktopEvidence::OwnedExecutable,
                acknowledged: true,
                receipt_digest: request.digest().unwrap().to_hex(),
            },
        },
    );
    let before = records.clone();
    push(
        &mut records,
        owned_event(&request, "exited", "unavailable", "retired"),
    );
    let state = BrokerLedgerView::replay(&records)
        .unwrap()
        .query(&request)
        .unwrap();
    assert!(state.original_retired());
    assert_eq!(state.primary, BrokerPrimary::Exited);
    assert_eq!(state.descendants, BrokerDescendants::Unknown);
    assert!(!state.cleanup_complete);
    for record in &records {
        let bytes = encode(record).unwrap();
        let mut d = Decoder::new();
        assert_eq!(d.feed(&bytes).unwrap().record, Some(record.clone()));
        d.finish().unwrap();
    }
    for (primary, tree, local) in [
        ("unknown", "unavailable", "retained"),
        ("running", "unknown", "retained"),
        ("running", "unavailable", "unknown"),
    ] {
        let mut bad = before.clone();
        push(&mut bad, owned_event(&request, primary, tree, local));
        assert!(
            !BrokerLedgerView::replay(&bad)
                .unwrap()
                .query(&request)
                .unwrap()
                .original_retired()
        );
        push(
            &mut bad,
            owned_event(&request, "exited", "unavailable", "retired"),
        );
        assert!(BrokerLedgerView::replay(&bad).is_err());
    }
    let close = request.close_request().unwrap();
    push(
        &mut records,
        BrokerEvent::Accepted {
            request: close.clone(),
            request_digest: close.digest().unwrap().to_hex(),
        },
    );
    let state = BrokerLedgerView::replay(&records)
        .unwrap()
        .query(&close)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&state).unwrap()["owned_lifetime"]["primary"],
        "exited"
    );
    assert!(
        !state.original_retired(),
        "close still requires its own acknowledged effect"
    );
}
#[test]
fn owned_primary_request_digest_changes_and_wrong_platform_refuses() {
    let (request, records) = owned_records();
    let mut raw = serde_json::to_value(&request).unwrap();
    raw["action"]
        .as_object_mut()
        .unwrap()
        .remove("owned_lifetime");
    let legacy: DesktopRequest = serde_json::from_value(raw).unwrap();
    assert_ne!(legacy.digest().unwrap(), request.digest().unwrap());
    assert_eq!(
        legacy.close_request().unwrap().digest().unwrap(),
        request.close_request().unwrap().digest().unwrap()
    );
    let Entry::IntegrationBrokerHeader { mut header } = records[0].entry().clone() else {
        panic!()
    };
    header.os = "linux".into();
    header.macos_security_session_id = None;
    header.display = SuppliedDisplay::X11 {
        display: ":92".into(),
        authority_file: "/fixture/auth".into(),
        bus: "/fixture/bus".into(),
    };
    let mut bad = request.clone();
    bad.binding_digest = header.digest().unwrap().to_hex();
    let close = bad.close_request().unwrap().digest().unwrap().to_hex();
    if let DesktopAction::Activate { close_binding, .. } = &mut bad.action {
        close_binding.action_digest = close;
    }
    let mut changed = records;
    changed[0] = Record::new(0, Entry::IntegrationBrokerHeader { header }).unwrap();
    changed[2] = Record::new(
        2,
        Entry::Broker {
            event: BrokerEvent::Accepted {
                request: bad.clone(),
                request_digest: bad.digest().unwrap().to_hex(),
            },
        },
    )
    .unwrap();
    assert!(BrokerLedgerView::replay(&changed).is_err());
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
    let m: v2::IntegrationManifest = parse_manifest(owned_manifest()).unwrap();
    let integration = m.digest().unwrap();
    let core = v2::Digest::parse(&m.core_manifest_digest).unwrap();
    let full = v2::integration_manifest_digest(core, integration);
    let h:IntegrationBrokerHeader=serde_json::from_value(json!({"profile":"native_integration_broker_v1","binding":"11".repeat(16),"manifest_digest":full.to_hex(),"workspace_marker":"33".repeat(16),"os":"macos","target_id":"fixture","ownership_id":"44".repeat(16),"provider_epoch":"55".repeat(32),"session_id":"session","session_epoch":"66".repeat(32),"broker_endpoint":{"kind":"unix_path","path":"/fixture/broker.sock"},"broker_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"helper_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"display":{"kind":"mac_os_gui","domain":"gui/1000"},"allowed_modes":["owned_executable"]})).unwrap();
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
            os: v2::Os::Macos,
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

fn helper_owned_request(
    m: &v2::IntegrationManifest,
    h: &IntegrationBrokerHeader,
) -> DesktopRequest {
    let (mut request, _) = owned_records();
    request.helper_lease = v2::LeaseId::from_bytes([7; 16]).to_hex();
    request.binding_digest = h.digest().unwrap().to_hex();
    if let DesktopAction::Activate {
        entry_digest,
        expectation_digest,
        ..
    } = &mut request.action
    {
        let d = m.desktop.as_ref().unwrap();
        *entry_digest = d.entry_digest.clone();
        *expectation_digest = d.expectation_digest.clone();
    }
    let close = request.close_request().unwrap().digest().unwrap().to_hex();
    if let DesktopAction::Activate { close_binding, .. } = &mut request.action {
        close_binding.action_digest = close;
    }
    request
}
#[test]
fn helper_owned_acceptance_is_atomic_exact_and_inherited_by_close() {
    let (m, h, mut records) = helper_graph();
    let request = helper_owned_request(&m, &h);
    let before = records.clone();
    event(
        &mut records,
        BrokerEvent::HelperActionAccepted {
            request: request.clone(),
            request_digest: request.digest().unwrap().to_hex(),
        },
    );
    let view = BrokerLedgerView::replay(&records).unwrap();
    let initial = view
        .helper_state(&request.identity().unwrap())
        .unwrap()
        .expect("Accepted must initialize explicit axis atomically")
        .clone();
    assert_eq!(
        serde_json::to_value(&initial).unwrap()["owned_lifetime"]["primary"],
        "pending"
    );
    let close = request.close_request().unwrap();
    event(
        &mut records,
        BrokerEvent::HelperActionAccepted {
            request: close.clone(),
            request_digest: close.digest().unwrap().to_hex(),
        },
    );
    assert_eq!(
        BrokerLedgerView::replay(&records)
            .unwrap()
            .helper_state(&close.identity().unwrap())
            .unwrap()
            .unwrap()
            .owned_lifetime,
        initial.owned_lifetime
    );
    let mut stripped = serde_json::to_value(&request).unwrap();
    stripped["action"]
        .as_object_mut()
        .unwrap()
        .remove("owned_lifetime");
    let stripped: DesktopRequest = serde_json::from_value(stripped).unwrap();
    let mut bad = before;
    event(
        &mut bad,
        BrokerEvent::HelperActionAccepted {
            request: stripped.clone(),
            request_digest: stripped.digest().unwrap().to_hex(),
        },
    );
    assert!(
        BrokerLedgerView::replay(&bad).is_err(),
        "request cannot omit full-M commitment"
    );
    for damage in ["erase", "unknown", "lost", "native"] {
        let mut bad = records.clone();
        let mut state = initial.clone();
        match damage {
            "erase" => state.owned_lifetime = None,
            "unknown" => {
                let axis = state.owned_lifetime.as_mut().unwrap();
                axis.primary = OwnedPrimaryState::Unknown;
            }
            "lost" => state.owner = BrokerOwner::Lost,
            _ => {
                state.effect = BrokerEffect::Receipt {
                    receipt: BrokerEffectReceipt {
                        evidence: DesktopEvidence::NativeActivation,
                        acknowledged: true,
                        receipt_digest: "ab".repeat(32),
                    },
                };
            }
        }
        event(
            &mut bad,
            BrokerEvent::HelperStateObserved {
                identity: request.identity().unwrap(),
                state: state.clone(),
            },
        );
        if matches!(damage, "erase" | "native") {
            assert!(BrokerLedgerView::replay(&bad).is_err());
            continue;
        }
        BrokerLedgerView::replay(&bad).unwrap();
        state = initial.clone();
        let mut axis = state.owned_lifetime.clone().unwrap();
        axis.primary = OwnedPrimaryState::Exited;
        axis.local_resources = OwnedLocalState::Retired;
        state.record_owned_lifetime(axis).unwrap();
        event(
            &mut bad,
            BrokerEvent::HelperStateObserved {
                identity: request.identity().unwrap(),
                state,
            },
        );
        assert!(
            BrokerLedgerView::replay(&bad).is_err(),
            "{damage} cannot regain original retirement"
        );
    }
}
#[test]
fn owned_state_strict_wire_and_shared_mirror_reject_false_retirement() {
    let (request, records) = owned_records();
    let state = BrokerLedgerView::replay(&records)
        .unwrap()
        .query(&request)
        .unwrap();
    for (field, value) in [
        ("primary", json!("not_created")),
        ("descendants", json!("joined")),
        ("definition", json!({"kind":"linux_original_primary_v1"})),
    ] {
        let mut raw = serde_json::to_value(&state).unwrap();
        raw["owned_lifetime"][field] = value;
        assert!(serde_json::from_value::<BrokerState>(raw).is_err());
    }
    for field in ["cleanup", "owner", "primary", "descendants"] {
        let mut bad = state.clone();
        match field {
            "cleanup" => bad.cleanup_complete = true,
            "owner" => bad.owner = BrokerOwner::Settled,
            "primary" => bad.primary = BrokerPrimary::NotCreated,
            _ => bad.descendants = BrokerDescendants::Joined,
        };
        assert!(bad.validate().is_err(), "{field}");
    }
    let mut legacy = BrokerState::unknown();
    assert!(
        legacy
            .record_owned_lifetime(state.owned_lifetime.clone().unwrap())
            .is_err()
    );
    let mut complete = state.clone();
    let mut axis = complete.owned_lifetime.clone().unwrap();
    axis.primary = OwnedPrimaryState::Exited;
    axis.local_resources = OwnedLocalState::Retired;
    complete.record_owned_lifetime(axis).unwrap();
    assert!(!complete.original_retired());
    complete.effect = BrokerEffect::Receipt {
        receipt: BrokerEffectReceipt {
            evidence: DesktopEvidence::OwnedExecutable,
            acknowledged: true,
            receipt_digest: "ab".repeat(32),
        },
    };
    assert!(complete.original_retired());
    complete.quarantined = true;
    assert!(!complete.original_retired());
    let message = v2::Message::Request {
        request: v2::Request::DesktopAction {
            request: Box::new(request),
        },
    };
    let frame = v2::Envelope::new(v2::SessionId::from_bytes([1; 16]), 1, message).unwrap();
    let bytes = v2::encode(&frame).unwrap();
    assert_eq!(v2::Decoder::new().feed(&bytes).unwrap().frame, Some(frame));
}

#[test]
fn helper_close_acceptance_preserves_lost_original_quarantine() {
    let (m, h, mut records) = helper_graph();
    let request = helper_owned_request(&m, &h);
    event(
        &mut records,
        BrokerEvent::HelperActionAccepted {
            request: request.clone(),
            request_digest: request.digest().unwrap().to_hex(),
        },
    );
    let mut state = BrokerLedgerView::replay(&records)
        .unwrap()
        .helper_state(&request.identity().unwrap())
        .unwrap()
        .unwrap()
        .clone();
    state.owner = BrokerOwner::Lost;
    state.quarantined = true;
    event(
        &mut records,
        BrokerEvent::HelperStateObserved {
            identity: request.identity().unwrap(),
            state,
        },
    );
    let close = request.close_request().unwrap();
    event(
        &mut records,
        BrokerEvent::HelperActionAccepted {
            request: close.clone(),
            request_digest: close.digest().unwrap().to_hex(),
        },
    );
    let view = BrokerLedgerView::replay(&records).unwrap();
    let state = view
        .helper_state(&close.identity().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(state.owner, BrokerOwner::Lost);
    assert!(state.quarantined);
}
