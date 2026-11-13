use super::*;
use v2::broker::{
    BrokerDescendants, BrokerEffect, BrokerEffectReceipt, BrokerOwner, BrokerPrimary, BrokerState,
    DesktopAction, DesktopEvidence as BrokerEvidence, DesktopRequest,
};
fn original() -> IntegrationActionRecord {
    let mut request:DesktopRequest=serde_json::from_value(serde_json::json!({"helper_lease":"01".repeat(16),"helper_operation":"02".repeat(16),"broker_lease":"03".repeat(16),"binding_digest":"04".repeat(32),"activation_nonce":"05".repeat(16),"slot":"desktop","prior_binding_digest":"06".repeat(32),"action":{"kind":"activate","mode":"owned_executable","entry_digest":"07".repeat(32),"expectation_digest":"08".repeat(32),"close_binding":{"operation":"09".repeat(16),"action_digest":"00".repeat(32),"nonce":"0a".repeat(16)}}})).unwrap();
    let close = request.close_request().unwrap().digest().unwrap().to_hex();
    if let DesktopAction::Activate { close_binding, .. } = &mut request.action {
        close_binding.action_digest = close;
    }
    let identity = request.identity().unwrap();
    let wire = v2::Request::DesktopAction {
        request: Box::new(request.clone()),
    };
    IntegrationActionRecord {
        ownership_id: "owner".into(),
        lease: request.helper_lease.clone(),
        slot: ResourceSlot::new("desktop").unwrap(),
        operation: request.helper_operation.clone(),
        request: IntegrationRequest::Desktop {
            lease: request.helper_lease.clone(),
            operation: request.helper_operation.clone(),
            slot: ResourceSlot::new("desktop").unwrap(),
            prior_binding_digest: request.prior_binding_digest.clone(),
            mode: DesktopMode::OwnedExecutable,
            request: Box::new(request),
        },
        request_digest: v2::request_digest(&wire).unwrap().to_hex(),
        prior_binding_digest: "06".repeat(32),
        broker_identity: Some(
            serde_json::from_value(serde_json::to_value(identity).unwrap()).unwrap(),
        ),
        completion: unknown("reply lost"),
        terminal: None,
    }
}
fn pending() -> BrokerState {
    BrokerState {
        owned_lifetime: None,
        native_lifetime: None,
        effect: BrokerEffect::Receipt {
            receipt: BrokerEffectReceipt {
                evidence: BrokerEvidence::SourceOwnedExecutable,
                acknowledged: true,
                receipt_digest: "0b".repeat(32),
            },
        },
        owner: BrokerOwner::Retained,
        primary: BrokerPrimary::Running,
        descendants: BrokerDescendants::Pending,
        cleanup_complete: false,
        quarantined: false,
    }
}
fn settled() -> BrokerState {
    BrokerState {
        owner: BrokerOwner::Settled,
        primary: BrokerPrimary::Exited,
        descendants: BrokerDescendants::Joined,
        cleanup_complete: true,
        ..pending()
    }
}
fn saved_desktop(store: &ReportStore) -> (NativeIntegrationReport, NativeIntegrationJournal) {
    use crate::native::model::*;
    let mut report = crate::native::integration::view::tests::report();
    let owner = "0c".repeat(16);
    let slot = ResourceSlot::new("desktop").unwrap();
    report.requirements = IntegrationRequirements {
        service: None,
        desktop: Some(DesktopRequirement {
            resource: slot.clone(),
            modes: vec![DesktopMode::OwnedExecutable],
            readiness: ReadinessRequirement {
                kind: ReadyKind::LocalChallenge,
                protocol: ReadyProtocol::TheseusFixtureReadyV1,
                deadline_ms: 1000,
            },
            lifetime: LifetimeRequirement::OwnedProcessTreeSettled,
        }),
    };
    report.compiled_requirements_digest = report.requirements.digest().unwrap();
    report.catalog.scenarios[0].ownership_id = owner.clone();
    report.catalog.scenarios[0].dispositions.local = unknown("historical controller owner lost");
    let evidence = &mut report.integrations[0];
    evidence.ownership_id = owner.clone();
    evidence.services.clear();
    evidence.desktops = vec![DesktopEvidence {
        slot: slot.clone(),
        observations: vec![],
        actions: vec![],
    }];
    evidence.checks[0].slot = slot.clone();
    evidence.checks[0].predicate = IntegrationPredicate::ApplicationReady;
    let header:v2::broker::IntegrationBrokerHeader=serde_json::from_value(serde_json::json!({"profile":"native_integration_broker_v1","binding":"11".repeat(16),"manifest_digest":report.manifest_digest,"workspace_marker":"12".repeat(16),"os":"linux","target_id":"original-target","ownership_id":owner,"provider_epoch":"13".repeat(32),"session_id":"supplied-session","session_epoch":"14".repeat(32),"broker_endpoint":{"kind":"unix_path","path":"/fixture/broker.sock"},"broker_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"helper_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"display":{"kind":"x11","display":":92","authority_file":"/fixture/auth","bus":"/fixture/bus.sock"},"allowed_modes":["owned_executable"]})).unwrap();
    let mut action = original();
    action.ownership_id = owner.clone();
    let IntegrationRequest::Desktop { request, .. } = &mut action.request else {
        unreachable!()
    };
    request.binding_digest = header.digest().unwrap().to_hex();
    let close_digest = request.close_request().unwrap().digest().unwrap().to_hex();
    if let DesktopAction::Activate { close_binding, .. } = &mut request.action {
        close_binding.action_digest = close_digest;
    }
    action.broker_identity = Some(
        serde_json::from_value(serde_json::to_value(request.identity().unwrap()).unwrap()).unwrap(),
    );
    action.request_digest = v2::request_digest(&action.request.wire().unwrap())
        .unwrap()
        .to_hex();
    let binding = ResourceBindingSnapshot {
        digest: action.prior_binding_digest.clone(),
        basis: BindingBasis::VacantRegistration,
        evidence_ids: vec!["original-entry-absence".into()],
        definition: None,
        artifact_commitment: "15".repeat(32),
        namespace_or_session_receipt_digest: "16".repeat(32),
        original_action: None,
    };
    let native = NativeResource {
        scenario_id: "clean".into(),
        ownership_id: owner.clone(),
        catalog_authority: NativeCatalogAuthority::Valid,
        ownership: NativeOwnership::Acquired {
            lease: action.lease.clone(),
            prepared: NativeIntent {
                intent: "17".repeat(16),
                binding: "18".repeat(16),
                manifest_digest: report.manifest_digest.clone(),
                workspace_marker: "19".repeat(16),
                reset_epoch: "1a".repeat(32),
                publication_scope: "file_and_directory_synced".into(),
            },
        },
    };
    let original_bind = v2::broker::BrokerBind {
        ownership_id: owner.clone(),
        helper_lease: action.lease.clone(),
        helper_operation: "1b".repeat(16),
        slot: v2::ResourceSlot::new("desktop").unwrap(),
        broker_intent: "1c".repeat(16),
        expected_header: header.clone(),
    };
    let broker = BrokerBindingRecord {
        original: original_bind.clone(),
        ownership_id: owner.clone(),
        helper_lease: action.lease.clone(),
        slot: slot.clone(),
        original_intent: BrokerIntentRecord {
            intent: original_bind.broker_intent.clone(),
            binding: header.binding.clone(),
            manifest_digest: header.manifest_digest.clone(),
            workspace_marker: header.workspace_marker.clone(),
        },
        publication_scope: Some(v2::broker::IntentPublicationScope::FileAndDirectorySynced),
        original_intent_reference: format!("{}.json", original_bind.broker_intent),
        expected_header_digest: header.digest().unwrap().to_hex(),
        session_id: header.session_id.clone(),
        session_epoch: header.session_epoch.clone(),
        broker_lease: Some("03".repeat(16)),
        disposition: settled_disposition("original durable mapping"),
    };
    let journal = NativeIntegrationJournal {
        kind: "theseus_native_journal".into(),
        schema: 2,
        profile: "native_integration_v1".into(),
        run_id: report.run_id.clone(),
        launch_fingerprint: Some("1d".repeat(32)),
        recipe_digest: report.recipe_digest.clone(),
        core_recipe_digest: report.core_recipe_digest.clone(),
        core_manifest_digest: report.core_manifest_digest.clone(),
        integration_manifest_digest: report.integration_manifest_digest.clone(),
        manifest_digest: report.manifest_digest.clone(),
        requirements: report.requirements.clone(),
        compiled_requirements_digest: report.compiled_requirements_digest.clone(),
        selected_scenarios: report.selected_scenarios.clone(),
        ownership: vec![native],
        operations: vec![],
        provider_calls: vec![],
        releases: vec![],
        recovery_attempts: vec![],
        failures: vec![],
        integration_ownership: vec![IntegrationResourceRecord {
            key: IntegrationResourceKey {
                scenario_id: "clean".into(),
                ownership_id: owner,
                slot,
            },
            kind: IntegrationResourceKind::Desktop,
            manifest_digest: report.manifest_digest.clone(),
            initial_binding: binding.clone(),
            binding_history: vec![binding.clone()],
            current_binding_digest: binding.digest,
            revocation: None,
            disposition: unknown("original lifetime unproved"),
        }],
        integration_actions: vec![action],
        broker_bindings: vec![broker],
        readiness_probes: vec![],
        integration_recoveries: vec![],
    };
    validate_pair(&report, &journal).unwrap();
    store.save_native_integration_report(&report).unwrap();
    store.save_native_integration_journal(&journal).unwrap();
    (report, journal)
}
fn settled_disposition(receipt: &str) -> Disposition {
    Disposition::Complete {
        receipt: receipt.into(),
    }
}
fn publish_recovery(projector: &mut RunProjector<'_>, owner: &str) -> String {
    let id = "1e".repeat(16);
    projector
        .record_recovery_attempt(crate::native::model::NativeRecoveryAttempt {
            recovery_id: id.clone(),
            ownership_id: owner.into(),
            prior_local: unknown("historical controller owner lost"),
            local: unknown("current local owner pending"),
            remote: unknown("provider not queried"),
        })
        .unwrap();
    publish_attempt(projector, &id, owner).unwrap();
    id
}
fn stored_bytes(store: &ReportStore) -> (Vec<u8>, Vec<u8>) {
    (
        std::fs::read(store.root().join(crate::native::model::JOURNAL_FILENAME)).unwrap(),
        std::fs::read(store.native_report_path()).unwrap(),
    )
}
fn bound_original_state(
    report: &NativeIntegrationReport,
    original: &IntegrationActionRecord,
    state: BrokerState,
) -> BoundDesktopObservation {
    let evidence = match &state.effect {
        BrokerEffect::Receipt { receipt } => Some(receipt.evidence),
        _ => None,
    };
    let unavailable = || IntegrationProvenance::Unavailable {
        reason: "not sampled by this exact original lifetime observation".into(),
    };
    BoundDesktopObservation {
        generation: None,
        entry_valid: None,
        broker_state: Some(state),
        readiness: None,
        evidence,
        binding: ObservationBinding {
            id: "original-before-recovery".into(),
            run_id: report.run_id.clone(),
            scenario_id: "clean".into(),
            ownership_id: original.ownership_id.clone(),
            lease: original.lease.clone(),
            slot: original.slot.clone(),
            request_id: "original-query".into(),
            action: Some(original.operation.clone()),
            binding_digest: original.prior_binding_digest.clone(),
            source_profile: "native_integration_broker_v1".into(),
            source_version: "1".into(),
            phase: IntegrationPhase::BeforeUninstall,
            definition: None,
            sequence: 1,
            started_ms: 0,
            finished_ms: 1,
            complete: true,
        },
        entry_present: unavailable(),
        activated: unavailable(),
        primary_exited: unavailable(),
        descendant_scope_settled: unavailable(),
    }
}
#[test]
fn first_recovered_effect_rejects_before_publication_and_cancel_then_correct_query_succeeds() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::report::create(&root.path().join("run")).unwrap();
    let (report, mut journal) = saved_desktop(&store);
    let receipt = "0b".repeat(32);
    journal.integration_actions[0].completion = settled_disposition(&receipt);
    journal.integration_actions[0].terminal = Some(IntegrationOutcome::Complete { receipt });
    let original = journal.integration_actions[0].clone();
    let owner = journal.ownership[0].clone();
    let mut projector = RunProjector::new(
        &store,
        StoredRun::NativeIntegration(report),
        StoredJournal::NativeIntegration(journal),
    )
    .unwrap();
    let recovery_id = publish_recovery(&mut projector, &owner.ownership_id);
    let before = stored_bytes(&store);
    let mut changed = pending();
    if let BrokerEffect::Receipt { receipt } = &mut changed.effect {
        receipt.receipt_digest = "ff".repeat(32);
    }
    let mut rejected_ack = pending();
    if let BrokerEffect::Receipt { receipt } = &mut rejected_ack.effect {
        receipt.acknowledged = false;
    }
    let unknown = BrokerState {
        effect: BrokerEffect::Unknown,
        ..pending()
    };
    for bad in [changed, rejected_ack, unknown] {
        let mut cancels = 0;
        let result = query_original_desktop_observing(
            &original,
            &CancellationToken::default(),
            |action| {
                let identity = match action {
                    Action::QueryDesktop { identity } => identity,
                    Action::CancelDesktop { identity } => {
                        cancels += 1;
                        identity
                    }
                    _ => panic!("only exact original query permitted"),
                };
                let state = if cancels == 0 {
                    bad.clone()
                } else {
                    BrokerState {
                        owner: BrokerOwner::Settled,
                        primary: BrokerPrimary::Exited,
                        descendants: BrokerDescendants::Joined,
                        cleanup_complete: true,
                        ..bad.clone()
                    }
                };
                Ok(v2::Reply::DesktopState {
                    identity: Box::new(identity),
                    state: Box::new(state),
                })
            },
            |observation| projector.record_broker_recovery_observation(&recovery_id, observation),
        );
        assert!(result.is_err());
        assert_eq!(
            cancels, 0,
            "contradictory first effect must not authorize subsequent original Cancel"
        );
        assert_eq!(
            stored_bytes(&store),
            before,
            "rejection must preserve journal/report bytes"
        );
        let StoredJournal::NativeIntegration(saved) = projector.journal() else {
            unreachable!()
        };
        let mut forged = saved.clone();
        let IntegrationRequest::Desktop { request, .. } = &original.request else {
            unreachable!()
        };
        let observation = RecoveredBrokerObservation {
            original: action_reference(&original),
            identity: request.identity().unwrap(),
            source: BrokerObservationSource::Query,
            state: bad,
        };
        assert!(
            projector
                .record_broker_recovery_observation(&recovery_id, observation.clone())
                .is_err(),
            "projector must independently reject the first contradictory effect"
        );
        assert_eq!(stored_bytes(&store), before);
        forged.integration_recoveries[0]
            .broker_observations
            .push(observation);
        assert!(
            NativeIntegrationJournal::decode(&serde_json::to_vec(&forged).unwrap()).is_err(),
            "saved contradictory first effect must refuse"
        );
    }
    let mut cancels = 0;
    let query = query_original_desktop_observing(
        &original,
        &CancellationToken::default(),
        |action| {
            let identity = match action {
                Action::QueryDesktop { identity } => identity,
                Action::CancelDesktop { identity } => {
                    cancels += 1;
                    identity
                }
                _ => panic!("no fresh effect"),
            };
            Ok(v2::Reply::DesktopState {
                identity: Box::new(identity),
                state: Box::new(if cancels == 0 { pending() } else { settled() }),
            })
        },
        |observation| projector.record_broker_recovery_observation(&recovery_id, observation),
    )
    .unwrap();
    assert_eq!(cancels, 1);
    assert_eq!(Some(query.outcome), original.terminal);
    drop(projector);
    let (_, NativeJournalVersion::IntegrationV2(saved)) =
        crate::native::storage::load_native_versioned(&store).unwrap()
    else {
        unreachable!()
    };
    assert_eq!(saved.integration_actions[0], original);
    assert_eq!(saved.integration_recoveries[0].broker_observations.len(), 2);
}
#[test]
fn first_recovered_lifetime_cannot_contradict_bound_pre_recovery_not_created() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::report::create(&root.path().join("run")).unwrap();
    let (mut report, mut journal) = saved_desktop(&store);
    let mut not_created = BrokerState {
        owner: BrokerOwner::Settled,
        primary: BrokerPrimary::NotCreated,
        descendants: BrokerDescendants::NotCreated,
        cleanup_complete: true,
        ..pending()
    };
    if let BrokerEffect::Receipt { receipt } = &mut not_created.effect {
        receipt.acknowledged = false;
    }
    journal.integration_actions[0].completion = Disposition::Failed {
        reason: "original desktop effect was not acknowledged".into(),
    };
    journal.integration_actions[0].terminal = Some(IntegrationOutcome::Failed {
        reason: "original desktop effect was not acknowledged".into(),
    });
    let original = journal.integration_actions[0].clone();
    let owner = journal.ownership[0].clone();
    let bound = bound_original_state(&report, &original, not_created.clone());
    report.integrations[0].desktops[0].observations.push(bound);
    let mut projector = RunProjector::new(
        &store,
        StoredRun::NativeIntegration(report.clone()),
        StoredJournal::NativeIntegration(journal),
    )
    .unwrap();
    let recovery_id = publish_recovery(&mut projector, &owner.ownership_id);
    let before = stored_bytes(&store);
    let IntegrationRequest::Desktop { request, .. } = &original.request else {
        unreachable!()
    };
    let identity = request.identity().unwrap();
    for (primary, descendants, owner, cleanup_complete) in [
        (
            BrokerPrimary::Running,
            BrokerDescendants::Pending,
            BrokerOwner::Retained,
            false,
        ),
        (
            BrokerPrimary::Exited,
            BrokerDescendants::Joined,
            BrokerOwner::Settled,
            true,
        ),
    ] {
        let bad = BrokerState {
            primary,
            descendants,
            owner,
            cleanup_complete,
            ..not_created.clone()
        };
        let observation = RecoveredBrokerObservation {
            original: action_reference(&original),
            identity: identity.clone(),
            source: BrokerObservationSource::Query,
            state: bad.clone(),
        };
        assert!(
            projector
                .record_broker_recovery_observation(&recovery_id, observation.clone())
                .is_err()
        );
        assert_eq!(stored_bytes(&store), before);
        let StoredJournal::NativeIntegration(saved) = projector.journal() else {
            unreachable!()
        };
        let mut forged = saved.clone();
        forged.integration_recoveries[0]
            .broker_observations
            .push(observation);
        assert!(
            validate_pair(&report, &forged).is_err(),
            "reload must compare recovery with the original bound report lifetime"
        );
        forged.integration_recoveries[0].broker_observations.insert(
            0,
            RecoveredBrokerObservation {
                original: action_reference(&original),
                identity: identity.clone(),
                source: BrokerObservationSource::Query,
                state: not_created.clone(),
            },
        );
        assert!(
            NativeIntegrationJournal::decode(&serde_json::to_vec(&forged).unwrap()).is_err(),
            "journal-only history must keep NotCreated irreversible"
        );
    }
    projector
        .record_broker_recovery_observation(
            &recovery_id,
            RecoveredBrokerObservation {
                original: action_reference(&original),
                identity,
                source: BrokerObservationSource::Query,
                state: not_created,
            },
        )
        .unwrap();
    assert_ne!(stored_bytes(&store), before);
    drop(projector);
    crate::native::storage::load_native_versioned(&store).unwrap();
}
#[test]
fn not_created_primary_and_scope_each_remain_irreversible() {
    for primary in [BrokerPrimary::Running, BrokerPrimary::Exited] {
        let previous = BrokerState {
            primary: BrokerPrimary::NotCreated,
            ..BrokerState::unknown()
        };
        let next = BrokerState {
            primary,
            ..previous.clone()
        };
        assert!(validate_broker_progress(&previous, &next).is_err());
    }
    for descendants in [BrokerDescendants::Pending, BrokerDescendants::Joined] {
        let previous = BrokerState {
            descendants: BrokerDescendants::NotCreated,
            ..BrokerState::unknown()
        };
        let next = BrokerState {
            descendants,
            ..previous.clone()
        };
        assert!(validate_broker_progress(&previous, &next).is_err());
    }
}
#[test]
fn earlier_unknown_observation_remains_saved_when_original_receipt_first_becomes_known() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::report::create(&root.path().join("run")).unwrap();
    let (report, journal) = saved_desktop(&store);
    let original = journal.integration_actions[0].clone();
    let owner = journal.ownership[0].clone();
    let mut projector = RunProjector::new(
        &store,
        StoredRun::NativeIntegration(report),
        StoredJournal::NativeIntegration(journal),
    )
    .unwrap();
    let recovery_id = publish_recovery(&mut projector, &owner.ownership_id);
    let mut cancels = 0;
    let queried = query_original_desktop_observing(
        &original,
        &CancellationToken::default(),
        |action| {
            let identity = match action {
                Action::QueryDesktop { identity } => identity,
                Action::CancelDesktop { identity } => {
                    cancels += 1;
                    identity
                }
                _ => panic!("only original control"),
            };
            let state = match cancels {
                0 => BrokerState {
                    effect: BrokerEffect::Unknown,
                    ..pending()
                },
                1 => pending(),
                _ => settled(),
            };
            Ok(v2::Reply::DesktopState {
                identity: Box::new(identity),
                state: Box::new(state),
            })
        },
        |observation| projector.record_broker_recovery_observation(&recovery_id, observation),
    )
    .unwrap();
    projector
        .record_integration_query(
            &recovery_id,
            OriginalQueryOutcome {
                original: action_reference(&original),
                view: OriginalQueryView::CachedTerminal,
                outcome: queried.outcome.clone(),
            },
        )
        .unwrap();
    let IntegrationOutcome::Complete { receipt } = &queried.outcome else {
        panic!("original receipt")
    };
    projector
        .record_integration_completion(
            &original.lease,
            &original.operation,
            settled_disposition(receipt),
            queried.outcome,
        )
        .expect("later first receipt must not retroactively invalidate earlier Unknown knowledge");
    drop(projector);
    let (NativeReportVersion::IntegrationV2(report), NativeJournalVersion::IntegrationV2(saved)) =
        crate::native::storage::load_native_versioned(&store).unwrap()
    else {
        unreachable!()
    };
    assert_eq!(saved.integration_recoveries[0].broker_observations.len(), 3);
    assert!(matches!(
        saved.integration_recoveries[0].broker_observations[0]
            .state
            .effect,
        BrokerEffect::Unknown
    ));
    assert!(matches!(
        saved.integration_actions[0].terminal,
        Some(IntegrationOutcome::Complete { .. })
    ));
    assert!(
        saved.integration_recoveries[0]
            .prior_local
            .iter()
            .any(|p| !complete(p))
    );
    assert_eq!(
        saved.integration_recoveries[0]
            .broker_effect_baselines
            .len(),
        1
    );
    assert!(
        saved.integration_recoveries[0].broker_effect_baselines[0]
            .terminal
            .is_none()
    );
    let mut legacy = serde_json::to_value(&saved).unwrap();
    legacy["integration_recoveries"][0]
        .as_object_mut()
        .unwrap()
        .remove("broker_effect_baselines");
    let legacy = NativeIntegrationJournal::decode(&serde_json::to_vec(&legacy).unwrap()).expect(
        "legacy omitted baseline must infer first terminal from original cached query provenance",
    );
    assert!(
        legacy.integration_recoveries[0]
            .broker_effect_baselines
            .is_empty()
    );
    let mut invented = saved.clone();
    invented.integration_recoveries[0].queries.clear();
    assert!(
        NativeIntegrationJournal::decode(&serde_json::to_vec(&invented).unwrap()).is_err(),
        "a new terminal needs original cached query provenance"
    );
    let mut changed_baseline = saved.clone();
    changed_baseline.integration_recoveries[0].broker_effect_baselines[0].terminal =
        saved.integration_actions[0].terminal.clone();
    assert!(
        NativeIntegrationJournal::decode(&serde_json::to_vec(&changed_baseline).unwrap()).is_err(),
        "later knowledge cannot be inserted before the earlier Unknown sample"
    );
    let mut changed_query = saved.clone();
    changed_query.integration_actions[0].terminal = None;
    changed_query.integration_actions[0].completion = unknown("terminal barrier not yet written");
    changed_query.integration_recoveries[0].queries[0].outcome = IntegrationOutcome::Complete {
        receipt: "ff".repeat(32),
    };
    assert!(
        NativeIntegrationJournal::decode(&serde_json::to_vec(&changed_query).unwrap()).is_err(),
        "cached query provenance cannot change an already observed original receipt"
    );
    let terminal = saved.integration_actions[0].terminal.clone();
    let mut projector = RunProjector::new(
        &store,
        StoredRun::NativeIntegration(report),
        StoredJournal::NativeIntegration(saved),
    )
    .unwrap();
    let next = "20".repeat(16);
    projector
        .record_recovery_attempt(crate::native::model::NativeRecoveryAttempt {
            recovery_id: next.clone(),
            ownership_id: owner.ownership_id.clone(),
            prior_local: unknown("historical controller owner lost"),
            local: unknown("new local owner pending"),
            remote: unknown("provider not queried"),
        })
        .unwrap();
    publish_attempt(&mut projector, &next, &owner.ownership_id).unwrap();
    let StoredJournal::NativeIntegration(saved) = projector.journal() else {
        unreachable!()
    };
    assert_eq!(
        saved.integration_recoveries[1].broker_effect_baselines[0].terminal,
        terminal
    );
    let mut regressed = saved.clone();
    regressed.integration_recoveries[1].broker_effect_baselines[0].terminal = None;
    assert!(
        NativeIntegrationJournal::decode(&serde_json::to_vec(&regressed).unwrap()).is_err(),
        "later attempt cannot forget prior original query terminal knowledge"
    );
}
fn ready_desktop_pair(store: &ReportStore) -> (NativeIntegrationReport, NativeIntegrationJournal) {
    let (mut report, mut journal) = saved_desktop(store);
    let expectation = ReadinessExpectation {
        role: DefinitionRole::Configured,
        artifact: ArtifactRole::Target,
        artifact_length: 123,
        configuration_digest: "21".repeat(32),
        artifact_digest: "22".repeat(32),
        version: "target-v1".into(),
        data_digest: "23".repeat(32),
        initialized: true,
    };
    let expectation_digest = expectation.digest().unwrap();
    let action = &mut journal.integration_actions[0];
    let IntegrationRequest::Desktop { request, .. } = &mut action.request else {
        unreachable!()
    };
    if let DesktopAction::Activate {
        expectation_digest: original,
        ..
    } = &mut request.action
    {
        *original = expectation_digest.clone();
    }
    let close = request.close_request().unwrap().digest().unwrap().to_hex();
    if let DesktopAction::Activate { close_binding, .. } = &mut request.action {
        close_binding.action_digest = close;
    }
    let identity = request.identity().unwrap();
    action.broker_identity =
        Some(serde_json::from_value(serde_json::to_value(&identity).unwrap()).unwrap());
    action.request_digest = v2::request_digest(&action.request.wire().unwrap())
        .unwrap()
        .to_hex();
    action.completion = settled_disposition(&"0b".repeat(32));
    action.terminal = Some(IntegrationOutcome::Complete {
        receipt: "0b".repeat(32),
    });
    let action = action.clone();
    let challenge = ReadinessChallenge {
        resource_binding_digest: action.prior_binding_digest.clone(),
        action_digest: identity.request_digest.clone(),
        expectation_digest: expectation_digest.clone(),
        ownership_id: action.ownership_id.clone(),
        lease: identity.broker_lease.clone(),
        slot: action.slot.clone(),
        operation: action.operation.clone(),
        nonce: identity.activation_nonce.clone(),
        deadline_ms: 1000,
    };
    let ready = v2::ReadinessObservation {
        peer_sid: None,
        expectation: expectation.wire(),
        slot: v2::ResourceSlot::new(action.slot.as_str()).unwrap(),
        operation: action.operation.clone(),
        action_digest: identity.request_digest.clone(),
        resource_binding_digest: action.prior_binding_digest.clone(),
        challenge_digest: challenge.digest().unwrap(),
        expectation_digest: expectation_digest.clone(),
        state: v2::ReadinessState::Ready,
        authenticated_instance: true,
        instance_binding_digest: identity.request_digest,
        peer_uid: Some(1000),
        peer_gid: Some(1000),
        artifact_sha256: Some(expectation.artifact_digest.clone()),
        version_literal: Some(expectation.version.clone()),
        data_sha256: Some(expectation.data_digest.clone()),
        configuration_digest: Some(expectation.configuration_digest.clone()),
        initialized: Some(true),
        response_digest: Some("24".repeat(32)),
        failure: None,
    };
    let mut bound = bound_original_state(&report, &action, pending());
    bound.binding.id = "desktop-ready-original".into();
    bound.binding.phase = IntegrationPhase::Configured;
    bound.readiness = Some(ready);
    journal.readiness_probes.push(ReadinessProbeRecord {
        challenge,
        expectation,
        expectation_digest,
        observation_ids: vec![bound.binding.id.clone()],
        completion: settled_disposition(&"24".repeat(32)),
    });
    report.integrations[0].desktops[0].observations.push(bound);
    (report, journal)
}
#[test]
fn desktop_ready_report_and_pair_use_original_instance_peer_and_private_receipt() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::report::create(&root.path().join("run")).unwrap();
    let (report, journal) = ready_desktop_pair(&store);
    report
        .validate()
        .expect("standalone desktop report uses desktop structural readiness contract");
    validate_pair(&report, &journal).unwrap();
    let mut wrong_receipt = journal.clone();
    wrong_receipt.readiness_probes[0].completion = settled_disposition(
        &report.integrations[0].desktops[0].observations[0]
            .binding
            .id,
    );
    assert!(
        validate_pair(&report, &wrong_receipt).is_err(),
        "public Ready must bind the exact private response digest"
    );
    let mut wrong_identity = report.clone();
    let ready = wrong_identity.integrations[0].desktops[0].observations[0]
        .readiness
        .as_mut()
        .unwrap();
    ready.action_digest = "ff".repeat(32);
    ready.instance_binding_digest = ready.action_digest.clone();
    assert!(
        wrong_identity.validate().is_ok(),
        "standalone fields cannot manufacture an independently trusted action identity"
    );
    assert!(validate_pair(&wrong_identity, &journal).is_err());
    let mut wrong_peer = report.clone();
    wrong_peer.integrations[0].desktops[0].observations[0]
        .readiness
        .as_mut()
        .unwrap()
        .peer_uid = Some(9999);
    assert!(wrong_peer.validate().is_ok());
    assert!(
        validate_pair(&wrong_peer, &journal).is_err(),
        "pair must use the original declared broker principal"
    );
}
#[test]
fn desktop_driver_readiness_completion_uses_response_digest_and_stays_unknown_without_it() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::report::create(&root.path().join("run")).unwrap();
    let (report, _) = ready_desktop_pair(&store);
    let mut observation = report.integrations[0].desktops[0].observations[0].clone();
    assert_eq!(
        crate::native::integration::desktop::desktop_readiness_completion(&observation),
        settled_disposition(&"24".repeat(32)),
        "completion receipt must be the authenticated response digest, not an observation label"
    );
    observation.readiness.as_mut().unwrap().response_digest = None;
    assert!(matches!(
        crate::native::integration::desktop::desktop_readiness_completion(&observation),
        Disposition::Unknown { .. }
    ));
}
#[test]
fn original_scope_history_and_lost_release_receipt_persist_without_rewriting_other_knowledge() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::report::create(&root.path().join("run")).unwrap();
    let (report, journal) = saved_desktop(&store);
    let original = journal.integration_actions[0].clone();
    let owner = journal.ownership[0].clone();
    let before_checks = report.integrations[0].checks.clone();
    let before_local = report.catalog.scenarios[0].dispositions.local.clone();
    let mut projector = RunProjector::new(
        &store,
        StoredRun::NativeIntegration(report),
        StoredJournal::NativeIntegration(journal),
    )
    .unwrap();
    let recovery_id = publish_recovery(&mut projector, &owner.ownership_id);
    let mut cancels = 0;
    let query = query_original_desktop_observing(
        &original,
        &CancellationToken::default(),
        |action| {
            let identity = match action {
                Action::QueryDesktop { identity } => identity,
                Action::CancelDesktop { identity } => {
                    cancels += 1;
                    identity
                }
                _ => panic!("only exact original Query/Cancel allowed"),
            };
            Ok(v2::Reply::DesktopState {
                identity: Box::new(identity),
                state: Box::new(if cancels >= 2 { settled() } else { pending() }),
            })
        },
        |observation| projector.record_broker_recovery_observation(&recovery_id, observation),
    )
    .unwrap();
    assert_eq!(cancels, 2);
    assert!(matches!(query.outcome, IntegrationOutcome::Complete { .. }));
    let StoredJournal::NativeIntegration(saved) = projector.journal() else {
        unreachable!()
    };
    assert!(
        saved.integration_actions[0].terminal.is_none(),
        "lifetime observation must not rewrite effect terminal"
    );
    assert!(
        matches!(
            saved.integration_ownership[0].disposition,
            Disposition::Unknown { .. }
        ),
        "native scope knowledge cannot rewrite the saved SUT resource disposition"
    );
    assert!(saved.integration_recoveries[0].broker_release.is_none());
    assert!(
        super::super::recovery::integration_release_blocked(saved, &owner.ownership_id),
        "unproved original effect still blocks a new Release"
    );
    let mut exact_effect = saved.clone();
    let receipt = "0b".repeat(32);
    exact_effect.integration_actions[0].completion = settled_disposition(&receipt);
    exact_effect.integration_actions[0].terminal = Some(IntegrationOutcome::Complete { receipt });
    exact_effect.integration_recoveries[0]
        .queries
        .push(OriginalQueryOutcome {
            original: action_reference(&original),
            view: OriginalQueryView::CachedTerminal,
            outcome: exact_effect.integration_actions[0]
                .terminal
                .clone()
                .unwrap(),
        });
    exact_effect.validate().unwrap();
    assert!(
        !super::super::recovery::integration_release_blocked(&exact_effect, &owner.ownership_id),
        "proved original scope permits cleanup release without upgrading the separate SUT resource Unknown"
    );
    exact_effect.integration_ownership[0].revocation = Some(AuthorityRevocation::ManagerRestarted);
    assert!(
        super::super::recovery::integration_release_blocked(&exact_effect, &owner.ownership_id),
        "a revoked original authority remains unresolved"
    );
    let lease = v2::LeaseId::parse(&original.lease).unwrap();
    let intent = v2::IntentId::from_bytes([0x17; 16]);
    reconcile_broker_release(
        &mut projector,
        &owner,
        lease,
        &v2::LeaseView::Held { intent },
        BrokerReleaseSource::QueryLease,
    )
    .unwrap();
    assert!(
        reconcile_broker_release(
            &mut projector,
            &owner,
            lease,
            &v2::LeaseView::AwaitingExternalReset {
                intent: v2::IntentId::from_bytes([0xff; 16])
            },
            BrokerReleaseSource::QueryLease
        )
        .is_err()
    );
    reconcile_broker_release(
        &mut projector,
        &owner,
        lease,
        &v2::LeaseView::AwaitingExternalReset { intent },
        BrokerReleaseSource::QueryLease,
    )
    .unwrap();
    super::super::recovery::record_observed_release(&mut projector, &owner, lease).unwrap();
    drop(projector);
    let (NativeReportVersion::IntegrationV2(report), NativeJournalVersion::IntegrationV2(saved)) =
        crate::native::storage::load_native_versioned(&store).unwrap()
    else {
        unreachable!()
    };
    assert_eq!(report.integrations[0].checks, before_checks);
    assert_eq!(report.catalog.scenarios[0].dispositions.local, before_local);
    assert!(matches!(
        report.outcome,
        crate::model::RunOutcome::Incomplete
    ));
    assert_eq!(saved.integration_actions[0], original);
    assert!(
        saved.releases.is_empty(),
        "query evidence must not fabricate a missing Release publication"
    );
    assert_eq!(saved.integration_recoveries[0].broker_observations.len(), 2);
    assert_eq!(
        saved.integration_recoveries[0]
            .broker_observations
            .last()
            .unwrap()
            .state,
        settled()
    );
    assert_eq!(
        saved.integration_recoveries[0]
            .broker_release
            .as_ref()
            .unwrap()
            .source,
        BrokerReleaseSource::QueryLease
    );
    assert!(matches!(
        saved.integration_recoveries[0].broker,
        Disposition::Complete { .. }
    ));
    assert!(
        saved.integration_recoveries[0]
            .prior_local
            .iter()
            .any(|p| !complete(p))
    );
    assert!(matches!(
        saved.recovery_attempts[0].local,
        Disposition::Unknown { .. }
    ));
}
#[test]
fn lost_quarantined_state_is_saved_and_cannot_be_replaced_by_an_invented_owner() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::report::create(&root.path().join("run")).unwrap();
    let (report, journal) = saved_desktop(&store);
    let original = journal.integration_actions[0].clone();
    let owner = journal.ownership[0].clone();
    let checks = report.integrations[0].checks.clone();
    let mut projector = RunProjector::new(
        &store,
        StoredRun::NativeIntegration(report),
        StoredJournal::NativeIntegration(journal),
    )
    .unwrap();
    let recovery_id = publish_recovery(&mut projector, &owner.ownership_id);
    let lost = BrokerState {
        quarantined: true,
        ..BrokerState::unknown()
    };
    let result = query_original_desktop_observing(
        &original,
        &CancellationToken::default(),
        |action| {
            let Action::QueryDesktop { identity } = action else {
                panic!("lost owner never authorizes Cancel, Activate or Close")
            };
            Ok(v2::Reply::DesktopState {
                identity: Box::new(identity),
                state: Box::new(lost.clone()),
            })
        },
        |observation| projector.record_broker_recovery_observation(&recovery_id, observation),
    )
    .unwrap();
    assert!(!result.cached_terminal);
    assert!(matches!(result.outcome, IntegrationOutcome::Unknown { .. }));
    let IntegrationRequest::Desktop { request, .. } = &original.request else {
        unreachable!()
    };
    let before = match projector.journal() {
        StoredJournal::NativeIntegration(j) => j.encode().unwrap(),
        _ => unreachable!(),
    };
    assert!(
        projector
            .record_broker_recovery_observation(
                &recovery_id,
                RecoveredBrokerObservation {
                    original: action_reference(&original),
                    identity: request.identity().unwrap(),
                    source: BrokerObservationSource::Query,
                    state: pending()
                }
            )
            .is_err()
    );
    let StoredJournal::NativeIntegration(saved) = projector.journal() else {
        unreachable!()
    };
    assert_eq!(
        saved.encode().unwrap(),
        before,
        "rejection preserves the saved original knowledge"
    );
    reconcile_broker_release(
        &mut projector,
        &owner,
        v2::LeaseId::parse(&original.lease).unwrap(),
        &v2::LeaseView::AwaitingExternalReset {
            intent: v2::IntentId::from_bytes([0x17; 16]),
        },
        BrokerReleaseSource::QueryLease,
    )
    .unwrap();
    drop(projector);
    let (NativeReportVersion::IntegrationV2(report), NativeJournalVersion::IntegrationV2(saved)) =
        crate::native::storage::load_native_versioned(&store).unwrap()
    else {
        unreachable!()
    };
    assert_eq!(report.integrations[0].checks, checks);
    assert_eq!(
        saved.integration_recoveries[0].broker_observations[0].state,
        lost
    );
    assert!(saved.integration_recoveries[0].broker_release.is_some());
    assert!(matches!(
        saved.integration_recoveries[0].broker,
        Disposition::Unknown { .. }
    ));
    assert!(matches!(
        saved.integration_ownership[0].disposition,
        Disposition::Unknown { .. }
    ));
    assert_eq!(saved.integration_actions[0], original);
}
#[test]
fn original_desktop_cancel_is_polled_until_independent_scope_settles() {
    let original = original();
    let IntegrationRequest::Desktop { request, .. } = &original.request else {
        unreachable!()
    };
    let identity = request.identity().unwrap();
    let mut trace = Vec::new();
    let mut cancels = 0;
    let result = query_original_desktop(&original, &CancellationToken::default(), |action| {
        let state = match action {
            Action::QueryDesktop { identity: observed } => {
                assert_eq!(observed, identity);
                trace.push("query");
                if cancels >= 2 { settled() } else { pending() }
            }
            Action::CancelDesktop { identity: observed } => {
                assert_eq!(observed, identity);
                trace.push("cancel");
                cancels += 1;
                if cancels >= 2 { settled() } else { pending() }
            }
            _ => panic!("recovery must not prepare/acquire/activate/request-close"),
        };
        Ok(v2::Reply::DesktopState {
            identity: Box::new(identity.clone()),
            state: Box::new(state),
        })
    })
    .unwrap();
    assert_eq!(
        cancels, 2,
        "one nonblocking cleanup call is not a scope join"
    );
    assert_eq!(trace, ["query", "cancel", "query", "cancel", "query"]);
    assert!(matches!(
        result.outcome,
        IntegrationOutcome::Complete { .. }
    ));
}
#[test]
fn lost_unknown_desktop_owner_never_gets_a_cancel_or_fresh_effect() {
    let original = original();
    let mut count = 0;
    let result = query_original_desktop(&original, &CancellationToken::default(), |action| {
        let Action::QueryDesktop { identity } = action else {
            panic!("lost native ownership grants no effect authority")
        };
        count += 1;
        Ok(v2::Reply::DesktopState {
            identity: Box::new(identity),
            state: Box::new(BrokerState::unknown()),
        })
    })
    .unwrap();
    assert_eq!(count, 1);
    assert!(matches!(result.outcome, IntegrationOutcome::Unknown { .. }));
    assert!(!result.cached_terminal);
}
