use super::*;

fn create(lease: v2::LeaseId, operation: v2::OperationId) -> Action<'static> {
    Action::CreateFile {
        lease,
        operation,
        path: v2::LeafName::new("note").unwrap(),
        bytes: v2::FileBytes::new(b"hello").unwrap(),
    }
}
#[test]
fn create_retains_original_identity_without_run_stream_and_requires_query_after_loss() {
    let mut r = Recovery::default();
    let e = crate::pump::tests::expected();
    let lease = v2::LeaseId::from_bytes([1; 16]);
    let operation = v2::OperationId::from_bytes([2; 16]);
    let request = r.request(create(lease, operation), &e).unwrap();
    assert!(matches!(request, v2::Request::CreateFile { .. }));
    assert!(r.captures.is_empty());
    assert_eq!(
        r.request(create(lease, operation), &e),
        Err(ClientError::RecoveryRequired)
    );
    assert_eq!(
        r.request(create(lease, v2::OperationId::from_bytes([3; 16])), &e),
        Err(ClientError::RecoveryRequired)
    );
    assert_eq!(
        r.request(
            Action::Run {
                lease,
                operation,
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
    for outcome in [
        v2::Outcome::FileCreated {
            length: 4,
            digest: v2::Digest::from_bytes(Sha256::digest(b"hello").into()),
        },
        v2::Outcome::FileCreated {
            length: 5,
            digest: v2::Digest::from_bytes([0; 32]),
        },
        v2::Outcome::LaunchFailed {
            phase: v2::LaunchPhase::NeverCreated,
            code: v2::FailureCode::Io,
            cleanup: v2::SpawnCleanup::NoChildCreated,
        },
    ] {
        assert_eq!(
            r.observe(&v2::Reply::Operation {
                lease,
                operation,
                state: v2::OperationView::Finished { outcome }
            }),
            Err(ClientError::Protocol)
        );
    }
    assert_eq!(
        r.observe(&v2::Reply::Operation {
            lease,
            operation,
            state: v2::OperationView::Accepted {
                operation_kind: v2::OperationKind::Run
            }
        }),
        Err(ClientError::Protocol)
    );
    let reply = v2::Reply::Operation {
        lease,
        operation,
        state: v2::OperationView::Finished {
            outcome: v2::Outcome::FileCreated {
                length: 5,
                digest: v2::Digest::from_bytes(Sha256::digest(b"hello").into()),
            },
        },
    };
    r.observe(&reply).unwrap();
    r.observe(&reply).unwrap();
    assert_eq!(
        r.observe(&v2::Reply::Operation {
            lease,
            operation,
            state: v2::OperationView::Unknown {
                reason: v2::UnknownReason::FileEffectUncertain
            }
        }),
        Err(ClientError::Protocol)
    );
    assert!(r.captures.is_empty());
}
