use theseus_native_protocol::{Role, SessionId, v2::*};

#[test]
fn scalar_boundaries_are_canonical_and_bounded() {
    assert_eq!(
        IntentId::parse(&"00".repeat(16)).unwrap(),
        IntentId::from_bytes([0; 16])
    );
    assert!(BindingId::parse(&"AA".repeat(16)).is_err());
    assert!(Digest::parse(&"a".repeat(63)).is_err());
    assert_eq!(Digest::from_bytes([255; 32]).to_hex(), "ff".repeat(32));
    for value in ["", "A", "1a", "a/b", "é", &"a".repeat(33)] {
        assert!(ProgramId::new(value).is_err(), "{value}");
    }
    assert!(ProgramId::new(&"a".repeat(32)).is_ok());
    for value in [
        "",
        "CON",
        "con.txt",
        "PrN.a",
        "AUX",
        "NUL",
        "COM1",
        "lPt9.x",
        "a.",
        "a/b",
        "a\\b",
        "a:b",
        "é",
        &"a".repeat(65),
    ] {
        assert!(LeafName::new(value).is_err(), "{value}");
    }
    assert!(LeafName::new(&"a".repeat(64)).is_ok());
    assert!(LeafName::new("COM0.bin").is_ok());
    assert!(SidText::new(&format!("S-1-{}", "x".repeat(252))).is_ok());
    assert!(SidText::new(&format!("S-1-{}", "x".repeat(253))).is_err());
    assert!(SidText::new("S-1-x").is_ok()); // lexical metadata, not a native SID validator
    assert!(SidText::new("S-1-☃").is_err());
    assert_eq!(FileBytes::new(&[0; 4096]).unwrap().as_slice().len(), 4096);
    assert!(FileBytes::new(&[0; 4097]).is_err());
    assert!(ProgramList::new(vec![]).is_err());
    assert!(ProgramList::new(vec![ProgramId::new("a").unwrap(); 2]).is_err());
    assert!(
        ProgramList::new(vec![
            ProgramId::new("b").unwrap(),
            ProgramId::new("a").unwrap()
        ])
        .is_err()
    );
    assert!(
        ProgramList::new(
            (0..9)
                .map(|i| ProgramId::new(&format!("p{i}")).unwrap())
                .collect()
        )
        .is_err()
    );
    assert_eq!(
        ProgramList::new(
            (0..8)
                .map(|i| ProgramId::new(&format!("p{i}")).unwrap())
                .collect()
        )
        .unwrap()
        .as_slice()
        .len(),
        8
    );
}
fn env(message: Message) -> Result<Envelope, Error> {
    Envelope::new(SessionId::from_bytes([0; 16]), u64::MAX, message)
}
fn terminal_message(outcome: Outcome) -> Message {
    Message::Response {
        reply_to: 1,
        result: Reply::Operation {
            lease: LeaseId::from_bytes([0; 16]),
            operation: OperationId::from_bytes([0; 16]),
            state: OperationView::Finished { outcome },
        },
    }
}
#[test]
fn envelope_checks_local_constraints_without_session_policy() {
    assert!(env(Message::Hello { role: Role::Helper }).is_ok());
    let lease = LeaseId::from_bytes([0; 16]);
    let operation = OperationId::from_bytes([0; 16]);
    assert!(
        env(Message::Output {
            lease,
            operation,
            channel: Channel::Stdout,
            offset: 0,
            bytes: FileBytes::new(&[]).unwrap()
        })
        .is_err()
    );
    assert!(
        env(Message::Output {
            lease,
            operation,
            channel: Channel::Stdout,
            offset: u64::MAX,
            bytes: FileBytes::new(&[1]).unwrap()
        })
        .is_err()
    );
    for length in [0, 4097] {
        assert!(
            env(Message::Request {
                request: Request::ReadFile {
                    lease,
                    path: LeafName::new("a").unwrap(),
                    offset: 0,
                    length
                }
            })
            .is_err()
        );
    }
    assert!(
        env(Message::Request {
            request: Request::ReadFile {
                lease,
                path: LeafName::new("a").unwrap(),
                offset: u64::MAX,
                length: 1
            }
        })
        .is_err()
    );
    assert!(
        env(terminal_message(Outcome::FileCreated {
            length: 4097,
            digest: Digest::from_bytes([0; 32])
        }))
        .is_err()
    );
    assert!(
        env(terminal_message(Outcome::RunCompleted {
            reason: RunReason::Natural,
            status: NativeStatus::PosixSignal { signal: 0 },
            cleanup: Cleanup::PosixLeaderReapedGroupTerminationRequested,
            stdout_bytes: 0,
            stderr_bytes: 0,
            truncated: false
        }))
        .is_err()
    );
    assert!(
        env(terminal_message(Outcome::RunCompleted {
            reason: RunReason::Natural,
            status: NativeStatus::WindowsExit { code: u32::MAX },
            cleanup: Cleanup::PosixLeaderReapedGroupTerminationRequested,
            stdout_bytes: 0,
            stderr_bytes: 0,
            truncated: false
        }))
        .is_err()
    );
    assert!(
        env(terminal_message(Outcome::LaunchFailed {
            phase: LaunchPhase::NeverCreated,
            code: FailureCode::Io,
            cleanup: SpawnCleanup::WindowsProcessExitedJobEmpty
        }))
        .is_err()
    );
    assert!(
        env(terminal_message(Outcome::LaunchFailed {
            phase: LaunchPhase::CreatedSuspended,
            code: FailureCode::Io,
            cleanup: SpawnCleanup::PosixLeaderReapedGroupTerminationRequested
        }))
        .is_err()
    );
    assert!(
        env(Message::Response {
            reply_to: 1,
            result: Reply::FileBytes {
                lease,
                path: LeafName::new("a").unwrap(),
                offset: 0,
                bytes: FileBytes::new(&[]).unwrap(),
                eof: false
            }
        })
        .is_err()
    );
    assert!(
        env(Message::Response {
            reply_to: 1,
            result: Reply::Description {
                binding: BindingId::from_bytes([0; 16]),
                manifest_digest: Digest::from_bytes([0; 32]),
                workspace_marker: WorkspaceMarker::from_bytes([0; 16]),
                os: Os::Linux,
                helper_principal: Principal::Windows {
                    token_user_sid: SidText::new("S-1-x").unwrap()
                },
                programs: ProgramList::new(vec![ProgramId::new("a").unwrap()]).unwrap()
            }
        })
        .is_err()
    );
}
#[test]
fn fixed_errors_never_include_peer_input() {
    use std::error::Error as _;
    let marker = "REJECTED_PEER_WITNESS";
    let control = format!("{marker}\n\r\t\0");
    let oversized = marker.repeat(257);
    let errors = [
        ProgramId::new(&control).unwrap_err(),
        ProgramId::new(&oversized).unwrap_err(),
        LeafName::new(&control).unwrap_err(),
        LeafName::new(&oversized).unwrap_err(),
        SidText::new(&format!("S-1-{control}")).unwrap_err(),
        SidText::new(&format!("S-1-{oversized}")).unwrap_err(),
        FileBytes::new(oversized.as_bytes()).unwrap_err(),
    ];
    for error in errors {
        for rendered in [error.to_string(), format!("{error:?}")] {
            assert!(rendered.len() <= 64);
            assert!(
                rendered
                    .bytes()
                    .all(|byte| byte.is_ascii_graphic() || byte == b' ')
            );
            assert!(!rendered.contains(marker));
        }
        assert!(error.source().is_none());
    }
}

