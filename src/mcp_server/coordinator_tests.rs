use super::*;
use std::collections::VecDeque;
#[derive(Default)]
struct FakeTransport {
    input: VecDeque<Vec<u8>>,
    output: Vec<u8>,
    writes: VecDeque<WriteStep>,
    reads: usize,
    write_calls: usize,
    in_flight: bool,
    stopped: bool,
    settle: bool,
    attempts: Vec<(usize, Vec<u8>)>,
}
impl Transport for FakeTransport {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<ReadStep> {
        assert!(bytes.len() <= 4096);
        assert!(!self.stopped);
        self.reads += 1;
        let Some(input) = self.input.pop_front() else {
            return Ok(ReadStep::Pending);
        };
        assert!(input.len() <= bytes.len());
        bytes[..input.len()].copy_from_slice(&input);
        Ok(ReadStep::Bytes(input.len()))
    }
    fn write(&mut self, bytes: &[u8]) -> io::Result<WriteStep> {
        assert!(bytes.len() <= 4096);
        assert!(!self.stopped);
        self.write_calls += 1;
        self.attempts
            .push((bytes.as_ptr() as usize, bytes.to_vec()));
        let result = self.writes.pop_front().unwrap_or(WriteStep::Pending);
        if let WriteStep::Bytes(n) = result {
            self.output.extend_from_slice(&bytes[..n]);
            if n > 0 {
                self.in_flight = false;
            }
        }
        Ok(result)
    }
    fn write_in_flight(&self) -> bool {
        self.in_flight
    }
    fn stop(&mut self) {
        self.stopped = true;
    }
    fn poll_stop(&mut self) -> io::Result<bool> {
        Ok(self.settle)
    }
    fn settled(&self) -> bool {
        self.settle
    }
}
fn runtime() -> ServerRuntime<FakeTransport> {
    let mut r = ServerRuntime::base(
        FakeTransport::default(),
        None,
        Instance::parse("0123456789abcdef0123456789abcdef").unwrap(),
        3600,
        CancellationToken::default(),
    );
    r.wire.selected = Some(Era::Legacy);
    r.wire.legacy_initialized = true;
    r
}
fn ping(id: i64) -> Vec<u8> {
    format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"ping\"}}\n").into_bytes()
}
fn cancel(id: i64) -> Vec<u8> {
    format!("{{\"jsonrpc\":\"2.0\",\"method\":\"notifications/cancelled\",\"params\":{{\"requestId\":{id}}}}}\n").into_bytes()
}
#[test]
fn four_slots_keep_notification_cancellation_serviceable() {
    let now = Instant::now();
    let mut r = runtime();
    r.transport
        .input
        .push_back([ping(1), ping(2), ping(3), ping(4), cancel(2)].concat());
    assert_eq!(r.poll(now), RuntimeAction::Continue);
    assert!(!r.shutdown.active());
    assert_eq!(r.requests.iter().flatten().count(), 3);
    assert!(r.transport.reads <= 4);
    assert!(r.transport.write_calls <= 4);
}
#[test]
fn fifth_request_closes_without_a_fifth_response() {
    let now = Instant::now();
    let mut r = runtime();
    r.transport
        .input
        .push_back((1..=5).flat_map(ping).collect());
    r.poll(now);
    assert!(r.shutdown.active());
    assert!(r.transport.stopped);
    assert_eq!(r.requests.iter().flatten().count(), 4);
}
#[test]
fn pending_write_reuses_identical_operation_and_zero_does_not_reset_stall() {
    let now = Instant::now();
    let mut r = runtime();
    r.transport.input.push_back(ping(1));
    r.poll(now);
    r.transport.in_flight = true;
    r.poll(now + Duration::from_secs(4));
    assert_eq!(r.transport.attempts[0], r.transport.attempts[1]);
    r.transport.writes.push_back(WriteStep::Bytes(0));
    r.poll(now + Duration::from_secs(5));
    assert!(r.shutdown.active());
}
#[test]
fn positive_progress_resets_stall_and_partial_cancel_closes() {
    let now = Instant::now();
    let mut r = runtime();
    r.transport.input.push_back(ping(1));
    r.poll(now);
    r.transport.writes.push_back(WriteStep::Bytes(1));
    r.poll(now + Duration::from_secs(4));
    r.poll(now + Duration::from_secs(5));
    assert!(!r.shutdown.active());
    r.transport.input.push_back(cancel(1));
    r.poll(now + Duration::from_secs(6));
    assert!(r.shutdown.active());
    assert_eq!(r.transport.output.len(), 1);
}
#[test]
fn uncertain_write_cancellation_never_releases_or_splices_slot() {
    let now = Instant::now();
    let mut r = runtime();
    r.transport.input.push_back(ping(1));
    r.poll(now);
    r.transport.in_flight = true;
    r.transport.input.push_back([cancel(1), ping(2)].concat());
    r.poll(now);
    assert!(r.shutdown.active());
    assert_eq!(r.requests.iter().flatten().count(), 1);
}

