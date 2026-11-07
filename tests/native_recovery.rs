#![cfg(feature = "native-catalog-test-support")]
//! Actual inert helper recovery, without SSH or physical-reset acceptance.
#[path = "support/native_catalog_setup.rs"]
mod setup;
use setup::NativeCatalogFixture;
use std::time::{Duration, Instant};
use theseus::{
    engine::{self, domain::*, projector::RunProjector},
    model::{RunOutcome, ScenarioKind},
    native::{
        model::*,
        provider::{NativeProvider, validate_baseline},
        transport::NativeSession,
    },
};
use theseus_native_client::{Action, ClientMode, Event, Poll};
use theseus_native_helper_store::IntentRecord;
use theseus_native_protocol::v2;

fn pending(
    acquire: bool,
    run: bool,
) -> (
    NativeCatalogFixture,
    theseus::native::launch::LaunchAuthority,
    theseus::report::ReportStore,
) {
    let fixture = NativeCatalogFixture::new();
    let launch = fixture.launch();
    let store = theseus::report::create(&fixture.root.path().join("run")).unwrap();
    let owner = NativeResource {
        scenario_id: "native-clean".into(),
        ownership_id: "original-owner".into(),
        ownership: NativeOwnership::Preparing {
            provider: launch.provider_name().into(),
            invocation_id: "original-prepare".into(),
            receipt: None,
        },
        catalog_authority: NativeCatalogAuthority::Valid,
    };
    let report = NativeReport {
        kind: "theseus_native_report".into(),
        schema: 1,
        run_id: "recover".into(),
        recipe_digest: fixture.recipe(ScenarioKind::Clean).digest,
        scenarios: vec![NativeScenario {
            id: owner.scenario_id.clone(),
            scenario: ScenarioKind::Clean,
            ownership_id: owner.ownership_id.clone(),
            stages: vec![],
            outcome: RunOutcome::Incomplete,
            dispositions: NativeDispositions::default(),
        }],
        identities: vec![],
        provider_claims: vec![],
        failures: vec![],
        outcome: RunOutcome::Incomplete,
    };
    let mut projector = RunProjector::new(
        &store,
        StoredRun::Native(report),
        StoredJournal::Native(NativeJournal::new("recover")),
    )
    .unwrap();
    projector.bind_launch(&launch.view().fingerprint).unwrap();
    let invocation = ProviderInvocation {
        ownership_id: owner.ownership_id.clone(),
        provider: launch.provider_name().into(),
        invocation_id: "original-prepare".into(),
        action: ProviderAction::Prepare,
        target_id: launch.view().target_id.clone(),
        baseline_label: launch.baseline_label().into(),
        launch_fingerprint: launch.view().fingerprint.clone(),
        prior_resource: owner.clone(),
    };
    let mut provider = launch.provider_factory().unwrap();
    let receipt = provider
        .invoke(
            projector
                .publish_provider(owner.clone(), invocation.clone())
                .unwrap(),
        )
        .unwrap();
    let baseline = validate_baseline(&launch, &invocation, receipt.clone()).unwrap();
    projector
        .record_provider_receipt(&invocation, receipt.clone())
        .unwrap();
    let active = launch.with_baseline(&baseline).unwrap();
    let mut intents = store.intent_store().unwrap();
    let prepared = intents
        .prepare(IntentRecord {
            intent: v2::IntentId::from_bytes([7; 16]),
            binding: active.binding().binding,
            manifest_digest: active.binding().manifest_digest,
            workspace_marker: active.binding().workspace_marker,
        })
        .unwrap();
    let identity = NativeIntent {
        intent: prepared.record().intent.to_hex(),
        binding: receipt.binding,
        manifest_digest: receipt.manifest_digest,
        workspace_marker: receipt.workspace_marker,
        reset_epoch: receipt.reset_epoch,
        publication_scope: match prepared.commit_scope() {
            theseus_host::fs::CommitScope::FileAndDirectorySynced => "file_and_directory_synced",
            theseus_host::fs::CommitScope::FileFlushedAtomicReplace => {
                "file_flushed_atomic_replace"
            }
        }
        .into(),
    };
    let mut resource = owner;
    resource.ownership = NativeOwnership::Intent { prepared: identity };
    let saved = projector
        .publish_intent(resource.clone(), prepared)
        .unwrap();
    if acquire {
        let mut session = NativeSession::start_authorized(&active, ClientMode::Fresh).unwrap();
        session.wait_ready().unwrap();
        let acquired = session.acquire(saved, &baseline).unwrap();
        resource.ownership = NativeOwnership::Acquired {
            prepared: acquired.prepared,
            lease: acquired.lease.to_hex(),
        };
        let published = projector.publish_acquired(resource).unwrap();
        if run {
            let operation = v2::OperationId::from_bytes([8; 16]);
            let command = CatalogCommand {
                id: "original-install".into(),
                program: CatalogProgram::InstallTarget,
                expected_status: if cfg!(windows) {
                    NativeStatus::WindowsExit { code: 0 }
                } else {
                    NativeStatus::PosixExit { code: 0 }
                },
                stdout_literal: None,
            };
            let op = NativeOperation {
                ownership_id: "original-owner".into(),
                lease: acquired.lease.to_hex(),
                operation_id: operation.to_hex(),
                request_digest: v2::request_digest(&v2::Request::Run {
                    lease: acquired.lease,
                    operation,
                    program: command.program.program_id(),
                })
                .unwrap()
                .to_hex(),
                command,
                completion: Disposition::Unknown {
                    reason: "terminal acknowledgement intentionally not saved".into(),
                },
            };
            let original = projector
                .publish_command(&published, op)
                .unwrap()
                .into_operation();
            session
                .submit(Action::Run {
                    lease: acquired.lease,
                    operation,
                    program: original.command.program.program_id(),
                })
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                assert!(Instant::now() < deadline);
                match session.poll().unwrap() {
                    Poll::Event(Event::Reply {
                        reply:
                            v2::Reply::Operation {
                                state: v2::OperationView::Finished { .. },
                                ..
                            },
                        ..
                    }) => break,
                    Poll::Event(Event::Reply {
                        reply:
                            v2::Reply::Operation {
                                state:
                                    v2::OperationView::Accepted { .. } | v2::OperationView::Running,
                                ..
                            },
                        ..
                    })
                    | Poll::Event(Event::Reply {
                        reply:
                            v2::Reply::Refused {
                                code: v2::RefusalCode::OwnershipPending,
                                ..
                            },
                        ..
                    }) => {
                        session
                            .submit(Action::Query {
                                lease: acquired.lease,
                                operation,
                            })
                            .unwrap();
                    }
                    _ => std::thread::yield_now(),
                }
            }
        }
        session.request_stop();
        let deadline = Instant::now() + Duration::from_secs(30);
        while !session.poll_settle().unwrap() {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(session.is_settled());
    }
    assert!(provider.poll_settle().unwrap());
    drop(projector);
    (fixture, launch, store)
}

