use super::*;
use theseus_native_protocol::v2::broker::*;
fn native_state(authority: NativeLifetimeAuthority) -> BrokerState {
    let manager = authority == NativeLifetimeAuthority::ManagerInstance;
    BrokerState {
        owned_lifetime: None,
        effect: BrokerEffect::Receipt {
            receipt: BrokerEffectReceipt {
                evidence: theseus_native_protocol::v2::broker::DesktopEvidence::NativeActivation,
                acknowledged: true,
                receipt_digest: "ab".repeat(32),
            },
        },
        owner: BrokerOwner::Settled,
        primary: if manager {
            BrokerPrimary::Unknown
        } else {
            BrokerPrimary::Exited
        },
        descendants: BrokerDescendants::Unknown,
        cleanup_complete: false,
        quarantined: false,
        native_lifetime: Some(NativeLifetimeState {
            authority,
            required: if manager {
                NativeLifetimeRequirement::ActivationInstanceTerminated
            } else {
                NativeLifetimeRequirement::OwnedPrimaryExited
            },
            instance: NativeInstanceState::Terminated,
            primary: if manager {
                NativePrimaryState::Unavailable
            } else {
                NativePrimaryState::Exited
            },
            descendants: NativeTreeState::Unavailable,
            local_resources: NativeLocalState::Retired,
        }),
    }
}
pub(super) fn observation(state: BrokerState) -> BoundDesktopObservation {
    BoundDesktopObservation {
        generation: None,
        binding: ObservationBinding {
            id: "native-observation".into(),
            run_id: "run".into(),
            scenario_id: "clean".into(),
            ownership_id: "owner".into(),
            lease: "01".repeat(16),
            slot: ResourceSlot::new("desktop").unwrap(),
            request_id: "query".into(),
            action: Some("02".repeat(16)),
            binding_digest: "03".repeat(32),
            source_profile: crate::native::integration::PROFILE.into(),
            source_version: "fixture".into(),
            phase: IntegrationPhase::BeforeUninstall,
            definition: None,
            sequence: 1,
            started_ms: 0,
            finished_ms: 1,
            complete: true,
        },
        evidence: Some(theseus_native_protocol::v2::broker::DesktopEvidence::NativeActivation),
        broker_state: Some(state),
        readiness: None,
        entry_valid: None,
        entry_present: IntegrationProvenance::Unavailable {
            reason: "no entry inspection".into(),
        },
        activated: IntegrationProvenance::Unavailable {
            reason: "separate effect receipt".into(),
        },
        primary_exited: IntegrationProvenance::Unavailable {
            reason: "manager-only".into(),
        },
        descendant_scope_settled: IntegrationProvenance::Unavailable {
            reason: "native tree unavailable".into(),
        },
    }
}
#[test]
fn native_scenario_excludes_only_committed_unavailable_axes_and_preserves_old_bytes() {
    let mut scenario = crate::native::integration::view::tests::report()
        .integrations
        .remove(0);
    let old = serde_json::to_value(&scenario).unwrap();
    assert!(old.get("native_lifetime").is_none());
    let state = native_state(NativeLifetimeAuthority::ManagerInstance);
    let bound = observation(state.clone());
    let reference = ActionReference {
        operation: bound.binding.action.clone().unwrap(),
        request_digest: "04".repeat(32),
    };
    scenario.desktops = vec![super::DesktopEvidence {
        slot: bound.binding.slot.clone(),
        actions: vec![reference.clone()],
        observations: vec![bound],
    }];
    scenario.native_lifetime = Some(NativeDesktopLifetimeProjection {
        original: reference,
        state: state.native_lifetime.unwrap(),
    });
    let done = || Disposition::Complete {
        receipt: "separate required domain completed".into(),
    };
    scenario.dispositions.helper_worker = done();
    scenario.dispositions.registration = done();
    scenario.dispositions.manager_action = done();
    scenario.dispositions.broker = done();
    scenario.dispositions.primary = Disposition::NotStarted;
    scenario.dispositions.descendant_scope = Disposition::NotStarted;
    assert!(scenario.complete());
    assert!(
        !scenario.dispositions.complete(),
        "legacy strong completion is unchanged"
    );
    for failed in [
        Disposition::Unknown {
            reason: "historically owned scope lost".into(),
        },
        Disposition::Failed {
            reason: "owned scope failed".into(),
        },
    ] {
        let mut bad = scenario.clone();
        bad.dispositions.descendant_scope = failed.clone();
        assert!(!bad.complete());
        let mut bad = scenario.clone();
        bad.dispositions.primary = failed;
        assert!(!bad.complete());
    }
    let mut wrong = scenario.clone();
    wrong
        .native_lifetime
        .as_mut()
        .unwrap()
        .original
        .request_digest = "05".repeat(32);
    assert!(!wrong.complete());
    let mut local = scenario;
    local
        .native_lifetime
        .as_mut()
        .unwrap()
        .state
        .local_resources = NativeLocalState::Retained;
    assert!(!local.complete());
}
#[test]
fn native_saved_view_names_unavailable_axes_and_rejects_borrowed_owned_mode() {
    let mut report = crate::native::integration::view::tests::report();
    let required = DesktopRequirement {
        resource: ResourceSlot::new("desktop").unwrap(),
        modes: vec![super::DesktopMode::NativeActivation],
        readiness: ReadinessRequirement {
            kind: ReadyKind::LocalChallenge,
            protocol: ReadyProtocol::TheseusFixtureReadyV1,
            deadline_ms: 1000,
        },
        lifetime: LifetimeRequirement::ActivationInstanceTerminated,
    };
    report.requirements = IntegrationRequirements {
        service: None,
        desktop: Some(required),
    };
    report.compiled_requirements_digest = report.requirements.digest().unwrap();
    let state = native_state(NativeLifetimeAuthority::ManagerInstance);
    let mut bound = observation(state.clone());
    bound.binding.run_id = report.run_id.clone();
    let reference = ActionReference {
        operation: bound.binding.action.clone().unwrap(),
        request_digest: "04".repeat(32),
    };
    let scenario = &mut report.integrations[0];
    scenario.services.clear();
    scenario.checks.clear();
    scenario.desktops = vec![super::DesktopEvidence {
        slot: bound.binding.slot.clone(),
        actions: vec![reference.clone()],
        observations: vec![bound],
    }];
    scenario.native_lifetime = Some(NativeDesktopLifetimeProjection {
        original: reference,
        state: state.native_lifetime.unwrap(),
    });
    let page = crate::native::integration::view::evidence_page(
        &report,
        crate::native::integration::view::Selector::Desktop,
        0,
        64,
    )
    .unwrap();
    let native = &page.records[0]["native_lifetime"]["state"];
    assert_eq!(native["primary"], "unavailable");
    assert_eq!(native["descendants"], "unavailable");
    assert_eq!(native["local_resources"], "retired");
    report.requirements.desktop.as_mut().unwrap().modes = vec![super::DesktopMode::OwnedExecutable];
    report.compiled_requirements_digest = report.requirements.digest().unwrap();
    assert!(
        report.validate().is_err(),
        "native receipt cannot satisfy an owned saved report"
    );
}
#[test]
fn public_two_generations_never_serialize_original_activation_nonce() {
    use theseus_native_protocol::v2;
    let prior = v2::broker::BrokerOperationIdentity {
        helper_lease: "01".repeat(16),
        helper_operation: "02".repeat(16),
        broker_lease: "03".repeat(16),
        request_digest: "04".repeat(32),
        binding_digest: "05".repeat(32),
        activation_nonce: "06".repeat(16),
    };
    let witness = |operation: &str, program: &str| {
        let program = v2::ProgramId::new(program).unwrap();
        let lease = v2::LeaseId::parse(&prior.helper_lease).unwrap();
        let op = v2::OperationId::parse(operation).unwrap();
        DesktopSutRunWitness {
            lease: prior.helper_lease.clone(),
            operation: operation.into(),
            request_digest: v2::request_digest(&v2::Request::Run {
                lease,
                operation: op,
                program: program.clone(),
            })
            .unwrap()
            .to_hex(),
            program,
        }
    };
    let first = DesktopActivationTransition {
        definition: v2::DesktopDefinitionKey::CONFIGURED_TARGET,
        tuple_digest: "08".repeat(32),
        sut: witness(&"07".repeat(16), "configure"),
        previous_activation: None,
    };
    let second = DesktopActivationTransition {
        definition: first.definition,
        tuple_digest: first.tuple_digest.clone(),
        sut: witness(&"09".repeat(16), "install_target"),
        previous_activation: Some(prior.clone()),
    };
    let mut a = observation(native_state(NativeLifetimeAuthority::ManagerInstance));
    a.generation = Some(desktop_generation_evidence(&first));
    let mut b = a.clone();
    b.generation = Some(desktop_generation_evidence(&second));
    let public = serde_json::to_string(&[a, b]).unwrap();
    assert!(
        !public.contains(&prior.activation_nonce),
        "public generation2 leaked private original activation_nonce"
    );
    assert!(!public.contains("activation_nonce"));
    assert!(public.contains(&second.sut.operation));
}
#[test]
fn native_manager_termination_satisfies_only_its_mandatory_original_lifetime() {
    let state = native_state(NativeLifetimeAuthority::ManagerInstance);
    state.validate().unwrap();
    assert!(state.original_retired());
    let observed = observation(state.clone());
    assert!(desktop_predicate_satisfied(
        &observed,
        &IntegrationPredicate::Lifetime(LifetimeRequirement::ActivationInstanceTerminated)
    ));
    assert!(!desktop_predicate_satisfied(
        &observed,
        &IntegrationPredicate::Lifetime(LifetimeRequirement::OwnedPrimaryExited)
    ));
    assert!(!desktop_predicate_satisfied(
        &observed,
        &IntegrationPredicate::Lifetime(LifetimeRequirement::OwnedProcessTreeSettled)
    ));
    assert!(settled_original_broker_state(&state));
}
#[test]
fn native_child_and_handle_use_original_projection_and_never_legacy_tree_retirement() {
    for authority in [
        NativeLifetimeAuthority::OriginalChild,
        NativeLifetimeAuthority::OriginalPrimaryHandle,
    ] {
        let state = native_state(authority);
        state.validate().unwrap();
        let observed = observation(state.clone());
        assert!(desktop_predicate_satisfied(
            &observed,
            &IntegrationPredicate::Lifetime(LifetimeRequirement::OwnedPrimaryExited)
        ));
        assert!(settled_original_broker_state(&state));
        let mut missing = observed.clone();
        missing.broker_state.as_mut().unwrap().native_lifetime = None;
        assert!(
            !desktop_predicate_satisfied(
                &missing,
                &IntegrationPredicate::Lifetime(LifetimeRequirement::OwnedPrimaryExited)
            ),
            "native evidence cannot borrow a legacy primary fact"
        );
        let mut forged = observed;
        let s = forged.broker_state.as_mut().unwrap();
        s.primary = BrokerPrimary::Exited;
        s.descendants = BrokerDescendants::Joined;
        s.cleanup_complete = true;
        assert!(!desktop_predicate_satisfied(
            &forged,
            &IntegrationPredicate::Lifetime(LifetimeRequirement::OwnedProcessTreeSettled)
        ));
    }
}
#[test]
fn native_original_progress_cannot_drop_projection_or_redeclare_unknown_unavailable() {
    let previous = native_state(NativeLifetimeAuthority::ManagerInstance);
    let mut missing = previous.clone();
    missing.native_lifetime = None;
    assert!(validate_broker_progress(&previous, &missing).is_err());
    let mut unknown = previous.clone();
    unknown.native_lifetime.as_mut().unwrap().primary = NativePrimaryState::Unknown;
    assert!(validate_broker_progress(&unknown, &previous).is_err());
    let mut regressed = previous.clone();
    regressed.native_lifetime.as_mut().unwrap().local_resources = NativeLocalState::Retained;
    assert!(validate_broker_progress(&previous, &regressed).is_err());
}