fn inspect() -> ToolCall {
    ToolCall::RecipeInspect(InspectArgs {
        recipe_path: "recipe.toml".into(),
        recipe_digest: None,
        selector: InspectSelector::Overview,
        limit: None,
        cursor: None,
    })
}
fn request(r: &mut ServerRuntime<FakeTransport>, id: i64, call: ToolCall, now: Instant) {
    r.event(
        WireEvent::Request(Request {
            id: RpcId::Integer(id),
            era: Era::Legacy,
            method: Method::ToolCall(call),
        }),
        now,
    );
}
fn args(r: &ServerRuntime<FakeTransport>) -> StartArgs {
    serde_json::from_value(serde_json::json!({"server_instance":r.instance,"request_key":"first","recipe_path":"theseus.toml","recipe_digest":"0".repeat(64),"profiles":["user"],"scenarios":["clean"]})).unwrap()
}
fn block_query(
    r: &mut ServerRuntime<FakeTransport>,
    success: bool,
) -> (
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::SyncSender<()>,
) {
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    r.query = Some(
        QueryWorker::spawn_executor(move |_, _| {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            if success {
                Ok(QueryValue::Validated(
                    crate::recipe::parse_bytes(
                        std::path::Path::new(concat!(
                            env!("CARGO_MANIFEST_DIR"),
                            "/tests/fixtures/recipes/v2_clean.toml"
                        )),
                        include_bytes!("../../tests/fixtures/recipes/v2_clean.toml"),
                    )
                    .unwrap(),
                ))
            } else {
                Err(QueryError::new(
                    ErrorCode::RecipeInvalid,
                    anyhow::anyhow!("ordinary error"),
                ))
            }
        })
        .unwrap(),
    );
    (entered_rx, release_tx)
}
fn collect(r: &mut ServerRuntime<FakeTransport>, now: Instant) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while r.active_query.is_some() {
        r.control(now);
        assert!(Instant::now() < deadline, "query completion watchdog");
        std::thread::yield_now();
    }
}
fn finish(r: &mut ServerRuntime<FakeTransport>, now: Instant) {
    r.transport.settle = true;
    r.begin_shutdown(now, false);
    let deadline = Instant::now() + Duration::from_secs(5);
    while r.poll(now) == RuntimeAction::Continue {
        assert!(Instant::now() < deadline, "join watchdog");
        std::thread::yield_now();
    }
    assert!(r.query.is_none());
    assert!(r.engine.is_none());
}
#[test]
fn query_timeout_keeps_worker_busy_but_services_cached_list_and_cancel() {
    let now = Instant::now();
    let mut r = runtime();
    let (entered, release) = block_query(&mut r, false);
    request(&mut r, 1, inspect(), now);
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    r.poll(now + Duration::from_secs(30));
    assert!(r.query.as_ref().unwrap().is_busy());
    assert!(!r.query_available());
    let instance = r.instance.clone();
    request(
        &mut r,
        2,
        ToolCall::RunList(ListArgs {
            server_instance: instance,
            limit: None,
            cursor: None,
        }),
        now + Duration::from_secs(30),
    );
    r.cancel_rpc(&RpcId::Integer(1), now + Duration::from_secs(30));
    let list = r
        .requests
        .iter()
        .flatten()
        .find(|s| s.reply.as_ref().unwrap().id == RpcId::Integer(2))
        .unwrap();
    assert!(r.slots.frame(list.slot).unwrap().is_some());
    release.send(()).unwrap();
    collect(&mut r, now + Duration::from_secs(30));
    assert_eq!(r.owners.len(), 0);
    assert!(!r.shutdown.active());
    finish(&mut r, now + Duration::from_secs(30));
}
#[test]
fn late_success_after_admission_timeout_never_dispatches_engine() {
    let now = Instant::now();
    let mut r = runtime();
    let (entered, release) = block_query(&mut r, true);
    let start = args(&r);
    request(&mut r, 1, ToolCall::RunStart(start), now);
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let handle = r.requests[0].as_ref().unwrap().handle.clone().unwrap();
    // Acknowledge admission reply before its independent validation deadline.
    let slot = r.requests[0].as_ref().unwrap().slot;
    let n = r.slots.frame(slot).unwrap().unwrap().bytes.len();
    r.transport.writes.push_back(WriteStep::Bytes(n));
    r.poll(now);
    r.cancel_rpc(&RpcId::Integer(1), now);
    assert_eq!(
        r.registry.cached(&handle).unwrap().state,
        RunState::Validating
    );
    r.poll(now + Duration::from_secs(30));
    release.send(()).unwrap();
    collect(&mut r, now + Duration::from_secs(30));
    assert!(r.engine.is_none());
    assert_eq!(
        r.registry.cached(&handle).unwrap().state,
        RunState::StartError
    );
    assert_eq!(
        r.registry
            .cached(&handle)
            .unwrap()
            .error
            .as_ref()
            .unwrap()
            .code,
        ErrorCode::QueryTimeout
    );
    finish(&mut r, now + Duration::from_secs(30));
}
#[test]
fn canceled_status_rpc_does_not_cancel_unrelated_run() {
    let now = Instant::now();
    let mut r = runtime();
    let AdmissionDecision::Reserved { handle, .. } = r.registry.admit(args(&r), true).unwrap()
    else {
        panic!()
    };
    let instance = r.instance.clone();
    request(
        &mut r,
        1,
        ToolCall::RunStatus(StatusArgs {
            server_instance: instance,
            handle: handle.clone(),
            refresh: false,
        }),
        now,
    );
    r.cancel_rpc(&RpcId::Integer(1), now);
    assert!(!r.registry.cached(&handle).unwrap().cancellation_requested);
}
#[test]
fn anonymous_protocol_error_and_empty_rpc_id_share_the_same_four_slots() {
    let now = Instant::now();
    let mut r = runtime();
    r.event(
        WireEvent::Request(Request {
            id: RpcId::String(String::new()),
            era: Era::Legacy,
            method: Method::Ping,
        }),
        now,
    );
    r.event(
        WireEvent::ProtocolError(ProtocolError {
            code: -32600,
            message: TextPreview::new("Invalid request", false),
            data: None,
            id: ReplyId::Null,
            close_after_send: false,
        }),
        now,
    );
    r.cancel_rpc(&RpcId::String(String::new()), now);
    assert_eq!(r.requests.iter().flatten().count(), 1);
    assert!(r.requests.iter().flatten().next().unwrap().reply.is_none());
}
#[test]
fn deferred_error_never_calls_display_or_source_on_coordinator() {
    use std::{
        fmt,
        sync::atomic::{AtomicUsize, Ordering},
    };
    #[derive(Debug)]
    struct Poison(Arc<AtomicUsize>);
    impl fmt::Display for Poison {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            self.0.fetch_add(1, Ordering::SeqCst);
            f.write_str("native owner")
        }
    }
    impl std::error::Error for Poison {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.0.fetch_add(1, Ordering::SeqCst);
            None
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let owner = OwnedError::deferred(QueryError::new(
        ErrorCode::InternalError,
        Poison(calls.clone()).into(),
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(owner.view().cleanup_blocked);
    assert!(owner.primary().is::<Poison>());
    let owner = std::thread::spawn(move || owner.finalize()).join().unwrap();
    assert!(calls.load(Ordering::SeqCst) > 0);
    assert!(!owner.view().cleanup_blocked);
    assert!(owner.primary().is::<Poison>());
}
#[test]
fn force_action_retains_real_native_owner_and_original_typed_primary() {
    use crate::runner::process::test_support::PendingFixture;
    let fixture = PendingFixture::new();
    #[derive(Debug)]
    struct Primary;
    impl std::fmt::Display for Primary {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("primary")
        }
    }
    impl std::error::Error for Primary {}
    let original = anyhow::Error::new(io::Error::other(fixture.failure.clone())).context(Primary);
    let owner = std::thread::spawn(move || {
        OwnedError::capture(QueryError::new(ErrorCode::WorkerError, original))
    })
    .join()
    .unwrap();
    let now = Instant::now();
    let mut r = runtime();
    r.transport.settle = true;
    r.owners.extend_registry(vec![owner]);
    r.begin_shutdown(now, false);
    assert_eq!(
        r.poll(now + Duration::from_secs(5)),
        RuntimeAction::ForceProcessExit(2)
    );
    assert!(r.owners.primary().unwrap().is::<Primary>());
    assert!(r.owners.iter().next().unwrap().view().cleanup_blocked);
    assert!(fixture.failure.pending());
    fixture.release_fault();
    let owners = r.owners.take_for_finalizer();
    r.owners.restore(
        std::thread::spawn(move || owners.into_iter().map(OwnedError::finalize).collect())
            .join()
            .unwrap(),
    );
    assert!(r.owners.primary().unwrap().is::<Primary>());
    assert!(!r.owners.iter().next().unwrap().view().cleanup_blocked);
}
#[test]
fn shutdown_waits_for_actual_query_join_and_discards_late_projection() {
    let now = Instant::now();
    let mut r = runtime();
    let (entered, release) = block_query(&mut r, false);
    request(&mut r, 1, inspect(), now);
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    r.stop.cancel();
    r.transport.settle = true;
    assert_eq!(r.poll(now), RuntimeAction::Continue);
    assert!(r.query.is_some());
    assert_eq!(
        r.poll(now + Duration::from_secs(5)),
        RuntimeAction::ForceProcessExit(2)
    );
    assert!(r.query.is_some());
    release.send(()).unwrap();
    collect(&mut r, now + Duration::from_secs(5));
    // Actual finished-thread observation, not completion, permits return.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let action = r.poll(now + Duration::from_secs(5));
        if r.query.is_none() {
            assert_eq!(action, RuntimeAction::OrderlyExit(0));
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(r.transport.output.is_empty());
    assert_eq!(r.owners.len(), 0);
}
#[test]
fn serialization_panic_is_retained_and_never_formatted_on_coordinator() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Witness(Arc<AtomicUsize>);
    impl Drop for Witness {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let now = Instant::now();
    let mut r = runtime();
    r.transport.input.push_back(ping(1));
    r.poll(now);
    let slot = r.requests[0].as_ref().unwrap().slot;
    let drops = Arc::new(AtomicUsize::new(0));
    let encoded =
        super::super::diagnostics::guard(|| std::panic::panic_any(Witness(drops.clone())));
    r.encoded(slot, encoded, now);
    assert!(r.shutdown.active());
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert!(r.owners.has_coordinator());
    finish(&mut r, now);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(r);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}
#[test]
fn an_empty_output_queue_clears_stall_clock_before_a_later_request() {
    let now = Instant::now();
    let mut r = runtime();
    r.transport.input.push_back(ping(1));
    r.poll(now);
    r.cancel_rpc(&RpcId::Integer(1), now);
    r.poll(now + Duration::from_secs(10));
    assert!(!r.shutdown.active());
}
#[test]
fn settled_query_error_lifetime_ends_at_response_newline_or_cancellation() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[derive(Debug)]
    struct Witness(Arc<AtomicUsize>);
    impl std::fmt::Display for Witness {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("ordinary query failure")
        }
    }
    impl std::error::Error for Witness {}
    impl Drop for Witness {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let now = Instant::now();
    let mut r = runtime();
    let drops = Arc::new(AtomicUsize::new(0));
    let observed = drops.clone();
    r.query = Some(
        QueryWorker::spawn_executor(move |_, _| {
            Err(QueryError::new(
                ErrorCode::IoError,
                Witness(observed.clone()).into(),
            ))
        })
        .unwrap(),
    );
    request(&mut r, 1, inspect(), now);
    collect(&mut r, now);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    r.cancel_rpc(&RpcId::Integer(1), now);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    request(&mut r, 2, inspect(), now);
    collect(&mut r, now);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    let slot = r.requests.iter().flatten().next().unwrap().slot;
    let n = r.slots.frame(slot).unwrap().unwrap().bytes.len();
    r.transport.writes.push_back(WriteStep::Bytes(n));
    r.poll(now);
    assert_eq!(drops.load(Ordering::SeqCst), 2);
    request(&mut r, 3, inspect(), now);
    r.cancel_rpc(&RpcId::Integer(3), now);
    collect(&mut r, now);
    assert_eq!(drops.load(Ordering::SeqCst), 3);
    assert_eq!(r.owners.len(), 0);
    finish(&mut r, now);
}
#[test]
fn ready_transport_work_is_limited_to_four_operations_per_direction() {
    let now = Instant::now();
    let mut r = runtime();
    for _ in 0..8 {
        r.transport
            .input
            .push_back(b"{\"jsonrpc\":\"2.0\",\"method\":\"unknown\"}\n".to_vec());
    }
    r.poll(now);
    assert_eq!(r.transport.reads, 4);
    assert_eq!(r.transport.input.len(), 4);
    r.transport.input.clear();
    r.transport.input.push_back(ping(1));
    r.transport.writes.extend([WriteStep::Bytes(1); 8]);
    r.poll(now);
    assert_eq!(r.transport.write_calls, 4);
    assert_eq!(r.transport.output.len(), 4);
    assert!(!r.shutdown.active());
}
#[test]
fn fatal_query_panic_cancels_admission_and_retains_payload_until_runtime_drops() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Witness(Arc<AtomicUsize>);
    impl Drop for Witness {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let now = Instant::now();
    let mut r = runtime();
    let drops = Arc::new(AtomicUsize::new(0));
    let observed = drops.clone();
    r.query = Some(
        QueryWorker::spawn_executor(move |_, _| std::panic::panic_any(Witness(observed.clone())))
            .unwrap(),
    );
    let start = args(&r);
    request(&mut r, 1, ToolCall::RunStart(start), now);
    let handle = r.requests[0].as_ref().unwrap().handle.clone().unwrap();
    collect(&mut r, now);
    assert!(r.shutdown.active());
    assert!(r.registry.cached(&handle).unwrap().cancellation_requested);
    assert_eq!(
        r.registry.cached(&handle).unwrap().cancellation_reason,
        Some(CancellationReason::ServerShutdown)
    );
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert!(r.owners.primary().is_some());
    finish(&mut r, now);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(r);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn saved_bindings_published_between_pre_drain_and_join_survive_worker_release() {
    use super::super::owned_error::EngineCompletion;
    use crate::model::{ResultCounts, RunOutcome};
    use std::{
        sync::{Mutex, mpsc},
        thread::JoinHandle,
    };
    use theseus_host::fs::RelativePath;
    struct RacingWorker {
        mailbox: Arc<Mutex<VecDeque<SavedBinding>>>,
        publish: Option<mpsc::SyncSender<()>>,
        thread: Option<JoinHandle<WorkerCompletion>>,
        before_join_reads: usize,
        after_join_reads: usize,
    }
    impl EnginePoll for RacingWorker {
        fn binding(&mut self) -> Option<SavedBinding> {
            if self.thread.is_some() {
                self.before_join_reads += 1;
            } else {
                self.after_join_reads += 1;
            }
            self.mailbox.lock().unwrap().pop_front()
        }
        fn joined(&mut self) -> Option<WorkerCompletion> {
            self.publish.take().unwrap().send(()).unwrap();
            let thread = self.thread.take().unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while !thread.is_finished() {
                assert!(Instant::now() < deadline, "publisher did not terminate");
                std::thread::yield_now();
            }
            Some(thread.join().unwrap())
        }
    }
    let now = Instant::now();
    let mut r = runtime();
    let AdmissionDecision::Reserved { handle, .. } = r.registry.admit(args(&r), true).unwrap()
    else {
        panic!("admission not reserved")
    };
    r.apply(&handle, RegistryUpdate::ValidationStarted { generation: 1 });
    let recipe = crate::recipe::parse_bytes(
        &std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/recipes/v2_clean.toml"
        ))
        .canonicalize()
        .unwrap(),
        include_bytes!("../../tests/fixtures/recipes/v2_clean.toml"),
    )
    .unwrap();
    r.apply(
        &handle,
        RegistryUpdate::Validation {
            generation: 1,
            result: Ok(recipe),
        },
    );
    let ready = r.registry.take_dispatch(&handle).unwrap();
    r.apply(&handle, RegistryUpdate::Dispatched);
    r.active_run = Some(ActiveRun {
        handle: handle.clone(),
        token: ready.token,
        deadline: now + Duration::from_secs(3600),
    });
    let mailbox = Arc::new(Mutex::new(VecDeque::with_capacity(2)));
    let published = mailbox.clone();
    let worker_handle = handle.clone();
    let (publish, release) = mpsc::sync_channel(1);
    let thread = std::thread::spawn(move || {
        release.recv_timeout(Duration::from_secs(3)).unwrap();
        let first = SavedBinding {
            handle: worker_handle.clone(),
            report_id: "saved-child".into(),
            child: RelativePath::parse("saved-child").unwrap(),
            hint_sequence: 1,
        };
        let mut latest = first.clone();
        latest.hint_sequence = 19;
        published.lock().unwrap().extend([first, latest]);
        WorkerCompletion::Engine(EngineCompletion {
            handle: worker_handle,
            result: Ok(FinalSummary {
                outcome: RunOutcome::Passed,
                counts: ResultCounts::default(),
                report_path: "output/saved-child/report.json".into(),
                journal_path: "output/saved-child/journal.json".into(),
            }),
        })
    });
    let mut worker = RacingWorker {
        mailbox,
        publish: Some(publish),
        thread: Some(thread),
        before_join_reads: 0,
        after_join_reads: 0,
    };
    let progress = poll_engine(&mut worker);
    assert_eq!(
        worker.before_join_reads, 1,
        "first observation was empty before publication"
    );
    r.apply_engine_progress(progress, now);
    assert!(r.engine.is_none());
    assert!(r.active_run.is_none());
    let view = r.registry.cached(&handle).unwrap();
    assert_eq!(view.state, RunState::Finished);
    assert_eq!(view.saved_hint_sequence.get(), 19);
    let binding = r
        .registry
        .binding(&handle)
        .unwrap()
        .expect("joined run keeps saved child authority");
    assert_eq!(binding.report_id, "saved-child");
    assert_eq!(binding.hint_sequence, 19);
    assert_eq!(
        worker.after_join_reads, 2,
        "bounded post-join drain reads both fixed cells"
    );
}