#[test]
fn recovery_queries_original_lease_and_run_without_replay_and_keeps_prior_local_unknown() {
    let (fixture, launch, store) = pending(true, true);
    let reader = if cfg!(windows) {
        "reader.exe"
    } else {
        "reader"
    };
    let before = std::fs::read(fixture.workspace.join(reader)).unwrap();
    let original = store.load_native_journal().unwrap();
    let result =
        engine::recover_native(store, &launch, engine::CancellationToken::default(), |_| {})
            .unwrap();
    assert!(!result.complete());
    assert!(matches!(result.local, Disposition::Complete { .. }));
    assert!(matches!(result.remote, Disposition::Complete { .. }));
    assert!(
        result
            .prior_local
            .iter()
            .any(|state| matches!(state, Disposition::Unknown { .. }))
    );
    assert!(matches!(
        result.report.scenarios[0].dispositions.local,
        Disposition::Unknown { .. }
    ));
    assert_eq!(result.report.outcome, RunOutcome::Incomplete);
    assert!(result.report.scenarios[0].stages.is_empty());
    assert_eq!(result.journal.operations.len(), 1);
    assert_eq!(
        result.journal.operations[0].operation_id,
        original.operations[0].operation_id
    );
    assert!(matches!(
        result.journal.operations[0].completion,
        Disposition::Complete { .. }
    ));
    assert_eq!(result.journal.releases.len(), 1);
    assert_eq!(result.journal.provider_calls.len(), 2);
    assert_eq!(
        std::fs::read(fixture.workspace.join(reader)).unwrap(),
        before
    );
    assert_eq!(result.journal.recovery_attempts.len(), 1);
    assert!(matches!(
        result.journal.recovery_attempts[0].local,
        Disposition::Complete { .. }
    ));
}

