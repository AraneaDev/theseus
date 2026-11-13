use super::*;
use theseus_native_protocol::v2;

fn original() -> IntegrationActionRecord {
    let request = IntegrationRequest::Service {
        lease: "01".repeat(16),
        operation: "02".repeat(16),
        slot: ResourceSlot::new("service").unwrap(),
        prior_binding_digest: "03".repeat(32),
        control: ServiceAction::Start,
    };
    IntegrationActionRecord {
        ownership_id: "owner".into(),
        lease: "01".repeat(16),
        slot: ResourceSlot::new("service").unwrap(),
        operation: "02".repeat(16),
        request_digest: v2::request_digest(&request.wire().unwrap())
            .unwrap()
            .to_hex(),
        request,
        prior_binding_digest: "03".repeat(32),
        broker_identity: None,
        completion: Disposition::Unknown {
            reason: "reply lost".into(),
        },
        terminal: None,
    }
}
#[test]
fn pending_original_control_is_cancelled_and_queried_without_replay() {
    let original = original();
    let lease = v2::LeaseId::parse(&original.lease).unwrap();
    let operation = v2::OperationId::parse(&original.operation).unwrap();
    let mut trace = vec![];
    let result = query_original_action(&original, &CancellationToken::default(), |action| {
        let state = match action {
            theseus_native_client::Action::Query {
                lease: l,
                operation: o,
            } => {
                assert_eq!((l, o), (lease, operation));
                trace.push("query");
                if trace.len() == 1 {
                    v2::OperationView::Accepted {
                        operation_kind: v2::OperationKind::ServiceAction,
                    }
                } else {
                    v2::OperationView::Finished {
                        outcome: v2::Outcome::ServiceActionCompleted {
                            outcome: v2::ServiceOutcome::Complete {
                                receipt: "04".repeat(32),
                            },
                        },
                    }
                }
            }
            theseus_native_client::Action::Cancel {
                lease: l,
                operation: o,
            } => {
                assert_eq!((l, o), (lease, operation));
                trace.push("cancel");
                v2::OperationView::Accepted {
                    operation_kind: v2::OperationKind::ServiceAction,
                }
            }
            _ => panic!("recovery submitted a fresh effect"),
        };
        Ok(v2::Reply::Operation {
            lease,
            operation,
            state,
        })
    })
    .unwrap();
    assert_eq!(trace, ["query", "cancel", "query"]);
    assert_eq!(
        result.outcome,
        IntegrationOutcome::Complete {
            receipt: "04".repeat(32)
        }
    );
    assert!(result.cached_terminal);
}
#[test]
fn lost_native_owner_stays_unknown_without_cancel_or_replacement() {
    let original = original();
    let mut count = 0;
    let result = query_original_action(&original, &CancellationToken::default(), |action| {
        assert!(matches!(
            action,
            theseus_native_client::Action::Query { .. }
        ));
        count += 1;
        Ok(v2::Reply::Operation {
            lease: v2::LeaseId::parse(&original.lease).unwrap(),
            operation: v2::OperationId::parse(&original.operation).unwrap(),
            state: v2::OperationView::Unknown {
                reason: v2::UnknownReason::HelperRestartedWithUnfinishedOperation,
            },
        })
    })
    .unwrap();
    assert_eq!(count, 1);
    assert!(matches!(result.outcome, IntegrationOutcome::Unknown { .. }));
    assert!(
        !result.cached_terminal,
        "lost owner must not fabricate a cached terminal outcome"
    );
}
#[test]
fn queried_terminal_receipt_cannot_replace_original_terminal() {
    let mut original = original();
    original.completion = Disposition::Complete {
        receipt: "04".repeat(32),
    };
    original.terminal = Some(IntegrationOutcome::Complete {
        receipt: "04".repeat(32),
    });
    let result = query_original_action(&original, &CancellationToken::default(), |_| {
        Ok(v2::Reply::Operation {
            lease: v2::LeaseId::parse(&original.lease).unwrap(),
            operation: v2::OperationId::parse(&original.operation).unwrap(),
            state: v2::OperationView::Finished {
                outcome: v2::Outcome::ServiceActionCompleted {
                    outcome: v2::ServiceOutcome::Complete {
                        receipt: "05".repeat(32),
                    },
                },
            },
        })
    });
    assert!(result.is_err(), "changed cached outcome must refuse");
}
#[test]
fn provider_reset_does_not_erase_prior_local_or_resource_uncertainty() {
    let report = crate::native::integration::view::tests::report();
    let recipe = crate::native::integration::ValidatedIntegrationRecipe::decode(
        include_bytes!("../../../examples/native/service-recipe.example.json"),
        "recipe.json".into(),
    )
    .unwrap();
    let journal = NativeIntegrationJournal::new(
        "run".into(),
        &recipe,
        vec![crate::model::ScenarioKind::Clean],
    )
    .unwrap();
    let complete = || Disposition::Complete {
        receipt: "settled".into(),
    };
    let mut result = IntegrationRecoveryResult {
        report,
        journal,
        report_path: "native-report.json".into(),
        prior_local: vec![Disposition::Unknown {
            reason: "lost prior transport owner".into(),
        }],
        local: complete(),
        remote: complete(),
        resources: vec![RecoveredResourceDisposition {
            ownership_id: "owner".into(),
            slot: ResourceSlot::new("service").unwrap(),
            original_action_ids: vec!["02".repeat(16)],
            local: complete(),
            remote: Disposition::Unknown {
                reason: "manager restarted".into(),
            },
            broker: complete(),
        }],
    };
    assert!(!result.complete());
    result.prior_local = vec![complete()];
    assert!(
        !result.complete(),
        "provider disposal cannot erase manager uncertainty"
    );
}

