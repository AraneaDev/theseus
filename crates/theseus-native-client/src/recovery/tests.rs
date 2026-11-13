use super::*;
fn ids() -> (v2::LeaseId, v2::OperationId) {
    (
        v2::LeaseId::from_bytes([1; 16]),
        v2::OperationId::from_bytes([2; 16]),
    )
}
#[test]
fn loss_requires_explicit_original_query_and_never_another_run() {
    let mut r = Recovery::default();
    let e = crate::pump::tests::expected();
    let (lease, operation) = ids();
    assert!(matches!(
        r.request(
            Action::Run {
                lease,
                operation,
                program: v2::ProgramId::new("probe").unwrap()
            },
            &e
        )
        .unwrap(),
        v2::Request::Run { .. }
    ));
    assert_eq!(
        r.request(
            Action::Run {
                lease,
                operation: v2::OperationId::from_bytes([3; 16]),
                program: v2::ProgramId::new("probe").unwrap()
            },
            &e
        ),
        Err(ClientError::RecoveryRequired)
    );
    assert_eq!(
        r.request(Action::Query { lease, operation }, &e).unwrap(),
        v2::Request::QueryOperation { lease, operation }
    );
}
#[test]
fn unknown_quarantines_new_identity_and_finished_never_changes() {
    let mut r = Recovery::default();
    let e = crate::pump::tests::expected();
    let (lease, operation) = ids();
    let finished = v2::Reply::Operation {
        lease,
        operation,
        state: v2::OperationView::Finished {
            outcome: v2::Outcome::NotStarted {
                reason: v2::NotStartedReason::CancelRequested,
            },
        },
    };
    r.observe(&finished).unwrap();
    r.observe(&finished).unwrap();
    assert_eq!(
        r.observe(&v2::Reply::Operation {
            lease,
            operation,
            state: v2::OperationView::Missing
        }),
        Err(ClientError::Protocol)
    );
    assert!(
        r.request(
            Action::Run {
                lease,
                operation: v2::OperationId::from_bytes([3; 16]),
                program: v2::ProgramId::new("probe").unwrap()
            },
            &e
        )
        .is_ok()
    );
    r.observe(&v2::Reply::Operation {
        lease,
        operation: v2::OperationId::from_bytes([3; 16]),
        state: v2::OperationView::Unknown {
            reason: v2::UnknownReason::ProcessStateUnproved,
        },
    })
    .unwrap();
    assert_eq!(
        r.request(
            Action::Run {
                lease,
                operation: v2::OperationId::from_bytes([4; 16]),
                program: v2::ProgramId::new("probe").unwrap()
            },
            &e
        ),
        Err(ClientError::RecoveryRequired)
    );
}
#[test]
fn prepared_acquire_copies_receipt_and_recovery_only_queries_same_intent() {
    use theseus_host::fs::HostDir;
    use theseus_native_helper_store::IntentStore;
    let temp = tempfile::tempdir().unwrap();
    let root = HostDir::create_private(&temp.path().join("intent")).unwrap();
    let mut store = IntentStore::new(root);
    let record = IntentRecord {
        intent: v2::IntentId::from_bytes([8; 16]),
        binding: v2::BindingId::from_bytes([1; 16]),
        manifest_digest: v2::Digest::from_bytes([2; 32]),
        workspace_marker: v2::WorkspaceMarker::from_bytes([3; 16]),
    };
    let prepared = store.prepare(record.clone()).unwrap();
    let e = crate::pump::tests::expected();
    let mut r = Recovery::default();
    assert_eq!(
        r.request(Action::Acquire(&prepared), &e).unwrap(),
        v2::Request::Acquire {
            intent: record.intent,
            binding: record.binding,
            manifest_digest: record.manifest_digest
        }
    );
    assert_eq!(
        r.request(Action::Acquire(&prepared), &e),
        Err(ClientError::RecoveryRequired)
    );
    assert_eq!(
        r.request(Action::RecoverAcquire(&prepared), &e).unwrap(),
        v2::Request::QueryAcquire {
            intent: record.intent,
            binding: record.binding,
            manifest_digest: record.manifest_digest
        }
    );
    assert_eq!(r.intents[0].0, record);
}
