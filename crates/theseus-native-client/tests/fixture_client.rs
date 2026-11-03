//! Actual local native-client qualification. All roots and bounded receipts are
//! retained. The source-owned outer fixture runner supplies the process deadline.
#[path = "support/native_fixture.rs"]
mod support;
use std::time::{Duration, Instant};
use support::*;
use theseus_host::{fs::HostDir, process::NativeCommand};
use theseus_native_client::{Action, Client, ClientError, Event, Poll};
use theseus_native_helper_store::IntentStore;
use theseus_native_protocol::v2::*;

#[test]
fn native_client_preflight_is_exact_and_read_only() {
    for mismatch in 0..5 {
        let fixture = Fixture::new("success");
        let before = fixture.ledger_bytes();
        let mut expected = fixture.expected();
        match mismatch {
            0 => expected.binding.binding = BindingId::from_bytes([99; 16]),
            1 => expected.binding.manifest_digest = Digest::from_bytes([99; 32]),
            2 => expected.binding.workspace_marker = WorkspaceMarker::from_bytes([99; 16]),
            3 => {
                expected.binding.os = if os() == Os::Windows {
                    Os::Linux
                } else {
                    Os::Windows
                }
            }
            4 => {
                expected.programs =
                    ProgramList::new(vec![ProgramId::new("wrong").unwrap()]).unwrap()
            }
            _ => unreachable!(),
        }
        let mut client = match Client::start(fixture.command(None), expected) {
            Ok(client) => client,
            Err(_) => panic!("normal helper spawn must succeed"),
        };
        assert!(client.submit(Action::Acquire(&fixture.prepared)).is_err());
        assert!(
            client
                .submit(Action::Run {
                    lease: LeaseId::from_bytes([1; 16]),
                    operation: OperationId::from_bytes([2; 16]),
                    program: ProgramId::new("probe").unwrap()
                })
                .is_err()
        );
        let deadline = Instant::now() + Duration::from_secs(7);
        loop {
            tick(deadline, "exact preflight refusal");
            match client.poll() {
                Err(ClientError::Preflight) => break,
                Err(other) => panic!("wrong preflight classification {other:?}"),
                Ok(Poll::Event(Event::Ready)) => panic!("wrong expected authority became ready"),
                Ok(_) => {}
            }
        }
        client.request_stop();
        let deadline = Instant::now() + Duration::from_secs(12);
        while !client.poll_settle().unwrap_or(false) {
            tick(deadline, "rejected helper actual join");
        }
        assert!(client.is_settled());
        assert_eq!(
            fixture.ledger_bytes(),
            before,
            "rejected preflight cannot reserve or run"
        );
        assert_eq!(fixture.records().len(), 1);
        assert!(!fixture.receipt.exists());
    }
}

