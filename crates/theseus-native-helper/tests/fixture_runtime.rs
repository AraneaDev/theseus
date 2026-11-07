mod support;
use support::*;
use theseus_native_protocol::v2::*;

#[test]
fn native_child_status_and_probe_principal_are_observed_truthfully() {
    let fixture = Fixture::new("success");
    let mut peer = fixture.start();
    let lease = peer.acquire(&fixture);
    let operation = OperationId::from_bytes([4; 16]);
    peer.run(lease, operation);
    let outcome = peer.terminal(lease, operation);
    fixture.assert_probe_receipt();
    assert!(matches!(
        outcome,
        Outcome::RunCompleted {
            reason: RunReason::Natural,
            status: NativeStatus::PosixExit { code: 23 } | NativeStatus::WindowsExit { code: 23 },
            ..
        }
    ));
    peer.close();
}

#[test]
fn explicit_config_and_wrong_lease_file_requests_never_create_authority() {
    prove_postjoin_ledger_rejects_corruption();
    let fixture = Fixture::new("success");
    let mut before = fixture.ledger_bytes();
    let mut peer = fixture.start();
    let lease = peer.acquire(&fixture);
    before.extend_from_slice(
        &ledger::encode(
            &ledger::Record::new(
                1,
                ledger::Entry::LeaseReserved {
                    intent: fixture.prepared.record().intent,
                    lease,
                },
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let operation = OperationId::from_bytes([5; 16]);
    let wrong = LeaseId::from_bytes([99; 16]);
    for request in [
        Request::CreateFile {
            lease: wrong,
            operation,
            path: LeafName::new("denied").unwrap(),
            bytes: FileBytes::new(b"denied").unwrap(),
        },
        Request::ReadFile {
            lease: wrong,
            path: LeafName::new("denied").unwrap(),
            offset: 0,
            length: 1,
        },
        Request::InspectFile {
            lease: wrong,
            path: LeafName::new("denied").unwrap(),
        },
    ] {
        assert_eq!(
            peer.request(request),
            Reply::Refused {
                code: RefusalCode::WrongLease,
                inspect_failure: None
            }
        );
    }
    assert!(!fixture.root.path().join("workspace/denied").exists());
    assert!(!fixture.receipt.exists());
    peer.close();
    assert_eq!(before, fixture.ledger_bytes());
    let mut data: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&fixture.config).unwrap()).unwrap();
    data["manifest_digest"] = serde_json::Value::String("00".repeat(32));
    std::fs::write(&fixture.config, serde_json::to_vec(&data).unwrap()).unwrap();
    let result = invalid_config(&fixture.config);
    assert_eq!(result.0.code(), Some(2));
    assert!(result.1.is_empty());
    assert_eq!(result.2, b"helper: invalid invocation\n");
    assert_eq!(before, fixture.ledger_bytes());
    assert!(!fixture.receipt.exists());
}

#[test]
fn ordinary_pipes_prove_commit_before_ack_spawn_and_finished() {
    prove_physical_commit_boundaries();
    let fixture = Fixture::new("wait");
    let mut peer = fixture.start();
    let lease = peer.acquire(&fixture);
    let operation = OperationId::from_bytes([4; 16]);
    peer.run(lease, operation);
    fixture.wait_receipt();
    fixture.assert_probe_receipt();
    std::fs::write(&fixture.release, fixture.nonce.as_bytes()).unwrap();
    let outcome = peer.terminal(lease, operation);
    assert!(matches!(
        peer.request(Request::Release { lease }),
        Reply::Lease {
            state: LeaseView::AwaitingExternalReset { .. },
            ..
        }
    ));
    assert!(matches!(
        peer.request(Request::QueryLease { lease }),
        Reply::Lease {
            state: LeaseView::AwaitingExternalReset { .. },
            ..
        }
    ));
    assert!(matches!(
        peer.request(fixture.prepared.request()),
        Reply::Acquired {
            state: AcquireView::Lease {
                state: LeaseView::AwaitingExternalReset { .. },
                ..
            },
            ..
        }
    ));
    peer.close();
    let records = fixture.records();
    assert!(
        records
            .iter()
            .any(|e| matches!(e,ledger::Entry::LeaseReserved{lease:found,..}if *found==lease))
    );
    assert!(
        records.iter().any(
            |e| matches!(e,ledger::Entry::OperationAccepted{operation:op,..}if *op==operation)
        )
    );
    assert!(records.iter().any(|e|matches!(e,ledger::Entry::OperationFinished{operation:op,outcome:found,..}if *op==operation&&*found==outcome)));
}

#[test]
fn lost_acquire_reply_reuses_committed_intent_and_lease() {
    let fixture = Fixture::new("success");
    let mut peer = fixture.start();
    let sequence = peer.send(fixture.prepared.request());
    // Simulate loss between transport receipt and application observation. The
    // controller deliberately discards Acquired and recovers only its original intent.
    let _lost = peer.response(sequence);
    peer.close();
    let lease = fixture
        .records()
        .into_iter()
        .find_map(|entry| match entry {
            ledger::Entry::LeaseReserved { lease, .. } => Some(lease),
            _ => None,
        })
        .unwrap();
    let mut replay = fixture.start();
    let result = replay.request(Request::QueryAcquire {
        intent: fixture.prepared.record().intent,
        binding: fixture.binding,
        manifest_digest: fixture.digest,
    });
    assert!(
        matches!(result,Reply::Acquired{state:AcquireView::Lease{lease:found,..},..} if found==lease)
    );
    assert_eq!(replay.acquire(&fixture), lease);
    assert!(!fixture.receipt.exists());
    replay.close();
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
fn finished_is_durable_before_optional_output_drain() {
    prove_finished_before_drain();
    let fixture = Fixture::new("flood");
    let mut peer = fixture.start();
    let lease = peer.acquire(&fixture);
    let operation = OperationId::from_bytes([4; 16]);
    peer.run(lease, operation);
    // No reader request occurs here. Native stdout is an ordinary unread pipe.
    fixture.wait_receipt();
    fixture.assert_probe_receipt();
    let wire = peer.terminal(lease, operation);
    let outcome = wire.clone();
    assert!(
        matches!(wire,Outcome::RunCompleted{reason:RunReason::Natural,stdout_bytes,stderr_bytes,truncated:true,..} if stdout_bytes>0&&stderr_bytes>0&&stdout_bytes+stderr_bytes>MAX_STREAM_BYTES as u64&&stdout_bytes<=4194304&&stderr_bytes<=4194304),
        "actual terminal: {wire:?}"
    );
    assert!(
        peer.output
            .iter()
            .map(|(_, _, bytes)| bytes.len())
            .sum::<usize>()
            <= MAX_STREAM_BYTES
    );
    peer.close();
    assert!(fixture.records().iter().any(
        |e| matches!(e,ledger::Entry::OperationFinished{outcome:found,..} if *found==outcome)
    ));
}

#[test]
fn cached_cancel_remains_responsive_under_saturated_output() {
    prove_cached_cancel_with_worker_gate();
    prove_cancel_before_drain();
    let fixture = Fixture::new("flood_wait");
    let mut peer = fixture.start();
    let lease = peer.acquire(&fixture);
    let operation = OperationId::from_bytes([4; 16]);
    peer.run(lease, operation);
    fixture.wait_receipt();
    // Probe publishes receipt only after writing both 4 MiB streams; controller
    // has consumed no Output. These writes exceed the entire 16 KiB wire budget.
    let cancel = peer.send(Request::Cancel { lease, operation });
    let reply = peer.response(cancel);
    assert!(matches!(
        reply,
        Reply::Operation {
            state: OperationView::Accepted { .. }
                | OperationView::Running
                | OperationView::Finished { .. },
            ..
        }
    ));
    let terminal = peer.terminal(lease, operation);
    assert!(matches!(
        terminal,
        Outcome::RunCompleted {
            reason: RunReason::CancelRequested,
            ..
        }
    ));
    fixture.assert_probe_receipt();
    assert!(
        peer.output
            .iter()
            .map(|(_, _, bytes)| bytes.len())
            .sum::<usize>()
            <= MAX_STREAM_BYTES
    );
    peer.close();
}

#[cfg(windows)]
#[test]
fn windows_unsigned_high_bit_exit_status_is_preserved() {
    let direct = Fixture::new("high_bit");
    let raw = direct_probe_status(&direct);
    assert_eq!(raw.code().unwrap() as u32, 2147483653);
    direct.assert_probe_receipt();
    let fixture = Fixture::new("high_bit");
    let mut peer = fixture.start();
    let lease = peer.acquire(&fixture);
    let operation = OperationId::from_bytes([4; 16]);
    peer.run(lease, operation);
    assert!(matches!(
        peer.terminal(lease, operation),
        Outcome::RunCompleted {
            reason: RunReason::Natural,
            status: NativeStatus::WindowsExit { code: 2147483653 },
            ..
        }
    ));
    fixture.assert_probe_receipt();
    peer.close();
}

#[test]
fn fixture_faults_are_absent_from_normal_helper_graph() {
    let invocation = Fixture::new("success");
    let mut ordinary = invocation.start();
    let lease = ordinary.acquire(&invocation);
    let operation = OperationId::from_bytes([4; 16]);
    ordinary.run(lease, operation);
    assert!(matches!(
        ordinary.terminal(lease, operation),
        Outcome::RunCompleted {
            reason: RunReason::Natural,
            ..
        }
    ));
    invocation.assert_probe_receipt();
    ordinary.close();
    let normal = pinned("THESEUS_NATIVE_HELPER_BINARY");
    let fixture = pinned("THESEUS_NATIVE_HELPER_FIXTURE");
    assert_ne!(normal, fixture);
    let normal_bytes = std::fs::read(&normal).unwrap();
    let fixture_bytes = std::fs::read(&fixture).unwrap();
    let marker = b"theseus-native-helper-fixture-only-v1";
    assert!(!normal_bytes.windows(marker.len()).any(|v| v == marker));
    assert!(fixture_bytes.windows(marker.len()).any(|v| v == marker));
    let metadata = artifact_metadata(&normal);
    for (key, path) in [("helper_production", &normal), ("helper_fixture", &fixture)] {
        assert_eq!(metadata[key]["path"], path.to_str().unwrap());
        use sha2::{Digest as _, Sha256};
        assert_eq!(
            metadata[key]["sha256"],
            hex(&Sha256::digest(std::fs::read(path).unwrap()))
        );
    }
    let production = metadata["helper_production_graph"]
        .as_array()
        .expect("normal graph required");
    for target in [
        "theseus_native_helper",
        "theseus_host",
        "theseus-native-helper",
    ] {
        let artifacts = production
            .iter()
            .filter(|a| a["target"]["name"] == target)
            .collect::<Vec<_>>();
        assert!(
            !artifacts.is_empty(),
            "missing normal graph target {target}"
        );
        for artifact in artifacts {
            assert!(
                !artifact["features"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|f| f == "test-support")
            );
        }
    }
    assert!(
        !production
            .iter()
            .any(|a| a["target"]["name"] == "theseus-native-helper-fixture")
    );
    let fixtures = metadata["helper_fixture_graph"]
        .as_array()
        .expect("fixture graph required");
    for target in ["theseus_native_helper", "theseus-native-helper-fixture"] {
        assert!(
            fixtures.iter().any(|a| a["target"]["name"] == target
                && a["features"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|f| f == "test-support")),
            "missing feature fixture target {target}"
        );
    }
}

#[test]
fn eof_and_unresolved_cleanup_keep_owners_until_join_or_exit_two() {
    let fixture = Fixture::new("wait");
    let mut peer = fixture.start();
    let lease = peer.acquire(&fixture);
    let operation = OperationId::from_bytes([4; 16]);
    peer.run(lease, operation);
    fixture.wait_receipt();
    fixture.assert_probe_receipt();
    // EOF is delivered with an actual live single-process probe. close retains
    // the helper's original Child through a bounded observed join.
    peer.close();
    let outcome = fixture
        .records()
        .into_iter()
        .find_map(|entry| match entry {
            ledger::Entry::OperationFinished {
                operation: op,
                outcome,
                ..
            } if op == operation => Some(outcome),
            _ => None,
        })
        .expect("EOF cleanup must commit actual native completion");
    assert!(matches!(
        outcome,
        Outcome::RunCompleted {
            reason: RunReason::CancelRequested,
            ..
        }
    ));
    let panicking = Fixture::new("wait");
    let mut panic_peer = panicking.start_feature("panic-after-owned-spawn", true);
    let lease = panic_peer.acquire(&panicking);
    let operation = OperationId::from_bytes([4; 16]);
    panic_peer.send(Request::Run {
        lease,
        operation,
        program: ProgramId::new("probe").unwrap(),
    });
    panicking.wait_witness(&["spawn_owned", "panic_injected"]);
    panic_peer.close_expected(2, b"helper: runtime failed\n");
    assert!(
        panicking
            .witness_path()
            .with_extension("owner-dropped")
            .exists(),
        "injected panic must settle native owners before normal scope return"
    );

    let unresolved = Fixture::new("success");
    let held = unresolved.start_feature("hold-init", false);
    let witness = unresolved.wait_witness(&["init_reached"]);
    assert_eq!(witness["spawn_attempted"], false);
    assert_eq!(witness["spawn_owned"], false);
    assert!(!unresolved.receipt.exists());
    held.close_expected(2, b"helper: cleanup incomplete\n");
    assert!(!unresolved.receipt.exists());
    assert!(
        !unresolved
            .witness_path()
            .with_extension("owner-dropped")
            .exists(),
        "whole Runtime owner must remain live until forced exit"
    );
    assert_eq!(
        unresolved.records().len(),
        1,
        "no-spawn fixture retains only bootstrap Header"
    );
}

fn prove_physical_commit_boundaries() {
    let fixture = Fixture::new("wait");
    let mut peer = fixture.start_feature("hold-before-spawn", true);
    let sequence = peer.send(fixture.prepared.request());
    fixture.wait_witness(&["acquire_committed"]);
    let lease = match peer.response(sequence) {
        Reply::Acquired {
            state: AcquireView::Lease { lease, .. },
            ..
        } => lease,
        other => panic!("acquire {other:?}"),
    };
    let operation = OperationId::from_bytes([4; 16]);
    let sequence = peer.send(Request::Run {
        lease,
        operation,
        program: ProgramId::new("probe").unwrap(),
    });
    let witness = fixture.wait_witness(&["accepted_committed", "before_spawn_reached"]);
    assert_eq!(witness["spawn_attempted"], false);
    assert!(!fixture.receipt.exists());
    assert!(matches!(
        peer.response(sequence),
        Reply::Operation {
            state: OperationView::Accepted { .. },
            ..
        }
    ));
    fixture.release_hook();
    fixture.wait_receipt();
    fixture.assert_probe_receipt();
    std::fs::write(&fixture.release, fixture.nonce.as_bytes()).unwrap();
    fixture.wait_witness(&["spawn_attempted", "spawn_owned", "terminal_committed"]);
    let outcome = peer.terminal(lease, operation);
    peer.close();
    assert!(fixture.records().iter().any(|entry|matches!(entry,ledger::Entry::OperationFinished{outcome:stored,..}if *stored==outcome)));
}
fn prove_finished_before_drain() {
    let fixture = Fixture::new("flood");
    let mut peer = fixture.start_feature("pressure", true);
    let lease = peer.acquire(&fixture);
    let operation = OperationId::from_bytes([4; 16]);
    peer.run(lease, operation);
    let saturation =
        fixture.wait_witness(&["capture_saturated", "write_pending", "terminal_committed"]);
    assert_pressure_facts(&saturation);
    fixture.assert_probe_receipt();
    // The real CommittedRecord witness precedes the first post-Run stdout read.
    let outcome = peer.terminal(lease, operation);
    assert!(matches!(
        outcome,
        Outcome::RunCompleted {
            reason: RunReason::Natural,
            truncated: true,
            ..
        }
    ));
    assert!(
        peer.output
            .iter()
            .map(|(_, _, bytes)| bytes.len())
            .sum::<usize>()
            <= MAX_STREAM_BYTES
    );
    peer.close();
    assert!(fixture.records().iter().any(|entry|matches!(entry,ledger::Entry::OperationFinished{outcome:stored,..}if *stored==outcome)));
}
fn prove_cancel_before_drain() {
    let fixture = Fixture::new("flood_wait");
    let mut peer = fixture.start_feature("pressure", true);
    let lease = peer.acquire(&fixture);
    let operation = OperationId::from_bytes([4; 16]);
    peer.run(lease, operation);
    fixture.wait_receipt();
    let saturation = fixture.wait_witness(&["capture_saturated", "write_pending"]);
    assert_pressure_facts(&saturation);
    let sequence = peer.send(Request::Cancel { lease, operation });
    fixture.wait_witness(&["cancel_requested", "terminal_committed"]);
    assert!(matches!(peer.response(sequence), Reply::Operation { .. }));
    assert!(matches!(
        peer.terminal(lease, operation),
        Outcome::RunCompleted {
            reason: RunReason::CancelRequested,
            ..
        }
    ));
    peer.close();
}
fn prove_cached_cancel_with_worker_gate() {
    let fixture = Fixture::new("success");
    let mut peer = fixture.start_feature("hold-before-spawn", true);
    let lease = peer.acquire(&fixture);
    let operation = OperationId::from_bytes([4; 16]);
    peer.run(lease, operation);
    fixture.wait_witness(&["before_spawn_reached", "accepted_committed"]);
    assert!(matches!(
        peer.request(Request::QueryOperation { lease, operation }),
        Reply::Operation {
            state: OperationView::Accepted { .. } | OperationView::Running,
            ..
        }
    ));
    assert!(matches!(
        peer.request(Request::Cancel { lease, operation }),
        Reply::Operation { .. }
    ));
    let witness = fixture.wait_witness(&["cancel_requested"]);
    assert_eq!(witness["spawn_attempted"], false);
    fixture.release_hook();
    assert_eq!(
        peer.terminal(lease, operation),
        Outcome::NotStarted {
            reason: NotStartedReason::CancelRequested
        }
    );
    assert!(!fixture.receipt.exists());
    peer.close();
}

fn prove_postjoin_ledger_rejects_corruption() {
    let fixture = Fixture::new("success");
    fixture.start().close();
    let ledger_path = fixture.root.path().join("ledger/ledger.bin");
    let original = fixture.ledger_bytes();
    assert_eq!(fixture.records().len(), 1);
    let mut torn = original.clone();
    torn.extend_from_slice(&[0, 0]);
    std::fs::write(&ledger_path, torn).unwrap();
    let torn_rejected = fixture.checked_records().is_err();
    let mut impossible = original.clone();
    impossible.extend_from_slice(
        &ledger::encode(
            &ledger::Record::new(
                1,
                ledger::Entry::OperationFinished {
                    lease: LeaseId::from_bytes([9; 16]),
                    operation: OperationId::from_bytes([4; 16]),
                    outcome: Outcome::NotStarted {
                        reason: NotStartedReason::CancelRequested,
                    },
                },
            )
            .unwrap(),
        )
        .unwrap(),
    );
    std::fs::write(&ledger_path, impossible).unwrap();
    let history_rejected = fixture.checked_records().is_err();
    let wrong = ledger::Record::new(
        0,
        ledger::Entry::Header {
            binding: BindingId::from_bytes([99; 16]),
            manifest_digest: fixture.digest,
            workspace_marker: fixture.marker,
            os: os(),
        },
    )
    .unwrap();
    std::fs::write(&ledger_path, ledger::encode(&wrong).unwrap()).unwrap();
    let binding_rejected = fixture.checked_records().is_err();
    std::fs::write(&ledger_path, &original).unwrap();
    let marker_path = fixture
        .root
        .path()
        .join("workspace/.theseus-workspace-marker");
    std::fs::write(&marker_path, WorkspaceMarker::from_bytes([98; 16]).to_hex()).unwrap();
    let marker_rejected = fixture.checked_records().is_err();
    std::fs::write(&marker_path, fixture.marker.to_hex()).unwrap();
    assert!(
        torn_rejected && history_rejected && binding_rejected && marker_rejected,
        "whole postjoin witness negatives: torn={torn_rejected}, history={history_rejected}, binding={binding_rejected}, marker={marker_rejected}"
    );
    assert_eq!(fixture.records().len(), 1);
}

fn assert_pressure_facts(witness: &serde_json::Value) {
    assert_eq!(witness["fixture_mode"], "pressure");
    assert_eq!(
        witness["profile"],
        serde_json::json!({"name":"output-pressure","raw_chunk_cap":512,"publication_cap":128,"output_body_bytes":65532,"output_frame_bytes":65536,"raw_lifetime_cap":16384,"data_permits":4})
    );
    let reserved = witness["saturation_wire_bytes"].as_u64().unwrap();
    let published = witness["saturation_published_frames"].as_u64().unwrap();
    assert!(
        (1..MAX_STREAM_BYTES as u64).contains(&reserved),
        "capture pressure must be observed before lifetime exhaustion: {witness}"
    );
    assert!(
        (4..=128).contains(&published),
        "actual successful data publications at first no-free discard: {witness}"
    );
}

fn create_request(lease: LeaseId, id: u8, name: &str, bytes: &[u8]) -> Request {
    Request::CreateFile {
        lease,
        operation: OperationId::from_bytes([id; 16]),
        path: LeafName::new(name).unwrap(),
        bytes: FileBytes::new(bytes).unwrap(),
    }
}

fn create_file(peer: &mut Peer, lease: LeaseId, id: u8, name: &str, bytes: &[u8]) -> Outcome {
    assert_eq!(
        peer.request(create_request(lease, id, name, bytes)),
        Reply::Operation {
            lease,
            operation: OperationId::from_bytes([id; 16]),
            state: OperationView::Accepted {
                operation_kind: OperationKind::CreateFile
            },
        }
    );
    let outcome = peer.terminal(lease, OperationId::from_bytes([id; 16]));
    assert_eq!(
        outcome,
        Outcome::FileCreated {
            length: bytes.len() as u16,
            digest: content_digest(bytes),
        }
    );
    outcome
}

fn content_digest(bytes: &[u8]) -> Digest {
    use sha2::{Digest as _, Sha256};
    Digest::from_bytes(Sha256::digest(bytes).into())
}

fn assert_observed(peer: &mut Peer, lease: LeaseId, name: &str, bytes: &[u8]) {
    let path = LeafName::new(name).unwrap();
    assert_eq!(
        peer.request(Request::InspectFile {
            lease,
            path: path.clone()
        }),
        Reply::FileInfo {
            lease,
            path: path.clone(),
            length: bytes.len() as u16,
            digest: content_digest(bytes)
        }
    );
    for (offset, length) in [
        (0, 4096),
        (0, 1),
        (1, 2),
        (bytes.len() as u64, 1),
        (u64::MAX - 4096, 4096),
    ] {
        let begin = offset.min(bytes.len() as u64) as usize;
        let end = (begin + length as usize).min(bytes.len());
        assert_eq!(
            peer.request(Request::ReadFile {
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
}

#[test]
fn native_leaf_files_observe_actual_content_empty_limit_and_ranges() {
    let fixture = Fixture::new("success");
    let mut peer = fixture.start();
    let lease = peer.acquire(&fixture);
    let full = (0..4096).map(|n| (n % 251) as u8).collect::<Vec<_>>();
    let mut expected = vec![];
    for (id, name, bytes) in [
        (21, "empty", &b""[..]),
        (22, "full", full.as_slice()),
        (23, "small", &b"original"[..]),
    ] {
        let request = create_request(lease, id, name, bytes);
        let outcome = create_file(&mut peer, lease, id, name, bytes);
        assert_eq!(
            std::fs::read(fixture.root.path().join("workspace").join(name)).unwrap(),
            bytes
        );
        assert_observed(&mut peer, lease, name, bytes);
        expected.push((
            OperationId::from_bytes([id; 16]),
            request_digest(&request).unwrap(),
            outcome,
        ));
    }
    // A later native observation must hash actual current bytes, not the Create request.
    std::fs::write(fixture.root.path().join("workspace/small"), b"changed").unwrap();
    assert_observed(&mut peer, lease, "small", b"changed");
    assert!(
        peer.output.is_empty(),
        "file requests have no output stream"
    );
    peer.close();
    let records = fixture.records();
    for (operation, digest, outcome) in expected {
        let accepted = records.iter().position(|entry| matches!(entry,
            ledger::Entry::OperationAccepted { lease: found, operation: op, operation_kind: OperationKind::CreateFile, request_digest, .. }
            if *found == lease && *op == operation && *request_digest == digest)).unwrap();
        let finished = records
            .iter()
            .position(|entry| {
                matches!(entry,
            ledger::Entry::OperationFinished { lease: found, operation: op, outcome: stored }
            if *found == lease && *op == operation && *stored == outcome)
            })
            .unwrap();
        assert!(
            accepted < finished,
            "physical accepted precedes matching finished"
        );
    }
    assert!(!fixture.receipt.exists());
}

#[test]
fn native_leaf_files_exclusive_create_quarantines_and_restarts_exact_reason() {
    let fixture = Fixture::new("success");
    let existing = fixture.root.path().join("workspace/existing");
    std::fs::write(&existing, b"preserved original").unwrap();
    let mut peer = fixture.start();
    let lease = peer.acquire(&fixture);
    let operation = OperationId::from_bytes([24; 16]);
    let request = create_request(lease, 24, "existing", b"must not overwrite");
    assert!(matches!(
        peer.request(request.clone()),
        Reply::Operation {
            state: OperationView::Accepted {
                operation_kind: OperationKind::CreateFile
            },
            ..
        }
    ));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
    loop {
        match peer.request(Request::QueryOperation { lease, operation }) {
            Reply::Operation {
                state:
                    OperationView::Unknown {
                        reason: UnknownReason::FileEffectUncertain,
                    },
                ..
            } => break,
            Reply::Operation {
                state: OperationView::Accepted { .. } | OperationView::Running,
                ..
            } => {
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            other => panic!("uncertain create {other:?}"),
        }
    }
    assert_eq!(
        peer.request(Request::QueryLease { lease }),
        Reply::Lease {
            lease,
            state: LeaseView::Quarantined {
                intent: fixture.prepared.record().intent,
                reason: UnknownReason::FileEffectUncertain
            }
        }
    );
    peer.close_expected(2, b"helper: cleanup incomplete\n");
    assert_eq!(std::fs::read(&existing).unwrap(), b"preserved original");
    let records = fixture.records();
    assert_eq!(records.len(), 4);
    assert!(
        matches!(&records[2],ledger::Entry::OperationAccepted { operation: op, operation_kind: OperationKind::CreateFile, request_digest: digest, .. }
        if *op == operation && *digest == request_digest(&request).unwrap())
    );
    assert_eq!(
        records[3],
        ledger::Entry::LeaseClosed {
            lease,
            disposition: ledger::Disposition::Quarantined {
                reason: UnknownReason::FileEffectUncertain
            }
        }
    );
    let mut restarted = fixture.start();
    assert_eq!(
        restarted.request(Request::QueryOperation { lease, operation }),
        Reply::Operation {
            lease,
            operation,
            state: OperationView::Unknown {
                reason: UnknownReason::FileEffectUncertain
            }
        }
    );
    assert_eq!(
        restarted.request(Request::InspectFile {
            lease,
            path: LeafName::new("existing").unwrap()
        }),
        Reply::Refused {
            code: RefusalCode::LeaseQuarantined,
            inspect_failure: None
        }
    );
    restarted.close();
    assert_eq!(fixture.records(), records);
    assert_eq!(std::fs::read(existing).unwrap(), b"preserved original");
}

#[test]
fn native_leaf_files_wrong_closed_lease_and_run_busy_preserve_workspace() {
    let fixture = Fixture::new("wait");
    let mut peer = fixture.start();
    let lease = peer.acquire(&fixture);
    let wrong = LeaseId::from_bytes([99; 16]);
    let requests = |lease| {
        [
            create_request(lease, 25, "absent", b"denied"),
            Request::ReadFile {
                lease,
                path: LeafName::new("absent").unwrap(),
                offset: 0,
                length: 1,
            },
            Request::InspectFile {
                lease,
                path: LeafName::new("absent").unwrap(),
            },
        ]
    };
    for request in requests(wrong) {
        assert_eq!(
            peer.request(request),
            Reply::Refused {
                code: RefusalCode::WrongLease,
                inspect_failure: None
            }
        );
    }
    let operation = OperationId::from_bytes([26; 16]);
    peer.run(lease, operation);
    fixture.wait_receipt();
    for request in requests(lease) {
        assert_eq!(
            peer.request(request),
            Reply::Refused {
                code: RefusalCode::Busy,
                inspect_failure: None
            }
        );
    }
    assert!(!fixture.root.path().join("workspace/absent").exists());
    std::fs::write(&fixture.release, fixture.nonce.as_bytes()).unwrap();
    peer.terminal(lease, operation);
    assert!(matches!(
        peer.request(Request::Release { lease }),
        Reply::Lease {
            state: LeaseView::AwaitingExternalReset { .. },
            ..
        }
    ));
    for request in requests(lease) {
        assert_eq!(
            peer.request(request),
            Reply::Refused {
                code: RefusalCode::AwaitingExternalReset,
                inspect_failure: None
            }
        );
    }
    peer.close();
    assert!(!fixture.root.path().join("workspace/absent").exists());
    assert!(!fixture.records().iter().any(|entry| matches!(
        entry,
        ledger::Entry::OperationAccepted {
            operation_kind: OperationKind::CreateFile,
            ..
        }
    )));
}

#[test]
fn native_leaf_files_replay_conflict_and_run_generations_retire() {
    let fixture = Fixture::new("success");
    let mut peer = fixture.start();
    let lease = peer.acquire(&fixture);
    let request = create_request(lease, 27, "first", b"first content");
    let outcome = create_file(&mut peer, lease, 27, "first", b"first content");
    assert_observed(&mut peer, lease, "first", b"first content");
    let replay = Reply::Operation {
        lease,
        operation: OperationId::from_bytes([27; 16]),
        state: OperationView::Finished { outcome },
    };
    assert_eq!(
        peer.request(request.clone()),
        Reply::Refused {
            code: RefusalCode::InvalidRequest,
            inspect_failure: None
        }
    );
    assert_eq!(
        peer.request(create_request(lease, 27, "first", b"conflict")),
        Reply::Refused {
            code: RefusalCode::InvalidRequest,
            inspect_failure: None
        }
    );
    let run = OperationId::from_bytes([28; 16]);
    peer.run(lease, run);
    assert!(matches!(
        peer.terminal(lease, run),
        Outcome::RunCompleted {
            reason: RunReason::Natural,
            ..
        }
    ));
    fixture.assert_probe_receipt();
    create_file(&mut peer, lease, 29, "after-run", b"next generation");
    peer.close();
    let before = fixture.records();
    let mut restarted = fixture.start();
    assert_eq!(
        restarted.request(create_request(lease, 27, "first", b"conflict")),
        Reply::Refused {
            code: RefusalCode::ReplayConflict,
            inspect_failure: None
        }
    );
    assert_eq!(restarted.request(request.clone()), replay);
    assert_eq!(
        restarted.request(request),
        Reply::Refused {
            code: RefusalCode::InvalidRequest,
            inspect_failure: None
        }
    );
    assert_observed(&mut restarted, lease, "after-run", b"next generation");
    restarted.close();
    assert_eq!(fixture.records(), before);
    assert_eq!(
        before
            .iter()
            .filter(|entry| matches!(
                entry,
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
fn native_leaf_files_commit_ordering_and_cached_control_at_worker_gates() {
    // These deliberate worker-boundary gates surround real synced store/file
    // facts; they do not claim to block a kernel file call.
    let before = Fixture::new("success");
    let mut peer = before.start_feature("hold-before-file-effect", true);
    let lease = peer.acquire(&before);
    let operation = OperationId::from_bytes([30; 16]);
    let request = create_request(lease, 30, "cancelled", b"never written");
    assert!(matches!(
        peer.request(request.clone()),
        Reply::Operation {
            state: OperationView::Accepted {
                operation_kind: OperationKind::CreateFile
            },
            ..
        }
    ));
    let witness = before.wait_witness(&["accepted_committed", "file_before_effect"]);
    assert_eq!(witness["file_effect_attempted"], false);
    assert!(!before.root.path().join("workspace/cancelled").exists());
    assert_eq!(
        peer.request(Request::QueryOperation { lease, operation }),
        Reply::Operation {
            lease,
            operation,
            state: OperationView::Accepted {
                operation_kind: OperationKind::CreateFile
            }
        }
    );
    assert!(matches!(
        peer.request(Request::Cancel { lease, operation }),
        Reply::Operation { .. }
    ));
    before.wait_witness(&["cancel_requested"]);
    before.release_hook();
    let cancelled = Outcome::NotStarted {
        reason: NotStartedReason::CancelRequested,
    };
    assert_eq!(peer.terminal(lease, operation), cancelled);
    assert!(!before.root.path().join("workspace/cancelled").exists());
    assert_eq!(before.witness()["file_effect_attempted"], false);
    peer.close();
    let records = before.records();
    assert_eq!(records.len(), 4);
    assert_eq!(
        records[2],
        ledger::Entry::OperationAccepted {
            lease,
            operation,
            operation_kind: OperationKind::CreateFile,
            request_digest: request_digest(&request).unwrap()
        }
    );
    assert_eq!(
        records[3],
        ledger::Entry::OperationFinished {
            lease,
            operation,
            outcome: cancelled
        }
    );

    let finished = Fixture::new("success");
    let mut peer = finished.start_feature("hold-after-file-finished", true);
    let lease = peer.acquire(&finished);
    let operation = OperationId::from_bytes([31; 16]);
    let bytes = b"physically committed before wire terminal";
    let request = create_request(lease, 31, "finished", bytes);
    assert!(matches!(
        peer.request(request.clone()),
        Reply::Operation {
            state: OperationView::Accepted {
                operation_kind: OperationKind::CreateFile
            },
            ..
        }
    ));
    let witness = finished.wait_witness(&["file_effect_attempted", "file_finished_committed"]);
    assert_eq!(witness["file_quarantine_committed"], false);
    let actual = std::fs::read(finished.root.path().join("workspace/finished")).unwrap();
    assert_eq!(actual, bytes);
    assert_eq!(content_digest(&actual), content_digest(bytes));
    // The nonce/hash-bound marker is emitted only after the physical sync;
    // live ledger reads are deliberately avoided, including on Windows.
    assert_eq!(
        peer.request(Request::QueryOperation { lease, operation }),
        Reply::Operation {
            lease,
            operation,
            state: OperationView::Accepted {
                operation_kind: OperationKind::CreateFile
            }
        }
    );
    finished.release_hook();
    let outcome = Outcome::FileCreated {
        length: bytes.len() as u16,
        digest: content_digest(bytes),
    };
    assert_eq!(peer.terminal(lease, operation), outcome);
    assert!(peer.output.is_empty());
    peer.close();
    let records = finished.records();
    assert_eq!(records.len(), 4);
    assert_eq!(
        records[2],
        ledger::Entry::OperationAccepted {
            lease,
            operation,
            operation_kind: OperationKind::CreateFile,
            request_digest: request_digest(&request).unwrap()
        }
    );
    assert_eq!(
        records[3],
        ledger::Entry::OperationFinished {
            lease,
            operation,
            outcome
        }
    );
}

#[test]
fn native_leaf_files_real_write_failure_quarantines_and_restarts() {
    // Fixture-induced read-only handle: the native write itself fails. This
    // does not model ordinary permissions or inject a ledger sync failure.
    let fixture = Fixture::new("success");
    let mut peer = fixture.start_feature("fail-file-write", true);
    let lease = peer.acquire(&fixture);
    let operation = OperationId::from_bytes([32; 16]);
    let request = create_request(lease, 32, "write-failed", b"native write must fail");
    assert!(matches!(
        peer.request(request.clone()),
        Reply::Operation {
            state: OperationView::Accepted {
                operation_kind: OperationKind::CreateFile
            },
            ..
        }
    ));
    let witness = fixture.wait_witness(&[
        "file_effect_attempted",
        "file_write_failed",
        "file_quarantine_committed",
    ]);
    assert_eq!(witness["file_finished_committed"], false);
    let unknown = Reply::Operation {
        lease,
        operation,
        state: OperationView::Unknown {
            reason: UnknownReason::FileEffectUncertain,
        },
    };
    // The synced witness survives the worker's failure-driven shutdown;
    // cached replies are not guaranteed to remain available in this window.
    peer.close_expected(2, b"helper: runtime failed\n");
    let path = fixture.root.path().join("workspace/write-failed");
    assert_eq!(std::fs::read(&path).unwrap(), b"");
    let records = fixture.records();
    assert_eq!(records.len(), 4);
    assert_eq!(
        records[2],
        ledger::Entry::OperationAccepted {
            lease,
            operation,
            operation_kind: OperationKind::CreateFile,
            request_digest: request_digest(&request).unwrap()
        }
    );
    assert_eq!(
        records[3],
        ledger::Entry::LeaseClosed {
            lease,
            disposition: ledger::Disposition::Quarantined {
                reason: UnknownReason::FileEffectUncertain
            }
        }
    );
    let mut restarted = fixture.start();
    assert_eq!(
        restarted.request(Request::QueryLease { lease }),
        Reply::Lease {
            lease,
            state: LeaseView::Quarantined {
                intent: fixture.prepared.record().intent,
                reason: UnknownReason::FileEffectUncertain
            }
        }
    );
    assert_eq!(
        restarted.request(Request::QueryOperation { lease, operation }),
        unknown
    );
    assert_eq!(
        restarted.request(create_request(lease, 33, "second-create", b"denied")),
        Reply::Refused {
            code: RefusalCode::LeaseQuarantined,
            inspect_failure: None
        }
    );
    restarted.close();
    assert_eq!(fixture.records(), records);
    assert_eq!(std::fs::read(path).unwrap(), b"");
    assert!(!fixture.root.path().join("workspace/second-create").exists());
}
