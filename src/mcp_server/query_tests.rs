use super::*;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

fn reply() -> ReplyContext {
    ReplyContext {
        era: Era::Modern,
        id: RpcId::Integer(7),
        instance: Instance::parse(&"a".repeat(32)).unwrap(),
    }
}
fn work() -> QueryWork {
    QueryWork::Inspect {
        args: InspectArgs {
            recipe_path: "recipe.toml".into(),
            recipe_digest: None,
            selector: InspectSelector::Overview,
            limit: None,
            cursor: None,
        },
        reply: reply(),
    }
}
fn job(generation: u64) -> QueryJob {
    QueryJob {
        generation,
        destination: QueryDestination::Request(SlotId {
            index: 2,
            generation: 9,
        }),
        cancel: CancellationToken::default(),
        work: work(),
    }
}
fn complete(worker: &mut QueryWorker) -> QueryCompletion {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(c) = worker.try_complete() {
            return c;
        }
        assert!(Instant::now() < deadline, "completion was never published");
        std::thread::yield_now();
    }
}
#[allow(
    clippy::result_large_err,
    reason = "Preserve the worker's ownership-bearing error at the test join boundary."
)]
fn join(worker: &mut QueryWorker) -> Result<(), OwnedError> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(c) = worker.try_join() {
            return c;
        }
        assert!(Instant::now() < deadline, "worker never actually joined");
        std::thread::yield_now();
    }
}
fn ordinary() -> Result<QueryValue, QueryError> {
    Err(QueryError::new(
        ErrorCode::RecipeInvalid,
        anyhow::anyhow!("ordinary validation failure"),
    ))
}

// Break caught: treating cancellation as completion frees capacity while native work still runs.
#[test]
fn cancellation_does_not_free_capacity_before_actual_completion_collection() {
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let mut worker = QueryWorker::spawn_executor(move |_, _| {
        entered_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        ordinary()
    })
    .unwrap();
    let first = job(1);
    let token = first.cancel.clone();
    worker.try_submit(first).unwrap();
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    token.cancel();
    assert!(worker.is_busy());
    assert!(worker.try_complete().is_none());
    let second = job(2);
    let original_token = second.cancel.flag();
    let rejected = worker.try_submit(second).unwrap_err();
    assert_eq!(rejected.generation, 2);
    assert!(Arc::ptr_eq(&original_token, &rejected.cancel.flag()));
    release_tx.send(()).unwrap();
    let first = complete(&mut worker);
    assert_eq!(first.generation, 1);
    assert_eq!(
        first.destination,
        QueryDestination::Request(SlotId {
            index: 2,
            generation: 9
        })
    );
    assert_eq!(
        first.result.unwrap_err().view().code,
        ErrorCode::RecipeInvalid
    );
    assert!(!worker.is_busy());
    worker.try_submit(rejected).unwrap();
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    release_tx.send(()).unwrap();
    assert_eq!(complete(&mut worker).generation, 2);
    worker.request_stop();
    join(&mut worker).unwrap();
}

// Break caught: accepting a duplicate/backward generation can reuse a canceled destination.
#[test]
fn generations_are_strictly_monotonic_and_exhaust_without_wrapping() {
    let mut worker = QueryWorker::spawn_executor(|_, _| ordinary()).unwrap();
    assert_eq!(worker.try_submit(job(0)).unwrap_err().generation, 0);
    worker.try_submit(job(3)).unwrap();
    complete(&mut worker);
    for generation in [0, 1, 3] {
        assert_eq!(
            worker.try_submit(job(generation)).unwrap_err().generation,
            generation
        );
    }
    worker.try_submit(job(u64::MAX)).unwrap();
    complete(&mut worker);
    assert_eq!(worker.try_submit(job(4)).unwrap_err().generation, 4);
    worker.request_stop();
    join(&mut worker).unwrap();
}