#[test]
fn native_client_requires_committed_intent_and_recovers_same_lease() {
    let fixture = Fixture::new("success");
    // The actual caller persists the canonical intent and observes its platform
    // publication barrier before constructing or starting the client transport.
    let persisted = bounded_file(
        &fixture.root.path().join("intents").join(format!(
            "{}.json",
            fixture.prepared.record().intent.to_hex()
        )),
        512,
    );
    let persisted: serde_json::Value = serde_json::from_slice(&persisted).unwrap();
    assert_eq!(
        persisted["intent"],
        fixture.prepared.record().intent.to_hex()
    );
    assert_eq!(persisted["workspace_marker"], fixture.marker.to_hex());
    #[cfg(unix)]
    assert_eq!(
        fixture.prepared.commit_scope(),
        theseus_host::fs::CommitScope::FileAndDirectorySynced
    );
    #[cfg(windows)]
    assert_eq!(
        fixture.prepared.commit_scope(),
        theseus_host::fs::CommitScope::FileFlushedAtomicReplace
    );
    let mut client = fixture.start(None);
    // Failed prepare produces no PreparedAcquire, hence there is no Action that
    // can send this uncommitted identity. Invalid-existing is a real disk error;
    // publication-barrier injection remains a store/core unit-test concern.
    let rejected = HostDir::create_private(&fixture.root.path().join("rejected-intents")).unwrap();
    let record = fixture.prepared.record().clone();
    std::fs::write(
        fixture
            .root
            .path()
            .join("rejected-intents")
            .join(format!("{}.json", record.intent.to_hex())),
        b"uncommitted",
    )
    .unwrap();
    assert!(IntentStore::new(rejected).prepare(record).is_err());
    assert!(!fixture.receipt.exists());
    let request = client
        .client
        .submit(Action::Acquire(&fixture.prepared))
        .unwrap();
    // Discard the application reply, then lose this connection; recover only
    // the same publication-backed intent through QueryAcquire on reconnect.
    let lost_lease = match client.response(request) {
        Reply::Acquired {
            state: AcquireView::Lease { lease, .. },
            ..
        } => lease,
        other => panic!("acquire reply {other:?}"),
    };
    client.settle();
    let first = fixture.records();
    assert_eq!(
        first
            .iter()
            .filter(|e| matches!(e, ledger::Entry::LeaseReserved { .. }))
            .count(),
        1
    );
    client.reconnect(fixture.command(None));
    assert!(
        matches!(client.request(Action::RecoverAcquire(&fixture.prepared)),Reply::Acquired { state: AcquireView::Lease { lease,.. },.. } if lease == lost_lease)
    );
    assert_eq!(
        client.client.submit(Action::Acquire(&fixture.prepared)),
        Err(ClientError::RecoveryRequired)
    );
    assert!(
        matches!(client.request(Action::RecoverAcquire(&fixture.prepared)),Reply::Acquired { state: AcquireView::Lease { lease,.. },.. } if lease == lost_lease)
    );
    client.settle();
    assert_eq!(
        fixture
            .records()
            .iter()
            .filter(|e| matches!(e, ledger::Entry::LeaseReserved { .. }))
            .count(),
        1
    );
    assert!(!fixture.receipt.exists());
}

#[test]
fn native_client_run_and_query_preserve_native_outcome() {
    let fixture = Fixture::new("wait");
    let mut client = fixture.start(None);
    let lease = client.acquire(&fixture);
    let operation = OperationId::from_bytes([4; 16]);
    client.run(lease, operation);
    fixture.wait_receipt();
    fixture.assert_probe_receipt();
    std::fs::write(&fixture.release, fixture.nonce.as_bytes()).unwrap();
    let outcome = client.terminal(lease, operation);
    assert!(matches!(
        outcome,
        Outcome::RunCompleted {
            reason: RunReason::Natural,
            ..
        }
    ));
    assert_eq!(client.terminal(lease, operation), outcome);
    client.settle();
    assert!(fixture.records().iter().any(|e| matches!(e,ledger::Entry::OperationFinished { operation: op,outcome: found,.. } if *op==operation && *found==outcome)));
    // A fresh native connection must keep already observed/durable Finished.
    client.reconnect(fixture.command(None));
    assert_eq!(client.terminal(lease, operation), outcome);
    assert!(matches!(
        client.request(Action::Release(lease)),
        Reply::Lease {
            state: LeaseView::AwaitingExternalReset { .. },
            ..
        }
    ));
    client.settle();
    assert!(fixture.records().iter().any(|e| matches!(
        e,
        ledger::Entry::LeaseClosed {
            disposition: ledger::Disposition::AwaitingExternalReset,
            ..
        }
    )));
}

