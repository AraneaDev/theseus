// Consumer transport model: encoded frames cross the same bounded
// Frame::advance / Client::consume_residual boundaries as native I/O. The
// owned child is our test executable --list, only to exercise actual
// Client start/reconnect/stop/join ownership. It is NOT a protocol peer or
// helper fault fixture, and no bytes are attributed to its native pipes.
use super::*;
use std::time::{Duration, Instant};
use theseus_native_helper_store::{IntentRecord, IntentStore};
fn command() -> NativeCommand {
    let mut c = NativeCommand::new(std::env::current_exe().unwrap());
    c.arg("--list");
    c.environment = theseus_host::process::ChildEnvironment::Replace(vec![]);
    c
}
fn client() -> Client {
    match Client::start(command(), crate::pump::tests::expected()) {
        Ok(c) => c,
        Err(_) => panic!("trusted model owner spawn failed"),
    }
}
fn sent(c: &mut Client) -> Vec<v2::Envelope> {
    let pump = c.settlement.owner.as_mut().unwrap().pump.as_mut().unwrap();
    let mut bytes = Vec::new();
    while let Some(frame) = pump.frames.front_mut() {
        let done = frame
            .advance(|chunk| {
                let n = chunk.len().min(7);
                assert!(bytes.len() + n <= 2 * v2::MAX_FRAME_BYTES);
                bytes.extend_from_slice(&chunk[..n]);
                Ok(n)
            })
            .unwrap();
        if done {
            pump.frames.pop_front();
        }
    }
    let mut decoder = v2::Decoder::new();
    let mut frames = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let progress = decoder.feed(&bytes[offset..]).unwrap();
        offset += progress.consumed;
        if let Some(frame) = progress.frame {
            frames.push(frame);
        }
    }
    decoder.finish().unwrap();
    frames
}
fn feed(
    c: &mut Client,
    session: &v2::SessionId,
    sequence: u64,
    message: v2::Message,
) -> Result<Option<Event>, ClientError> {
    let bytes =
        v2::encode(&v2::Envelope::new(session.clone(), sequence, message).unwrap()).unwrap();
    assert!(bytes.len() <= 4096);
    c.settlement
        .owner
        .as_mut()
        .unwrap()
        .pump
        .as_mut()
        .unwrap()
        .residual
        .extend_from_slice(&bytes);
    c.consume_residual()
}
fn ready(c: &mut Client) -> v2::SessionId {
    let hello = sent(c);
    assert_eq!(hello.len(), 1);
    assert_eq!(hello[0].sequence(), 0);
    assert!(matches!(
        hello[0].message(),
        v2::Message::Hello {
            role: v2::Role::Controller
        }
    ));
    let session = hello[0].session().clone();
    assert!(
        feed(
            c,
            &session,
            0,
            v2::Message::Hello {
                role: v2::Role::Helper
            }
        )
        .unwrap()
        .is_none()
    );
    let describe = sent(c);
    assert_eq!(describe.len(), 1);
    assert_eq!(describe[0].sequence(), 1);
    assert!(matches!(
        describe[0].message(),
        v2::Message::Request {
            request: v2::Request::Describe
        }
    ));
    assert!(matches!(
        feed(
            c,
            &session,
            1,
            v2::Message::Response {
                reply_to: 1,
                result: crate::pump::tests::description()
            }
        )
        .unwrap(),
        Some(Event::Ready)
    ));
    for _ in 0..3 {
        assert!(
            sent(c).is_empty(),
            "automatic post-preflight action transmitted"
        );
    }
    session
}
fn settle(c: &mut Client) {
    c.request_stop();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !c.poll_settle().unwrap() {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
}
fn reconnect(c: &mut Client, old: &v2::SessionId) -> v2::SessionId {
    assert!(matches!(
        c.connection_lost().unwrap(),
        Poll::Event(Event::ConnectionLost)
    ));
    settle(c);
    c.reconnect(command()).unwrap();
    let session = ready(c);
    assert_ne!(&session, old);
    session
}
fn prepared() -> (
    tempfile::TempDir,
    theseus_native_helper_store::PreparedAcquire,
) {
    let temp = tempfile::tempdir().unwrap();
    let mut store = IntentStore::new(
        theseus_host::fs::HostDir::create_private(&temp.path().join("intent")).unwrap(),
    );
    let p = store
        .prepare(IntentRecord {
            intent: v2::IntentId::from_bytes([8; 16]),
            binding: v2::BindingId::from_bytes([1; 16]),
            manifest_digest: v2::Digest::from_bytes([2; 32]),
            workspace_marker: v2::WorkspaceMarker::from_bytes([3; 16]),
        })
        .unwrap();
    (temp, p)
}
fn reply(state: v2::AcquireView) -> v2::Reply {
    v2::Reply::Acquired {
        intent: v2::IntentId::from_bytes([8; 16]),
        binding: v2::BindingId::from_bytes([1; 16]),
        manifest_digest: v2::Digest::from_bytes([2; 32]),
        state,
    }
}
#[test]
fn r3_transmitted_acquire_reply_absent_then_fresh_epoch_only_explicit_same_intent_query() {
    let (_temp, p) = prepared();
    let mut c = client();
    let old = ready(&mut c);
    let acquire = c.submit(Action::Acquire(&p)).unwrap();
    let first = sent(&mut c);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].sequence(), 2);
    assert_eq!(
        first[0].message(),
        &v2::Message::Request {
            request: p.request()
        }
    );
    // Model the remote committed reservation, but intentionally deliver NO
    // Acquired frame before EOF. No client state can observe this result.
    let lease = v2::LeaseId::from_bytes([5; 16]);
    let committed = reply(v2::AcquireView::Lease {
        lease,
        state: v2::LeaseView::Held {
            intent: p.record().intent,
        },
    });
    let session = reconnect(&mut c, &old);
    let recovery = c.submit(Action::RecoverAcquire(&p)).unwrap();
    assert_ne!(acquire, recovery);
    let second = sent(&mut c);
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].sequence(), 2);
    assert_eq!(
        second[0].message(),
        &v2::Message::Request {
            request: v2::Request::QueryAcquire {
                intent: p.record().intent,
                binding: p.record().binding,
                manifest_digest: p.record().manifest_digest
            }
        }
    );
    assert!(
        matches!(feed(&mut c,&session,2,v2::Message::Response { reply_to:2,result:committed }).unwrap(),Some(Event::Reply { request, .. }) if request==recovery)
    );
    assert!(sent(&mut c).is_empty());
    settle(&mut c);
}
#[test]
fn r3_transmitted_lost_run_has_no_automatic_action_then_only_explicit_original_query() {
    let mut c = client();
    let old = ready(&mut c);
    let lease = v2::LeaseId::from_bytes([5; 16]);
    let operation = v2::OperationId::from_bytes([9; 16]);
    c.submit(Action::Run {
        lease,
        operation,
        program: v2::ProgramId::new("probe").unwrap(),
    })
    .unwrap();
    let first = sent(&mut c);
    assert_eq!(first.len(), 1);
    assert!(
        matches!(first[0].message(),v2::Message::Request { request:v2::Request::Run { lease:l,operation:o,.. } } if *l==lease && *o==operation)
    );
    let session = reconnect(&mut c, &old);
    c.submit(Action::Query { lease, operation }).unwrap();
    let recovered = sent(&mut c);
    assert_eq!(recovered.len(), 1);
    assert_eq!(
        recovered[0].message(),
        &v2::Message::Request {
            request: v2::Request::QueryOperation { lease, operation }
        }
    );
    feed(
        &mut c,
        &session,
        2,
        v2::Message::Response {
            reply_to: 2,
            result: v2::Reply::Operation {
                lease,
                operation,
                state: v2::OperationView::Running,
            },
        },
    )
    .unwrap();
    assert!(sent(&mut c).is_empty());
    settle(&mut c);
}
#[test]
fn r2_client_epochs_keep_observed_lease_and_allow_only_same_identity_progression() {
    let (_temp, p) = prepared();
    let lease = v2::LeaseId::from_bytes([5; 16]);
    for contradiction in [
        v2::AcquireView::NotFound,
        v2::AcquireView::Lease {
            lease: v2::LeaseId::from_bytes([6; 16]),
            state: v2::LeaseView::Held {
                intent: p.record().intent,
            },
        },
    ] {
        let mut c = client();
        let old = ready(&mut c);
        c.submit(Action::Acquire(&p)).unwrap();
        sent(&mut c);
        feed(
            &mut c,
            &old,
            2,
            v2::Message::Response {
                reply_to: 2,
                result: reply(v2::AcquireView::Lease {
                    lease,
                    state: v2::LeaseView::Held {
                        intent: p.record().intent,
                    },
                }),
            },
        )
        .unwrap();
        let session = reconnect(&mut c, &old);
        c.submit(Action::RecoverAcquire(&p)).unwrap();
        sent(&mut c);
        assert!(matches!(
            feed(
                &mut c,
                &session,
                2,
                v2::Message::Response {
                    reply_to: 2,
                    result: reply(contradiction)
                }
            ),
            Err(ClientError::Protocol)
        ));
        settle(&mut c);
    }
    let mut c = client();
    let old = ready(&mut c);
    c.submit(Action::RecoverAcquire(&p)).unwrap();
    sent(&mut c);
    feed(
        &mut c,
        &old,
        2,
        v2::Message::Response {
            reply_to: 2,
            result: reply(v2::AcquireView::NotFound),
        },
    )
    .unwrap();
    let second = reconnect(&mut c, &old);
    c.submit(Action::RecoverAcquire(&p)).unwrap();
    sent(&mut c);
    feed(
        &mut c,
        &second,
        2,
        v2::Message::Response {
            reply_to: 2,
            result: reply(v2::AcquireView::Lease {
                lease,
                state: v2::LeaseView::Held {
                    intent: p.record().intent,
                },
            }),
        },
    )
    .unwrap();
    let third = reconnect(&mut c, &second);
    c.submit(Action::RecoverAcquire(&p)).unwrap();
    sent(&mut c);
    feed(
        &mut c,
        &third,
        2,
        v2::Message::Response {
            reply_to: 2,
            result: reply(v2::AcquireView::Lease {
                lease,
                state: v2::LeaseView::AwaitingExternalReset {
                    intent: p.record().intent,
                },
            }),
        },
    )
    .unwrap();
    settle(&mut c);
}
#[test]
fn r1_client_epochs_reject_contradictory_terminal_without_emitting_reply() {
    let lease = v2::LeaseId::from_bytes([5; 16]);
    let operation = v2::OperationId::from_bytes([9; 16]);
    let completed = |truncated| v2::Outcome::RunCompleted {
        reason: v2::RunReason::Natural,
        status: v2::NativeStatus::PosixExit { code: 0 },
        cleanup: v2::Cleanup::PosixLeaderReapedGroupTerminationRequested,
        stdout_bytes: 10,
        stderr_bytes: 0,
        truncated,
    };
    for evidence in 0..4 {
        let mut c = client();
        let old = ready(&mut c);
        c.submit(Action::Run {
            lease,
            operation,
            program: v2::ProgramId::new("probe").unwrap(),
        })
        .unwrap();
        sent(&mut c);
        feed(
            &mut c,
            &old,
            2,
            v2::Message::Response {
                reply_to: 2,
                result: v2::Reply::Operation {
                    lease,
                    operation,
                    state: v2::OperationView::Running,
                },
            },
        )
        .unwrap();
        if evidence == 3 {
            feed(
                &mut c,
                &old,
                3,
                v2::Message::Log {
                    lease,
                    operation,
                    code: v2::LogCode::OutputTruncated,
                },
            )
            .unwrap();
        } else {
            feed(
                &mut c,
                &old,
                3,
                v2::Message::Output {
                    lease,
                    operation,
                    channel: v2::Channel::Stdout,
                    offset: 0,
                    bytes: v2::FileBytes::new(&[1]).unwrap(),
                },
            )
            .unwrap();
            if evidence == 2 {
                feed(
                    &mut c,
                    &old,
                    4,
                    v2::Message::Output {
                        lease,
                        operation,
                        channel: v2::Channel::Stdout,
                        offset: 3,
                        bytes: v2::FileBytes::new(&[2]).unwrap(),
                    },
                )
                .unwrap();
            }
        }
        let session = reconnect(&mut c, &old);
        c.submit(Action::Query { lease, operation }).unwrap();
        sent(&mut c);
        let bad = match evidence {
            0 => v2::Outcome::NotStarted {
                reason: v2::NotStartedReason::Deadline,
            },
            1 => v2::Outcome::LaunchFailed {
                phase: v2::LaunchPhase::NeverCreated,
                code: v2::FailureCode::Io,
                cleanup: v2::SpawnCleanup::NoChildCreated,
            },
            _ => completed(false),
        };
        assert!(matches!(
            feed(
                &mut c,
                &session,
                2,
                v2::Message::Response {
                    reply_to: 2,
                    result: v2::Reply::Operation {
                        lease,
                        operation,
                        state: v2::OperationView::Finished { outcome: bad }
                    }
                }
            ),
            Err(ClientError::Protocol)
        ));
        assert_eq!(
            c.submit(Action::Run {
                lease,
                operation: v2::OperationId::from_bytes([10; 16]),
                program: v2::ProgramId::new("probe").unwrap()
            }),
            Err(ClientError::RecoveryRequired)
        );
        let next = reconnect(&mut c, &session);
        c.submit(Action::Query { lease, operation }).unwrap();
        sent(&mut c);
        assert!(matches!(
            feed(
                &mut c,
                &next,
                2,
                v2::Message::Response {
                    reply_to: 2,
                    result: v2::Reply::Operation {
                        lease,
                        operation,
                        state: v2::OperationView::Finished {
                            outcome: completed(evidence >= 2)
                        }
                    }
                }
            )
            .unwrap(),
            Some(Event::Reply { .. })
        ));
        settle(&mut c);
    }
}
#[test]
fn create_reconnect_preserves_identity_checks_receipt_and_never_automatically_reexecutes() {
    use sha2::{Digest as _, Sha256};
    for bad in 0..4 {
        let mut c = client();
        let old = ready(&mut c);
        let lease = v2::LeaseId::from_bytes([1; 16]);
        let operation = v2::OperationId::from_bytes([2; 16]);
        c.submit(Action::CreateFile {
            lease,
            operation,
            path: v2::LeafName::new("note").unwrap(),
            bytes: v2::FileBytes::new(b"hello").unwrap(),
        })
        .unwrap();
        let frames = sent(&mut c);
        assert!(matches!(
            frames[0].message(),
            v2::Message::Request {
                request: v2::Request::CreateFile { .. }
            }
        ));
        let session = reconnect(&mut c, &old);
        assert!(sent(&mut c).is_empty());
        let query = c.submit(Action::Query { lease, operation }).unwrap();
        let frames = sent(&mut c);
        assert!(matches!(
            frames[0].message(),
            v2::Message::Request {
                request: v2::Request::QueryOperation { .. }
            }
        ));
        let digest = v2::Digest::from_bytes(Sha256::digest(b"hello").into());
        let state = match bad {
            0 => v2::OperationView::Finished {
                outcome: v2::Outcome::FileCreated { length: 4, digest },
            },
            1 => v2::OperationView::Finished {
                outcome: v2::Outcome::FileCreated {
                    length: 5,
                    digest: v2::Digest::from_bytes([0; 32]),
                },
            },
            2 => v2::OperationView::Accepted {
                operation_kind: v2::OperationKind::Run,
            },
            _ => v2::OperationView::Finished {
                outcome: v2::Outcome::LaunchFailed {
                    phase: v2::LaunchPhase::NeverCreated,
                    code: v2::FailureCode::Io,
                    cleanup: v2::SpawnCleanup::NoChildCreated,
                },
            },
        };
        assert!(matches!(
            feed(
                &mut c,
                &session,
                2,
                v2::Message::Response {
                    reply_to: 2,
                    result: v2::Reply::Operation {
                        lease,
                        operation,
                        state
                    }
                }
            ),
            Err(ClientError::Protocol)
        ));
        assert_eq!(
            c.submit(Action::CreateFile {
                lease,
                operation: v2::OperationId::from_bytes([3; 16]),
                path: v2::LeafName::new("other").unwrap(),
                bytes: v2::FileBytes::new(&[]).unwrap()
            }),
            Err(ClientError::RecoveryRequired)
        );
        let next = reconnect(&mut c, &session);
        let next_query = c.submit(Action::Query { lease, operation }).unwrap();
        assert_ne!(query, next_query);
        sent(&mut c);
        assert!(
            matches!(feed(&mut c,&next,2,v2::Message::Response {reply_to:2,result:v2::Reply::Operation {lease,operation,state:v2::OperationView::Finished {outcome:v2::Outcome::FileCreated {length:5,digest}}}}).unwrap(),Some(Event::Reply {request,..}) if request == next_query)
        );
        settle(&mut c);
    }
}
