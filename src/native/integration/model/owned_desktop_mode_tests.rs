use super::*;
use theseus_native_protocol::v2::broker::*;
fn scenario() -> IntegrationScenarioEvidence {
    let mut scenario = crate::native::integration::view::tests::report()
        .integrations
        .remove(0);
    let definition = OwnedDesktopLifetimeDefinition::MacosOriginalPrimaryV1;
    let owned = OwnedLifetimeState {
        definition,
        primary: OwnedPrimaryState::Exited,
        descendants: OwnedTreeState::Unavailable,
        local_resources: OwnedLocalState::Retired,
    };
    let mut state = BrokerState {
        owned_lifetime: Some(OwnedLifetimeState::initial(definition)),
        owner: BrokerOwner::Retained,
        quarantined: false,
        ..BrokerState::unknown()
    };
    state.record_owned_lifetime(owned.clone()).unwrap();
    state.effect = BrokerEffect::Receipt {
        receipt: BrokerEffectReceipt {
            evidence: theseus_native_protocol::v2::broker::DesktopEvidence::OwnedExecutable,
            acknowledged: true,
            receipt_digest: "ab".repeat(32),
        },
    };
    state.validate().unwrap();
    assert!(!state.cleanup_complete);
    let mut bound = super::native_desktop_mode_tests::observation(state);
    bound.evidence = Some(theseus_native_protocol::v2::broker::DesktopEvidence::OwnedExecutable);
    let original = ActionReference {
        operation: bound.binding.action.clone().unwrap(),
        request_digest: "04".repeat(32),
    };
    scenario.desktops = vec![super::DesktopEvidence {
        slot: bound.binding.slot.clone(),
        actions: vec![original.clone()],
        observations: vec![bound],
    }];
    scenario.native_lifetime = None;
    scenario.owned_lifetime = Some(OwnedDesktopLifetimeProjection {
        original,
        state: owned,
    });
    let done = || Disposition::Complete {
        receipt: "separate original obligation completed".into(),
    };
    scenario.dispositions = IntegrationDispositions {
        helper_worker: done(),
        registration: done(),
        manager_action: done(),
        broker: done(),
        primary: done(),
        descendant_scope: Disposition::NotStarted,
    };
    scenario
}
#[test]
fn explicit_original_owned_primary_can_complete_without_a_tree_claim() {
    let scenario = scenario();
    assert!(scenario.complete());
    assert!(
        !scenario.dispositions.complete(),
        "legacy strong completion is unchanged"
    );
    let state = scenario.desktops[0].observations[0]
        .broker_state
        .as_ref()
        .unwrap();
    assert!(!state.cleanup_complete);
    assert_eq!(state.descendants, BrokerDescendants::Unknown);
}
#[test]
fn owned_primary_never_waives_missing_original_projection_or_uncertainty() {
    let original = scenario();
    let mut bad = original.clone();
    bad.owned_lifetime = None;
    assert!(!bad.complete());
    let mut bad = original.clone();
    bad.owned_lifetime.as_mut().unwrap().original.request_digest = "05".repeat(32);
    assert!(!bad.complete());
    let mut bad = original.clone();
    bad.dispositions.descendant_scope = Disposition::Unknown {
        reason: "original historical uncertainty".into(),
    };
    assert!(!bad.complete());
    let mut bad = original.clone();
    bad.dispositions.broker = Disposition::Unknown {
        reason: "original lease still held".into(),
    };
    assert!(!bad.complete());
    let mut bad = original.clone();
    bad.desktops[0].observations[0]
        .broker_state
        .as_mut()
        .unwrap()
        .quarantined = true;
    assert!(!bad.complete());
    let mut bad = original.clone();
    bad.owned_lifetime.as_mut().unwrap().state.descendants = OwnedTreeState::Unknown;
    assert!(!bad.complete());
    let mut erased_latest = original.clone();
    let mut last = erased_latest.desktops[0].observations[0].clone();
    last.broker_state.as_mut().unwrap().owned_lifetime = None;
    erased_latest.desktops[0].observations.push(last);
    assert!(
        !erased_latest.complete(),
        "an earlier retired sample cannot hide erased latest ownership facts"
    );
    let legacy = crate::native::integration::view::tests::report()
        .integrations
        .remove(0);
    assert!(
        serde_json::to_value(legacy)
            .unwrap()
            .get("owned_lifetime")
            .is_none()
    );
}
#[test]
fn compact_owned_lifetime_keeps_unavailable_tree_and_local_retirement_visible() {
    let mut report = crate::native::integration::view::tests::report();
    report.integrations = vec![scenario()];
    let compact = serde_json::to_value(crate::native::integration::view::summary(&report)).unwrap();
    assert_eq!(
        compact["owned_lifetimes"][0]["state"]["descendants"],
        "unavailable"
    );
    assert_eq!(
        compact["owned_lifetimes"][0]["state"]["local_resources"],
        "retired"
    );
    assert_eq!(compact["owned_lifetimes"][0]["state"]["primary"], "exited");
    let legacy = crate::native::integration::view::tests::report();
    assert!(
        serde_json::to_value(crate::native::integration::view::summary(&legacy))
            .unwrap()
            .get("owned_lifetimes")
            .is_none()
    );
}

