use super::*;
use theseus_host::process::{ChildEnvironment, NativeCommand};
use theseus_native_helper_store::{BootstrapRequest, StoreBinding, bootstrap};
fn fixture() -> (
    tempfile::TempDir,
    Worker,
    DispatcherPorts,
    [ControlPermit; 2],
    Invocation,
) {
    let tmp = tempfile::tempdir().unwrap();
    let lp = tmp.path().join("l");
    let wp = tmp.path().join("w");
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
    let mut command = NativeCommand::new(std::env::current_exe().unwrap());
    command.environment = ChildEnvironment::Replace(vec![]);
    let invocation = Invocation::new(
        lp,
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
    let (ports, wp, controls, tokens) = channels();
    (tmp, Worker::new(wp, tokens), ports, controls, invocation)
}
fn accepted_file(
    cancelled: bool,
) -> (
    tempfile::TempDir,
    Worker,
    DispatcherPorts,
    StartGrant,
    ResponseTicket,
) {
    let (tmp, mut worker, ports, [p0, p1], inv) = fixture();
    let request = v2::Request::Acquire {
        intent: v2::IntentId::from_bytes([3; 16]),
        binding: inv.expected.binding,
        manifest_digest: inv.expected.manifest_digest,
    };
    worker
        .initialize(inv)
        .unwrap_or_else(|_| panic!("initialize"));
    ports.events.try_recv().unwrap();
    worker
        .acquire(
            request,
            ResponseTicket {
                reply_to: 1,
                permit: p0,
            },
        )
        .unwrap_or_else(|_| panic!("acquire"));
    ports.events.try_recv().unwrap();
    let (spare, _, _) = worker.response.take().unwrap();
    let key = RunKey {
        lease: worker.lease.as_ref().unwrap().1,
        operation: v2::OperationId::from_bytes([4; 16]),
        generation: 1,
    };
    let (grant, start) = start_pair(key, 2);
    worker
        .begin_file(
            v2::Request::CreateFile {
                lease: key.lease,
                operation: key.operation,
                path: v2::LeafName::new("leaf").unwrap(),
                bytes: v2::FileBytes::new(b"actual").unwrap(),
            },
            ResponseTicket {
                reply_to: 2,
                permit: p1,
            },
            CancelToken {
                key,
                flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(cancelled)),
            },
            start,
        )
        .unwrap_or_else(|_| panic!("file admission"));
    ports.events.try_recv().unwrap();
    (tmp, worker, ports, grant, spare)
}
#[test]
fn native_prefix_rename_replacement_removal_permanently_blocks_all_catalog_effects() {
    for replace in [false, true] {
        let (tmp, mut worker, ports, [p0, p1], mut inv) = fixture();
        let epoch = v2::Digest::from_bytes([0x44; 32]);
        inv.profile = v2::HelperProfile::Native {
            provider_epoch: epoch,
        };
        // Synthetic artifact authority for this prefix-only worker fixture.
        inv.artifact_commitment = Some(v2::Digest::from_bytes([0x66; 32]));
        let header = Record::new(
            0,
            Entry::NativeHeader {
                binding: inv.expected.binding,
                manifest_digest: inv.expected.manifest_digest,
                workspace_marker: inv.expected.workspace_marker,
                os: inv.expected.os,
                capabilities: v2::NativeCapabilities,
                provider_epoch: epoch,
            },
        )
        .unwrap();
        std::fs::write(
            inv.ledger_root.join("ledger.bin"),
            v2::ledger::encode(&header).unwrap(),
        )
        .unwrap();
        let original = inv.workspace_root.clone();
        let moved = tmp.path().join("moved");
        let acquire = v2::Request::Acquire {
            intent: v2::IntentId::from_bytes([3; 16]),
            binding: inv.expected.binding,
            manifest_digest: inv.expected.manifest_digest,
        };
        worker
            .initialize(inv)
            .unwrap_or_else(|_| panic!("native initialize"));
        ports.events.try_recv().unwrap();
        worker
            .acquire(
                acquire,
                ResponseTicket {
                    reply_to: 1,
                    permit: p0,
                },
            )
            .unwrap_or_else(|_| panic!("native acquire"));
        ports.events.try_recv().unwrap();
        worker.response.take();
        let lease = worker.lease.as_ref().unwrap().1;
        std::fs::rename(&original, &moved).unwrap();
        if replace {
            std::fs::create_dir(&original).unwrap();
            std::fs::write(original.join("reader"), b"replacement").unwrap();
        }
        worker
            .observe_file(
                v2::Request::InspectNativeFile {
                    lease,
                    path: v2::LeafName::new("reader").unwrap(),
                },
                ResponseTicket {
                    reply_to: 2,
                    permit: p1,
                },
            )
            .unwrap_or_else(|_| panic!("native inspect"));
        let WorkerEvent::ResponseReady {
            ticket,
            reply: v2::Reply::PrefixAuthorityRevoked { reason, .. },
            ..
        } = ports.events.try_recv().unwrap()
        else {
            panic!("must revoke whole native authority")
        };
        assert_eq!(worker.native_authority_failure, Some(reason));
        assert_eq!(worker.prefix_mapping_error.is_some(), !replace);
        assert_eq!(worker.stopping, !replace);
        assert_eq!(
            worker
                .store
                .as_ref()
                .unwrap()
                .restart_view()
                .native_authority_failure(),
            Some(reason)
        );
        // Restore the original mapping; revocation must remain sticky.
        if replace {
            std::fs::remove_file(original.join("reader")).unwrap();
            std::fs::remove_dir(&original).unwrap();
        }
        std::fs::rename(&moved, &original).unwrap();
        let mut ticket = ticket;
        for (n, id) in ["check_target", "uninstall"].iter().enumerate() {
            let key = RunKey {
                lease,
                operation: v2::OperationId::from_bytes([n as u8 + 8; 16]),
                generation: n as u64 + 1,
            };
            let (_grant, start) = start_pair(key, 3 + n as u64);
            worker
                .begin(
                    v2::Request::Run {
                        lease,
                        operation: key.operation,
                        program: v2::ProgramId::new(id).unwrap(),
                    },
                    ticket,
                    CancelToken {
                        key,
                        flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    },
                    start,
                )
                .unwrap_or_else(|_| panic!("revoked run refusal"));
            let WorkerEvent::ResponseReady {
                ticket: returned,
                reply: v2::Reply::PrefixAuthorityRevoked { .. },
                ..
            } = ports.events.try_recv().unwrap()
            else {
                panic!("zero later catalog Run")
            };
            ticket = returned;
        }
        let key = RunKey {
            lease,
            operation: v2::OperationId::from_bytes([10; 16]),
            generation: 3,
        };
        let (_grant, start) = start_pair(key, 5);
        worker
            .begin_file(
                v2::Request::CreateFile {
                    lease,
                    operation: key.operation,
                    path: v2::LeafName::new("app").unwrap(),
                    bytes: v2::FileBytes::new(b"forbidden").unwrap(),
                },
                ticket,
                CancelToken {
                    key,
                    flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                },
                start,
            )
            .unwrap_or_else(|_| panic!("revoked create refusal"));
        assert!(matches!(
            ports.events.try_recv().unwrap(),
            WorkerEvent::ResponseReady {
                reply: v2::Reply::PrefixAuthorityRevoked { .. },
                ..
            }
        ));
        assert!(worker.active.is_none() && worker.file.is_none());
        assert!(!original.join("app").exists());
        assert!(
            !worker
                .store
                .as_ref()
                .unwrap()
                .records()
                .iter()
                .any(|r| matches!(r.entry(), Entry::OperationAccepted { .. }))
        );
    }
}
#[test]
fn file_acceptance_grant_actual_finish_and_zero_boundary_retirement() {
    let (_tmp, mut worker, ports, mut grant, ticket) = accepted_file(false);
    let path = worker
        .invocation
        .as_ref()
        .unwrap()
        .workspace_root
        .join("leaf");
    assert!(!path.exists());
    assert!(matches!(
        worker
            .store
            .as_ref()
            .unwrap()
            .records()
            .last()
            .unwrap()
            .entry(),
        Entry::OperationAccepted {
            operation_kind: v2::OperationKind::CreateFile,
            ..
        }
    ));
    worker.advance_file().unwrap_or_else(|_| panic!("waiting"));
    assert!(
        !path.exists(),
        "physical create must wait original Accepted admission"
    );
    let key = worker.file.as_ref().unwrap().key.unwrap();
    grant
        .grant(AcceptedAdmission {
            key,
            reply_to: 2,
            outbound_sequence: 3,
        })
        .unwrap();
    worker.advance_file().unwrap_or_else(|_| panic!("create"));
    assert_eq!(std::fs::read(&path).unwrap(), b"actual");
    assert!(matches!(
        worker
            .store
            .as_ref()
            .unwrap()
            .records()
            .last()
            .unwrap()
            .entry(),
        Entry::OperationFinished {
            outcome: v2::Outcome::FileCreated { length: 6, .. },
            ..
        }
    ));
    let WorkerEvent::CacheUpdate(CachePatch::Terminal { boundary, view, .. }) =
        ports.events.try_recv().unwrap()
    else {
        panic!("terminal")
    };
    assert_eq!(boundary, file_boundary(key));
    assert!(matches!(
        view,
        v2::OperationView::Finished {
            outcome: v2::Outcome::FileCreated { .. }
        }
    ));
    assert!(
        worker.active.is_none(),
        "file never owns process output state"
    );
    ports
        .returned
        .try_send(DataReturn::ProducerDrained {
            key,
            final_ordinal: 0,
        })
        .unwrap_or_else(|_| panic!("drain"));
    worker.returns();
    assert!(worker.file.is_none());
    worker
        .observe_file(
            v2::Request::ReadFile {
                lease: key.lease,
                path: v2::LeafName::new("leaf").unwrap(),
                offset: u64::MAX - 4096,
                length: 4096,
            },
            ticket,
        )
        .unwrap_or_else(|_| panic!("read EOF"));
    let WorkerEvent::ResponseReady {
        reply: v2::Reply::FileBytes { bytes, eof, .. },
        ..
    } = ports.events.try_recv().unwrap()
    else {
        panic!("read")
    };
    assert!(bytes.as_slice().is_empty() && eof);
}
#[test]
fn file_cancel_before_attempt_commits_not_started_without_native_open() {
    let (_tmp, mut worker, ports, _grant, _ticket) = accepted_file(true);
    worker.advance_file().unwrap_or_else(|_| panic!("cancel"));
    let pending = worker.file.as_ref().unwrap();
    assert!(!pending.owner.attempted && pending.owner.handle.is_none());
    assert!(matches!(
        worker
            .store
            .as_ref()
            .unwrap()
            .records()
            .last()
            .unwrap()
            .entry(),
        Entry::OperationFinished {
            outcome: v2::Outcome::NotStarted {
                reason: v2::NotStartedReason::CancelRequested
            },
            ..
        }
    ));
    assert!(matches!(
        ports.events.try_recv().unwrap(),
        WorkerEvent::CacheUpdate(CachePatch::Terminal {
            boundary: FinalBoundary {
                final_ordinal: 0,
                ..
            },
            ..
        })
    ));
}
#[test]
fn file_uncertainty_is_exact_synced_close_and_never_finished() {
    let (_tmp, mut worker, ports, mut grant, _ticket) = accepted_file(false);
    let path = worker
        .invocation
        .as_ref()
        .unwrap()
        .workspace_root
        .join("leaf");
    std::fs::write(&path, b"untouched").unwrap();
    let key = worker.file.as_ref().unwrap().key.unwrap();
    grant
        .grant(AcceptedAdmission {
            key,
            reply_to: 2,
            outbound_sequence: 3,
        })
        .unwrap();
    worker
        .advance_file()
        .unwrap_or_else(|_| panic!("uncertain"));
    assert_eq!(std::fs::read(path).unwrap(), b"untouched");
    assert!(matches!(
        worker
            .store
            .as_ref()
            .unwrap()
            .records()
            .last()
            .unwrap()
            .entry(),
        Entry::LeaseClosed {
            disposition: Disposition::Quarantined {
                reason: v2::UnknownReason::FileEffectUncertain
            },
            ..
        }
    ));
    assert!(
        !worker
            .store
            .as_ref()
            .unwrap()
            .records()
            .iter()
            .any(|r| matches!(r.entry(), Entry::OperationFinished { .. }))
    );
    assert!(worker.stopping && worker.file.as_ref().unwrap().owner.sticky_open);
    ports.events.try_recv().unwrap(); // exact lease close is already committed
    worker.advance_file().unwrap_or_else(|_| panic!("publish"));
    assert!(matches!(
        ports.events.try_recv().unwrap(),
        WorkerEvent::CacheUpdate(CachePatch::Terminal {
            view: v2::OperationView::Unknown {
                reason: v2::UnknownReason::FileEffectUncertain
            },
            ..
        })
    ));
    worker.retire_file();
    let failure = worker.failure.as_ref().unwrap();
    assert!(matches!(
        failure.primary,
        WorkerPrimary::File(crate::files::FileFailure::OpenUnclassified(_))
    ));
    assert!(failure.retained_file.as_ref().unwrap().sticky_open);
}
#[test]
fn observation_requires_exact_held_lease_and_busy_never_opens() {
    for state in 0..5 {
        let (_tmp, mut worker, ports, [p0, p1], inv) = fixture();
        let binding = inv.expected.binding;
        let manifest_digest = inv.expected.manifest_digest;
        worker.initialize(inv).unwrap_or_else(|_| panic!("init"));
        ports.events.try_recv().unwrap();
        worker
            .acquire(
                v2::Request::Acquire {
                    intent: v2::IntentId::from_bytes([3; 16]),
                    binding,
                    manifest_digest,
                },
                ResponseTicket {
                    reply_to: 1,
                    permit: p0,
                },
            )
            .unwrap_or_else(|_| panic!("acquire"));
        ports.events.try_recv().unwrap();
        worker.response.take();
        let (intent, actual, _) = worker.lease.clone().unwrap();
        let lease = if state == 0 {
            v2::LeaseId::from_bytes([9; 16])
        } else {
            actual
        };
        let expected = match state {
            0 | 1 => v2::RefusalCode::WrongLease,
            2 => v2::RefusalCode::AwaitingExternalReset,
            3 => v2::RefusalCode::LeaseQuarantined,
            _ => v2::RefusalCode::Busy,
        };
        match state {
            1 => worker.lease = None,
            2 => {
                worker.lease = Some((
                    intent,
                    actual,
                    v2::LeaseView::AwaitingExternalReset { intent },
                ))
            }
            3 => {
                worker.lease = Some((
                    intent,
                    actual,
                    v2::LeaseView::Quarantined {
                        intent,
                        reason: v2::UnknownReason::FileEffectUncertain,
                    },
                ))
            }
            4 => {
                worker.file = Some(PendingFileRequest {
                    request: v2::Request::Describe,
                    ticket: None,
                    owner: Box::new(crate::files::FileOwner::new()),
                    key: None,
                    create: None,
                })
            }
            _ => {}
        }
        worker
            .observe_file(
                v2::Request::InspectFile {
                    lease,
                    path: v2::LeafName::new("absent").unwrap(),
                },
                ResponseTicket {
                    reply_to: 2,
                    permit: p1,
                },
            )
            .unwrap_or_else(|_| panic!("authorization"));
        assert!(
            matches!(ports.events.try_recv().unwrap(),WorkerEvent::ResponseReady { reply:v2::Reply::Refused { code, .. },.. } if code==expected)
        );
        assert!(
            !worker.stopping,
            "absent open would latch sticky uncertainty"
        );
        assert!(worker.file.as_ref().is_none_or(|p| !p.owner.attempted));
    }
}
#[test]
fn file_observation_open_uncertainty_stops_but_known_oversize_retires() {
    for missing in [false, true] {
        let (_tmp, mut worker, ports, _grant, ticket) = accepted_file(true);
        worker
            .advance_file()
            .unwrap_or_else(|_| panic!("cancel create"));
        ports.events.try_recv().unwrap();
        let key = worker.file.as_ref().unwrap().key.unwrap();
        ports
            .returned
            .try_send(DataReturn::ProducerDrained {
                key,
                final_ordinal: 0,
            })
            .unwrap_or_else(|_| panic!("drain"));
        worker.returns();
        worker.response.take();
        let lease = worker.lease.as_ref().unwrap().1;
        if !missing {
            std::fs::write(
                worker
                    .invocation
                    .as_ref()
                    .unwrap()
                    .workspace_root
                    .join("leaf"),
                vec![1u8; 4097],
            )
            .unwrap();
        }
        let records = worker.store.as_ref().unwrap().records().len();
        worker
            .observe_file(
                v2::Request::InspectFile {
                    lease,
                    path: v2::LeafName::new("leaf").unwrap(),
                },
                ticket,
            )
            .unwrap_or_else(|_| panic!("observe"));
        assert!(matches!(
            ports.events.try_recv().unwrap(),
            WorkerEvent::ResponseReady {
                reply: v2::Reply::Refused {
                    code: v2::RefusalCode::Unsupported,
                    ..
                },
                ..
            }
        ));
        assert_eq!(worker.store.as_ref().unwrap().records().len(), records);
        assert_eq!(worker.stopping, missing);
        if missing {
            let pending = worker.file.as_ref().unwrap();
            assert!(pending.owner.sticky_open && pending.owner.handle.is_none());
            worker.retire_file();
            assert!(matches!(
                worker.failure.as_ref().unwrap().primary,
                WorkerPrimary::File(crate::files::FileFailure::OpenUnclassified(_))
            ));
        } else {
            assert!(worker.file.is_none() && worker.failure.is_none());
        }
    }
}
#[test]
fn file_panic_preserves_exact_payload_primary_and_committed_finish() {
    for finished in [false, true] {
        let (_tmp, mut worker, ports, mut grant, _ticket) = accepted_file(false);
        let key = worker.file.as_ref().unwrap().key.unwrap();
        if finished {
            grant
                .grant(AcceptedAdmission {
                    key,
                    reply_to: 2,
                    outbound_sequence: 3,
                })
                .unwrap();
            worker.advance_file().unwrap_or_else(|_| panic!("create"));
            ports.events.try_recv().unwrap();
        } else {
            worker.file.as_mut().unwrap().owner.attempted = true;
            worker.file.as_mut().unwrap().owner.opening = true;
        }
        let payload = Box::new([91u8; 32]);
        let address = payload.as_ptr();
        worker.file_panic(payload);
        let last = worker
            .store
            .as_ref()
            .unwrap()
            .records()
            .last()
            .unwrap()
            .entry();
        if finished {
            assert!(matches!(
                last,
                Entry::OperationFinished {
                    outcome: v2::Outcome::FileCreated { .. },
                    ..
                }
            ));
        } else {
            assert!(matches!(
                last,
                Entry::LeaseClosed {
                    disposition: Disposition::Quarantined {
                        reason: v2::UnknownReason::FileEffectUncertain
                    },
                    ..
                }
            ));
        }
        worker.retire_file();
        assert_eq!(
            worker
                .failure
                .as_ref()
                .unwrap()
                .panic_payload()
                .unwrap()
                .downcast_ref::<[u8; 32]>()
                .unwrap()
                .as_ptr(),
            address
        );
        assert_eq!(
            worker
                .failure
                .as_ref()
                .unwrap()
                .retained_file
                .as_ref()
                .unwrap()
                .sticky_open,
            !finished
        );
    }
}
fn dispatch_input(d: &mut crate::dispatcher::Dispatcher, sequence: u64, message: v2::Message) {
    let bytes = v2::encode(
        &v2::Envelope::new(v2::SessionId::from_bytes([7; 16]), sequence, message).unwrap(),
    )
    .unwrap();
    d.input_buffer().unwrap()[..bytes.len()].copy_from_slice(&bytes);
    d.read_bytes(bytes.len());
    d.input_quantum().unwrap();
}
fn dispatch_flush(d: &mut crate::dispatcher::Dispatcher) {
    while !d.frames.is_empty() || d.current.is_some() {
        if d.current.is_none() {
            d.current = d.frames.pop_front();
        }
        let f = d.current.as_mut().unwrap();
        let n = f.write_slice().len();
        if f.acknowledged(n).unwrap() {
            d.frame_done().unwrap();
        }
    }
}
fn request(d: &mut crate::dispatcher::Dispatcher, sequence: u64, request: v2::Request) {
    dispatch_input(d, sequence, v2::Message::Request { request });
}
#[derive(Debug)]
struct Original(u32);
impl std::fmt::Display for Original {
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        panic!("must retain typed source")
    }
}
impl std::error::Error for Original {}
#[test]
fn completion_retains_original_spawn_beside_later_primary() {
    for active in [false, true] {
        for store_error in [false, true] {
            let (_tmp, mut worker, ports, [p0, p1], inv) = fixture();
            let binding = inv.expected.binding;
            let manifest_digest = inv.expected.manifest_digest;
            worker.initialize(inv).unwrap_or_else(|_| panic!("init"));
            ports.events.try_recv().ok().unwrap();
            worker
                .acquire(
                    v2::Request::Acquire {
                        intent: v2::IntentId::from_bytes([3; 16]),
                        binding,
                        manifest_digest,
                    },
                    ResponseTicket {
                        reply_to: 1,
                        permit: p0,
                    },
                )
                .unwrap_or_else(|_| panic!("acquire"));
            ports.events.try_recv().ok().unwrap();
            worker.response.take();
            let lease = worker.lease.as_ref().unwrap().1;
            let key = RunKey {
                lease,
                operation: v2::OperationId::from_bytes([4; 16]),
                generation: 1,
            };
            let (_, start) = start_pair(key, 2);
            worker
                .begin(
                    v2::Request::Run {
                        lease,
                        operation: key.operation,
                        program: v2::ProgramId::new("probe").unwrap(),
                    },
                    ResponseTicket {
                        reply_to: 2,
                        permit: p1,
                    },
                    CancelToken {
                        key,
                        flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    },
                    start,
                )
                .unwrap_or_else(|_| panic!("begin"));
            let error = io::Error::other(Original(71));
            let original =
                error.get_ref().unwrap().downcast_ref::<Original>().unwrap() as *const Original;
            let failure = theseus_host::process::SpawnFailure {
                launch: theseus_host::process::LaunchPhase::MayRun,
                cleanup: HostCleanup::Complete,
                error,
                cleanup_errors: vec![io::Error::other(Original(72))],
            };
            if active {
                worker.active.as_mut().unwrap().spawn_failure = Some(failure);
            } else {
                worker.last_spawn = Some(failure);
            }
            let primary = if store_error {
                WorkerPrimary::Store(io::Error::other(Original(73)).into())
            } else {
                WorkerPrimary::Io(io::Error::other(Original(74)))
            };
            worker.fail(primary);
            // Manager owners pin the worker to its creation thread. This
            // case has only explicitly settled spawn ownership to retire.
            worker.settle_native();
            let WorkerCompletion::Failed(f) = worker.completion() else {
                panic!("failure")
            };
            assert_eq!(f.classification(), if store_error { "store" } else { "io" });
            let retained = f
                .spawn_failure()
                .expect("original launch failure survives completion");
            assert!(matches!(
                retained.launch,
                theseus_host::process::LaunchPhase::MayRun
            ));
            assert!(matches!(retained.cleanup, HostCleanup::Complete));
            assert_eq!(
                retained
                    .error
                    .get_ref()
                    .unwrap()
                    .downcast_ref::<Original>()
                    .unwrap() as *const Original,
                original
            );
            assert_eq!(
                retained
                    .error
                    .get_ref()
                    .unwrap()
                    .downcast_ref::<Original>()
                    .unwrap()
                    .0,
                71
            );
            assert_eq!(
                retained.cleanup_errors[0]
                    .get_ref()
                    .unwrap()
                    .downcast_ref::<Original>()
                    .unwrap()
                    .0,
                72
            );
            assert_eq!(
                format!("{f:?} {f}"),
                "helper worker failure helper worker failure"
            );
        }
    }
}
#[test]
fn simultaneous_cleanup_errors_survive_with_free_slots() {
    let (_tmp, mut worker, _ports, _controls, _inv) = fixture();
    worker.fail(WorkerPrimary::Io(io::Error::other(Original(70))));
    worker.cleanup_results([
        Some(io::Error::other(Original(71))),
        Some(io::Error::other(Original(72))),
        None,
    ]);
    worker.settle_native();
    let WorkerCompletion::Failed(f) = worker.completion() else {
        panic!("failure")
    };
    let values: Vec<_> = f
        .cleanup_errors
        .iter()
        .flatten()
        .map(|e| e.get_ref().unwrap().downcast_ref::<Original>().unwrap().0)
        .collect();
    assert_eq!(values, vec![71, 72]);
}
fn integrated_admitted(
    file: bool,
) -> (
    tempfile::TempDir,
    Worker,
    crate::dispatcher::Dispatcher,
    v2::LeaseId,
    v2::OperationId,
) {
    let (tmp, mut worker, ports, controls, inv) = fixture();
    let binding = inv.expected.binding;
    let manifest_digest = inv.expected.manifest_digest;
    worker.initialize(inv).unwrap_or_else(|_| panic!("init"));
    let mut d = crate::dispatcher::Dispatcher::new(ports, controls);
    d.event_quantum().unwrap();
    dispatch_input(
        &mut d,
        0,
        v2::Message::Hello {
            role: v2::Role::Controller,
        },
    );
    dispatch_flush(&mut d);
    request(&mut d, 1, v2::Request::Describe);
    d.response_quantum().unwrap();
    dispatch_flush(&mut d);
    request(
        &mut d,
        2,
        v2::Request::Acquire {
            intent: v2::IntentId::from_bytes([3; 16]),
            binding,
            manifest_digest,
        },
    );
    worker
        .command(worker.ports.commands.try_recv().ok().unwrap())
        .unwrap_or_else(|_| panic!("acquire"));
    d.event_quantum().unwrap();
    let (t, r, k) = worker.response.take().unwrap();
    worker.respond(t, r, k);
    d.event_quantum().unwrap();
    d.response_quantum().unwrap();
    dispatch_flush(&mut d);
    let lease = worker.lease.as_ref().unwrap().1;
    let operation = v2::OperationId::from_bytes([4; 16]);
    request(
        &mut d,
        3,
        if file {
            v2::Request::CreateFile {
                lease,
                operation,
                path: v2::LeafName::new("leaf").unwrap(),
                bytes: v2::FileBytes::new(b"bytes").unwrap(),
            }
        } else {
            v2::Request::Run {
                lease,
                operation,
                program: v2::ProgramId::new("probe").unwrap(),
            }
        },
    );
    worker
        .command(worker.ports.commands.try_recv().ok().unwrap())
        .unwrap_or_else(|_| panic!("run"));
    d.event_quantum().unwrap(); // Accepted cache, original ResponseReady deliberately retained.
    (tmp, worker, d, lease, operation)
}
fn integrated_accepted() -> (
    tempfile::TempDir,
    Worker,
    crate::dispatcher::Dispatcher,
    v2::LeaseId,
    v2::OperationId,
) {
    integrated_admitted(false)
}
#[test]
fn file_terminal_waits_for_original_accepted_and_matching_zero_drain() {
    let (_tmp, mut worker, mut d, lease, operation) = integrated_admitted(true);
    request(&mut d, 4, v2::Request::Cancel { lease, operation });
    d.response_quantum().unwrap();
    dispatch_flush(&mut d);
    worker
        .advance_file()
        .unwrap_or_else(|_| panic!("cancel physical"));
    d.event_quantum().unwrap();
    request(&mut d, 5, v2::Request::QueryOperation { lease, operation });
    assert!(
        !d.response_quantum().unwrap(),
        "terminal cannot precede original Accepted"
    );
    let (ticket, reply, key) = worker.response.take().unwrap();
    worker.respond(ticket, reply, key);
    d.event_quantum().unwrap();
    assert!(d.response_quantum().unwrap()); // original Accepted
    assert!(d.response_quantum().unwrap()); // terminal
    dispatch_flush(&mut d);
    d.return_quantum().unwrap();
    worker.returns();
    assert!(worker.file.is_none() && worker.active.is_none());
    request(
        &mut d,
        6,
        v2::Request::Run {
            lease,
            operation: v2::OperationId::from_bytes([8; 16]),
            program: v2::ProgramId::new("probe").unwrap(),
        },
    );
    worker
        .command(worker.ports.commands.try_recv().unwrap())
        .unwrap_or_else(|_| panic!("run after file"));
    assert!(worker.active.is_some() && worker.file.is_none());
    assert_eq!(worker.active.as_ref().unwrap().key.generation, 2);
}
#[cfg(feature = "test-support")]
#[test]
fn file_fixture_native_write_error_retains_both_handles_and_exact_quarantine() {
    use crate::test_support::{FixtureControl, FixtureMode};
    let (_tmp, mut worker, ports, mut grant, _ticket) = accepted_file(false);
    let control = FixtureControl::new(FixtureMode::FailFileWrite);
    worker.invocation.as_mut().unwrap().fixture = Some(control.clone());
    let key = worker.file.as_ref().unwrap().key.unwrap();
    grant
        .grant(AcceptedAdmission {
            key,
            reply_to: 2,
            outbound_sequence: 3,
        })
        .unwrap();
    worker
        .advance_file()
        .unwrap_or_else(|_| panic!("native failure"));
    let owner = &worker.file.as_ref().unwrap().owner;
    let Some(crate::files::FileFailure::IoAfterOpen {
        stage: crate::files::FileStage::Write,
        error,
    }) = &owner.primary
    else {
        panic!("actual write source")
    };
    assert!(
        error.raw_os_error().is_some(),
        "write_all on actual read-only native file supplies OS source"
    );
    assert!(owner.handle.is_some() && owner.fixture_reopened.is_some());
    assert!(!owner.sticky_open && !owner.opening);
    assert_eq!(
        std::fs::read(
            worker
                .invocation
                .as_ref()
                .unwrap()
                .workspace_root
                .join("leaf")
        )
        .unwrap(),
        b""
    );
    let snapshot = control.snapshot();
    assert!(
        snapshot.file_before_effect
            && snapshot.file_effect_attempted
            && snapshot.file_write_failed
            && snapshot.file_quarantine_committed
    );
    assert!(!snapshot.file_finished_committed);
    assert!(matches!(
        worker
            .store
            .as_ref()
            .unwrap()
            .records()
            .last()
            .unwrap()
            .entry(),
        Entry::LeaseClosed {
            disposition: Disposition::Quarantined {
                reason: v2::UnknownReason::FileEffectUncertain
            },
            ..
        }
    ));
    assert!(
        !worker
            .store
            .as_ref()
            .unwrap()
            .records()
            .iter()
            .any(|r| matches!(r.entry(), Entry::OperationFinished { .. }))
    );
    ports.events.try_recv().unwrap();
    worker
        .advance_file()
        .unwrap_or_else(|_| panic!("publish unknown"));
    assert!(matches!(
        ports.events.try_recv().unwrap(),
        WorkerEvent::CacheUpdate(CachePatch::Terminal {
            view: v2::OperationView::Unknown {
                reason: v2::UnknownReason::FileEffectUncertain
            },
            ..
        })
    ));
    worker.retire_file();
    let retained = worker
        .failure
        .as_ref()
        .unwrap()
        .retained_file
        .as_ref()
        .unwrap();
    assert!(retained.handle.is_some() && retained.fixture_reopened.is_some());
}
#[test]
fn file_close_error_preserves_original_file_source_and_secondary_store_error() {
    let (_tmp, mut worker, _ports, _grant, _ticket) = accepted_file(false);
    let key = worker.file.as_ref().unwrap().key.unwrap();
    worker.file.as_mut().unwrap().owner.primary = Some(crate::files::FileFailure::IoAfterOpen {
        stage: crate::files::FileStage::Write,
        error: io::Error::other(Original(73)),
    });
    // An actual ledger rejection provides a typed secondary StoreError; this
    // is an ownership model, not a claim of an injected native sync failure.
    worker
        .append(Entry::LeaseClosed {
            lease: key.lease,
            disposition: Disposition::Quarantined {
                reason: v2::UnknownReason::HelperRestartedWithUnfinishedOperation,
            },
        })
        .unwrap_or_else(|_| panic!("close"));
    let records = worker.store.as_ref().unwrap().records().len();
    worker
        .quarantine_file()
        .unwrap_or_else(|_| panic!("retained failure"));
    assert_eq!(worker.store.as_ref().unwrap().records().len(), records);
    assert!(worker.file.as_ref().unwrap().owner.ledger_error.is_some());
    worker.retire_file();
    let failure = worker.failure.as_ref().unwrap();
    let WorkerPrimary::File(crate::files::FileFailure::IoAfterOpen { error, .. }) =
        &failure.primary
    else {
        panic!("original file error")
    };
    assert_eq!(
        error
            .get_ref()
            .unwrap()
            .downcast_ref::<Original>()
            .unwrap()
            .0,
        73
    );
    assert!(
        failure
            .retained_file
            .as_ref()
            .unwrap()
            .ledger_error
            .is_some()
    );
    assert!(worker.stopping);
}
#[test]
fn cancelled_terminal_waits_for_original_accepted_fifo() {
    let (_tmp, mut worker, mut d, lease, operation) = integrated_accepted();
    #[cfg(feature = "test-support")]
    let control =
        crate::test_support::FixtureControl::new(crate::test_support::FixtureMode::Observe);
    #[cfg(feature = "test-support")]
    {
        d.fixture = Some(control.clone());
        worker.invocation.as_mut().unwrap().fixture = Some(control.clone());
    }
    request(
        &mut d,
        4,
        v2::Request::Cancel {
            lease: v2::LeaseId::from_bytes([9; 16]),
            operation,
        },
    );
    assert!(
        !worker
            .active
            .as_ref()
            .unwrap()
            .cancel
            .flag
            .load(Ordering::Acquire)
    );
    #[cfg(feature = "test-support")]
    assert!(!control.snapshot().cancel_requested);
    d.response_quantum().unwrap();
    dispatch_flush(&mut d);
    request(&mut d, 5, v2::Request::Cancel { lease, operation });
    #[cfg(feature = "test-support")]
    assert!(control.snapshot().cancel_requested);

    d.response_quantum().unwrap();
    dispatch_flush(&mut d);
    worker
        .advance()
        .unwrap_or_else(|_| panic!("cancel physical finish"));
    assert!(worker.active.as_ref().unwrap().committed);
    assert!(!worker.active.as_ref().unwrap().attempted);
    #[cfg(feature = "test-support")]
    {
        let snapshot = control.snapshot();
        assert!(snapshot.terminal_committed);
        assert!(!snapshot.spawn_attempted && !snapshot.spawn_owned);
        assert!(snapshot.control_write_acks > 0);
    }

    assert!(matches!(
        worker
            .store
            .as_ref()
            .unwrap()
            .records()
            .last()
            .unwrap()
            .entry(),
        Entry::OperationFinished {
            outcome: v2::Outcome::NotStarted { .. },
            ..
        }
    ));
    d.event_quantum().unwrap();
    d.event_quantum().unwrap(); // Terminal before original response.
    request(&mut d, 6, v2::Request::QueryOperation { lease, operation });
    assert!(
        !d.response_quantum().unwrap(),
        "terminal must await original Accepted admission"
    );
    let (t, r, k) = worker.response.take().unwrap();
    worker.respond(t, r, k);
    d.event_quantum().unwrap();
    assert!(d.response_quantum().unwrap()); // Accepted admitted first despite later arrival.
    assert_eq!(d.frames.len(), 1);
    assert!(d.response_quantum().unwrap()); // terminal guard transition now valid.
    dispatch_flush(&mut d);
    d.return_quantum().unwrap();
    request(
        &mut d,
        7,
        v2::Request::Run {
            lease,
            operation: v2::OperationId::from_bytes([5; 16]),
            program: v2::ProgramId::new("probe").unwrap(),
        },
    );
    // Delay the old producer's next advance until the dispatcher has installed
    // the new generation, before the worker consumes its drain acknowledgment.
    worker
        .advance()
        .unwrap_or_else(|_| panic!("terminal advance"));
    assert!(
        !d.event_quantum().unwrap(),
        "Terminal is the sole final boundary event"
    );
    worker.returns();
    assert!(worker.active.is_none());
    worker
        .command(worker.ports.commands.try_recv().ok().unwrap())
        .unwrap_or_else(|_| panic!("next run"));
    assert!(d.event_quantum().unwrap());
    assert!(!worker.active.as_ref().unwrap().attempted);
}
#[test]
fn delayed_final_advance_cannot_cross_next_generation_after_data_ack() {
    // Deterministic native-facts model; the data permit, physical store,
    // dispatcher guard/FIFO, and return channels are the actual components.
    let (_tmp, mut worker, mut d, lease, operation) = integrated_accepted();
    let (t, r, k) = worker.response.take().unwrap();
    worker.respond(t, r, k);
    d.event_quantum().unwrap();
    d.response_quantum().unwrap();
    dispatch_flush(&mut d);
    let a = worker.active.as_mut().unwrap();
    let key = a.key;
    assert!(matches!(a.start.poll(), StartState::Granted));
    a.attempted = true;
    a.out.observe(v2::Channel::Stdout, 1, true).unwrap();
    a.out.ordinal = 1;
    let mut permit = worker.tokens.pop().unwrap();
    permit.storage[0] = b'x';
    a.outstanding[permit.id as usize] = Some(1);
    worker
        .ports
        .data
        .try_send(DataChunk {
            key,
            ordinal: 1,
            channel: v2::Channel::Stdout,
            offset: 0,
            len: 1,
            permit,
        })
        .ok()
        .unwrap();
    let status = if cfg!(windows) {
        v2::NativeStatus::WindowsExit { code: 0 }
    } else {
        v2::NativeStatus::PosixExit { code: 0 }
    };
    worker
        .finish(v2::Outcome::RunCompleted {
            reason: v2::RunReason::Natural,
            status,
            cleanup: cleanup_fact(),
            stdout_bytes: 1,
            stderr_bytes: 0,
            truncated: false,
        })
        .unwrap_or_else(|_| panic!("physical finish"));
    assert!(
        worker.active.as_ref().unwrap().committed,
        "terminal commits before data acknowledgment"
    );
    d.event_quantum().unwrap();
    d.event_quantum().unwrap();
    request(&mut d, 4, v2::Request::QueryOperation { lease, operation });
    assert!(!d.response_quantum().unwrap());
    d.data_quantum().unwrap();
    dispatch_flush(&mut d);
    assert!(d.response_quantum().unwrap());
    dispatch_flush(&mut d);
    assert!(d.return_quantum().unwrap()); // exact original data token
    assert!(d.return_quantum().unwrap()); // ProducerDrained, old barrier retired
    request(
        &mut d,
        5,
        v2::Request::Run {
            lease,
            operation: v2::OperationId::from_bytes([5; 16]),
            program: v2::ProgramId::new("probe").unwrap(),
        },
    );
    // This is precisely the late-old-event window: new dispatcher key,
    // old worker context, and a not-yet-consumed drain acknowledgment.
    worker
        .advance()
        .unwrap_or_else(|_| panic!("late old advance"));
    assert!(
        !d.event_quantum().unwrap(),
        "no duplicate final boundary crosses retirement"
    );
    worker.returns();
    assert!(worker.active.is_none());
    worker
        .command(worker.ports.commands.try_recv().ok().unwrap())
        .unwrap_or_else(|_| panic!("next run"));
    assert!(d.event_quantum().unwrap());
    assert_eq!(
        worker.active.as_ref().unwrap().key.generation,
        key.generation + 1
    );
    assert!(!worker.active.as_ref().unwrap().attempted);
}
#[test]
fn closed_history_replays_before_effect_admission() {
    for quarantine in [false, true] {
        for choice in 0..3 {
            let (_tmp, mut worker, ports, [p0, p1], inv) = fixture();
            let binding = inv.expected.binding;
            let manifest_digest = inv.expected.manifest_digest;
            worker.initialize(inv).unwrap_or_else(|_| panic!("init"));
            ports.events.try_recv().ok().unwrap();
            worker
                .acquire(
                    v2::Request::Acquire {
                        intent: v2::IntentId::from_bytes([3; 16]),
                        binding,
                        manifest_digest,
                    },
                    ResponseTicket {
                        reply_to: 1,
                        permit: p0,
                    },
                )
                .unwrap_or_else(|_| panic!("acquire"));
            ports.events.try_recv().ok().unwrap();
            worker.response.take();
            let lease = worker.lease.as_ref().unwrap().1;
            let operation = v2::OperationId::from_bytes([4; 16]);
            let mut req = v2::Request::Run {
                lease,
                operation,
                program: v2::ProgramId::new("probe").unwrap(),
            };
            worker
                .append(Entry::OperationAccepted {
                    lease,
                    operation,
                    request_digest: v2::request_digest(&req).unwrap(),
                    operation_kind: v2::OperationKind::Run,
                })
                .unwrap_or_else(|_| panic!("accept"));
            if !quarantine {
                worker
                    .append(Entry::OperationFinished {
                        lease,
                        operation,
                        outcome: v2::Outcome::NotStarted {
                            reason: v2::NotStartedReason::CancelRequested,
                        },
                    })
                    .unwrap_or_else(|_| panic!("finish"));
                worker
                    .append(Entry::LeaseClosed {
                        lease,
                        disposition: Disposition::AwaitingExternalReset,
                    })
                    .unwrap_or_else(|_| panic!("close"));
            }
            let inv = worker.invocation.take().unwrap();
            worker.store.take();
            worker.lease = None;
            worker.initialize(inv).unwrap_or_else(|_| panic!("reopen"));
            ports.events.try_recv().ok().unwrap();
            let expected = worker
                .store
                .as_ref()
                .unwrap()
                .restart_view()
                .query_operation(lease, operation)
                .unwrap();
            if choice == 1 {
                req = v2::Request::Run {
                    lease,
                    operation,
                    program: v2::ProgramId::new("different").unwrap(),
                };
            }
            if choice == 2 {
                req = v2::Request::Run {
                    lease,
                    operation: v2::OperationId::from_bytes([5; 16]),
                    program: v2::ProgramId::new("probe").unwrap(),
                };
            }
            let op = if choice == 2 {
                v2::OperationId::from_bytes([5; 16])
            } else {
                operation
            };
            let key = RunKey {
                lease,
                operation: op,
                generation: 1,
            };
            let (_, start) = start_pair(key, 2);
            let before = worker.store.as_ref().unwrap().records().len();
            worker
                .begin(
                    req,
                    ResponseTicket {
                        reply_to: 2,
                        permit: p1,
                    },
                    CancelToken {
                        key,
                        flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    },
                    start,
                )
                .unwrap_or_else(|_| panic!("replay"));
            let actual = if let Some((_, reply, _)) = worker.response.take() {
                reply
            } else {
                let WorkerEvent::ResponseReady { reply, .. } =
                    ports.events.try_recv().ok().unwrap()
                else {
                    panic!("response")
                };
                reply
            };
            let wanted = match choice {
                0 => v2::Reply::Operation {
                    lease,
                    operation,
                    state: expected,
                },
                1 => v2::Reply::Refused {
                    code: v2::RefusalCode::ReplayConflict,
                    inspect_failure: None,
                },
                _ => v2::Reply::Refused {
                    code: if quarantine {
                        v2::RefusalCode::LeaseQuarantined
                    } else {
                        v2::RefusalCode::AwaitingExternalReset
                    },
                    inspect_failure: None,
                },
            };
            assert_eq!(actual, wanted);
            assert_eq!(before, worker.store.as_ref().unwrap().records().len());
            assert!(
                worker
                    .active
                    .as_ref()
                    .is_none_or(|a| !a.attempted && a.child.is_none())
            );
        }
    }
}
#[test]
fn cancel_and_deadline_before_attempt_commit_through_saturated_events() {
    // Private grant/clock/event-saturation model; appends are the actual store barrier.
    for deadline in [false, true] {
        let (_tmp, mut worker, ports, [p0, p1], inv) = fixture();
        let binding = inv.expected.binding;
        let manifest_digest = inv.expected.manifest_digest;
        worker
            .initialize(inv)
            .unwrap_or_else(|_| panic!("fixture init"));
        let intent = v2::IntentId::from_bytes([3; 16]);
        worker
            .acquire(
                v2::Request::Acquire {
                    intent,
                    binding,
                    manifest_digest,
                },
                ResponseTicket {
                    reply_to: 1,
                    permit: p0,
                },
            )
            .unwrap_or_else(|_| panic!("fixture acquire"));
        let lease = worker.lease.as_ref().unwrap().1;
        let operation = v2::OperationId::from_bytes([4; 16]);
        let key = RunKey {
            lease,
            operation,
            generation: 1,
        };
        let (_grant, start) = start_pair(key, 2);
        // Acquisition reply is safely abandoned before any native submission in this model.
        let _acquire_response = worker.response.take().unwrap();
        worker
            .begin(
                v2::Request::Run {
                    lease,
                    operation,
                    program: v2::ProgramId::new("probe").unwrap(),
                },
                ResponseTicket {
                    reply_to: 2,
                    permit: p1,
                },
                CancelToken {
                    key,
                    flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(!deadline)),
                },
                start,
            )
            .unwrap_or_else(|_| panic!("fixture accept"));
        assert!(worker.pending.is_some());
        let accepted = worker.active.as_ref().unwrap().accepted_at;
        let expires = worker.active.as_ref().unwrap().deadline;
        assert_eq!(expires.duration_since(accepted), Duration::from_secs(30));
        worker
            .advance_at(if deadline { expires } else { accepted })
            .unwrap_or_else(|_| panic!("terminal persistence"));
        let active = worker.active.as_ref().unwrap();
        assert!(!active.attempted);
        assert!(active.committed);
        assert!(active.child.is_none());
        assert_eq!(active.out.ordinal, 0);
        assert!(
            matches!(worker.store.as_ref().unwrap().records().last().unwrap().entry(),Entry::OperationFinished{outcome:v2::Outcome::NotStarted{reason},..} if *reason==if deadline{v2::NotStartedReason::Deadline}else{v2::NotStartedReason::CancelRequested})
        );
        assert!(ports.events.try_recv().is_ok());
        assert!(ports.events.try_recv().is_ok());
    }
}
#[test]
fn full_ledger_and_marker_validation_precedes_description() {
    for corruption in [0, 1, 2] {
        let (_tmp, mut worker, ports, _controls, mut invocation) = fixture();
        match corruption {
            0 => {
                let mut file = std::fs::OpenOptions::new()
                    .append(true)
                    .open(invocation.ledger_root.join("ledger.bin"))
                    .unwrap();
                use std::io::Write;
                file.write_all(&[0]).unwrap();
            }
            1 => {
                std::fs::write(
                    invocation.workspace_root.join(".theseus-workspace-marker"),
                    b"wrong",
                )
                .unwrap();
            }
            _ => invocation.expected.manifest_digest = v2::Digest::from_bytes([9; 32]),
        }
        assert!(worker.initialize(invocation).is_err());
        assert!(ports.events.try_recv().is_err());
        assert!(worker.store.is_none());
    }
}
