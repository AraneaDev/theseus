use theseus_native_protocol::v2::{ledger::*, *};
fn header() -> Record {
    Record::new(
        0,
        Entry::Header {
            binding: BindingId::from_bytes([1; 16]),
            manifest_digest: Digest::from_bytes([2; 32]),
            workspace_marker: WorkspaceMarker::from_bytes([3; 16]),
            os: Os::Linux,
        },
    )
    .unwrap()
}
fn reserved() -> Record {
    Record::new(
        1,
        Entry::LeaseReserved {
            intent: IntentId::from_bytes([4; 16]),
            lease: LeaseId::from_bytes([5; 16]),
        },
    )
    .unwrap()
}
fn request() -> Request {
    Request::Run {
        lease: LeaseId::from_bytes([5; 16]),
        operation: OperationId::from_bytes([6; 16]),
        program: ProgramId::new("probe").unwrap(),
    }
}
fn accepted() -> Record {
    Record::new(
        2,
        Entry::OperationAccepted {
            lease: LeaseId::from_bytes([5; 16]),
            operation: OperationId::from_bytes([6; 16]),
            request_digest: request_digest(&request()).unwrap(),
            operation_kind: OperationKind::Run,
        },
    )
    .unwrap()
}
#[test]
fn restart_unknown_and_equal_replay() {
    let v = LedgerView::replay(&[header(), reserved(), accepted()]).unwrap();
    assert_eq!(
        v.replay_operation(&request()),
        Ok(ReplayDecision::Recorded(OperationView::Unknown {
            reason: UnknownReason::HelperRestartedWithUnfinishedOperation
        }))
    );
    assert!(matches!(
        v.query_lease(LeaseId::from_bytes([5; 16])),
        Ok(LeaseView::Quarantined { .. })
    ));
}
#[test]
fn header_only_absence_and_binding() {
    let v = LedgerView::replay(&[header()]).unwrap();
    assert_eq!(
        v.query_acquire(
            IntentId::from_bytes([4; 16]),
            BindingId::from_bytes([1; 16]),
            Digest::from_bytes([2; 32])
        ),
        Ok(AcquireView::NotFound)
    );
    assert_eq!(
        v.query_acquire(
            IntentId::from_bytes([4; 16]),
            BindingId::from_bytes([0; 16]),
            Digest::from_bytes([2; 32])
        ),
        Err(RefusalCode::WrongBinding)
    );
    assert!(LedgerView::replay(&[]).is_err());
}
#[test]
fn lost_acquire_reply_recovers_same_lease() {
    let v = LedgerView::replay(&[header(), reserved()]).unwrap();
    assert_eq!(
        v.query_acquire(
            IntentId::from_bytes([4; 16]),
            BindingId::from_bytes([1; 16]),
            Digest::from_bytes([2; 32])
        ),
        Ok(AcquireView::Lease {
            lease: LeaseId::from_bytes([5; 16]),
            state: LeaseView::Held {
                intent: IntentId::from_bytes([4; 16])
            }
        })
    );
    assert_eq!(
        v.replay_operation(&request()),
        Ok(ReplayDecision::NotRecorded)
    );
}
fn finished(outcome: Outcome) -> Record {
    Record::new(
        3,
        Entry::OperationFinished {
            lease: LeaseId::from_bytes([5; 16]),
            operation: OperationId::from_bytes([6; 16]),
            outcome,
        },
    )
    .unwrap()
}
fn close(index: u16, disposition: Disposition) -> Record {
    Record::new(
        index,
        Entry::LeaseClosed {
            lease: LeaseId::from_bytes([5; 16]),
            disposition,
        },
    )
    .unwrap()
}
#[test]
fn terminal_replay_conflict_and_final_close() {
    let out = Outcome::NotStarted {
        reason: NotStartedReason::Deadline,
    };
    let h = vec![
        header(),
        reserved(),
        accepted(),
        finished(out.clone()),
        close(4, Disposition::AwaitingExternalReset),
    ];
    let v = LedgerView::replay(&h).unwrap();
    assert_eq!(
        v.replay_operation(&request()),
        Ok(ReplayDecision::Recorded(OperationView::Finished {
            outcome: out
        }))
    );
    let mut changed = request();
    if let Request::Run { program, .. } = &mut changed {
        *program = ProgramId::new("other").unwrap();
    }
    assert_eq!(
        v.replay_operation(&changed),
        Err(RefusalCode::ReplayConflict)
    );
    if let Request::Run { operation, .. } = &mut changed {
        *operation = OperationId::from_bytes([7; 16]);
    }
    assert_eq!(
        v.replay_operation(&changed),
        Err(RefusalCode::AwaitingExternalReset)
    );
    let mut after = h;
    after.push(close(
        5,
        Disposition::Quarantined {
            reason: UnknownReason::LedgerWriteUncertain,
        },
    ));
    assert!(LedgerView::replay(&after).is_err());
}
#[test]
fn all_no_effect_outcome_claims_structural_for_both_kinds() {
    for kind in [OperationKind::CreateFile, OperationKind::Run] {
        for reason in [
            NotStartedReason::CancelRequested,
            NotStartedReason::Deadline,
        ] {
            let mut a = accepted();
            if let Entry::OperationAccepted {
                lease,
                operation,
                request_digest,
                ..
            } = a.entry()
            {
                a = Record::new(
                    2,
                    Entry::OperationAccepted {
                        lease: *lease,
                        operation: *operation,
                        request_digest: *request_digest,
                        operation_kind: kind,
                    },
                )
                .unwrap();
            }
            assert!(
                LedgerView::replay(&[
                    header(),
                    reserved(),
                    a,
                    finished(Outcome::NotStarted { reason })
                ])
                .is_ok()
            );
        }
    }
    for code in [
        FileCreateFailureCode::AlreadyExists,
        FileCreateFailureCode::Permission,
    ] {
        let a = Record::new(
            2,
            Entry::OperationAccepted {
                lease: LeaseId::from_bytes([5; 16]),
                operation: OperationId::from_bytes([6; 16]),
                request_digest: Digest::from_bytes([0; 32]),
                operation_kind: OperationKind::CreateFile,
            },
        )
        .unwrap();
        assert!(
            LedgerView::replay(&[
                header(),
                reserved(),
                a,
                finished(Outcome::FileCreateFailed { code })
            ])
            .is_ok()
        );
        assert!(
            LedgerView::replay(&[
                header(),
                reserved(),
                accepted(),
                finished(Outcome::FileCreateFailed { code })
            ])
            .is_err()
        );
    }
}
#[test]
fn invalid_order_ids_kind_and_os_fail_closed() {
    let out = Outcome::NotStarted {
        reason: NotStartedReason::Deadline,
    };
    let histories = vec![
        vec![reserved()],
        vec![header(), header()],
        vec![
            header(),
            reserved(),
            Record::new(2, reserved().entry().clone()).unwrap(),
        ],
        vec![
            header(),
            reserved(),
            accepted(),
            Record::new(3, accepted().entry().clone()).unwrap(),
        ],
        vec![header(), reserved(), finished(out.clone())],
        vec![
            header(),
            reserved(),
            accepted(),
            finished(out.clone()),
            Record::new(4, finished(out).entry().clone()).unwrap(),
        ],
        vec![
            header(),
            reserved(),
            accepted(),
            close(3, Disposition::AwaitingExternalReset),
        ],
        vec![
            header(),
            reserved(),
            accepted(),
            finished(Outcome::FileCreated {
                length: 0,
                digest: Digest::from_bytes([0; 32]),
            }),
        ],
        vec![
            header(),
            reserved(),
            accepted(),
            finished(Outcome::RunCompleted {
                reason: RunReason::Natural,
                status: NativeStatus::WindowsExit { code: 0 },
                cleanup: Cleanup::WindowsProcessExitedJobEmpty,
                stdout_bytes: 0,
                stderr_bytes: 0,
                truncated: false,
            }),
        ],
    ];
    for h in histories {
        assert!(LedgerView::replay(&h).is_err());
    }
    let v = LedgerView::replay(&[header(), reserved()]).unwrap();
    assert_eq!(
        v.query_lease(LeaseId::from_bytes([9; 16])),
        Err(RefusalCode::WrongLease)
    );
    assert_eq!(
        v.query_operation(
            LeaseId::from_bytes([9; 16]),
            OperationId::from_bytes([6; 16])
        ),
        Err(RefusalCode::WrongLease)
    );
    assert_eq!(
        v.replay_operation(&Request::Describe),
        Err(RefusalCode::InvalidRequest)
    );
    assert_eq!(
        v.query_acquire(
            IntentId::from_bytes([4; 16]),
            BindingId::from_bytes([1; 16]),
            Digest::from_bytes([0; 32])
        ),
        Err(RefusalCode::WrongManifest)
    );
}
#[test]
fn quarantine_active_unknown_and_absence_refused() {
    for reason in [
        UnknownReason::HelperRestartedWithUnfinishedOperation,
        UnknownReason::LedgerWriteUncertain,
        UnknownReason::ProcessStateUnproved,
        UnknownReason::FileEffectUncertain,
    ] {
        let v = LedgerView::replay(&[
            header(),
            reserved(),
            accepted(),
            close(3, Disposition::Quarantined { reason }),
        ])
        .unwrap();
        assert_eq!(
            v.query_operation(
                LeaseId::from_bytes([5; 16]),
                OperationId::from_bytes([6; 16])
            ),
            Ok(OperationView::Unknown { reason })
        );
        let mut new = request();
        if let Request::Run { operation, .. } = &mut new {
            *operation = OperationId::from_bytes([9; 16])
        }
        assert_eq!(v.replay_operation(&new), Err(RefusalCode::LeaseQuarantined));
    }
}
#[test]
fn full_128_operation_history_can_always_close_and_129_refused() {
    let mut h = vec![header(), reserved()];
    for i in 0..128u16 {
        let operation = OperationId::from_bytes([i as u8; 16]);
        h.push(
            Record::new(
                2 + i * 2,
                Entry::OperationAccepted {
                    lease: LeaseId::from_bytes([5; 16]),
                    operation,
                    request_digest: Digest::from_bytes([0; 32]),
                    operation_kind: OperationKind::Run,
                },
            )
            .unwrap(),
        );
        h.push(
            Record::new(
                3 + i * 2,
                Entry::OperationFinished {
                    lease: LeaseId::from_bytes([5; 16]),
                    operation,
                    outcome: Outcome::NotStarted {
                        reason: NotStartedReason::CancelRequested,
                    },
                },
            )
            .unwrap(),
        );
    }
    assert!(LedgerView::replay(&h).is_ok());
    let mut extra = h.clone();
    extra.push(
        Record::new(
            258,
            Entry::OperationAccepted {
                lease: LeaseId::from_bytes([5; 16]),
                operation: OperationId::from_bytes([128; 16]),
                request_digest: Digest::from_bytes([0; 32]),
                operation_kind: OperationKind::Run,
            },
        )
        .unwrap(),
    );
    assert!(matches!(
        LedgerView::replay(&extra),
        Err(LedgerError::Capacity)
    ));
    h.push(close(258, Disposition::AwaitingExternalReset));
    assert_eq!(h.len(), 259);
    assert!(LedgerView::replay(&h).is_ok());
    assert!(
        h.iter()
            .map(|r| theseus_native_protocol::v2::ledger::encode(r)
                .unwrap()
                .len())
            .sum::<usize>()
            <= 97020
    );
}
#[test]
fn independent_request_digest_oracles() {
    let lease = LeaseId::from_bytes([0x22; 16]);
    for (req, expected) in [
        (
            Request::Run {
                lease,
                operation: OperationId::from_bytes([0x33; 16]),
                program: ProgramId::new("probe_success").unwrap(),
            },
            "c75e02da2d19356ec35e0f4114228f39fca7b71257673eb4d26a2becf2c60e5a",
        ),
        (
            Request::CreateFile {
                lease,
                operation: OperationId::from_bytes([0x44; 16]),
                path: LeafName::new("probe.bin").unwrap(),
                bytes: FileBytes::new(&[0, 255]).unwrap(),
            },
            "28aaac294213674416d991c389734bd950438e2fcfe13f52fa254e2ec80eb846",
        ),
    ] {
        assert_eq!(
            request_digest(&req).unwrap(),
            Digest::parse(expected).unwrap()
        );
    }
    assert!(request_digest(&Request::Describe).is_err());
}
#[test]
fn native_outcomes_match_each_header_os() {
    for os in [Os::Linux, Os::Macos, Os::Windows] {
        let head = Record::new(
            0,
            Entry::Header {
                binding: BindingId::from_bytes([1; 16]),
                manifest_digest: Digest::from_bytes([2; 32]),
                workspace_marker: WorkspaceMarker::from_bytes([3; 16]),
                os,
            },
        )
        .unwrap();
        for (out, windows) in [
            (
                Outcome::RunCompleted {
                    reason: RunReason::Natural,
                    status: NativeStatus::PosixExit { code: 0 },
                    cleanup: Cleanup::PosixLeaderReapedGroupTerminationRequested,
                    stdout_bytes: 0,
                    stderr_bytes: 1,
                    truncated: false,
                },
                Some(false),
            ),
            (
                Outcome::RunCompleted {
                    reason: RunReason::Deadline,
                    status: NativeStatus::PosixSignal { signal: 255 },
                    cleanup: Cleanup::PosixLeaderReapedGroupTerminationRequested,
                    stdout_bytes: 2,
                    stderr_bytes: 3,
                    truncated: true,
                },
                Some(false),
            ),
            (
                Outcome::RunCompleted {
                    reason: RunReason::CancelRequested,
                    status: NativeStatus::WindowsExit { code: u32::MAX },
                    cleanup: Cleanup::WindowsProcessExitedJobEmpty,
                    stdout_bytes: 4,
                    stderr_bytes: 5,
                    truncated: false,
                },
                Some(true),
            ),
            (
                Outcome::LaunchFailed {
                    phase: LaunchPhase::NeverCreated,
                    code: FailureCode::Io,
                    cleanup: SpawnCleanup::NoChildCreated,
                },
                None,
            ),
            (
                Outcome::LaunchFailed {
                    phase: LaunchPhase::MayRun,
                    code: FailureCode::Permission,
                    cleanup: SpawnCleanup::PosixLeaderReapedGroupTerminationRequested,
                },
                Some(false),
            ),
            (
                Outcome::LaunchFailed {
                    phase: LaunchPhase::CreatedSuspended,
                    code: FailureCode::Unsupported,
                    cleanup: SpawnCleanup::WindowsProcessExitedJobEmpty,
                },
                Some(true),
            ),
            (
                Outcome::LaunchFailed {
                    phase: LaunchPhase::MayRun,
                    code: FailureCode::NativeFailure,
                    cleanup: SpawnCleanup::WindowsProcessExitedJobEmpty,
                },
                Some(true),
            ),
        ] {
            assert_eq!(
                LedgerView::replay(&[head.clone(), reserved(), accepted(), finished(out)]).is_ok(),
                windows.is_none_or(|w| w == (os == Os::Windows))
            );
        }
    }
}
#[test]
fn wrong_finish_identity_and_duplicate_acceptance_are_invalid() {
    for (lease, operation) in [
        (
            LeaseId::from_bytes([9; 16]),
            OperationId::from_bytes([6; 16]),
        ),
        (
            LeaseId::from_bytes([5; 16]),
            OperationId::from_bytes([9; 16]),
        ),
    ] {
        let f = Record::new(
            3,
            Entry::OperationFinished {
                lease,
                operation,
                outcome: Outcome::NotStarted {
                    reason: NotStartedReason::Deadline,
                },
            },
        )
        .unwrap();
        assert!(LedgerView::replay(&[header(), reserved(), accepted(), f]).is_err());
    }
    let f = finished(Outcome::NotStarted {
        reason: NotStartedReason::Deadline,
    });
    let a = Record::new(4, accepted().entry().clone()).unwrap();
    assert!(LedgerView::replay(&[header(), reserved(), accepted(), f, a]).is_err());
}