#[test]
fn changed_original_identity_refuses_before_transport_submission() {
    let mut original = original();
    original.operation = "09".repeat(16);
    let mut calls = 0;
    let result = query_original_action(&original, &CancellationToken::default(), |_| {
        calls += 1;
        anyhow::bail!("transport must not be called")
    });
    assert!(result.is_err());
    assert_eq!(
        calls, 0,
        "record operation must match canonical request before runtime"
    );
}

#[cfg(unix)]
fn disposed_run() -> (tempfile::TempDir, ReportStore, LaunchAuthority) {
    use crate::native::model::*;
    let report = crate::native::integration::view::tests::report();
    let mut input: serde_json::Value =
        serde_json::from_str(include_str!("../../../examples/native/launch.example.json")).unwrap();
    input["schema"] = 2.into();
    input["helper_profile"] = "native_integration_v1".into();
    input["core_manifest_digest"] = report.core_manifest_digest.clone().into();
    input["integration_manifest_digest"] = report.integration_manifest_digest.clone().into();
    input["manifest_digest"] = report.manifest_digest.clone().into();
    input["service_authority"] = serde_json::json!({
        "kind":"systemd","slot":"fixture_service","scope":"user","bus_endpoint":{"kind":"unix_path","path":"/nonexistent/recovery-private/bus"},
        "qualification":{"manager":"systemd","version":"262","api_profile":"systemd_dbus_private_v1","features":["unit_properties","start_stop","manager_reload"],"receipt_sha256":"03".repeat(32)},
        "namespace":{"kind":"exclusive_owned_registration","slot":"fixture_service","unit_load_roots":["/nonexistent/recovery-private/units"],"dropins":"forbidden","provider_context":"private-fixture"},
        "manager_context":{"kind":"whole_private","slot":"manager","sut_allowed_actions":["daemon_reload"],"rehearsal_allowed_actions":[],"provider_receipt_required":true},
        "expected_peer_uid":1000,"expected_manager_uid":1000,"definition_root":"/nonexistent/recovery-private/units","unit":"theseus-fixture.service","managed_by":"installer",
        "sut_effects":["registration","configuration","removal","daemon_reload"],"allowed_actions":["inspect","start","stop"]
    });
    input["desktop_authority"] = serde_json::Value::Null;
    let launch = LaunchAuthority::decode(&serde_json::to_vec(&input).unwrap()).unwrap();
    let root = tempfile::tempdir().unwrap();
    let store = crate::report::create(&root.path().join("run")).unwrap();
    let mut intents = store.intent_store().unwrap();
    let prepared = intents
        .prepare(theseus_native_helper_store::IntentRecord {
            intent: v2::IntentId::from_bytes([11; 16]),
            binding: launch.binding().binding,
            manifest_digest: launch.binding().manifest_digest,
            workspace_marker: launch.binding().workspace_marker,
        })
        .unwrap();
    let owner = NativeResource {
        scenario_id: "clean".into(),
        ownership_id: "owner".into(),
        catalog_authority: NativeCatalogAuthority::Valid,
        ownership: NativeOwnership::Acquired {
            lease: "01".repeat(16),
            prepared: NativeIntent {
                intent: prepared.record().intent.to_hex(),
                binding: launch.binding().binding.to_hex(),
                manifest_digest: report.manifest_digest.clone(),
                workspace_marker: launch.binding().workspace_marker.to_hex(),
                reset_epoch: launch.provider_epoch().to_hex(),
                publication_scope: "file_and_directory_synced".into(),
            },
        },
    };
    let disposal = ProviderInvocation {
        ownership_id: "owner".into(),
        provider: launch.provider_name().into(),
        invocation_id: "original-dispose".into(),
        action: ProviderAction::Dispose,
        target_id: launch.view().target_id.clone(),
        baseline_label: launch.baseline_label().into(),
        launch_fingerprint: launch.view().fingerprint.clone(),
        prior_resource: owner.clone(),
    };
    let receipt = ProviderReceipt {
        provider: disposal.provider.clone(),
        invocation_id: disposal.invocation_id.clone(),
        invocation_digest: disposal.digest().unwrap(),
        binding: launch.binding().binding.to_hex(),
        manifest_digest: report.manifest_digest.clone(),
        workspace_marker: launch.binding().workspace_marker.to_hex(),
        reset_epoch: "05".repeat(32),
        receipt: "already-disposed-original-target".into(),
    };
    let binding = ResourceBindingSnapshot {
        digest: "03".repeat(32),
        basis: BindingBasis::VacantRegistration,
        evidence_ids: vec!["original-baseline-absence".into()],
        definition: None,
        artifact_commitment: "06".repeat(32),
        namespace_or_session_receipt_digest: "07".repeat(32),
        original_action: None,
    };
    let mut action = original();
    action.slot = ResourceSlot::new("fixture_service").unwrap();
    if let IntegrationRequest::Service { slot, .. } = &mut action.request {
        *slot = action.slot.clone();
    }
    action.request_digest = v2::request_digest(&action.request.wire().unwrap())
        .unwrap()
        .to_hex();
    let journal = NativeIntegrationJournal {
        kind: "theseus_native_journal".into(),
        schema: 2,
        profile: "native_integration_v1".into(),
        run_id: report.run_id.clone(),
        launch_fingerprint: Some(launch.view().fingerprint.clone()),
        recipe_digest: report.recipe_digest.clone(),
        core_recipe_digest: report.core_recipe_digest.clone(),
        core_manifest_digest: report.core_manifest_digest.clone(),
        integration_manifest_digest: report.integration_manifest_digest.clone(),
        manifest_digest: report.manifest_digest.clone(),
        requirements: report.requirements.clone(),
        compiled_requirements_digest: report.compiled_requirements_digest.clone(),
        selected_scenarios: report.selected_scenarios.clone(),
        ownership: vec![owner],
        operations: vec![],
        provider_calls: vec![NativeProviderCall {
            invocation: disposal,
            receipt: Some(receipt),
            completion: settled("original dispose receipt"),
        }],
        releases: vec![],
        recovery_attempts: vec![],
        failures: vec![],
        integration_ownership: vec![IntegrationResourceRecord {
            key: IntegrationResourceKey {
                scenario_id: "clean".into(),
                ownership_id: "owner".into(),
                slot: action.slot.clone(),
            },
            kind: IntegrationResourceKind::Service,
            manifest_digest: report.manifest_digest.clone(),
            initial_binding: binding.clone(),
            binding_history: vec![binding.clone()],
            current_binding_digest: binding.digest,
            revocation: Some(AuthorityRevocation::ManagerRestarted),
            disposition: unknown("original native handle lost"),
        }],
        integration_actions: vec![action],
        broker_bindings: vec![],
        readiness_probes: vec![],
        integration_recoveries: vec![],
    };
    validate_pair(&report, &journal).unwrap();
    store.save_native_integration_journal(&journal).unwrap();
    store.save_native_integration_report(&report).unwrap();
    (root, store, launch)
}
#[cfg(unix)]
#[test]
fn schema2_disposal_recovers_typed_original_resource_without_transport_or_new_effects() {
    let (_root, store, launch) = disposed_run();
    let original = crate::native::storage::load_native_versioned(&store).unwrap();
    let NativeJournalVersion::IntegrationV2(before) = original.1 else {
        panic!("schema2 fixture")
    };
    let report_path = store.native_report_path();
    let result =
        recover_selected_native(store, &launch, CancellationToken::default(), |_| {}).unwrap();
    let SelectedNativeRecoveryResult::IntegrationV2(result) = result else {
        panic!("typed integration result")
    };
    assert!(!result.complete());
    assert!(complete(&result.local));
    assert!(complete(&result.remote));
    assert!(result.prior_local.iter().any(|p| !complete(p)));
    assert_eq!(result.resources.len(), 1);
    assert_eq!(result.resources[0].original_action_ids, ["02".repeat(16)]);
    assert!(!complete(&result.resources[0].remote));
    assert_eq!(
        result.journal.integration_actions,
        before.integration_actions
    );
    assert_eq!(result.journal.provider_calls, before.provider_calls);
    assert_eq!(result.journal.ownership, before.ownership);
    assert!(result.journal.operations.is_empty());
    assert!(result.journal.releases.is_empty());
    assert_eq!(result.journal.integration_recoveries.len(), 1);
    assert!(result.journal.integration_recoveries[0].queries.is_empty());
    assert!(
        crate::native::model::NativeReport::decode(&std::fs::read(&report_path).unwrap()).is_err()
    );
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(report_path).unwrap()).unwrap();
    assert_eq!(saved["schema"], 2);
}
#[cfg(unix)]
#[test]
fn missing_original_intent_refuses_before_events_and_keeps_schema2_files_unchanged() {
    let (_root, store, launch) = disposed_run();
    let before_report = std::fs::read(store.native_report_path()).unwrap();
    let before_journal =
        std::fs::read(store.root().join(crate::native::model::JOURNAL_FILENAME)).unwrap();
    let NativeJournalVersion::IntegrationV2(journal) =
        crate::native::storage::load_native_versioned(&store)
            .unwrap()
            .1
    else {
        panic!("schema2")
    };
    let crate::native::model::NativeOwnership::Acquired { prepared, .. } =
        &journal.ownership[0].ownership
    else {
        panic!("acquired")
    };
    std::fs::remove_file(
        store
            .root()
            .join("native-intents")
            .join(format!("{}.json", prepared.intent)),
    )
    .unwrap();
    let path = store.native_report_path();
    let journal_path = store.root().join(crate::native::model::JOURNAL_FILENAME);
    let mut events = 0;
    assert!(
        recover_selected_native(store, &launch, CancellationToken::default(), |_| events +=
            1)
        .is_err()
    );
    assert_eq!(events, 0);
    assert_eq!(std::fs::read(path).unwrap(), before_report);
    assert_eq!(std::fs::read(journal_path).unwrap(), before_journal);
}

