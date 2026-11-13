use super::*;
use std::io::Write;
use theseus_host::process::{ChildEnvironment, NativeCommand};
use theseus_native_helper_store::{BootstrapRequest, StoreBinding, bootstrap};
#[test]
#[ignore]
fn fixed_probe_child() {
    let Some(path) = std::env::var_os("THESEUS_FIXED_PROBE_RECEIPT") else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    let nonce = std::env::var("THESEUS_FIXED_PROBE_NONCE").unwrap();
    std::fs::write(path.with_extension("started"), nonce.as_bytes()).unwrap();
    if std::env::var_os("THESEUS_FIXED_PROBE_PACED").is_some() {
        let deadline = Instant::now() + Duration::from_secs(10);
        // One bounded batch waits for an explicit parent acknowledgment.
        // A descheduled parent cannot let the child consume all four tokens
        // and drain the remainder before the recycling proof has run.
        for stage in 1..=6 {
            std::io::stderr().write_all(&[b'e'; 1024]).unwrap();
            std::io::stderr().flush().unwrap();
            let progress = format!("{nonce}:{stage}");
            std::fs::write(path.with_extension("progress"), progress.as_bytes()).unwrap();
            while std::fs::read(path.with_extension("ack")).ok().as_deref()
                != Some(progress.as_bytes())
            {
                assert!(
                    Instant::now() < deadline,
                    "bounded original fixture acknowledgment"
                );
                thread::yield_now();
            }
        }
    }
    std::io::stdout().write_all(&[b'o'; 32768]).unwrap();
    std::io::stderr().write_all(&[b'e'; 32768]).unwrap();
    std::fs::write(path, nonce.as_bytes()).unwrap();
}