// Break caught: stop forgets the active token, joins a message instead of the thread, or loses a queued owner.
#[test]
fn stop_cancels_active_rejects_dispatch_and_waits_for_collection_before_join() {
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let mut worker = QueryWorker::spawn_executor(move |_, token| {
        entered_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        assert!(token.is_cancelled());
        ordinary()
    })
    .unwrap();
    let first = job(1);
    let token = first.cancel.clone();
    worker.try_submit(first).unwrap();
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    worker.request_stop();
    worker.request_stop();
    assert!(token.is_cancelled());
    assert!(worker.try_submit(job(2)).is_err());
    assert!(worker.try_join().is_none());
    release_tx.send(()).unwrap();
    // Even a finished producer cannot discharge an uncollected completion.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !worker
        .owner
        .as_ref()
        .unwrap()
        .thread
        .as_ref()
        .unwrap()
        .is_finished()
    {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(worker.try_join().is_none());
    assert!(worker.is_busy());
    assert_eq!(complete(&mut worker).generation, 1);
    join(&mut worker).unwrap();
    assert!(worker.try_submit(job(2)).is_err());
}

struct Witness(Arc<AtomicUsize>);
impl Drop for Witness {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
// Break caught: panic lowering drops its original non-string payload or permits another query.
#[test]
fn caught_query_panic_publishes_retained_fatal_completion_then_actually_joins() {
    let drops = Arc::new(AtomicUsize::new(0));
    let worker_drops = drops.clone();
    let mut worker = QueryWorker::spawn_executor(move |_, _| {
        std::panic::panic_any(Witness(worker_drops.clone()))
    })
    .unwrap();
    worker.try_submit(job(1)).unwrap();
    assert!(worker.try_join().is_none());
    let completion = complete(&mut worker);
    assert!(completion.worker_panicked);
    let owner = completion.result.unwrap_err();
    assert_eq!(owner.view().code, ErrorCode::WorkerError);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(serde_json::to_vec(owner.view()).unwrap());
    assert!(worker.try_submit(job(2)).is_err());
    join(&mut worker).unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(owner);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn idle_stop_wakes_and_joins_without_a_job() {
    let mut worker = QueryWorker::spawn_executor(|_, _| ordinary()).unwrap();
    worker.request_stop();
    assert!(worker.try_submit(job(1)).is_err());
    join(&mut worker).unwrap();
}

fn fixture() -> (tempfile::TempDir, Arc<Roots>) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("target")).unwrap();
    std::fs::write(
        root.join("recipe.toml"),
        include_bytes!("../../tests/fixtures/recipes/v2_clean.toml"),
    )
    .unwrap();
    let roots = Arc::new(Roots::open(&root, &root.join("output")).unwrap());
    (temp, roots)
}
fn start(digest: &str) -> StartArgs {
    serde_json::from_value(serde_json::json!({ "server_instance":"a".repeat(32), "request_key":"key", "recipe_path":"recipe.toml", "recipe_digest":digest, "profiles":["user"], "scenarios":["clean"] })).unwrap()
}
fn submit_work(worker: &mut QueryWorker, generation: u64, work: QueryWork) -> QueryCompletion {
    let mut next = job(generation);
    next.work = work;
    worker.try_submit(next).unwrap();
    complete(worker)
}
// Break caught: query dispatch omits pinned roots, startup limit, digest check, shared validation or fresh reread.
#[test]
fn real_inspection_and_validation_preserve_exact_recipe_and_failures() {
    let (_temp, roots) = fixture();
    let mut worker = QueryWorker::spawn(roots.clone(), 123).unwrap();
    let first = submit_work(&mut worker, 1, work());
    let QueryValue::Inspect(InspectData::Overview(overview)) = first.result.unwrap() else {
        panic!()
    };
    assert_eq!(overview.name.text, "clean-only");
    assert_eq!(overview.max_run_seconds.get(), 123);
    let mut valid = start(overview.recipe_digest.as_str());
    let destination = QueryDestination::Admission(binding().handle);
    let mut validation = job(2);
    validation.destination = destination.clone();
    validation.work = QueryWork::ValidateStart(valid.clone());
    worker.try_submit(validation).unwrap();
    let completion = complete(&mut worker);
    assert_eq!(completion.destination, destination);
    let validated = completion.result.unwrap();
    let QueryValue::Validated(validated) = validated else {
        panic!()
    };
    assert_eq!(validated.digest, overview.recipe_digest.as_str());
    assert_eq!(validated.recipe.name, "clean-only");
    valid.scenarios = vec![crate::model::ScenarioKind::Upgrade];
    assert_eq!(
        submit_work(&mut worker, 3, QueryWork::ValidateStart(valid))
            .result
            .unwrap_err()
            .view()
            .code,
        ErrorCode::RecipeInvalid
    );
    assert_eq!(
        submit_work(
            &mut worker,
            4,
            QueryWork::ValidateStart(start(&"b".repeat(64)))
        )
        .result
        .unwrap_err()
        .view()
        .code,
        ErrorCode::RecipeChanged
    );
    std::fs::remove_file(roots.project_path.join("recipe.toml")).unwrap();
    let error = submit_work(&mut worker, 5, work()).result.unwrap_err();
    assert_eq!(error.view().code, ErrorCode::IoError);
    assert!(error.primary().is::<std::io::Error>());
    worker.request_stop();
    join(&mut worker).unwrap();
}

fn binding() -> SavedBinding {
    SavedBinding {
        handle: Handle::parse(&format!("{}:1", "a".repeat(32))).unwrap(),
        report_id: "actual-run".into(),
        child: theseus_host::fs::RelativePath::parse("actual-run").unwrap(),
        hint_sequence: 1,
    }
}
fn evidence_args() -> EvidenceArgs {
    serde_json::from_value(serde_json::json!({"server_instance":"a".repeat(32),"handle":format!("{}:1","a".repeat(32)),"selector":{"kind":"catalog","collection":"run"}})).unwrap()
}
// Break caught: caching terminal reports or losing sampled hint/guest facts instead of loading each query.
#[test]
fn refresh_and_evidence_load_fresh_bound_report_and_publish_only_projection() {
    let (_temp, roots) = fixture();
    let store = roots.output.create_child("actual-run").unwrap();
    let mut document: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../tests/fixtures/reports/schema1-report.json"
    ))
    .unwrap();
    document["finished_at"] = "fixture-finished".into();
    let bytes = serde_json::to_vec(&document).unwrap();
    std::fs::write(store.report_path(), &bytes).unwrap();
    let mut worker = QueryWorker::spawn(roots, 123).unwrap();
    let QueryValue::Refreshed(first) = submit_work(
        &mut worker,
        1,
        QueryWork::RefreshStatus {
            binding: binding(),
            sampled_hint: 17,
        },
    )
    .result
    .unwrap() else {
        panic!()
    };
    assert_eq!(first.sampled_hint, 17);
    assert_eq!(first.saved.schema_version, 1);
    assert_eq!(
        first.saved.finished_at.as_ref().unwrap().text,
        "fixture-finished"
    );
    assert_eq!(first.saved.outcome, crate::model::RunOutcome::Passed);
    assert_eq!(first.guest, GuestCleanup::Complete);
    let mut changed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    changed["outcome"] = "failed".into();
    std::fs::write(store.report_path(), serde_json::to_vec(&changed).unwrap()).unwrap();
    let QueryValue::Refreshed(second) = submit_work(
        &mut worker,
        2,
        QueryWork::RefreshStatus {
            binding: binding(),
            sampled_hint: 18,
        },
    )
    .result
    .unwrap() else {
        panic!()
    };
    assert_eq!(second.sampled_hint, 18);
    assert_eq!(second.saved.outcome, crate::model::RunOutcome::Failed);
    assert_ne!(first.saved.revision, second.saved.revision);
    let QueryValue::Evidence(EvidenceData::Catalog(page)) = submit_work(
        &mut worker,
        3,
        QueryWork::Evidence {
            binding: binding(),
            args: evidence_args(),
            sampled_hint: 19,
            reply: reply(),
        },
    )
    .result
    .unwrap() else {
        panic!()
    };
    assert_eq!(page.revision, second.saved.revision);
    assert_eq!(page.records.len(), 1);
    assert_eq!(page.records[0].record_id.as_str(), "run");
    std::fs::write(store.report_path(), b"invalid").unwrap();
    assert_eq!(
        submit_work(
            &mut worker,
            4,
            QueryWork::RefreshStatus {
                binding: binding(),
                sampled_hint: 20
            }
        )
        .result
        .unwrap_err()
        .view()
        .code,
        ErrorCode::EvidenceInvalid
    );
    worker.request_stop();
    join(&mut worker).unwrap();
}