#[test]
fn native_client_cancel_remains_responsive_under_output_pressure() {
    let fixture = Fixture::new("flood_wait");
    let mut client = fixture.start(Some("pressure"));
    let lease = client.acquire(&fixture);
    let operation = OperationId::from_bytes([4; 16]);
    client.run(lease, operation);
    // Stop all client reads after Accepted. The feature helper's real published
    // witness proves padded output pressure before Cancel admission. On Windows
    // write_pending observes API pending state, not a kernel pipe quota guarantee.
    fixture.wait_receipt();
    fixture.assert_probe_receipt();
    let witness = fixture.wait_witness(&["capture_saturated", "write_pending"]);
    assert_eq!(
        witness["profile"],
        serde_json::json!({"name":"output-pressure","raw_chunk_cap":512,"publication_cap":128,"output_body_bytes":65532,"output_frame_bytes":65536,"raw_lifetime_cap":16384,"data_permits":4})
    );
    assert!(
        (1..MAX_STREAM_BYTES as u64).contains(&witness["saturation_wire_bytes"].as_u64().unwrap())
    );
    assert!((4..=128).contains(&witness["saturation_published_frames"].as_u64().unwrap()));
    let query = client
        .client
        .submit(Action::Query { lease, operation })
        .unwrap();
    assert!(
        client.client.submit(Action::QueryLease(lease)).is_err(),
        "ordinary slot occupied"
    );
    let cancel = client
        .client
        .submit(Action::Cancel { lease, operation })
        .expect("reserved Cancel remains admissible");
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut seen_query = false;
    let mut seen_cancel = false;
    while !(seen_query && seen_cancel) {
        if let Some(Event::Reply { request, reply }) = client.next(deadline) {
            assert!(matches!(reply, Reply::Operation { .. }));
            if request == query {
                seen_query = true;
            } else {
                assert_eq!(request, cancel);
                seen_cancel = true;
            }
        }
    }
    let terminal = client.terminal(lease, operation);
    assert!(
        matches!(terminal,Outcome::RunCompleted { reason: RunReason::CancelRequested, stdout_bytes, stderr_bytes, truncated:true,.. } if stdout_bytes>0 && stderr_bytes>0 && stdout_bytes+stderr_bytes>MAX_STREAM_BYTES as u64)
    );
    assert!(
        client
            .output
            .iter()
            .map(|(_, _, bytes)| bytes.len())
            .sum::<usize>()
            <= MAX_STREAM_BYTES
    );
    client.settle();
    assert!(
        fixture.records().iter().any(
            |e| matches!(e,ledger::Entry::OperationFinished {outcome,..} if *outcome==terminal)
        )
    );
}

