//! Pure contract tests: no SDK, process, manager, session or IPC calls.
use serde_json::{Value, json};
use theseus_native_protocol::v2::{self, broker::*, ledger::*};

fn manifest() -> Value {
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
#[test]
fn native_definition_is_committed_and_requires_exact_static_bootstrap_capability() {
    let native = parse_manifest(manifest()).unwrap();
    assert_eq!(
        v2::IntegrationCapabilities::for_manifest(&native)
            .unwrap()
            .names(),
        ["inspect_desktop", "native_activation", "request_close"]
    );
    let mut changed = manifest();
    changed["desktop_native"]["sdk"]["application_receipt_sha256"] = json!("aa".repeat(32));
    assert_ne!(
        native.digest().unwrap(),
        parse_manifest(changed).unwrap().digest().unwrap()
    );
    for (pointer, value) in [
        ("/desktop/namespace_policy", json!("shared")),
        (
            "/desktop_capabilities",
            json!(["inspect_desktop", "owned_executable", "request_close"]),
        ),
        ("/desktop_native/bootstrap", json!("argv_template")),
        (
            "/desktop_native/lifetime",
            json!("owned_process_tree_settled"),
        ),
        (
            "/desktop_native/sdk/sdk_receipt_sha256",
            json!("00".repeat(32)),
        ),
        ("/desktop/allowed_modes", json!(["owned_executable"])),
    ] {
        let mut bad = manifest();
        *bad.pointer_mut(pointer).unwrap() = value;
        assert!(parse_manifest(bad).is_err(), "{pointer}");
    }
    let mut missing = manifest();
    missing.as_object_mut().unwrap().remove("desktop_native");
    assert!(parse_manifest(missing).is_err());
}
#[test]
fn old_owned_manifest_and_state_omit_every_native_extension() {
    let mut owned = manifest();
    owned.as_object_mut().unwrap().remove("desktop_native");
    owned["desktop"]["allowed_modes"] = json!(["owned_executable"]);
    owned["desktop_capabilities"] = json!(["inspect_desktop", "owned_executable", "request_close"]);
    let encoded = serde_json::to_value(parse_manifest(owned).unwrap()).unwrap();
    assert!(encoded.get("desktop_native").is_none());
    let state = serde_json::to_string(&BrokerState::unknown()).unwrap();
    assert_eq!(
        state,
        r#"{"effect":{"kind":"unknown"},"owner":"lost","primary":"unknown","descendants":"unknown","cleanup_complete":false,"quarantined":true}"#
    );
}
fn native_state() -> NativeLifetimeState {
    NativeLifetimeState {
        authority: NativeLifetimeAuthority::ManagerInstance,
        required: NativeLifetimeRequirement::ActivationInstanceTerminated,
        instance: NativeInstanceState::Terminated,
        primary: NativePrimaryState::Unavailable,
        descendants: NativeTreeState::Unavailable,
        local_resources: NativeLocalState::Retired,
    }
}
#[test]
fn native_local_retirement_never_fabricates_primary_tree_or_clears_unknown() {
    let native = native_state();
    native.validate().unwrap();
    assert!(native.required_lifetime_satisfied());
    let mut state = BrokerState::unknown();
    state.quarantined = false;
    state.owner = BrokerOwner::Settled;
    state.effect = BrokerEffect::Receipt {
        receipt: BrokerEffectReceipt {
            evidence: DesktopEvidence::NativeActivation,
            acknowledged: true,
            receipt_digest: "12".repeat(32),
        },
    };
    state.native_lifetime = Some(native);
    state.validate().unwrap();
    assert!(state.original_retired());
    assert!(!state.cleanup_complete);
    state.cleanup_complete = true;
    assert!(state.validate().is_err());
    state.cleanup_complete = false;
    state.native_lifetime.as_mut().unwrap().instance = NativeInstanceState::Unknown;
    assert!(!state.original_retired());
    let mut old = native_state();
    old.primary = NativePrimaryState::Unknown;
    old.descendants = NativeTreeState::Unknown;
    old.local_resources = NativeLocalState::Unknown;
    assert!(native_state().validate_progress_from(&old).is_err());
    state.native_lifetime = Some(native_state());
    state.owner = BrokerOwner::Lost;
    assert!(!state.original_retired());
    state.owner = BrokerOwner::Settled;
    state.quarantined = true;
    assert!(!state.original_retired());
}
fn native_records() -> (DesktopRequest, Vec<Record>) {
    let header:IntegrationBrokerHeader=serde_json::from_value(json!({"profile":"native_integration_broker_v1","binding":"11".repeat(16),"manifest_digest":"22".repeat(32),"workspace_marker":"33".repeat(16),"os":"macos","target_id":"fixture","ownership_id":"44".repeat(16),"provider_epoch":"55".repeat(32),"session_id":"gui-fixture","session_epoch":"66".repeat(32),"broker_endpoint":{"kind":"unix_path","path":"/fixture/broker.sock"},"broker_principal":{"kind":"posix","effective_uid":501,"effective_gid":20},"helper_principal":{"kind":"posix","effective_uid":501,"effective_gid":20},"display":{"kind":"mac_os_gui","domain":"gui/501"},"allowed_modes":["native_activation"],"macos_security_session_id":123})).unwrap();
    header.validate().unwrap();
    let mut action:DesktopRequest=serde_json::from_value(json!({"helper_lease":"77".repeat(16),"helper_operation":"88".repeat(16),"broker_lease":"99".repeat(16),"binding_digest":header.digest().unwrap().to_hex(),"activation_nonce":"aa".repeat(16),"slot":"desktop","prior_binding_digest":"bb".repeat(32),"action":{"kind":"activate","mode":"native_activation","entry_digest":"cc".repeat(32),"expectation_digest":"dd".repeat(32),"close_binding":{"operation":"ee".repeat(16),"action_digest":"00".repeat(32),"nonce":"ff".repeat(16)}}})).unwrap();
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
#[test]
fn native_lifetime_event_replays_original_manager_retirement_without_tree_cleanup() {
    let (action, mut records) = native_records();
    push(
        &mut records,
        BrokerEvent::EffectRecorded {
            operation: action.helper_operation.clone(),
            receipt: BrokerEffectReceipt {
                evidence: DesktopEvidence::NativeActivation,
                acknowledged: true,
                receipt_digest: action.digest().unwrap().to_hex(),
            },
        },
    );
    push(
        &mut records,
        BrokerEvent::NativeLifetimeRecorded {
            operation: action.helper_operation.clone(),
            state: native_state(),
        },
    );
    let state = BrokerLedgerView::replay(&records)
        .unwrap()
        .query(&action)
        .unwrap();
    assert!(state.original_retired());
    assert_eq!(state.primary, BrokerPrimary::Unknown);
    assert_eq!(state.descendants, BrokerDescendants::Unknown);
    assert!(!state.cleanup_complete);
    for record in &records {
        let encoded = encode(record).unwrap();
        let mut decoder = Decoder::new();
        assert_eq!(decoder.feed(&encoded).unwrap().record, Some(record.clone()));
        decoder.finish().unwrap();
    }
    push(
        &mut records,
        BrokerEvent::Quarantined {
            operation: action.helper_operation.clone(),
        },
    );
    assert!(
        !BrokerLedgerView::replay(&records)
            .unwrap()
            .query(&action)
            .unwrap()
            .original_retired()
    );
}
#[test]
fn mac_native_header_requires_explicit_security_session_and_platform_match() {
    let (_, records) = native_records();
    let Entry::IntegrationBrokerHeader { mut header } = records[0].entry().clone() else {
        unreachable!()
    };
    header.macos_security_session_id = None;
    assert!(header.validate().is_err());
    header.macos_security_session_id = Some(0);
    assert!(header.validate().is_err());
    header.allowed_modes = vec![DesktopMode::OwnedExecutable];
    header.macos_security_session_id = None;
    header.validate().unwrap();
    assert!(
        serde_json::to_value(&header)
            .unwrap()
            .get("macos_security_session_id")
            .is_none()
    );
}
#[test]
fn native_lease_release_requires_real_required_lifetime_and_local_retirement() {
    let (action, mut records) = native_records();
    push(
        &mut records,
        BrokerEvent::EffectRecorded {
            operation: action.helper_operation.clone(),
            receipt: BrokerEffectReceipt {
                evidence: DesktopEvidence::NativeActivation,
                acknowledged: true,
                receipt_digest: action.digest().unwrap().to_hex(),
            },
        },
    );
    push(
        &mut records,
        BrokerEvent::NativeLifetimeRecorded {
            operation: action.helper_operation.clone(),
            state: native_state(),
        },
    );
    records.push(
        Record::new(
            records.len() as u16,
            Entry::LeaseClosed {
                lease: v2::LeaseId::parse(&action.broker_lease).unwrap(),
                disposition: Disposition::AwaitingExternalReset,
            },
        )
        .unwrap(),
    );
    let view = LedgerView::replay(&records).unwrap();
    assert!(matches!(
        view.query_lease(v2::LeaseId::parse(&action.broker_lease).unwrap())
            .unwrap(),
        v2::LeaseView::AwaitingExternalReset { .. }
    ));
    for field in ["instance", "local_resources"] {
        let mut bad = records.clone();
        let mut state = native_state();
        if field == "instance" {
            state.instance = NativeInstanceState::Live;
        } else {
            state.local_resources = NativeLocalState::Retained;
        }
        bad[4] = Record::new(
            4,
            Entry::Broker {
                event: BrokerEvent::NativeLifetimeRecorded {
                    operation: action.helper_operation.clone(),
                    state,
                },
            },
        )
        .unwrap();
        assert!(LedgerView::replay(&bad).is_err(), "{field}");
    }
}
#[test]
fn native_close_receipt_keeps_original_native_mode_and_lifetime() {
    let (action, mut records) = native_records();
    let close = action.close_request().unwrap();
    for request in [&action, &close] {
        if request == &close {
            push(
                &mut records,
                BrokerEvent::Accepted {
                    request: close.clone(),
                    request_digest: close.digest().unwrap().to_hex(),
                },
            );
        }
        push(
            &mut records,
            BrokerEvent::EffectRecorded {
                operation: request.helper_operation.clone(),
                receipt: BrokerEffectReceipt {
                    evidence: DesktopEvidence::NativeActivation,
                    acknowledged: true,
                    receipt_digest: request.digest().unwrap().to_hex(),
                },
            },
        );
    }
    push(
        &mut records,
        BrokerEvent::NativeLifetimeRecorded {
            operation: action.helper_operation.clone(),
            state: native_state(),
        },
    );
    let view = BrokerLedgerView::replay(&records).unwrap();
    assert!(view.query(&close).unwrap().original_retired());
    let mut replay = records.clone();
    let mut unknown = native_state();
    unknown.instance = NativeInstanceState::Unknown;
    push(
        &mut replay,
        BrokerEvent::NativeLifetimeRecorded {
            operation: action.helper_operation.clone(),
            state: unknown,
        },
    );
    assert!(BrokerLedgerView::replay(&replay).is_err());
}
#[test]
fn quarantined_lost_native_owner_cannot_later_declare_unavailable_scope() {
    let (action, mut records) = native_records();
    push(
        &mut records,
        BrokerEvent::Quarantined {
            operation: action.helper_operation.clone(),
        },
    );
    push(
        &mut records,
        BrokerEvent::NativeLifetimeRecorded {
            operation: action.helper_operation.clone(),
            state: native_state(),
        },
    );
    assert!(BrokerLedgerView::replay(&records).is_err());
}
#[test]
fn owned_retirement_keeps_its_strong_scope_and_uncertainty_requirements() {
    let mut state = BrokerState::unknown();
    state.quarantined = false;
    state.owner = BrokerOwner::Settled;
    state.primary = BrokerPrimary::Exited;
    state.descendants = BrokerDescendants::Joined;
    state.cleanup_complete = true;
    state.effect = BrokerEffect::Receipt {
        receipt: BrokerEffectReceipt {
            evidence: DesktopEvidence::OwnedExecutable,
            acknowledged: true,
            receipt_digest: "ab".repeat(32),
        },
    };
    assert!(state.original_retired());
    state.descendants = BrokerDescendants::Unknown;
    assert!(!state.original_retired());
    state.descendants = BrokerDescendants::Joined;
    state.effect = BrokerEffect::Unknown;
    assert!(!state.original_retired());
}
fn mac_manifest() -> Value {
    let mut value = manifest();
    value["desktop_native"] = json!({"kind":"macos_bundle","bootstrap":"theseus_fixture_ready_bootstrap_v1","lifetime":"activation_instance_terminated","app_leaf":"Fixture.app","bundle_identifier":"org.theseus.fixture","executable_leaf":"app","info_plist":{"length":128,"sha256":"22".repeat(32)}});
    value["desktop"]["entry"] = json!("Fixture.app");
    value["desktop_executable"]["root"] = json!("/fixture/entries");
    value["desktop_executable"]["relative_path"] = json!("Fixture.app/Contents/MacOS/app");
    value["desktop_executable"]["arguments"][0] =
        json!("/fixture/entries/Fixture.app/Contents/MacOS/app");
    value["artifact_slots"][0]["relative_path"] = json!("Fixture.app/Contents/Info.plist");
    value
}
#[test]
fn mac_bundle_metadata_commitment_is_separate_from_app_directory_presence() {
    let native = parse_manifest(mac_manifest()).unwrap();
    assert_eq!(
        native.native_entry_relative_path().unwrap(),
        "Fixture.app/Contents/Info.plist"
    );
    assert_eq!(native.desktop.as_ref().unwrap().entry, "Fixture.app");
    for (pointer, value) in [
        ("/artifact_slots/0/relative_path", json!("Fixture.app")),
        ("/desktop_native/info_plist/length", json!(129)),
        (
            "/desktop_executable/relative_path",
            json!("Other.app/Contents/MacOS/app"),
        ),
        ("/desktop_native/lifetime", json!("owned_primary_exited")),
    ] {
        let mut bad = mac_manifest();
        *bad.pointer_mut(pointer).unwrap() = value;
        assert!(parse_manifest(bad).is_err(), "{pointer}");
    }
}
#[test]
fn native_manifest_cannot_borrow_a_foreign_platform_header() {
    let native = parse_manifest(mac_manifest()).unwrap();
    let (_, records) = native_records();
    let Entry::IntegrationBrokerHeader { mut header } = records[0].entry().clone() else {
        unreachable!()
    };
    let integration = native.digest().unwrap();
    header.manifest_digest = v2::integration_manifest_digest(
        v2::Digest::parse(&native.core_manifest_digest).unwrap(),
        integration,
    )
    .to_hex();
    header.os = "linux".into();
    header.display = SuppliedDisplay::X11 {
        display: ":92".into(),
        authority_file: "/fixture/auth".into(),
        bus: "/fixture/bus.sock".into(),
    };
    header.macos_security_session_id = None;
    let context:v2::IntegrationRuntimeContext=serde_json::from_value(json!({"kind":"theseus_native_integration_context","schema":1,"ownership_id":header.ownership_id,"binding":header.binding,"manifest_digest":header.manifest_digest,"workspace_marker":header.workspace_marker,"provider_epoch":header.provider_epoch,"integration_manifest_digest":integration.to_hex(),"desktop_broker_header":header})).unwrap();
    assert!(native.validate_context(&context, v2::Os::Linux).is_err());
}
#[test]
fn native_receipt_cannot_borrow_the_legacy_owned_tree_retirement_path() {
    let mut state = BrokerState::unknown();
    state.owner = BrokerOwner::Settled;
    state.quarantined = false;
    state.primary = BrokerPrimary::Exited;
    state.descendants = BrokerDescendants::Joined;
    state.cleanup_complete = true;
    state.effect = BrokerEffect::Receipt {
        receipt: BrokerEffectReceipt {
            evidence: DesktopEvidence::NativeActivation,
            acknowledged: true,
            receipt_digest: "ac".repeat(32),
        },
    };
    assert!(!state.original_retired());
    assert!(state.validate().is_err());
}
#[test]
fn created_native_instance_never_regresses_to_pending_or_noncreation() {
    let live = NativeLifetimeState {
        authority: NativeLifetimeAuthority::OriginalChild,
        required: NativeLifetimeRequirement::OwnedPrimaryExited,
        instance: NativeInstanceState::Live,
        primary: NativePrimaryState::Running,
        descendants: NativeTreeState::Unavailable,
        local_resources: NativeLocalState::Retained,
    };
    for (instance, primary) in [
        (NativeInstanceState::Pending, NativePrimaryState::Pending),
        (
            NativeInstanceState::NotCreated,
            NativePrimaryState::NotCreated,
        ),
    ] {
        let mut next = live.clone();
        next.instance = instance;
        next.primary = primary;
        assert!(next.validate_progress_from(&live).is_err());
    }
}