#[test]
fn distinct_identity_types_and_getters_round_trip() {
    macro_rules! check {
        ($ty:ident, $n:expr) => {{
            let value = $ty::from_bytes([0xab; $n]);
            assert_eq!($ty::parse(&value.to_hex()).unwrap(), value);
            for text in [
                "a".repeat($n * 2 - 1),
                "a".repeat($n * 2 + 1),
                "A".repeat($n * 2),
                "g".repeat($n * 2),
                "é".repeat($n),
            ] {
                assert_eq!($ty::parse(&text), Err(Error::InvalidValue));
            }
        }};
    }
    check!(BindingId, 16);
    check!(WorkspaceMarker, 16);
    check!(IntentId, 16);
    check!(LeaseId, 16);
    check!(OperationId, 16);
    check!(Digest, 32);
    assert_eq!(
        ProgramId::new("probe_success").unwrap().as_str(),
        "probe_success"
    );
    assert_eq!(LeafName::new("probe.bin").unwrap().as_str(), "probe.bin");
    assert_eq!(SidText::new("S-1-x").unwrap().as_str(), "S-1-x");
    let envelope = env(Message::Hello {
        role: Role::Controller,
    })
    .unwrap();
    assert_eq!(envelope.session(), &SessionId::from_bytes([0; 16]));
    assert_eq!(envelope.sequence(), u64::MAX);
    assert_eq!(
        envelope.message(),
        &Message::Hello {
            role: Role::Controller
        }
    );
}

#[test]
fn all_reserved_leaf_stems_are_rejected_without_overrejecting_neighbors() {
    for prefix in ["COM", "LPT"] {
        for digit in 1..=9 {
            for suffix in ["", ".txt"] {
                assert!(LeafName::new(&format!("{prefix}{digit}{suffix}")).is_err());
            }
        }
    }
    for v in ["CONSOLE", "COM10", "LPT0", "xCON", "a..b", "A_0-z.txt"] {
        assert!(LeafName::new(v).is_ok());
    }
    for v in ["S-1-", "s-1-x", "S-1-X", "S-1-+", "S-1- 0", "S-1-0\0"] {
        assert!(SidText::new(v).is_err());
    }
}