// Break caught: opening a caller/forged child before enforcing the trusted saved binding.
#[test]
fn binding_identity_and_evidence_correlation_are_checked_before_file_access() {
    let (_temp, roots) = fixture();
    let mut worker = QueryWorker::spawn(roots, 123).unwrap();
    let mut invalid = binding();
    invalid.report_id = "other".into();
    assert_eq!(
        submit_work(
            &mut worker,
            1,
            QueryWork::RefreshStatus {
                binding: invalid,
                sampled_hint: 0
            }
        )
        .result
        .unwrap_err()
        .view()
        .code,
        ErrorCode::InternalError
    );
    let mut args = evidence_args();
    args.handle = Handle::parse(&format!("{}:2", "a".repeat(32))).unwrap();
    assert_eq!(
        submit_work(
            &mut worker,
            2,
            QueryWork::Evidence {
                binding: binding(),
                args,
                sampled_hint: 0,
                reply: reply()
            }
        )
        .result
        .unwrap_err()
        .view()
        .code,
        ErrorCode::UnknownRun
    );
    let mut wrong_reply = reply();
    wrong_reply.instance = Instance::parse(&"b".repeat(32)).unwrap();
    assert_eq!(
        submit_work(
            &mut worker,
            3,
            QueryWork::Evidence {
                binding: binding(),
                args: evidence_args(),
                sampled_hint: 0,
                reply: wrong_reply
            }
        )
        .result
        .unwrap_err()
        .view()
        .code,
        ErrorCode::StaleInstance
    );
    worker.request_stop();
    join(&mut worker).unwrap();
}

