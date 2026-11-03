use sha2::{Digest as _, Sha256};
use theseus_native_protocol::v2::{
    ledger::{self, *},
    *,
};
fn fixture(name: &str) -> Vec<u8> {
    match name {
        "ledger-header-00" => include_bytes!("fixtures/v2/ledger-header-00.frame").to_vec(),
        "ledger-header-01" => include_bytes!("fixtures/v2/ledger-header-01.frame").to_vec(),
        "ledger-header-02" => include_bytes!("fixtures/v2/ledger-header-02.frame").to_vec(),
        "ledger-lease_closed-00" => {
            include_bytes!("fixtures/v2/ledger-lease_closed-00.frame").to_vec()
        }
        "ledger-lease_closed-01" => {
            include_bytes!("fixtures/v2/ledger-lease_closed-01.frame").to_vec()
        }
        "ledger-lease_closed-02" => {
            include_bytes!("fixtures/v2/ledger-lease_closed-02.frame").to_vec()
        }
        "ledger-lease_closed-03" => {
            include_bytes!("fixtures/v2/ledger-lease_closed-03.frame").to_vec()
        }
        "ledger-lease_closed-04" => {
            include_bytes!("fixtures/v2/ledger-lease_closed-04.frame").to_vec()
        }
        "ledger-lease_reserved-00" => {
            include_bytes!("fixtures/v2/ledger-lease_reserved-00.frame").to_vec()
        }
        "ledger-operation_accepted-00" => {
            include_bytes!("fixtures/v2/ledger-operation_accepted-00.frame").to_vec()
        }
        "ledger-operation_accepted-01" => {
            include_bytes!("fixtures/v2/ledger-operation_accepted-01.frame").to_vec()
        }
        "ledger-operation_finished-00" => {
            include_bytes!("fixtures/v2/ledger-operation_finished-00.frame").to_vec()
        }
        "ledger-operation_finished-01" => {
            include_bytes!("fixtures/v2/ledger-operation_finished-01.frame").to_vec()
        }
        "ledger-operation_finished-02" => {
            include_bytes!("fixtures/v2/ledger-operation_finished-02.frame").to_vec()
        }
        "ledger-operation_finished-03" => {
            include_bytes!("fixtures/v2/ledger-operation_finished-03.frame").to_vec()
        }
        "ledger-operation_finished-04" => {
            include_bytes!("fixtures/v2/ledger-operation_finished-04.frame").to_vec()
        }
        "ledger-operation_finished-05" => {
            include_bytes!("fixtures/v2/ledger-operation_finished-05.frame").to_vec()
        }
        "ledger-operation_finished-06" => {
            include_bytes!("fixtures/v2/ledger-operation_finished-06.frame").to_vec()
        }
        "ledger-operation_finished-07" => {
            include_bytes!("fixtures/v2/ledger-operation_finished-07.frame").to_vec()
        }
        "ledger-operation_finished-08" => {
            include_bytes!("fixtures/v2/ledger-operation_finished-08.frame").to_vec()
        }
        "ledger-operation_finished-09" => {
            include_bytes!("fixtures/v2/ledger-operation_finished-09.frame").to_vec()
        }
        "ledger-operation_finished-10" => {
            include_bytes!("fixtures/v2/ledger-operation_finished-10.frame").to_vec()
        }
        "ledger-operation_finished-11" => {
            include_bytes!("fixtures/v2/ledger-operation_finished-11.frame").to_vec()
        }
        "ledger-operation_finished-12" => {
            include_bytes!("fixtures/v2/ledger-operation_finished-12.frame").to_vec()
        }
        "ledger-operation_finished-13" => {
            include_bytes!("fixtures/v2/ledger-operation_finished-13.frame").to_vec()
        }
        "ledger-operation_finished-14" => {
            include_bytes!("fixtures/v2/ledger-operation_finished-14.frame").to_vec()
        }
        "ledger-operation_finished-15" => {
            include_bytes!("fixtures/v2/ledger-operation_finished-15.frame").to_vec()
        }
        "ledger-operation_finished-16" => {
            include_bytes!("fixtures/v2/ledger-operation_finished-16.frame").to_vec()
        }
        "ledger-operation_finished-17" => {
            include_bytes!("fixtures/v2/ledger-operation_finished-17.frame").to_vec()
        }
        _ => panic!("unknown fixture"),
    }
}
fn expected(name: &str) -> Record {
    let lease = LeaseId::from_bytes([255; 16]);
    let operation = OperationId::from_bytes([255; 16]);
    let suffix = name.rsplit('-').next().unwrap().parse::<usize>().unwrap();
    let (index, entry) = if name.starts_with("ledger-header") {
        (
            0,
            Entry::Header {
                binding: BindingId::from_bytes([255; 16]),
                manifest_digest: Digest::from_bytes([255; 32]),
                workspace_marker: WorkspaceMarker::from_bytes([255; 16]),
                os: [Os::Linux, Os::Macos, Os::Windows][suffix],
            },
        )
    } else if name.starts_with("ledger-lease_reserved") {
        (
            1,
            Entry::LeaseReserved {
                intent: IntentId::from_bytes([255; 16]),
                lease,
            },
        )
    } else if name.starts_with("ledger-operation_accepted") {
        (
            256,
            Entry::OperationAccepted {
                lease,
                operation,
                request_digest: Digest::from_bytes([255; 32]),
                operation_kind: [OperationKind::CreateFile, OperationKind::Run][suffix],
            },
        )
    } else if name.starts_with("ledger-lease_closed") {
        (
            258,
            Entry::LeaseClosed {
                lease,
                disposition: if suffix == 0 {
                    Disposition::AwaitingExternalReset
                } else {
                    Disposition::Quarantined {
                        reason: [
                            UnknownReason::HelperRestartedWithUnfinishedOperation,
                            UnknownReason::LedgerWriteUncertain,
                            UnknownReason::ProcessStateUnproved,
                            UnknownReason::FileEffectUncertain,
                        ][suffix - 1],
                    }
                },
            },
        )
    } else {
        let outcome = match suffix {
            0..=5 | 7..=9 => {
                let reason = [
                    RunReason::Natural,
                    RunReason::CancelRequested,
                    RunReason::Deadline,
                ][if suffix <= 5 { suffix % 3 } else { suffix - 7 }];
                let (status, cleanup) = match suffix {
                    0..=2 => (
                        NativeStatus::PosixExit { code: 255 },
                        Cleanup::PosixLeaderReapedGroupTerminationRequested,
                    ),
                    3..=5 => (
                        NativeStatus::PosixSignal { signal: 255 },
                        Cleanup::PosixLeaderReapedGroupTerminationRequested,
                    ),
                    _ => (
                        NativeStatus::WindowsExit { code: u32::MAX },
                        Cleanup::WindowsProcessExitedJobEmpty,
                    ),
                };
                Outcome::RunCompleted {
                    reason,
                    status,
                    cleanup,
                    stdout_bytes: u64::MAX,
                    stderr_bytes: u64::MAX,
                    truncated: false,
                }
            }
            6 | 10 | 11 | 17 => Outcome::LaunchFailed {
                phase: match suffix {
                    11 => LaunchPhase::CreatedSuspended,
                    17 => LaunchPhase::NeverCreated,
                    _ => LaunchPhase::MayRun,
                },
                code: FailureCode::InvalidCatalog,
                cleanup: match suffix {
                    6 => SpawnCleanup::PosixLeaderReapedGroupTerminationRequested,
                    17 => SpawnCleanup::NoChildCreated,
                    _ => SpawnCleanup::WindowsProcessExitedJobEmpty,
                },
            },
            12 | 13 => Outcome::NotStarted {
                reason: if suffix == 12 {
                    NotStartedReason::CancelRequested
                } else {
                    NotStartedReason::Deadline
                },
            },
            14 | 15 => Outcome::FileCreateFailed {
                code: if suffix == 14 {
                    FileCreateFailureCode::AlreadyExists
                } else {
                    FileCreateFailureCode::Permission
                },
            },
            16 => Outcome::FileCreated {
                length: 4096,
                digest: Digest::from_bytes([255; 32]),
            },
            _ => panic!("unknown fixture"),
        };
        (
            257,
            Entry::OperationFinished {
                lease,
                operation,
                outcome,
            },
        )
    };
    Record::new(index, entry).unwrap()
}
fn names() -> Vec<String> {
    let mut n = vec![];
    for (kind, count) in [
        ("header", 3),
        ("lease_reserved", 1),
        ("operation_accepted", 2),
        ("operation_finished", 18),
        ("lease_closed", 5),
    ] {
        for i in 0..count {
            n.push(format!("ledger-{kind}-{i:02}"));
        }
    }
    n
}
#[test]
fn independent_literals_decode_to_typed_records_and_encode_separately() {
    let mut maxima = [0; 5];
    for name in names() {
        let bytes = fixture(&name);
        let expected = expected(&name);
        let mut d = ledger::Decoder::new();
        let p = d.feed(&bytes).unwrap();
        assert_eq!(p.consumed, bytes.len(), "{name}");
        assert_eq!(p.record, Some(expected.clone()), "{name}");
        d.finish().unwrap();
        assert_eq!(ledger::encode(&expected).unwrap(), bytes, "{name}");
        let j = match expected.entry() {
            Entry::Header { .. } => 0,
            Entry::LeaseReserved { .. } => 1,
            Entry::OperationAccepted { .. } => 2,
            Entry::OperationFinished { .. } => 3,
            Entry::LeaseClosed { .. } => 4,
        };
        maxima[j] = maxima[j].max(bytes.len());
    }
    assert_eq!(maxima, [357, 180, 304, 448, 227]);
    assert_eq!(357 + 180 + 128 * (304 + 448) + 227, 97020);
}
#[test]
fn all_splits_one_byte_and_coalesced_retention() {
    for name in names() {
        let bytes = fixture(&name);
        for split in 0..=bytes.len() {
            let mut d = ledger::Decoder::new();
            let a = d.feed(&bytes[..split]).unwrap();
            let b = d.feed(&bytes[split..]).unwrap();
            assert_eq!(a.consumed + b.consumed, bytes.len());
            assert_eq!(a.record.or(b.record), Some(expected(&name)));
            d.finish().unwrap();
        }
        let mut d = ledger::Decoder::new();
        let mut result = None;
        for b in &bytes {
            let p = d.feed(&[*b]).unwrap();
            if p.record.is_some() {
                result = p.record
            }
        }
        assert_eq!(result, Some(expected(&name)));
        let joined = [bytes.clone(), bytes.clone()].concat();
        let mut d = ledger::Decoder::new();
        let first = d.feed(&joined).unwrap();
        assert_eq!(first.consumed, bytes.len());
        assert!(first.record.is_some());
        assert!(d.feed(&joined[first.consumed..]).unwrap().record.is_some());
    }
}
fn raw(b: &[u8]) -> Vec<u8> {
    let mut v = (b.len() as u32).to_be_bytes().to_vec();
    v.extend(b);
    v.extend(Sha256::digest(b));
    v
}
fn reject(b: Vec<u8>, e: LedgerError) {
    let mut d = ledger::Decoder::new();
    assert_eq!(d.feed(&b).err(), Some(e));
    assert_eq!(d.feed(&[]).err(), Some(LedgerError::Poisoned));
    assert_eq!(d.finish(), Err(LedgerError::Poisoned));
}
#[test]
fn canonical_precedes_hash_and_rejections_poison() {
    let bytes = fixture("ledger-header-00");
    let text = String::from_utf8(bytes[4..bytes.len() - 32].to_vec()).unwrap();
    for s in [
        text.replacen("{", "{ ", 1),
        text.replacen("\"schema\":1,\"index\":0", "\"index\":0,\"schema\":1", 1),
        text.replacen("trusted", "\\u0074rusted", 1),
    ] {
        let mut bad = raw(s.as_bytes());
        *bad.last_mut().unwrap() ^= 1;
        reject(bad, LedgerError::NonCanonical);
    }
    let mut bad = bytes.clone();
    *bad.last_mut().unwrap() ^= 1;
    reject(bad, LedgerError::HashMismatch);
    for (from, to, error) in [
        (
            "\"schema\":1",
            "\"schema\":2",
            LedgerError::UnsupportedSchema,
        ),
        ("\"index\":0", "\"index\":259", LedgerError::InvalidRecord),
        ("\"index\":0", "\"index\":null", LedgerError::InvalidRecord),
        ("\"index\":0", "\"index\":0.0", LedgerError::InvalidRecord),
        (
            "\"index\":0",
            "\"index\":0,\"index\":0",
            LedgerError::InvalidJson,
        ),
        (
            "\"os\":\"linux\"",
            "\"os\":\"other\"",
            LedgerError::InvalidRecord,
        ),
        (
            "\"os\":\"linux\"",
            "\"os\":\"linux\",\"extra\":0",
            LedgerError::InvalidRecord,
        ),
        (
            "ffffffffffffffffffffffffffffffff\"",
            "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF\"",
            LedgerError::InvalidRecord,
        ),
    ] {
        reject(raw(text.replace(from, to).as_bytes()), error);
    }
    reject(raw(&[255]), LedgerError::InvalidUtf8);
    reject(raw(b"{}{}"), LedgerError::InvalidJson);
    for n in [0u32, 16385, u32::MAX] {
        reject(n.to_be_bytes().to_vec(), LedgerError::InvalidLength);
    }
    for n in 1..bytes.len() {
        let mut d = ledger::Decoder::new();
        assert!(d.feed(&bytes[..n]).unwrap().record.is_none());
        assert_eq!(d.finish(), Err(LedgerError::Truncated));
        assert_eq!(d.finish(), Err(LedgerError::Poisoned));
    }
}
#[test]
fn asymmetric_ids_and_scalar_fields_are_not_swapped() {
    let text=br#"{"schema":1,"index":22,"record":{"kind":"operation_accepted","lease":"11111111111111111111111111111111","operation":"22222222222222222222222222222222","request_digest":"3333333333333333333333333333333333333333333333333333333333333333","operation_kind":"run"}}"#;
    let expected = Record::new(
        22,
        Entry::OperationAccepted {
            lease: LeaseId::from_bytes([0x11; 16]),
            operation: OperationId::from_bytes([0x22; 16]),
            request_digest: Digest::from_bytes([0x33; 32]),
            operation_kind: OperationKind::Run,
        },
    )
    .unwrap();
    let bytes = raw(text);
    let mut d = ledger::Decoder::new();
    assert_eq!(d.feed(&bytes).unwrap().record, Some(expected.clone()));
    assert_eq!(ledger::encode(&expected).unwrap(), bytes);
    let nested=br#"{"schema":1,"index":3,"record":{"kind":"operation_finished","lease":"11111111111111111111111111111111","operation":"22222222222222222222222222222222","outcome":{"kind":"run_completed","reason":"deadline","status":{"kind":"posix_exit","code":7},"cleanup":"posix_leader_reaped_group_termination_requested","stdout_bytes":13,"stderr_bytes":29,"truncated":true}}}"#;
    let expected = Record::new(
        3,
        Entry::OperationFinished {
            lease: LeaseId::from_bytes([0x11; 16]),
            operation: OperationId::from_bytes([0x22; 16]),
            outcome: Outcome::RunCompleted {
                reason: RunReason::Deadline,
                status: NativeStatus::PosixExit { code: 7 },
                cleanup: Cleanup::PosixLeaderReapedGroupTerminationRequested,
                stdout_bytes: 13,
                stderr_bytes: 29,
                truncated: true,
            },
        },
    )
    .unwrap();
    let bytes = raw(nested);
    assert_eq!(
        ledger::Decoder::new().feed(&bytes).unwrap().record,
        Some(expected.clone())
    );
    assert_eq!(ledger::encode(&expected).unwrap(), bytes);
}
#[test]
fn strict_nested_json_and_invalid_record_values() {
    let base = String::from_utf8(
        fixture("ledger-operation_finished-00")
            [4..fixture("ledger-operation_finished-00").len() - 32]
            .to_vec(),
    )
    .unwrap();
    for replacement in [
        "\"code\":255,\"code\":1",
        "\"code\":null",
        "\"code\":256",
        "\"code\":1e0",
        "\"code\":-0",
        "\"code\":255,\"unknown\":0",
    ] {
        let err = if replacement.contains("\"code\":255,\"code\"") {
            LedgerError::InvalidJson
        } else {
            LedgerError::InvalidRecord
        };
        reject(
            raw(base.replace("\"code\":255", replacement).as_bytes()),
            err,
        );
    }
    let deep = format!("{}0{}", "[".repeat(65), "]".repeat(65));
    reject(raw(deep.as_bytes()), LedgerError::InvalidJson);
}
