mod common;
use common::*;
use std::io::Write;
use theseus_native_helper_store::*;
use theseus_native_protocol::v2::{ledger::*, *};
#[test]
fn append_receipt_tracks_physical_bytes_and_preflight_preserves_state() {
    let f = Fixture::new();
    let b = f.bootstrap();
    let mut s =
        LedgerStore::open(&f.ledger, &f.workspace, f.binding(b.workspace_marker())).unwrap();
    let r = s.append(reserved()).unwrap();
    assert_eq!(r.record(), &reserved());
    // While the store is open, physical bytes are read through its own locked
    // handle; Windows refuses reads through any other handle.
    let before = s.read_physical().unwrap();
    assert_eq!(r.ledger_bytes(), before.len() as u64);
    assert!(s.append(reserved()).is_err());
    assert_eq!(s.read_physical().unwrap(), before);
    assert_eq!(s.records().len(), 2);
    assert!(!s.is_write_poisoned());
    drop(s);
    assert_eq!(f.bytes(), before);
    let s = LedgerStore::open(&f.ledger, &f.workspace, f.binding(b.workspace_marker())).unwrap();
    assert_eq!(s.records().len(), 2);
}
#[test]
fn whole_input_and_finish_reject_torn_tail_corruption_and_oversize() {
    for tail in [
        vec![0],
        ledger::encode(&reserved()).unwrap()[..20].to_vec(),
        {
            let mut v = ledger::encode(&reserved()).unwrap();
            v.pop();
            v
        },
        vec![0; MAX_LEDGER_BYTES],
    ] {
        let f = Fixture::new();
        let b = f.bootstrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(f.tmp.path().join("ledger/ledger.bin"))
            .unwrap()
            .write_all(&tail)
            .unwrap();
        assert!(
            LedgerStore::open(&f.ledger, &f.workspace, f.binding(b.workspace_marker())).is_err()
        );
    }
}
#[test]
fn exact_marker_and_every_header_field_are_required() {
    let f = Fixture::new();
    let b = f.bootstrap();
    for which in 0..4 {
        let mut expected = f.binding(b.workspace_marker());
        match which {
            0 => expected.binding = BindingId::from_bytes([9; 16]),
            1 => expected.manifest_digest = Digest::from_bytes([9; 32]),
            2 => expected.workspace_marker = WorkspaceMarker::from_bytes([9; 16]),
            _ => expected.os = Os::Windows,
        };
        assert!(LedgerStore::open(&f.ledger, &f.workspace, expected).is_err());
    }
    std::fs::write(
        f.tmp.path().join("workspace/.theseus-workspace-marker"),
        format!("{}\n", b.workspace_marker().to_hex()),
    )
    .unwrap();
    let result = LedgerStore::open(&f.ledger, &f.workspace, f.binding(b.workspace_marker()));
    assert!(
        matches!(result, Err(StoreError::MarkerMismatch)),
        "unexpected error: {:?}",
        result.err()
    );
}
#[test]
fn reopened_unfinished_allows_only_exact_quarantine() {
    let f = Fixture::new();
    let b = f.bootstrap();
    let mut s =
        LedgerStore::open(&f.ledger, &f.workspace, f.binding(b.workspace_marker())).unwrap();
    s.append(reserved()).unwrap();
    s.append(accepted()).unwrap();
    assert!(matches!(
        s.restart_view()
            .query_operation(
                LeaseId::from_bytes([4; 16]),
                OperationId::from_bytes([5; 16])
            )
            .unwrap(),
        OperationView::Unknown { .. }
    ));
    drop(s);
    let mut s =
        LedgerStore::open(&f.ledger, &f.workspace, f.binding(b.workspace_marker())).unwrap();
    assert!(matches!(
        s.append(reserved()),
        Err(StoreError::RecoveredUnfinished)
    ));
    let finished = Record::new(
        3,
        Entry::OperationFinished {
            lease: LeaseId::from_bytes([4; 16]),
            operation: OperationId::from_bytes([5; 16]),
            outcome: Outcome::NotStarted {
                reason: NotStartedReason::CancelRequested,
            },
        },
    )
    .unwrap();
    assert!(matches!(
        s.append(finished),
        Err(StoreError::RecoveredUnfinished)
    ));
    let new_accepted = Record::new(
        3,
        Entry::OperationAccepted {
            lease: LeaseId::from_bytes([4; 16]),
            operation: OperationId::from_bytes([7; 16]),
            request_digest: Digest::from_bytes([8; 32]),
            operation_kind: OperationKind::Run,
        },
    )
    .unwrap();
    assert!(matches!(
        s.append(new_accepted),
        Err(StoreError::RecoveredUnfinished)
    ));
    let wrong = Record::new(
        3,
        Entry::LeaseClosed {
            lease: LeaseId::from_bytes([4; 16]),
            disposition: Disposition::Quarantined {
                reason: UnknownReason::FileEffectUncertain,
            },
        },
    )
    .unwrap();
    assert!(matches!(
        s.append(wrong),
        Err(StoreError::RecoveredUnfinished)
    ));
    s.append(
        Record::new(
            3,
            Entry::LeaseClosed {
                lease: LeaseId::from_bytes([4; 16]),
                disposition: Disposition::Quarantined {
                    reason: UnknownReason::HelperRestartedWithUnfinishedOperation,
                },
            },
        )
        .unwrap(),
    )
    .unwrap();
    assert!(s.append(reserved()).is_err());
}
#[test]
fn malformed_complete_ledgers_and_explicit_quarantine_are_not_reinterpreted() {
    for kind in 0..5 {
        let f = Fixture::new();
        let b = f.bootstrap();
        let mut bytes = f.bytes();
        match kind {
            0 => bytes.clear(),
            1 => {
                bytes[8] ^= 1;
            }
            2 => {
                bytes.extend(
                    ledger::encode(&Record::new(2, reserved().entry().clone()).unwrap()).unwrap(),
                );
            }
            3 => {
                bytes.extend(ledger::encode(&accepted()).unwrap());
            }
            _ => {
                bytes.extend(ledger::encode(&reserved()).unwrap());
                bytes.extend(ledger::encode(&reserved()).unwrap());
            }
        }
        std::fs::write(f.tmp.path().join("ledger/ledger.bin"), bytes).unwrap();
        assert!(
            LedgerStore::open(&f.ledger, &f.workspace, f.binding(b.workspace_marker())).is_err()
        );
    }
    for reason in [
        UnknownReason::FileEffectUncertain,
        UnknownReason::ProcessStateUnproved,
    ] {
        let f = Fixture::new();
        let b = f.bootstrap();
        let mut s =
            LedgerStore::open(&f.ledger, &f.workspace, f.binding(b.workspace_marker())).unwrap();
        s.append(reserved()).unwrap();
        s.append(accepted()).unwrap();
        s.append(
            Record::new(
                3,
                Entry::LeaseClosed {
                    lease: LeaseId::from_bytes([4; 16]),
                    disposition: Disposition::Quarantined { reason },
                },
            )
            .unwrap(),
        )
        .unwrap();
        drop(s);
        let mut s =
            LedgerStore::open(&f.ledger, &f.workspace, f.binding(b.workspace_marker())).unwrap();
        assert!(
            matches!(s.restart_view().query_lease(LeaseId::from_bytes([4;16])).unwrap(),LeaseView::Quarantined{reason:actual,..} if actual==reason)
        );
        assert!(s.append(reserved()).is_err());
    }
}
#[test]
fn correctly_hashed_unsupported_schema_and_noncanonical_records_fail_before_binding() {
    for (bytes, expected) in [
        (
            include_bytes!("fixtures/unsupported-schema.bin").as_slice(),
            LedgerError::UnsupportedSchema,
        ),
        (
            include_bytes!("fixtures/noncanonical.bin").as_slice(),
            LedgerError::NonCanonical,
        ),
    ] {
        let f = Fixture::new();
        let b = f.bootstrap();
        std::fs::write(f.tmp.path().join("ledger/ledger.bin"), bytes).unwrap();
        assert!(
            matches!(LedgerStore::open(&f.ledger, &f.workspace, f.binding(b.workspace_marker())), Err(StoreError::Protocol(error)) if error == expected)
        );
    }
}