#[test]
fn every_native_status_and_spawn_cleanup_branch_is_locally_checked() {
    let statuses = [
        NativeStatus::PosixExit { code: u8::MAX },
        NativeStatus::PosixSignal { signal: 255 },
        NativeStatus::WindowsExit { code: u32::MAX },
    ];
    for status in statuses {
        for cleanup in [
            Cleanup::PosixLeaderReapedGroupTerminationRequested,
            Cleanup::WindowsProcessExitedJobEmpty,
        ] {
            let valid = matches!(
                (&status, cleanup),
                (
                    NativeStatus::WindowsExit { .. },
                    Cleanup::WindowsProcessExitedJobEmpty
                ) | (
                    NativeStatus::PosixExit { .. } | NativeStatus::PosixSignal { .. },
                    Cleanup::PosixLeaderReapedGroupTerminationRequested
                )
            );
            assert_eq!(
                env(terminal_message(Outcome::RunCompleted {
                    reason: RunReason::Natural,
                    status: status.clone(),
                    cleanup,
                    stdout_bytes: u64::MAX,
                    stderr_bytes: u64::MAX,
                    truncated: true
                }))
                .is_ok(),
                valid
            );
        }
    }
    for signal in [0, 256, u16::MAX] {
        assert!(
            env(terminal_message(Outcome::RunCompleted {
                reason: RunReason::Natural,
                status: NativeStatus::PosixSignal { signal },
                cleanup: Cleanup::PosixLeaderReapedGroupTerminationRequested,
                stdout_bytes: 0,
                stderr_bytes: 0,
                truncated: false
            }))
            .is_err()
        );
    }
    for phase in [
        LaunchPhase::NeverCreated,
        LaunchPhase::CreatedSuspended,
        LaunchPhase::MayRun,
    ] {
        for cleanup in [
            SpawnCleanup::NoChildCreated,
            SpawnCleanup::PosixLeaderReapedGroupTerminationRequested,
            SpawnCleanup::WindowsProcessExitedJobEmpty,
        ] {
            let valid = matches!(
                (phase, cleanup),
                (LaunchPhase::NeverCreated, SpawnCleanup::NoChildCreated)
                    | (
                        LaunchPhase::CreatedSuspended,
                        SpawnCleanup::WindowsProcessExitedJobEmpty
                    )
                    | (
                        LaunchPhase::MayRun,
                        SpawnCleanup::PosixLeaderReapedGroupTerminationRequested
                            | SpawnCleanup::WindowsProcessExitedJobEmpty
                    )
            );
            assert_eq!(
                env(terminal_message(Outcome::LaunchFailed {
                    phase,
                    code: FailureCode::NativeFailure,
                    cleanup
                }))
                .is_ok(),
                valid
            );
        }
    }
}

#[test]
fn size_limits_and_empty_reply_rules_are_checked_recursively() {
    let lease = LeaseId::from_bytes([0; 16]);
    for length in [0, 4096, 4097, u16::MAX] {
        assert_eq!(
            env(terminal_message(Outcome::FileCreated {
                length,
                digest: Digest::from_bytes([0; 32])
            }))
            .is_ok(),
            length <= 4096
        );
        assert_eq!(
            env(Message::Response {
                reply_to: 0,
                result: Reply::FileInfo {
                    lease,
                    path: LeafName::new("a").unwrap(),
                    length,
                    digest: Digest::from_bytes([0; 32])
                }
            })
            .is_ok(),
            length <= 4096
        );
    }
    for eof in [true, false] {
        assert_eq!(
            env(Message::Response {
                reply_to: 0,
                result: Reply::FileBytes {
                    lease,
                    path: LeafName::new("a").unwrap(),
                    offset: u64::MAX,
                    bytes: FileBytes::new(&[]).unwrap(),
                    eof
                }
            })
            .is_ok(),
            eof
        );
    }
    assert!(
        env(Message::Response {
            reply_to: 0,
            result: Reply::FileBytes {
                lease,
                path: LeafName::new("a").unwrap(),
                offset: u64::MAX,
                bytes: FileBytes::new(&[0]).unwrap(),
                eof: true
            }
        })
        .is_err()
    );
    for reason in [
        NotStartedReason::CancelRequested,
        NotStartedReason::Deadline,
    ] {
        assert!(env(terminal_message(Outcome::NotStarted { reason })).is_ok());
    }
    for code in [
        FileCreateFailureCode::AlreadyExists,
        FileCreateFailureCode::Permission,
    ] {
        assert!(env(terminal_message(Outcome::FileCreateFailed { code })).is_ok());
    }
}

#[test]
fn acquired_reply_requires_its_nested_lease_intent_to_match() {
    let intent = IntentId::from_bytes([1; 16]);
    let other = IntentId::from_bytes([2; 16]);
    let make = |state| Message::Response {
        reply_to: 1,
        result: Reply::Acquired {
            intent,
            binding: BindingId::from_bytes([0; 16]),
            manifest_digest: Digest::from_bytes([0; 32]),
            state,
        },
    };
    for nested_intent in [intent, other] {
        for state in [
            LeaseView::Held {
                intent: nested_intent,
            },
            LeaseView::AwaitingExternalReset {
                intent: nested_intent,
            },
            LeaseView::Quarantined {
                intent: nested_intent,
                reason: UnknownReason::LedgerWriteUncertain,
            },
        ] {
            assert_eq!(
                env(make(AcquireView::Lease {
                    lease: LeaseId::from_bytes([0; 16]),
                    state
                }))
                .is_ok(),
                nested_intent == intent
            );
        }
    }
    assert!(env(make(AcquireView::NotFound)).is_ok());
}