#[cfg(unix)]
#[test]
fn saved_schema2_recovery_history_rejects_fabricated_original_query_identity() {
    let (_root, store, launch) = disposed_run();
    let SelectedNativeRecoveryResult::IntegrationV2(result) =
        recover_selected_native(store, &launch, CancellationToken::default(), |_| {}).unwrap()
    else {
        panic!("schema2")
    };
    let mut journal = result.journal;
    assert!(
        journal.encode().is_ok(),
        "legitimate original recovery history must round-trip"
    );
    journal.integration_recoveries[0]
        .queries
        .push(OriginalQueryOutcome {
            original: ActionReference {
                operation: "09".repeat(16),
                request_digest: "10".repeat(32),
            },
            view: OriginalQueryView::CachedTerminal,
            outcome: IntegrationOutcome::Complete {
                receipt: "11".repeat(32),
            },
        });
    assert!(
        journal.encode().is_err(),
        "strict stored readers must refuse invented recovery query identities"
    );
}

#[cfg(unix)]
#[test]
fn saved_schema2_rejects_action_identity_different_from_canonical_request() {
    let (_root, store, _) = disposed_run();
    let NativeJournalVersion::IntegrationV2(mut journal) =
        crate::native::storage::load_native_versioned(&store)
            .unwrap()
            .1
    else {
        panic!("schema2")
    };
    journal.integration_actions[0].operation = "09".repeat(16);
    assert!(
        journal.encode().is_err(),
        "saved action metadata cannot target a different original operation"
    );
}
#[cfg(unix)]
#[test]
fn repeated_schema2_recovery_preserves_every_prior_uncertainty_and_original_identity() {
    let (_root, store, launch) = disposed_run();
    let root = store.root().to_path_buf();
    let SelectedNativeRecoveryResult::IntegrationV2(first) =
        recover_selected_native(store, &launch, CancellationToken::default(), |_| {}).unwrap()
    else {
        panic!("schema2")
    };
    let SelectedNativeRecoveryResult::IntegrationV2(second) = recover_selected_native(
        crate::report::open_existing(&root).unwrap(),
        &launch,
        CancellationToken::default(),
        |_| {},
    )
    .unwrap() else {
        panic!("schema2")
    };
    assert!(!second.complete());
    assert_eq!(
        second.journal.integration_actions,
        first.journal.integration_actions
    );
    assert_eq!(second.journal.provider_calls, first.journal.provider_calls);
    assert_eq!(second.journal.integration_recoveries.len(), 2);
    assert_eq!(
        second.journal.integration_recoveries[0],
        first.journal.integration_recoveries[0]
    );
    let latest = &second.journal.integration_recoveries[1];
    assert!(
        first.journal.integration_recoveries[0]
            .prior_local
            .iter()
            .all(|p| latest.prior_local.contains(p))
    );
    assert!(
        latest
            .prior_local
            .contains(&first.journal.integration_recoveries[0].local)
    );
    assert!(
        second
            .prior_local
            .iter()
            .any(|p| matches!(p, Disposition::Unknown { .. }))
    );
    let mut forged = second.journal;
    forged.integration_recoveries[1].prior_local = vec![settled("new recovery joined")];
    assert!(
        forged.encode().is_err(),
        "new completion cannot erase prior local uncertainty"
    );
}
#[test]
fn original_unknown_sut_run_prevents_complete_even_after_provider_settlement() {
    use crate::native::model::*;
    let mut report = crate::native::integration::view::tests::report();
    let done = || settled("proved independent settlement");
    for i in &mut report.integrations {
        i.dispositions = IntegrationDispositions {
            helper_worker: done(),
            registration: done(),
            manager_action: done(),
            broker: done(),
            primary: done(),
            descendant_scope: done(),
        };
    }
    for s in &mut report.catalog.scenarios {
        s.dispositions = NativeDispositions {
            local: done(),
            remote: done(),
            release: done(),
            reset: done(),
        };
    }
    let recipe = crate::native::integration::ValidatedIntegrationRecipe::decode(
        include_bytes!("../../../examples/native/service-recipe.example.json"),
        "recipe.json".into(),
    )
    .unwrap();
    let mut journal = NativeIntegrationJournal::new(
        "run".into(),
        &recipe,
        vec![crate::model::ScenarioKind::Clean],
    )
    .unwrap();
    let mut result = IntegrationRecoveryResult {
        report,
        journal: journal.clone(),
        report_path: "native-report.json".into(),
        prior_local: vec![done()],
        local: done(),
        remote: done(),
        resources: vec![],
    };
    assert!(
        result.complete(),
        "all mandatory settlement fields are proved in control case"
    );
    let operation = NativeOperation {
        ownership_id: "owner".into(),
        lease: "01".repeat(16),
        operation_id: "02".repeat(16),
        request_digest: "03".repeat(32),
        command: CatalogCommand {
            id: "install_target".into(),
            program: CatalogProgram::InstallTarget,
            expected_status: NativeStatus::PosixExit { code: 0 },
            stdout_literal: None,
        },
        completion: unknown("original SUT Run was accepted before helper restart"),
    };
    journal.operations.push(operation);
    result.journal = journal;
    assert!(
        !result.complete(),
        "external reset cannot erase original SUT Run uncertainty"
    );
}
#[cfg(unix)]
fn sut_probe_fixture() -> (NativeIntegrationReport, NativeIntegrationJournal) {
    use crate::{
        model::{StageOutcome, StepName},
        native::model::*,
    };
    let (_root, store, _) = disposed_run();
    let (
        NativeReportVersion::IntegrationV2(mut report),
        NativeJournalVersion::IntegrationV2(mut journal),
    ) = crate::native::storage::load_native_versioned(&store).unwrap()
    else {
        panic!("schema2")
    };
    report
        .requirements
        .service
        .as_mut()
        .unwrap()
        .controls
        .configured_activation = ActivationCause::Sut;
    report.compiled_requirements_digest = report.requirements.digest().unwrap();
    journal.requirements = report.requirements.clone();
    journal.compiled_requirements_digest = report.compiled_requirements_digest.clone();
    let mut operation = NativeOperation {
        ownership_id: "owner".into(),
        lease: "01".repeat(16),
        operation_id: "12".repeat(16),
        request_digest: String::new(),
        command: CatalogCommand {
            id: "configure".into(),
            program: CatalogProgram::Configure,
            expected_status: NativeStatus::PosixExit { code: 0 },
            stdout_literal: None,
        },
        completion: settled("original Configure Run completed"),
    };
    operation.request_digest = v2::request_digest(&operation.request().unwrap())
        .unwrap()
        .to_hex();
    report.catalog.scenarios[0].stages.push(NativeStage {
        name: "configure".into(),
        step: Some(StepName::Configure),
        checkpoint: None,
        outcome: StageOutcome::Passed,
        commands: vec![NativeCommandEvidence {
            command: operation.command.clone(),
            operation_id: operation.operation_id.clone(),
            request_digest: operation.request_digest.clone(),
            status: Some(NativeStatus::PosixExit { code: 0 }),
            stdout: String::new(),
            stdout_complete: true,
            stdout_utf8: true,
            stderr_complete: true,
            stdout_log: None,
            stderr_log: None,
            failure: None,
        }],
        checks: vec![],
        native_files: vec![],
        failures: vec![],
    });
    let resource = &mut journal.integration_ownership[0];
    resource.revocation = None;
    let mut binding = resource.initial_binding.clone();
    binding.digest = "13".repeat(32);
    binding.basis = BindingBasis::ExclusiveOwnedRegistration;
    binding.original_action = Some(operation.operation_id.clone());
    binding.definition = Some(DefinitionKey {
        role: DefinitionRole::Configured,
        artifact: ArtifactRole::Target,
    });
    resource.current_binding_digest = binding.digest.clone();
    resource.binding_history.push(binding.clone());
    let expectation = ReadinessExpectation {
        role: DefinitionRole::Configured,
        artifact: ArtifactRole::Target,
        artifact_length: 1,
        configuration_digest: "14".repeat(32),
        artifact_digest: "15".repeat(32),
        version: "fixture-v1".into(),
        data_digest: "16".repeat(32),
        initialized: true,
    };
    let expectation_digest = expectation.digest().unwrap();
    journal.readiness_probes.push(ReadinessProbeRecord {
        challenge: ReadinessChallenge {
            resource_binding_digest: binding.digest,
            action_digest: operation.request_digest.clone(),
            expectation_digest: expectation_digest.clone(),
            ownership_id: operation.ownership_id.clone(),
            lease: operation.lease.clone(),
            slot: resource.key.slot.clone(),
            operation: operation.operation_id.clone(),
            nonce: "17".repeat(16),
            deadline_ms: 1000,
        },
        expectation,
        expectation_digest,
        observation_ids: vec![],
        completion: unknown("published exact SUT activation readiness probe"),
    });
    journal.operations.push(operation);
    (report, journal)
}
#[cfg(unix)]
#[test]
fn sut_readiness_requires_exact_successful_original_activation_run() {
    use crate::native::model::*;
    let (report, journal) = sut_probe_fixture();
    validate_pair(&report, &journal)
        .expect("exact successful SUT Configure is an admitted readiness activation cause");
    let mut journal_ahead = journal.clone();
    journal_ahead.readiness_probes[0].observation_ids =
        vec!["reply-observed-before-report-barrier".into()];
    journal_ahead.readiness_probes[0].completion = settled("private-response-receipt");
    validate_pair(&report, &journal_ahead)
        .expect("crash between private completion and public report preserves incomplete evidence");
    let mut wrong = journal.clone();
    wrong.operations[0].command.program = CatalogProgram::CheckData;
    wrong.operations[0].request_digest =
        v2::request_digest(&wrong.operations[0].request().unwrap())
            .unwrap()
            .to_hex();
    wrong.readiness_probes[0].challenge.action_digest = wrong.operations[0].request_digest.clone();
    assert!(
        wrong.encode().is_err(),
        "an arbitrary successful Run cannot become activation authority"
    );
    let mut failed = report.clone();
    failed.catalog.scenarios[0].stages[0].outcome = crate::model::StageOutcome::Failed;
    failed.catalog.scenarios[0].stages[0].commands[0].status =
        Some(NativeStatus::PosixExit { code: 7 });
    assert!(
        validate_pair(&failed, &journal).is_err(),
        "terminal completion alone does not prove successful SUT activation"
    );
    let mut wrong = journal.clone();
    wrong.readiness_probes[0].expectation.role = DefinitionRole::Target;
    wrong.readiness_probes[0].expectation_digest =
        wrong.readiness_probes[0].expectation.digest().unwrap();
    wrong.readiness_probes[0].challenge.expectation_digest =
        wrong.readiness_probes[0].expectation_digest.clone();
    assert!(
        wrong.encode().is_err(),
        "activation binding definition must match the exact readiness expectation"
    );
}
#[cfg(unix)]
#[test]
fn recovered_resource_preserves_known_failure_until_actual_uncertainty_dominates() {
    let (_root, store, _) = disposed_run();
    let (
        NativeReportVersion::IntegrationV2(mut report),
        NativeJournalVersion::IntegrationV2(mut journal),
    ) = crate::native::storage::load_native_versioned(&store).unwrap()
    else {
        panic!("schema2")
    };
    let done = || settled("proved native disposition");
    report.integrations[0].dispositions = IntegrationDispositions {
        helper_worker: done(),
        registration: done(),
        manager_action: done(),
        broker: done(),
        primary: done(),
        descendant_scope: done(),
    };
    journal.integration_ownership[0].revocation = None;
    journal.integration_ownership[0].disposition = done();
    journal.integration_actions[0].completion = Disposition::Failed {
        reason: "original manager Start rejected".into(),
    };
    journal.integration_actions[0].terminal = Some(IntegrationOutcome::Failed {
        reason: "original manager Start rejected".into(),
    });
    let row = resource_dispositions(&report, &journal).remove(0);
    assert_eq!(
        row.remote,
        Disposition::Failed {
            reason: "original manager Start rejected".into()
        }
    );
    let failed_action = journal.integration_actions[0].clone();
    report.integrations[0].dispositions.helper_worker = unknown("original worker settlement lost");
    let row = resource_dispositions(&report, &journal).remove(0);
    assert!(matches!(row.remote, Disposition::Unknown { .. }));
    assert_eq!(
        journal.integration_actions[0], failed_action,
        "aggregate uncertainty cannot rewrite exact known failure"
    );
    report.integrations[0].dispositions.helper_worker = done();
    journal.integration_actions[0].completion = done();
    journal.integration_actions[0].terminal = Some(IntegrationOutcome::Complete {
        receipt: "proved native disposition".into(),
    });
    journal.integration_ownership[0].revocation = Some(AuthorityRevocation::ManagerRestarted);
    assert!(matches!(
        resource_dispositions(&report, &journal)[0].remote,
        Disposition::Unknown { .. }
    ));
    journal.integration_ownership[0].revocation = None;
    assert!(complete(
        &resource_dispositions(&report, &journal)[0].remote
    ));
    journal.integration_ownership[0].disposition = Disposition::Failed {
        reason: "SUT left original registration present".into(),
    };
    assert_eq!(
        resource_dispositions(&report, &journal)[0].remote,
        Disposition::Failed {
            reason: "SUT left original registration present".into()
        }
    );
}
#[cfg(unix)]
#[test]
fn persisted_nonterminal_query_history_allows_later_original_cached_terminal() {
    use crate::native::model::NativeRecoveryAttempt;
    for terminal in [
        IntegrationOutcome::Complete {
            receipt: "21".repeat(32),
        },
        IntegrationOutcome::Failed {
            reason: "original manager rejected request".into(),
        },
        IntegrationOutcome::Unknown {
            reason: "cached original native uncertainty".into(),
        },
    ] {
        let (_root, store, _) = disposed_run();
        let (
            NativeReportVersion::IntegrationV2(report),
            NativeJournalVersion::IntegrationV2(journal),
        ) = crate::native::storage::load_native_versioned(&store).unwrap()
        else {
            panic!("schema2")
        };
        let original = journal.integration_actions[0].clone();
        let mut projector = RunProjector::new(
            &store,
            StoredRun::NativeIntegration(report),
            StoredJournal::NativeIntegration(journal),
        )
        .unwrap();
        let prior = unknown("original controller owner lost");
        let first = "18".repeat(16);
        projector
            .record_recovery_attempt(NativeRecoveryAttempt {
                recovery_id: first.clone(),
                ownership_id: "owner".into(),
                prior_local: prior.clone(),
                local: unknown("recovery interrupted before local join"),
                remote: unknown("provider not settled"),
            })
            .unwrap();
        publish_attempt(&mut projector, &first, "owner").unwrap();
        let cancel = CancellationToken::default();
        cancel.cancel();
        let interrupted = query_original_action(&original, &cancel, |_| {
            panic!("cancelled recovery must not submit a new action")
        })
        .unwrap();
        assert!(!interrupted.cached_terminal);
        projector
            .record_integration_query(
                &first,
                OriginalQueryOutcome {
                    original: action_reference(&original),
                    view: OriginalQueryView::NonterminalUnknown,
                    outcome: interrupted.outcome,
                },
            )
            .unwrap();
        drop(projector);
        let (
            NativeReportVersion::IntegrationV2(report),
            NativeJournalVersion::IntegrationV2(journal),
        ) = crate::native::storage::load_native_versioned(&store).unwrap()
        else {
            panic!("schema2")
        };
        let retained_first = journal.integration_recoveries[0].clone();
        assert!(journal.integration_actions[0].terminal.is_none());
        let mut projector = RunProjector::new(
            &store,
            StoredRun::NativeIntegration(report),
            StoredJournal::NativeIntegration(journal),
        )
        .unwrap();
        let second = "19".repeat(16);
        projector
            .record_recovery_attempt(NativeRecoveryAttempt {
                recovery_id: second.clone(),
                ownership_id: "owner".into(),
                prior_local: prior.clone(),
                local: unknown("new local settlement pending"),
                remote: unknown("provider not settled"),
            })
            .unwrap();
        publish_attempt(&mut projector, &second, "owner").unwrap();
        let queried = query_original_action(&original, &CancellationToken::default(), |action| {
            assert!(
                matches!(action, Action::Query { .. }),
                "only the original cached action may be queried"
            );
            let outcome = match &terminal {
                IntegrationOutcome::Complete { receipt } => v2::ServiceOutcome::Complete {
                    receipt: receipt.clone(),
                },
                IntegrationOutcome::Failed { reason } => v2::ServiceOutcome::Failed {
                    reason: reason.clone(),
                },
                IntegrationOutcome::Unknown { reason } => v2::ServiceOutcome::Unknown {
                    reason: reason.clone(),
                },
            };
            Ok(v2::Reply::Operation {
                lease: v2::LeaseId::parse(&original.lease).unwrap(),
                operation: v2::OperationId::parse(&original.operation).unwrap(),
                state: v2::OperationView::Finished {
                    outcome: v2::Outcome::ServiceActionCompleted { outcome },
                },
            })
        })
        .unwrap();
        assert!(queried.cached_terminal);
        projector
            .record_integration_query(
                &second,
                OriginalQueryOutcome {
                    original: action_reference(&original),
                    view: OriginalQueryView::CachedTerminal,
                    outcome: queried.outcome.clone(),
                },
            )
            .unwrap();
        if matches!(terminal, IntegrationOutcome::Unknown { .. }) {
            assert!(
                projector
                    .record_integration_completion(
                        &original.lease,
                        &original.operation,
                        settled("contradictory cached receipt"),
                        IntegrationOutcome::Complete {
                            receipt: "22".repeat(32)
                        }
                    )
                    .is_err()
            );
            let StoredJournal::NativeIntegration(saved) = projector.journal() else {
                panic!("schema2")
            };
            saved
                .encode()
                .expect("contradictory terminal rejection must preserve a valid owned journal");
        }
        let completion = match &queried.outcome {
            IntegrationOutcome::Complete { receipt } => Disposition::Complete {
                receipt: receipt.clone(),
            },
            IntegrationOutcome::Failed { reason } => Disposition::Failed {
                reason: reason.clone(),
            },
            IntegrationOutcome::Unknown { reason } => Disposition::Unknown {
                reason: reason.clone(),
            },
        };
        projector.record_integration_completion(&original.lease, &original.operation, completion, queried.outcome)
            .expect("earlier nonterminal uncertainty cannot block a later exact original cached terminal");
        let StoredJournal::NativeIntegration(saved) = projector.journal() else {
            panic!("schema2")
        };
        let decoded = NativeIntegrationJournal::decode(&saved.encode().unwrap()).unwrap();
        assert_eq!(decoded.integration_recoveries[0], retained_first);
        assert_eq!(
            decoded.integration_actions[0].terminal.as_ref(),
            Some(&terminal)
        );
        assert!(
            decoded.integration_recoveries[1]
                .prior_local
                .contains(&prior)
        );
        assert!(
            decoded.integration_recoveries[1]
                .prior_local
                .iter()
                .any(|p| !complete(p))
        );
        if matches!(terminal, IntegrationOutcome::Unknown { .. }) {
            assert!(
                projector
                    .record_integration_completion(
                        &original.lease,
                        &original.operation,
                        settled("different cached receipt"),
                        IntegrationOutcome::Complete {
                            receipt: "22".repeat(32)
                        }
                    )
                    .is_err(),
                "a genuinely cached terminal Unknown remains immutable"
            );
        }
    }
}
#[cfg(unix)]
#[test]
fn failed_outcome_stays_failed_after_clean_recovery_and_unknown_remains_independent() {
    use crate::{
        model::{CheckOutcome, RunOutcome},
        native::model::*,
    };
    for resource_failure in [false, true] {
        for actual_unknown in [false, true] {
            let (_root, store, launch) = disposed_run();
            let (
                NativeReportVersion::IntegrationV2(mut report),
                NativeJournalVersion::IntegrationV2(mut journal),
            ) = crate::native::storage::load_native_versioned(&store).unwrap()
            else {
                panic!("schema2")
            };
            let done = || settled("original independent owner proved settled");
            report.catalog.scenarios[0].outcome = RunOutcome::Failed;
            report.catalog.scenarios[0].dispositions = NativeDispositions {
                local: done(),
                remote: done(),
                release: done(),
                reset: done(),
            };
            if actual_unknown && resource_failure {
                report.catalog.scenarios[0].dispositions.local =
                    unknown("original controller local owner remains unproved");
            }
            report.catalog.outcome = RunOutcome::Failed;
            report.outcome = RunOutcome::Failed;
            report.integrations[0].checks[0].outcome = CheckOutcome::Failed;
            report.integrations[0].checks[0].detail =
                "decisive original SUT readiness mismatch".into();
            let original_failure = report.integrations[0].checks[0].clone();
            report.integrations[0].dispositions = IntegrationDispositions {
                helper_worker: done(),
                registration: done(),
                manager_action: done(),
                broker: done(),
                primary: done(),
                descendant_scope: done(),
            };
            journal.integration_ownership[0].revocation = None;
            journal.integration_ownership[0].disposition = if actual_unknown {
                unknown("original manager owner lost")
            } else if resource_failure {
                Disposition::Failed {
                    reason: "SUT left original registration present".into(),
                }
            } else {
                done()
            };
            journal.integration_actions[0].completion = done();
            journal.integration_actions[0].terminal = Some(IntegrationOutcome::Complete {
                receipt: "original independent owner proved settled".into(),
            });
            validate_pair(&report, &journal).unwrap();
            store.save_native_integration_journal(&journal).unwrap();
            store.save_native_integration_report(&report).unwrap();
            let path = store.native_report_path();
            let SelectedNativeRecoveryResult::IntegrationV2(result) =
                recover_selected_native(store, &launch, CancellationToken::default(), |_| {})
                    .unwrap()
            else {
                panic!("schema2")
            };
            assert_eq!(result.report.catalog.outcome, RunOutcome::Failed);
            assert_eq!(result.report.integrations[0].checks[0], original_failure);
            assert_eq!(
                result.report.outcome,
                if actual_unknown {
                    RunOutcome::Incomplete
                } else {
                    RunOutcome::Failed
                },
                "known Failed is separate from actual cleanup uncertainty"
            );
            assert_eq!(
                result.complete(),
                !actual_unknown && !resource_failure,
                "cleanup completion is independent from decisive SUT failure"
            );
            let saved = NativeIntegrationReport::decode(&std::fs::read(path).unwrap()).unwrap();
            assert_eq!(saved.outcome, result.report.outcome);
            if resource_failure && !actual_unknown {
                assert!(matches!(
                    result.resources[0].remote,
                    Disposition::Failed { .. }
                ));
            }
        }
    }
}
#[cfg(unix)]
#[test]
fn failed_source_retains_unpublished_core_release_uncertainty_after_disposal() {
    use crate::{
        model::{CheckOutcome, RunOutcome},
        native::model::*,
    };
    for release in [
        unknown("original Release was never proved"),
        Disposition::NotStarted,
        settled("original release proved"),
        Disposition::Failed {
            reason: "original release definitely failed".into(),
        },
    ] {
        let uncertain = matches!(
            release,
            Disposition::Unknown { .. } | Disposition::NotStarted
        );
        let release_complete = complete(&release);
        let (_root, store, launch) = disposed_run();
        let (
            NativeReportVersion::IntegrationV2(mut report),
            NativeJournalVersion::IntegrationV2(mut journal),
        ) = crate::native::storage::load_native_versioned(&store).unwrap()
        else {
            panic!("schema2")
        };
        let done = || settled("original independent owner proved settled");
        report.catalog.scenarios[0].outcome = RunOutcome::Failed;
        report.catalog.scenarios[0].dispositions = NativeDispositions {
            local: done(),
            remote: done(),
            release: release.clone(),
            reset: done(),
        };
        report.catalog.outcome = RunOutcome::Failed;
        report.outcome = RunOutcome::Failed;
        report.integrations[0].checks[0].outcome = CheckOutcome::Failed;
        report.integrations[0].checks[0].detail =
            "decisive original SUT failure is retained".into();
        let original_failure = report.integrations[0].checks[0].clone();
        report.integrations[0].dispositions = IntegrationDispositions {
            helper_worker: done(),
            registration: done(),
            manager_action: done(),
            broker: done(),
            primary: done(),
            descendant_scope: done(),
        };
        journal.integration_ownership[0].revocation = None;
        journal.integration_ownership[0].disposition = done();
        journal.integration_actions[0].completion = done();
        journal.integration_actions[0].terminal = Some(IntegrationOutcome::Complete {
            receipt: "original independent owner proved settled".into(),
        });
        assert!(
            journal.releases.is_empty(),
            "fixture has no release publication to aggregate"
        );
        validate_pair(&report, &journal).unwrap();
        store.save_native_integration_journal(&journal).unwrap();
        store.save_native_integration_report(&report).unwrap();
        let path = store.native_report_path();
        let SelectedNativeRecoveryResult::IntegrationV2(result) =
            recover_selected_native(store, &launch, CancellationToken::default(), |_| {}).unwrap()
        else {
            panic!("schema2")
        };
        assert_eq!(
            result.report.catalog.scenarios[0].outcome,
            RunOutcome::Failed
        );
        assert_eq!(result.report.catalog.outcome, RunOutcome::Failed);
        assert_eq!(result.report.integrations[0].checks[0], original_failure);
        assert_eq!(
            result.report.catalog.scenarios[0].dispositions.release,
            release
        );
        assert!(
            result.journal.releases.is_empty(),
            "matching disposal cannot invent or prove an original Release"
        );
        assert_eq!(
            result.report.outcome,
            if uncertain {
                RunOutcome::Incomplete
            } else {
                RunOutcome::Failed
            },
            "all retained core disposition fields count even without journal rows"
        );
        assert_eq!(result.complete(), release_complete);
        let persisted = NativeIntegrationReport::decode(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(persisted.outcome, result.report.outcome);
        assert_eq!(persisted.catalog.scenarios[0].dispositions.release, release);
    }
}