fn event(ports: &DispatcherPorts) -> WorkerEvent {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(e) = ports.events.try_recv() {
            return e;
        }
        assert!(Instant::now() < deadline, "bounded worker event deadline");
        thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn physical_finish_does_not_wait_for_output_drain() {
    run_native(false);
}
#[test]
fn readily_drained_native_streams_share_lifetime_budget() {
    run_native(true);
}
fn run_native(drain: bool) {
    let tmp = tempfile::tempdir().unwrap();
    let lp = tmp.path().join("ledger");
    let wp = tmp.path().join("workspace");
    let l = HostDir::create_private(&lp).unwrap();
    let w = HostDir::create_private(&wp).unwrap();
    let binding = v2::BindingId::from_bytes([1; 16]);
    let manifest_digest = v2::Digest::from_bytes([2; 32]);
    let receipt = bootstrap(
        &l,
        &w,
        BootstrapRequest {
            binding,
            manifest_digest,
            os: crate::invocation::host_os(),
        },
    )
    .unwrap();
    let nonce = tmp.path().join("completion");
    let mut nonce_bytes = [0; 16];
    theseus_host::random_bytes(&mut nonce_bytes).unwrap();
    let nonce_value: String = nonce_bytes.iter().map(|b| format!("{b:02x}")).collect();
    let mut command = NativeCommand::new(std::env::current_exe().unwrap());
    command.args([
        "--exact",
        "worker::native_tests::fixed_probe_child",
        "--ignored",
        "--nocapture",
    ]);
    command.environment = ChildEnvironment::Replace(vec![
        (
            "THESEUS_FIXED_PROBE_RECEIPT".into(),
            nonce.clone().into_os_string(),
        ),
        (
            "THESEUS_FIXED_PROBE_NONCE".into(),
            nonce_value.clone().into(),
        ),
    ]);
    if drain && let ChildEnvironment::Replace(entries) = &mut command.environment {
        entries.push(("THESEUS_FIXED_PROBE_PACED".into(), "1".into()));
    }
    let inv = Invocation::new(
        lp.clone(),
        wp,
        StoreBinding {
            binding,
            manifest_digest,
            workspace_marker: receipt.workspace_marker(),
            os: crate::invocation::host_os(),
        },
        vec![crate::CatalogEntry {
            id: v2::ProgramId::new("probe").unwrap(),
            command,
        }],
    )
    .unwrap();
    #[cfg(feature = "test-support")]
    let control =
        crate::test_support::FixtureControl::new(crate::test_support::FixtureMode::Observe);
    #[cfg(feature = "test-support")]
    let inv = {
        let mut inv = inv;
        inv.fixture = Some(control.clone());
        inv
    };
    let (ports, worker, [permit0, permit1], tokens) = channels();
    let join = spawn(worker, tokens).unwrap();
    ports
        .commands
        .try_send(WorkerCommand::Init(inv))
        .ok()
        .unwrap();
    assert!(matches!(
        event(&ports),
        WorkerEvent::CacheUpdate(CachePatch::Initialized { .. })
    ));
    #[cfg(feature = "test-support")]
    assert!(control.snapshot().init_reached);
    let intent = v2::IntentId::from_bytes([3; 16]);
    ports
        .commands
        .try_send(WorkerCommand::Acquire {
            request: v2::Request::Acquire {
                intent,
                binding,
                manifest_digest,
            },
            ticket: ResponseTicket {
                reply_to: 3,
                permit: permit0,
            },
        })
        .ok()
        .unwrap();
    let lease = match event(&ports) {
        WorkerEvent::CacheUpdate(CachePatch::Lease { lease, .. }) => lease,
        _ => panic!("lease cache"),
    };
    let _acquire_permit = match event(&ports) {
        WorkerEvent::ResponseReady {
            ticket,
            reply: v2::Reply::Acquired { .. },
            ..
        } => ticket.permit,
        _ => panic!("acquire response"),
    };
    #[cfg(feature = "test-support")]
    assert!(control.snapshot().acquire_committed);
    let operation = v2::OperationId::from_bytes([4; 16]);
    let key = RunKey {
        lease,
        operation,
        generation: 1,
    };
    let (mut grant, start) = start_pair(key, 4);
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    ports
        .commands
        .try_send(WorkerCommand::Run {
            request: v2::Request::Run {
                lease,
                operation,
                program: v2::ProgramId::new("probe").unwrap(),
            },
            ticket: ResponseTicket {
                reply_to: 4,
                permit: permit1,
            },
            cancel: CancelToken { key, flag },
            start,
        })
        .ok()
        .unwrap();
    assert!(matches!(
        event(&ports),
        WorkerEvent::CacheUpdate(CachePatch::Phase {
            phase: LivePhase::Accepted,
            ..
        })
    ));
    assert!(
        !nonce.with_extension("started").exists(),
        "Accepted durability alone grants no spawn authority"
    );
    assert!(matches!(event(&ports),WorkerEvent::ResponseReady{start_key:Some(k),..} if k==key));
    // Worker-only admission model: integrated dispatcher proves actual FIFO insertion.
    grant
        .grant(AcceptedAdmission {
            key,
            reply_to: 4,
            outbound_sequence: 3,
        })
        .unwrap();
    #[cfg(feature = "test-support")]
    assert!(control.snapshot().accepted_committed);
    let mut raw_emitted = 0usize;
    let mut returned = 0u32;
    let mut stderr_returned = 0u64;
    let mut acknowledged_stage = 0;
    let mut original_storage = [None; 4];
    let mut recycled = 0;
    let overall_deadline = Instant::now() + Duration::from_secs(10);
    let boundary = loop {
        if drain && let Ok(chunk) = ports.data.try_recv() {
            raw_emitted += usize::from(chunk.len);
            if chunk.channel == v2::Channel::Stderr {
                stderr_returned += u64::from(chunk.len);
            }
            let original = &mut original_storage[chunk.permit.id as usize];
            let address = chunk.permit.storage.as_ptr();
            if let Some(previous) = original.replace(address) {
                assert_eq!(previous, address, "same original storage returns");
                recycled += 1;
            }
            assert_eq!(chunk.ordinal, returned + 1);
            returned = chunk.ordinal;
            ports
                .returned
                .try_send(DataReturn::Reusable {
                    key: chunk.key,
                    ordinal: chunk.ordinal,
                    disposition: ReturnDisposition::TransportAcknowledged,
                    permit: chunk.permit,
                })
                .ok()
                .unwrap();
        }
        if drain && acknowledged_stage < 6 {
            let stage = acknowledged_stage + 1;
            let progress = format!("{nonce_value}:{stage}");
            if stderr_returned >= stage * 1024
                && std::fs::read(nonce.with_extension("progress"))
                    .ok()
                    .as_deref()
                    == Some(progress.as_bytes())
            {
                // The exact chunk permits have entered the original return
                // mailbox before authorizing the next child publication.
                std::fs::write(nonce.with_extension("ack"), progress.as_bytes()).unwrap();
                acknowledged_stage = stage;
            }
        }
        let next = if drain {
            match ports.events.try_recv() {
                Ok(e) => e,
                Err(TryRecvError::Empty) => {
                    assert!(Instant::now() < overall_deadline);
                    thread::sleep(Duration::from_millis(1));
                    continue;
                }
                Err(_) => panic!("worker event disconnected"),
            }
        } else {
            event(&ports)
        };
        match next {
            WorkerEvent::CacheUpdate(CachePatch::Terminal {
                view:
                    v2::OperationView::Finished {
                        outcome:
                            v2::Outcome::RunCompleted {
                                reason: v2::RunReason::Natural,
                                status,
                                ..
                            },
                    },
                boundary,
                ..
            }) => {
                assert!(matches!(
                    status,
                    v2::NativeStatus::PosixExit { code: 0 }
                        | v2::NativeStatus::WindowsExit { code: 0 }
                ));
                break boundary;
            }
            WorkerEvent::CacheUpdate(CachePatch::Phase { .. }) => {}
            _ => panic!("truthful durable completion"),
        }
    };
    #[cfg(feature = "test-support")]
    {
        let snapshot = control.snapshot();
        assert!(snapshot.before_spawn_reached && snapshot.spawn_attempted && snapshot.spawn_owned);
        assert!(snapshot.terminal_committed);
        assert!(!snapshot.panic_injected);
        if !drain {
            assert!(snapshot.capture_saturated);
        }
    }
    assert_eq!(std::fs::read(&nonce).unwrap(), nonce_value.as_bytes());
    assert!(boundary.truncated);
    assert!(boundary.stdout_bytes + boundary.stderr_bytes >= 65536);
    if drain {
        while returned < boundary.final_ordinal {
            assert!(Instant::now() < overall_deadline);
            if let Ok(chunk) = ports.data.try_recv() {
                raw_emitted += usize::from(chunk.len);
                returned = chunk.ordinal;
                ports
                    .returned
                    .try_send(DataReturn::Reusable {
                        key: chunk.key,
                        ordinal: chunk.ordinal,
                        disposition: ReturnDisposition::TransportAcknowledged,
                        permit: chunk.permit,
                    })
                    .ok()
                    .unwrap();
            } else {
                thread::sleep(Duration::from_millis(1));
            }
        }
        assert!(
            boundary.final_ordinal > 4,
            "original four tokens actually recycled"
        );
        assert_eq!(acknowledged_stage, 6);
        assert!(
            recycled > 0,
            "publication consumed returned original storage"
        );
        assert!(raw_emitted <= 16384);
        assert!(raw_emitted > 0);
    } else {
        assert!(boundary.final_ordinal <= 4);
    }
    // Undrained case has consumed no Output or returned tokens; neither case
    // sends ProducerDrained before checking the physical Finished barrier.
    // Read through the live worker's own locked store handle: Windows
    // byte-range locks refuse reads through any other handle, and reading
    // after the owner drops would no longer prove Finished precedes drain.
    let (physical_tx, physical_rx) = std::sync::mpsc::sync_channel(1);
    ports
        .commands
        .try_send(WorkerCommand::ReadPhysicalLedger(physical_tx))
        .ok()
        .unwrap();
    let physical = physical_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    let mut decoder = v2::ledger::Decoder::new();
    let mut offset = 0;
    let mut finished = false;
    while offset < physical.len() {
        let p = decoder.feed(&physical[offset..]).unwrap();
        offset += p.consumed;
        if let Some(r) = p.record {
            finished |= matches!(r.entry(), Entry::OperationFinished { .. });
        }
    }
    decoder.finish().unwrap();
    assert!(
        finished,
        "physical terminal record before optional output acknowledgment"
    );
    ports.stop.store(true, Ordering::Release);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !join.is_finished() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    assert!(matches!(
        join.join().unwrap(),
        WorkerCompletion::Clean {
            native_settled: true,
            ..
        }
    ));
}