// Break caught: cancellation replaces the original returned error before native authority and payload capture.
#[test]
fn cancelled_late_error_keeps_actual_pending_native_authority_and_original_panic_payload() {
    use crate::runner::process::{test_support::PendingFixture, worker_panic};
    let fixture = PendingFixture::new();
    let failure = fixture.failure.clone();
    let drops = Arc::new(AtomicUsize::new(0));
    let worker_drops = drops.clone();
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let mut worker = QueryWorker::spawn_executor(move |_, _| {
        entered_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        let mut cleanup = crate::runner::process::CleanupFailures::default();
        cleanup.retain(&failure.clone().into());
        Err(QueryError::new(
            ErrorCode::IoError,
            worker_panic(Box::new(Witness(worker_drops.clone()))).context(cleanup),
        ))
    })
    .unwrap();
    let first = job(1);
    let token = first.cancel.clone();
    worker.try_submit(first).unwrap();
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    token.cancel();
    release_tx.send(()).unwrap();
    let owner = complete(&mut worker).result.unwrap_err();
    assert_eq!(owner.view().code, ErrorCode::IoError);
    assert!(
        owner
            .primary()
            .is::<crate::runner::process::CleanupFailures>()
    );
    assert_eq!(owner.view().native, NativeCleanup::Pending);
    assert_eq!(owner.view().launch, Some(Launch::MayRun));
    drop(serde_json::to_vec(owner.view()).unwrap());
    struct Full;
    impl std::io::Write for Full {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("encoding output exhausted"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    assert!(serde_json::to_writer(Full, owner.view()).is_err());
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    worker.request_stop();
    join(&mut worker).unwrap();
    fixture.release_fault();
    let owner = std::thread::spawn(move || owner.finalize()).join().unwrap();
    assert!(!owner.view().cleanup_blocked);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(owner);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

// Break caught: an uncaught panic on executor teardown is stringified/dropped rather than retained after actual join.
#[test]
fn uncaught_thread_exit_panic_is_captured_only_after_actual_join() {
    struct ExitPanic(Arc<AtomicUsize>);
    impl Drop for ExitPanic {
        fn drop(&mut self) {
            std::panic::panic_any(Witness(self.0.clone()));
        }
    }
    let drops = Arc::new(AtomicUsize::new(0));
    let exit = ExitPanic(drops.clone());
    let mut worker = QueryWorker::spawn_executor(move |_, _| {
        let _keep = &exit;
        ordinary()
    })
    .unwrap();
    worker.try_submit(job(1)).unwrap();
    assert!(!complete(&mut worker).worker_panicked);
    worker.request_stop();
    let owner = join(&mut worker).unwrap_err();
    assert_eq!(owner.view().code, ErrorCode::WorkerError);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert!(worker.try_submit(job(2)).is_err());
    drop(owner);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

// Break caught: a disconnected producer with an unfinished job is reported as successful work.
#[test]
fn joined_producer_without_required_completion_is_fatal() {
    let (commands, _incoming) = mpsc::sync_channel(1);
    let (outgoing, completions) = mpsc::sync_channel(1);
    drop(outgoing);
    let mut worker = QueryWorker {
        owner: Some(Box::new(WorkerOwner {
            commands,
            completions,
            thread: Some(std::thread::spawn(|| {})),
            stopping: Arc::new(AtomicBool::new(false)),
            active: Some(CancellationToken::default()),
            pending: None,
            last_generation: 1,
        })),
    };
    let owner = join(&mut worker).unwrap_err();
    assert_eq!(owner.view().code, ErrorCode::WorkerError);
    assert!(!worker.is_busy());
    assert!(worker.try_submit(job(2)).is_err());
}

// Break caught: capture catches a diagnostic panic but the query worker treats it as an ordinary error and accepts more work.
#[test]
fn capture_display_panic_is_fatal_and_retains_original_native_and_panic_owners() {
    use crate::runner::process::{CleanupFailures, test_support::PendingFixture};
    #[derive(Debug)]
    struct Diagnostic {
        primary_drops: Arc<AtomicUsize>,
        panic_drops: Arc<AtomicUsize>,
    }
    impl Drop for Diagnostic {
        fn drop(&mut self) {
            self.primary_drops.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl std::fmt::Display for Diagnostic {
        fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            std::panic::panic_any(Witness(self.panic_drops.clone()));
        }
    }
    impl std::error::Error for Diagnostic {}
    let fixture = PendingFixture::new();
    let mut cleanup = CleanupFailures::default();
    cleanup.retain(&fixture.failure.clone().into());
    let primary_drops = Arc::new(AtomicUsize::new(0));
    let panic_drops = Arc::new(AtomicUsize::new(0));
    let source = anyhow::Error::new(Diagnostic {
        primary_drops: primary_drops.clone(),
        panic_drops: panic_drops.clone(),
    })
    .context(cleanup);
    let mut source = Some(source);
    let mut worker = QueryWorker::spawn_executor(move |_, _| {
        Err(QueryError::new(
            ErrorCode::IoError,
            source.take().expect("one query only"),
        ))
    })
    .unwrap();
    worker.try_submit(job(1)).unwrap();
    let completion = complete(&mut worker);
    let fatal = completion.worker_panicked;
    let owner = completion.result.unwrap_err();
    assert!(owner.primary().is::<Diagnostic>());
    assert!(owner.has_worker_panic());
    assert_eq!(owner.view().code, ErrorCode::IoError);
    assert_eq!(owner.view().native, NativeCleanup::Pending);
    assert_eq!(owner.view().launch, Some(Launch::MayRun));
    assert_eq!(primary_drops.load(Ordering::SeqCst), 0);
    assert_eq!(panic_drops.load(Ordering::SeqCst), 0);
    drop(serde_json::to_vec(owner.view()).unwrap());
    if !fatal {
        worker.request_stop();
    }
    join(&mut worker).unwrap();
    assert!(worker.try_submit(job(2)).is_err());
    fixture.release_fault();
    while !fixture.failure.retry_cleanup().unwrap() {
        std::thread::yield_now();
    }
    assert_eq!(primary_drops.load(Ordering::SeqCst), 0);
    assert_eq!(panic_drops.load(Ordering::SeqCst), 0);
    drop(owner);
    assert_eq!(primary_drops.load(Ordering::SeqCst), 1);
    assert_eq!(panic_drops.load(Ordering::SeqCst), 1);
    assert!(
        fatal,
        "a capture-time diagnostic panic must stop the query worker and mark its completion fatal"
    );
}