#[test]
fn native_client_connection_loss_never_reexecutes_run() {
    let fixture = Fixture::new("wait");
    let mut client = fixture.start(Some("panic-after-owned-spawn"));
    let lease = client.acquire(&fixture);
    let operation = OperationId::from_bytes([5; 16]);
    // This is an extant-process connection recovery test. It makes no claim
    // about recovering a controller process crash or persisting Run in Client.
    let request = client
        .client
        .submit(Action::Run {
            lease,
            operation,
            program: ProgramId::new("probe").unwrap(),
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        tick(deadline, "unexpected helper connection loss");
        match client.client.poll() {
            Ok(Poll::Event(Event::ConnectionLost | Event::PeerClosed(_)))
            | Err(ClientError::Native) => break,
            Err(other) => panic!("unexpected connection failure classification {other:?}"),
            Ok(Poll::Event(Event::Reply { request: found, .. })) => assert_eq!(found, request),
            Ok(_) => {}
        }
    }
    fixture.wait_witness(&["spawn_owned", "panic_injected"]);
    client.settle();
    assert_eq!(
        fixture
            .records()
            .iter()
            .filter(|e| matches!(e, ledger::Entry::OperationAccepted { .. }))
            .count(),
        1
    );
    client.reconnect(fixture.command(None));
    // Caller supplies the original IDs explicitly. Never resubmit Run.
    let recovered = client.request(Action::Query { lease, operation });
    assert!(matches!(
        recovered,
        Reply::Operation {
            state: OperationView::Finished { .. } | OperationView::Unknown { .. },
            ..
        }
    ));
    client.settle();
    assert_eq!(
        fixture
            .records()
            .iter()
            .filter(|e| matches!(e, ledger::Entry::OperationAccepted { .. }))
            .count(),
        1
    );
}

#[test]
fn native_client_reconnect_ids_and_partial_failure_owners_are_safe() {
    let fixture = Fixture::new("success");
    let mut client = fixture.start(None);
    let first = client
        .client
        .submit(Action::Acquire(&fixture.prepared))
        .unwrap();
    let lease = match client.response(first) {
        Reply::Acquired {
            state: AcquireView::Lease { lease, .. },
            ..
        } => lease,
        other => panic!("acquire {other:?}"),
    };
    client.settle();
    client.reconnect(fixture.command(None));
    let second = client.client.submit(Action::QueryLease(lease)).unwrap();
    assert_ne!(first, second);
    assert!(matches!(client.response(second), Reply::Lease { .. }));
    client.settle();
    let mut missing = NativeCommand::new(fixture.root.path().join("missing-native-helper"));
    missing.environment = theseus_host::process::ChildEnvironment::Replace(vec![]);
    assert!(client.client.reconnect(missing).is_err());
    client.joined_again_after_failed_attempt();
    // A real helper attempt then stalls before readiness. Timeout leaves its
    // original helper/worker owned inside Client until explicit actual join.
    client
        .client
        .reconnect(fixture.command(Some("hold-init")))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(7);
    loop {
        tick(deadline, "retained failed reconnect preflight");
        match client.client.poll() {
            Err(ClientError::PreflightTimeout) => break,
            Err(other) => panic!("held init error {other:?}"),
            Ok(_) => {}
        }
    }
    fixture.wait_witness(&["init_reached"]);
    assert!(
        !client.client.is_settled(),
        "failed live attempt is not an actual join"
    );
    assert!(matches!(
        client.client.reconnect(fixture.command(None)),
        Err(ClientError::NotSettled | ClientError::Stopping)
    ));
    client.joined_again_after_failed_attempt();
    client.reconnect(fixture.command(None));
    let third = client
        .client
        .submit(Action::RecoverAcquire(&fixture.prepared))
        .unwrap();
    assert_ne!(first, third);
    assert_ne!(second, third);
    assert!(
        matches!(client.response(third),Reply::Acquired {state:AcquireView::Lease {lease: found,..},..} if found==lease)
    );
    client.settle();
    assert_eq!(
        fixture
            .records()
            .iter()
            .filter(|e| matches!(e, ledger::Entry::LeaseReserved { .. }))
            .count(),
        1
    );
}

#[test]
fn native_client_transport_join_does_not_claim_remote_cleanup() {
    let fixture = Fixture::new("success");
    let mut client = fixture.start(Some("hold-before-spawn"));
    let lease = client.acquire(&fixture);
    let operation = OperationId::from_bytes([6; 16]);
    client.run(lease, operation);
    let witness = fixture.wait_witness(&["accepted_committed", "before_spawn_reached"]);
    assert_eq!(witness["spawn_attempted"], false);
    assert_eq!(witness["spawn_owned"], false);
    client.settle(); // helper exits incomplete; local join is still a local fact
    assert!(!fixture.receipt.exists());
    assert!(
        !fixture
            .witness_path()
            .with_extension("owner-dropped")
            .exists()
    );
    let entries = fixture.records();
    assert!(entries.iter().any(
        |e| matches!(e,ledger::Entry::OperationAccepted {operation: op,..} if *op==operation)
    ));
    assert!(
        !entries.iter().any(|e| matches!(
            e,
            ledger::Entry::OperationFinished { .. }
                | ledger::Entry::LeaseClosed {
                    disposition: ledger::Disposition::AwaitingExternalReset,
                    ..
                }
        )),
        "local transport join cannot mint remote finish/reset receipt"
    );
    client.reconnect(fixture.command(None));
    assert!(matches!(
        client.request(Action::Query { lease, operation }),
        Reply::Operation {
            state: OperationView::Unknown { .. },
            ..
        }
    ));
    client.settle();
    fixture.records();
}

#[cfg(windows)]
#[test]
fn native_client_windows_unsigned_status_is_preserved() {
    let fixture = Fixture::new("high_bit");
    let mut client = fixture.start(None);
    let lease = client.acquire(&fixture);
    let operation = OperationId::from_bytes([7; 16]);
    client.run(lease, operation);
    let outcome = client.terminal(lease, operation);
    assert!(matches!(
        outcome,
        Outcome::RunCompleted {
            reason: RunReason::Natural,
            status: NativeStatus::WindowsExit { code: 2147483653 },
            ..
        }
    ));
    fixture.assert_probe_receipt();
    client.settle();
    assert!(fixture.records().iter().any(
        |e| matches!(e,ledger::Entry::OperationFinished {outcome:found,..} if *found==outcome)
    ));
}

#[test]
fn native_client_file_roundtrip_empty_bounded_ranges_and_run() {
    use sha2::{Digest as _, Sha256};
    // Opt-in is fixture-local and binds the explicitly pinned new normal helper.
    // Description's program list is not a file capability advertisement.
    let fixture = Fixture::new("success");
    let mut client = fixture.start(None);
    let lease = client.acquire(&fixture);
    for (index, bytes) in [Vec::new(), vec![b'x'; MAX_FILE_BYTES]]
        .into_iter()
        .enumerate()
    {
        let operation = OperationId::from_bytes([20 + index as u8; 16]);
        let path = LeafName::new(&format!("file-{index}")).unwrap();
        assert!(matches!(
            client.request(Action::CreateFile {
                lease,
                operation,
                path: path.clone(),
                bytes: FileBytes::new(&bytes).unwrap()
            }),
            Reply::Operation {
                state: OperationView::Accepted {
                    operation_kind: OperationKind::CreateFile
                },
                ..
            }
        ));
        let digest = Digest::from_bytes(Sha256::digest(&bytes).into());
        assert_eq!(
            client.terminal(lease, operation),
            Outcome::FileCreated {
                length: bytes.len() as u16,
                digest
            }
        );
        assert!(client.output.is_empty(), "CreateFile has no Run stream");
        assert_eq!(
            client.request(Action::InspectFile {
                lease,
                path: path.clone()
            }),
            Reply::FileInfo {
                lease,
                path: path.clone(),
                length: bytes.len() as u16,
                digest
            }
        );
        for (offset, length) in [(0, 1), (4095, 2), (u64::MAX - 1, 1)] {
            let begin = offset.min(bytes.len() as u64) as usize;
            let end = (begin + usize::from(length)).min(bytes.len());
            assert_eq!(
                client.request(Action::ReadFile {
                    lease,
                    path: path.clone(),
                    offset,
                    length
                }),
                Reply::FileBytes {
                    lease,
                    path: path.clone(),
                    offset,
                    bytes: FileBytes::new(&bytes[begin..end]).unwrap(),
                    eof: end == bytes.len()
                }
            );
        }
        assert_eq!(
            std::fs::read(
                fixture
                    .root
                    .path()
                    .join("workspace")
                    .join(format!("file-{index}"))
            )
            .unwrap(),
            bytes
        );
    }
    let operation = OperationId::from_bytes([25; 16]);
    client.run(lease, operation);
    assert!(matches!(
        client.terminal(lease, operation),
        Outcome::RunCompleted { .. }
    ));
    client.settle();
    assert_eq!(
        fixture
            .records()
            .iter()
            .filter(|e| matches!(
                e,
                ledger::Entry::OperationAccepted {
                    operation_kind: OperationKind::CreateFile,
                    ..
                }
            ))
            .count(),
        2
    );
}

#[test]
fn native_client_file_connection_loss_queries_original_without_second_create() {
    let fixture = Fixture::new("success");
    let mut client = fixture.start(None);
    let lease = client.acquire(&fixture);
    let operation = OperationId::from_bytes([30; 16]);
    let path = LeafName::new("recover-file").unwrap();
    // Recovery here retains this live Client. A controller-crash caller must
    // durably persist this FULL typed request plus request_digest before submit.
    let full = Request::CreateFile {
        lease,
        operation,
        path: path.clone(),
        bytes: FileBytes::new(b"retained").unwrap(),
    };
    let _identity_digest = request_digest(&full).unwrap();
    assert!(matches!(
        client.request(Action::CreateFile {
            lease,
            operation,
            path: path.clone(),
            bytes: FileBytes::new(b"retained").unwrap()
        }),
        Reply::Operation {
            state: OperationView::Accepted {
                operation_kind: OperationKind::CreateFile
            },
            ..
        }
    ));
    // Lose the application terminal reply; no automatic mutation follows join.
    client.settle();
    client.reconnect(fixture.command(None));
    assert_eq!(
        client.client.submit(Action::CreateFile {
            lease,
            operation,
            path,
            bytes: FileBytes::new(b"retained").unwrap()
        }),
        Err(ClientError::RecoveryRequired)
    );
    let reply = client.request(Action::Query { lease, operation });
    assert!(matches!(
        reply,
        Reply::Operation {
            state: OperationView::Finished {
                outcome: Outcome::FileCreated { .. }
            } | OperationView::Unknown { .. },
            ..
        }
    ));
    if matches!(
        reply,
        Reply::Operation {
            state: OperationView::Unknown { .. },
            ..
        }
    ) {
        assert_eq!(
            client.client.submit(Action::CreateFile {
                lease,
                operation: OperationId::from_bytes([31; 16]),
                path: LeafName::new("other").unwrap(),
                bytes: FileBytes::new(&[]).unwrap()
            }),
            Err(ClientError::RecoveryRequired)
        );
    }
    client.settle();
    assert_eq!(
        fixture
            .records()
            .iter()
            .filter(|e| matches!(e, ledger::Entry::OperationAccepted { .. }))
            .count(),
        1
    );
}
