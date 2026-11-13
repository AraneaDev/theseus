use super::*;
fn ids() -> (v2::LeaseId, v2::OperationId) {
    (
        v2::LeaseId::from_bytes([1; 16]),
        v2::OperationId::from_bytes([2; 16]),
    )
}
fn run(r: &mut Recovery) {
    let (lease, operation) = ids();
    r.request(
        Action::Run {
            lease,
            operation,
            program: v2::ProgramId::new("probe").unwrap(),
        },
        &crate::pump::tests::expected(),
    )
    .unwrap();
    r.observe(&v2::Reply::Operation {
        lease,
        operation,
        state: v2::OperationView::Running,
    })
    .unwrap();
}
fn completed(truncated: bool) -> v2::Outcome {
    v2::Outcome::RunCompleted {
        reason: v2::RunReason::Natural,
        status: v2::NativeStatus::PosixExit { code: 0 },
        cleanup: v2::Cleanup::PosixLeaderReapedGroupTerminationRequested,
        stdout_bytes: 30,
        stderr_bytes: 20,
        truncated,
    }
}
fn terminal(outcome: v2::Outcome) -> v2::Reply {
    let (lease, operation) = ids();
    v2::Reply::Operation {
        lease,
        operation,
        state: v2::OperationView::Finished { outcome },
    }
}
#[test]
fn r1_output_before_loss_rejects_not_started_and_launch_failed_without_mutation() {
    let (lease, operation) = ids();
    for outcome in [
        v2::Outcome::NotStarted {
            reason: v2::NotStartedReason::CancelRequested,
        },
        v2::Outcome::LaunchFailed {
            phase: v2::LaunchPhase::NeverCreated,
            code: v2::FailureCode::Io,
            cleanup: v2::SpawnCleanup::NoChildCreated,
        },
    ] {
        let mut r = Recovery::default();
        run(&mut r);
        r.output(lease, operation, v2::Channel::Stdout, 0, 4)
            .unwrap();
        assert_eq!(r.observe(&terminal(outcome)), Err(ClientError::Protocol));
        assert!(matches!(
            r.operations[0].2,
            Some(v2::OperationView::Running)
        ));
        assert_eq!(
            r.request(
                Action::Run {
                    lease,
                    operation: v2::OperationId::from_bytes([4; 16]),
                    program: v2::ProgramId::new("probe").unwrap()
                },
                &crate::pump::tests::expected()
            ),
            Err(ClientError::RecoveryRequired)
        );
        r.observe(&terminal(completed(false))).unwrap();
    }
}
#[test]
fn r1_gap_and_truncation_log_survive_loss_and_require_truncated_terminal() {
    let (lease, operation) = ids();
    for log_only in [false, true] {
        let mut r = Recovery::default();
        run(&mut r);
        if log_only {
            r.log(lease, operation, v2::LogCode::OutputTruncated)
                .unwrap();
        } else {
            r.output(lease, operation, v2::Channel::Stdout, 0, 1)
                .unwrap();
            r.output(lease, operation, v2::Channel::Stdout, 3, 1)
                .unwrap();
        }
        assert_eq!(
            r.observe(&terminal(completed(false))),
            Err(ClientError::Protocol)
        );
        assert!(matches!(
            r.operations[0].2,
            Some(v2::OperationView::Running)
        ));
        assert_eq!(
            r.observe(&terminal(v2::Outcome::NotStarted {
                reason: v2::NotStartedReason::Deadline
            })),
            Err(ClientError::Protocol)
        );
        r.observe(&terminal(completed(true))).unwrap();
    }
}
#[test]
fn r1_no_output_evidence_still_allows_valid_recovered_not_started() {
    let mut r = Recovery::default();
    run(&mut r);
    r.observe(&terminal(v2::Outcome::NotStarted {
        reason: v2::NotStartedReason::Deadline,
    }))
    .unwrap();
}
#[test]
fn r2_observed_lease_cannot_change_or_disappear_but_can_progress() {
    use theseus_native_helper_store::IntentStore;
    let temp = tempfile::tempdir().unwrap();
    let mut store = IntentStore::new(
        theseus_host::fs::HostDir::create_private(&temp.path().join("intent")).unwrap(),
    );
    let record = IntentRecord {
        intent: v2::IntentId::from_bytes([8; 16]),
        binding: v2::BindingId::from_bytes([1; 16]),
        manifest_digest: v2::Digest::from_bytes([2; 32]),
        workspace_marker: v2::WorkspaceMarker::from_bytes([3; 16]),
    };
    let prepared = store.prepare(record.clone()).unwrap();
    let mut r = Recovery::default();
    let e = crate::pump::tests::expected();
    r.request(Action::RecoverAcquire(&prepared), &e).unwrap();
    let acquired = |state| v2::Reply::Acquired {
        intent: record.intent,
        binding: record.binding,
        manifest_digest: record.manifest_digest,
        state,
    };
    r.observe(&acquired(v2::AcquireView::NotFound)).unwrap();
    let lease = v2::LeaseId::from_bytes([5; 16]);
    r.observe(&acquired(v2::AcquireView::Lease {
        lease,
        state: v2::LeaseView::Held {
            intent: record.intent,
        },
    }))
    .unwrap();
    assert_eq!(
        r.observe(&acquired(v2::AcquireView::NotFound)),
        Err(ClientError::Protocol)
    );
    assert_eq!(
        r.observe(&acquired(v2::AcquireView::Lease {
            lease: v2::LeaseId::from_bytes([6; 16]),
            state: v2::LeaseView::Held {
                intent: record.intent
            }
        })),
        Err(ClientError::Protocol)
    );
    r.observe(&acquired(v2::AcquireView::Lease {
        lease,
        state: v2::LeaseView::AwaitingExternalReset {
            intent: record.intent,
        },
    }))
    .unwrap();
    assert!(r.quarantined.contains(&lease));
}