#[test]
fn legacy_original_recovery_cannot_acquire_owned_primary_capability() {
    // Saved DTOs only; this does not construct a Host proof or run a target.
    let (_, journal) = super::desktop_generation_presence_tests::pair(ScenarioKind::Clean, true);
    let mut action = journal.integration_actions[0].clone();
    action.terminal = None;
    let state = scenario().desktops[0].observations[0]
        .broker_state
        .clone()
        .unwrap();
    assert!(
        validate_original_broker_effect(&action, &state).is_err(),
        "a legacy original Activate cannot acquire another lifetime contract during recovery"
    );
}
// Valid saved DTO pair only; no Host owner, application or native target is created.
fn owned_saved_pair() -> (NativeIntegrationReport, NativeIntegrationJournal) {
    use theseus_native_protocol::v2;
    let (mut report, mut journal) =
        super::desktop_generation_presence_tests::pair(ScenarioKind::Clean, true);
    report.requirements.desktop.as_mut().unwrap().lifetime =
        LifetimeRequirement::OwnedPrimaryExited;
    report.compiled_requirements_digest = report.requirements.digest().unwrap();
    journal.requirements = report.requirements.clone();
    journal.compiled_requirements_digest = report.compiled_requirements_digest.clone();
    let mut header = journal.broker_bindings[0].original.expected_header.clone();
    header.os = "macos".into();
    header.display = SuppliedDisplay::MacOsGui {
        domain: "gui/1000".into(),
    };
    header.validate().unwrap();
    let header_digest = header.digest().unwrap().to_hex();
    let binding = &mut journal.broker_bindings[0];
    binding.original.expected_header = header;
    binding.expected_header_digest = header_digest.clone();
    journal.integration_ownership[0]
        .initial_binding
        .namespace_or_session_receipt_digest = header_digest.clone();
    journal.integration_ownership[0].binding_history[0].namespace_or_session_receipt_digest =
        header_digest.clone();
    let action = &mut journal.integration_actions[0];
    let IntegrationRequest::Desktop { request, .. } = &mut action.request else {
        panic!()
    };
    request.binding_digest = header_digest;
    if let DesktopAction::Activate { owned_lifetime, .. } = &mut request.action {
        *owned_lifetime = Some(OwnedDesktopLifetimeDefinition::MacosOriginalPrimaryV1);
    }
    let close_digest = request.close_request().unwrap().digest().unwrap().to_hex();
    if let DesktopAction::Activate { close_binding, .. } = &mut request.action {
        close_binding.action_digest = close_digest;
    }
    action.broker_identity = Some(
        serde_json::from_value(serde_json::to_value(request.identity().unwrap()).unwrap()).unwrap(),
    );
    let receipt_digest = request.digest().unwrap().to_hex();
    action.request_digest = v2::request_digest(&action.request.wire().unwrap())
        .unwrap()
        .to_hex();
    action.completion = Disposition::Complete {
        receipt: receipt_digest.clone(),
    };
    action.terminal = Some(IntegrationOutcome::Complete {
        receipt: receipt_digest.clone(),
    });
    let original = ActionReference {
        operation: action.operation.clone(),
        request_digest: action.request_digest.clone(),
    };
    let completed = scenario();
    let mut state = completed.desktops[0].observations[0]
        .broker_state
        .clone()
        .unwrap();
    let BrokerEffect::Receipt { receipt } = &mut state.effect else {
        panic!()
    };
    receipt.receipt_digest = receipt_digest;
    receipt.evidence = theseus_native_protocol::v2::broker::DesktopEvidence::SourceOwnedExecutable;
    let integration = &mut report.integrations[0];
    integration.dispositions = completed.dispositions;
    integration.owned_lifetime = Some(OwnedDesktopLifetimeProjection {
        original: original.clone(),
        state: state.owned_lifetime.clone().unwrap(),
    });
    integration.desktops[0].actions = vec![original];
    integration.desktops[0].observations[0].broker_state = Some(state);
    validate_pair(&report, &journal).expect("positive owned saved pair");
    (report, journal)
}
#[test]
fn owned_saved_pair_binds_actual_private_original_and_retains_pending_history() {
    let (mut report, journal) = owned_saved_pair();
    assert!(report.integrations[0].complete());
    let mut pending = report.integrations[0].desktops[0].observations[0].clone();
    pending.binding.id = "earlier-owned-pending".into();
    pending.binding.sequence = 0;
    pending.binding.complete = false;
    pending.evidence = None;
    pending.broker_state = Some(BrokerState {
        owned_lifetime: Some(OwnedLifetimeState::initial(
            OwnedDesktopLifetimeDefinition::MacosOriginalPrimaryV1,
        )),
        native_lifetime: None,
        effect: BrokerEffect::Unknown,
        owner: BrokerOwner::Retained,
        primary: BrokerPrimary::Unknown,
        descendants: BrokerDescendants::Unknown,
        cleanup_complete: false,
        quarantined: false,
    });
    report.integrations[0].desktops[0]
        .observations
        .insert(0, pending);
    validate_pair(&report, &journal)
        .expect("a later original terminal cannot erase legitimate Pending history");
}
#[test]
fn owned_saved_pair_rejects_two_public_digest_changes_without_private_change() {
    let (mut report, journal) = owned_saved_pair();
    let wrong = "de".repeat(32);
    report.integrations[0]
        .owned_lifetime
        .as_mut()
        .unwrap()
        .original
        .request_digest = wrong.clone();
    report.integrations[0].desktops[0].actions[0].request_digest = wrong;
    report.validate().unwrap();
    journal.validate().unwrap();
    assert!(
        validate_pair(&report, &journal).is_err(),
        "public-to-public equality is not exact private original request binding"
    );
}
#[test]
fn owned_saved_pair_rejects_observed_receipt_for_another_original_effect() {
    let (mut report, journal) = owned_saved_pair();
    let state = report.integrations[0].desktops[0].observations[0]
        .broker_state
        .as_mut()
        .unwrap();
    let BrokerEffect::Receipt { receipt } = &mut state.effect else {
        panic!()
    };
    receipt.receipt_digest = "ed".repeat(32);
    report.validate().unwrap();
    journal.validate().unwrap();
    assert!(
        validate_pair(&report, &journal).is_err(),
        "owned report receipt must equal private original acknowledged terminal"
    );
}