#[test]
fn recovery_not_found_never_acquires_or_runs_a_replacement() {
    let (_fixture, launch, store) = pending(false, false);
    let result =
        engine::recover_native(store, &launch, engine::CancellationToken::default(), |_| {})
            .unwrap();
    assert!(!result.complete());
    assert!(matches!(
        result.journal.ownership[0].ownership,
        NativeOwnership::Intent { .. }
    ));
    assert!(result.journal.operations.is_empty());
    assert!(result.journal.releases.is_empty());
    assert_eq!(result.journal.provider_calls.len(), 2);
    assert!(matches!(result.remote, Disposition::Complete { .. }));
}

#[test]
fn recovery_missing_or_changed_original_intent_refuses_before_any_event_or_effect() {
    for changed in [false, true] {
        let (fixture, launch, store) = pending(true, false);
        let filename = fixture
            .root
            .path()
            .join("run/native-intents")
            .join(format!("{}.json", "07".repeat(16)));
        if changed {
            std::fs::write(&filename, b"changed canonical intent").unwrap();
        } else {
            std::fs::remove_file(&filename).unwrap();
        }
        let before = std::fs::read(store.root().join(JOURNAL_FILENAME)).unwrap();
        assert!(
            engine::recover_native(
                store,
                &launch,
                engine::CancellationToken::default(),
                |_| panic!("invalid original intent reached runtime")
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(fixture.root.path().join("run").join(JOURNAL_FILENAME)).unwrap(),
            before
        );
        if changed {
            assert_eq!(
                std::fs::read(filename).unwrap(),
                b"changed canonical intent"
            );
        } else {
            assert!(!filename.exists());
        }
    }
}

#[test]
fn recovery_resolves_original_unknown_dispose_by_query_without_new_invocation_or_transport() {
    let (fixture, launch, store) = pending(false, false);
    let mut projector = RunProjector::new(
        &store,
        StoredRun::Native(store.load_native_report().unwrap()),
        StoredJournal::Native(store.load_native_journal().unwrap()),
    )
    .unwrap();
    let StoredJournal::Native(journal) = projector.journal() else {
        panic!("wrong family")
    };
    let prepare = journal.provider_calls[0].invocation.clone();
    let resource = journal.ownership[0].clone();
    let mut provider = launch.provider_factory().unwrap();
    provider.query(&prepare).unwrap().unwrap();
    let invocation = ProviderInvocation {
        ownership_id: resource.ownership_id.clone(),
        provider: launch.provider_name().into(),
        invocation_id: "original-dispose".into(),
        action: ProviderAction::Dispose,
        target_id: launch.view().target_id.clone(),
        baseline_label: launch.baseline_label().into(),
        launch_fingerprint: launch.view().fingerprint.clone(),
        prior_resource: resource.clone(),
    };
    // Original provider completed, but its terminal receipt was lost before journal save.
    provider
        .invoke(
            projector
                .publish_provider(resource, invocation.clone())
                .unwrap(),
        )
        .unwrap();
    assert!(provider.poll_settle().unwrap());
    drop(projector);
    std::fs::write(
        &fixture.config,
        b"invalid helper config proves no transport is started",
    )
    .unwrap();
    let result =
        engine::recover_native(store, &launch, engine::CancellationToken::default(), |_| {})
            .unwrap();
    assert!(!result.complete());
    assert!(matches!(result.remote, Disposition::Complete { .. }));
    assert_eq!(result.journal.provider_calls.len(), 2);
    assert_eq!(result.journal.provider_calls[1].invocation, invocation);
    assert!(result.journal.provider_calls[1].receipt.is_some());
    assert!(result.journal.operations.is_empty());
    assert!(result.journal.releases.is_empty());
}

#[test]
fn recovery_rejects_changed_known_terminal_status_without_rewriting_original_evidence() {
    let (_fixture, launch, store) = pending(true, true);
    let journal = store.load_native_journal().unwrap();
    let original = &journal.operations[0];
    let mut report = store.load_native_report().unwrap();
    let status = if cfg!(windows) {
        NativeStatus::WindowsExit { code: 9 }
    } else {
        NativeStatus::PosixExit { code: 9 }
    };
    report.scenarios[0].stages.push(NativeStage {
        name: "original-install".into(),
        step: Some(theseus::model::StepName::InstallTarget),
        checkpoint: None,
        outcome: theseus::model::StageOutcome::Failed,
        commands: vec![NativeCommandEvidence {
            command: original.command.clone(),
            operation_id: original.operation_id.clone(),
            request_digest: original.request_digest.clone(),
            status: Some(status.clone()),
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
    store.save_native_report(&report).unwrap();
    let path = store.root().to_owned();
    assert!(
        engine::recover_native(store, &launch, engine::CancellationToken::default(), |_| {})
            .is_err()
    );
    let saved = theseus::report::open_existing(&path).unwrap();
    let report = saved.load_native_report().unwrap();
    assert_eq!(
        report.scenarios[0].stages[0].commands[0].status,
        Some(status)
    );
    assert_eq!(
        report.scenarios[0].stages[0].outcome,
        theseus::model::StageOutcome::Failed
    );
    assert!(matches!(
        saved.load_native_journal().unwrap().operations[0].completion,
        Disposition::Unknown { .. }
    ));
}

#[test]
fn retained_native_finalizer_joins_only_original_local_session_and_preserves_remote_unknown() {
    let fixture = NativeCatalogFixture::new();
    let launch = fixture.launch();
    let mut session = NativeSession::start_authorized(&launch, ClientMode::Fresh).unwrap();
    session.wait_ready().unwrap();
    let error =
        engine::fixture_retained_native_session(session, true).context("owning finalizer context");
    assert_eq!(
        engine::fixture_native_cleanup_facts(&error),
        (true, true, false)
    );
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut facts = engine::fixture_finalize_native_cleanup(&error);
    while facts.0 {
        assert!(
            Instant::now() < deadline,
            "original local owner did not join"
        );
        facts = engine::fixture_finalize_native_cleanup(&error);
    }
    assert_eq!(facts, (false, true, false));
    assert_eq!(
        std::fs::read_dir(fixture.root.path().join("provider"))
            .unwrap()
            .count(),
        0,
        "local finalizer invoked or queried a provider"
    );
}

#[test]
fn recovery_rejects_changed_known_stdout_count_without_inventing_output() {
    let (_fixture, launch, store) = pending(true, true);
    let journal = store.load_native_journal().unwrap();
    let original = &journal.operations[0];
    let mut report = store.load_native_report().unwrap();
    let status = if cfg!(windows) {
        NativeStatus::WindowsExit { code: 0 }
    } else {
        NativeStatus::PosixExit { code: 0 }
    };
    // The original actual fixture install emits zero bytes. A nonempty saved
    // complete UTF-8 capture therefore contradicts the exact terminal counter.
    let stdout = "contradictory retained stdout".into();
    report.scenarios[0].stages.push(NativeStage {
        name: "original-install".into(),
        step: Some(theseus::model::StepName::InstallTarget),
        checkpoint: None,
        outcome: theseus::model::StageOutcome::Incomplete,
        commands: vec![NativeCommandEvidence {
            command: original.command.clone(),
            operation_id: original.operation_id.clone(),
            request_digest: original.request_digest.clone(),
            status: Some(status),
            stdout,
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
    store.save_native_report(&report).unwrap();
    let path = store.root().to_owned();
    assert!(
        engine::recover_native(store, &launch, engine::CancellationToken::default(), |_| {})
            .is_err()
    );
    let saved = theseus::report::open_existing(&path)
        .unwrap()
        .load_native_report()
        .unwrap();
    assert_eq!(
        saved.scenarios[0].stages[0].commands[0].stdout,
        report.scenarios[0].stages[0].commands[0].stdout
    );
    assert_eq!(
        saved.scenarios[0].stages[0].outcome,
        theseus::model::StageOutcome::Incomplete
    );
}
