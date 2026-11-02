use super::*;
use crate::{
    engine::{CancellationToken, RunOptions},
    model::{OwnedResource, RunOutcome},
    recipe::{Limits, ValidatedRecipe},
    report::ReportStore,
    runner::{CommandRequest, Environment, ExecutedCommand, PreparedEnvironment},
    snapshot::Snapshot,
};
use std::{
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

#[derive(Default)]
struct EnvironmentFixture {
    prepare_calls: Option<Arc<std::sync::atomic::AtomicUsize>>,
    prepare_panic: Option<Witness>,
    drop_panic: Option<Witness>,
    prepare: Option<anyhow::Error>,
    dropping: Option<mpsc::Sender<()>>,
    release: Option<mpsc::Receiver<()>>,
}
impl Environment for EnvironmentFixture {
    fn prepare(
        &mut self,
        _: &ValidatedRecipe,
        _: bool,
        _: &CancellationToken,
    ) -> anyhow::Result<PreparedEnvironment> {
        if let Some(calls) = &self.prepare_calls {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        if let Some(payload) = self.prepare_panic.take() {
            std::panic::panic_any(payload);
        }
        Err(self.prepare.take().expect("one prepare"))
    }
    fn create(&mut self, _: &str, _: &str, _: &ValidatedRecipe) -> anyhow::Result<OwnedResource> {
        unreachable!()
    }
    fn setup(
        &mut self,
        _: &OwnedResource,
        _: &ValidatedRecipe,
        _: &CancellationToken,
    ) -> anyhow::Result<()> {
        unreachable!()
    }
    fn execute(
        &mut self,
        _: &OwnedResource,
        _: CommandRequest,
        _: &ReportStore,
        _: &CancellationToken,
    ) -> anyhow::Result<ExecutedCommand> {
        unreachable!()
    }
    fn observe(
        &mut self,
        _: &OwnedResource,
        _: &[String],
        _: Limits,
        _: &CancellationToken,
    ) -> anyhow::Result<Vec<Snapshot>> {
        unreachable!()
    }
    fn stop(&mut self, _: &OwnedResource) -> anyhow::Result<()> {
        unreachable!()
    }
    fn remove(&mut self, _: &OwnedResource) -> anyhow::Result<()> {
        unreachable!()
    }
}
impl Drop for EnvironmentFixture {
    fn drop(&mut self) {
        if let Some(dropping) = self.dropping.take() {
            dropping.send(()).unwrap();
        }
        if let Some(release) = self.release.take() {
            release.recv_timeout(Duration::from_secs(10)).unwrap();
        }
        if let Some(payload) = self.drop_panic.take() {
            std::panic::panic_any(payload);
        }
    }
}
fn project() -> (tempfile::TempDir, ValidatedRecipe, Arc<ReportStore>) {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("target")).unwrap();
    let path = temp.path().join("recipe.toml");
    std::fs::write(
        &path,
        include_str!("../../../tests/fixtures/recipes/v2_clean.toml"),
    )
    .unwrap();
    let recipe = crate::recipe::load(&path).unwrap();
    let output = Arc::new(ReportStore::safe_root(&temp.path().join("pinned"), true).unwrap());
    (temp, recipe, output)
}
fn handle() -> super::super::types::Handle {
    super::super::types::Handle::parse(&format!("{}:1", "a".repeat(32))).unwrap()
}
fn joined(worker: &mut EngineWorker) -> WorkerCompletion {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(completion) = worker.try_join() {
            return completion;
        }
        assert!(Instant::now() < deadline, "worker did not join");
        std::thread::sleep(Duration::from_millis(1));
    }
}

// Catches treating Finished or a saved report as engine success before cleanup/join.
#[test]
fn worker_finished_before_cleanup_failure_is_not_a_joined_engine_success() {
    let fixture = crate::runner::process::test_support::PendingFixture::new();
    let pending = fixture.failure.clone();
    let (_temp, recipe, output) = project();
    let options = RunOptions::all(&recipe);
    let (dropping, dropped) = mpsc::channel();
    let (release, waiting) = mpsc::channel();
    let mut worker = EngineWorker::spawn_with_environment(
        handle(),
        recipe,
        options,
        CancellationToken::default(),
        output.clone(),
        move || EnvironmentFixture {
            prepare: Some(pending.into()),
            dropping: Some(dropping),
            release: Some(waiting),
            prepare_calls: None,
            prepare_panic: None,
            drop_panic: None,
        },
    )
    .unwrap();
    dropped.recv_timeout(Duration::from_secs(10)).unwrap();
    let first = worker.try_binding().expect("reliable Started binding");
    assert_eq!(first.hint_sequence, 1);
    let latest = worker.try_binding().expect("coalesced Finished hint");
    assert_eq!(latest.report_id, first.report_id);
    assert_eq!(latest.child, first.child);
    assert!(latest.hint_sequence > first.hint_sequence);
    let saved = crate::report::load(&output.root().join(&first.report_id)).unwrap();
    assert!(saved.finished_at.is_some());
    assert!(
        worker.try_join().is_none(),
        "Environment still has not finished dropping"
    );
    release.send(()).unwrap();
    let WorkerCompletion::Engine(completion) = joined(&mut worker) else {
        panic!("engine completion")
    };
    let owner = completion
        .result
        .expect_err("native cleanup failure cannot become saved outcome success");
    assert_eq!(
        owner.view().code,
        super::super::types::ErrorCode::WorkerError
    );
    assert!(owner.view().cleanup_blocked);
    assert!(
        owner
            .primary()
            .is::<crate::runner::process::CleanupFailures>()
    );
    fixture.release_fault();
    let mut finalizer = EngineWorker::spawn_finalizer(vec![owner]).unwrap();
    let WorkerCompletion::Finalizer { owners } = joined(&mut finalizer) else {
        panic!("finalizer completion")
    };
    assert_eq!(owners.len(), 1);
    assert!(!owners[0].view().cleanup_blocked);
}

#[derive(Debug)]
struct Witness(Arc<std::sync::atomic::AtomicUsize>);
impl Drop for Witness {
    fn drop(&mut self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}
fn drops() -> Arc<std::sync::atomic::AtomicUsize> {
    Arc::new(std::sync::atomic::AtomicUsize::new(0))
}
fn count(n: &std::sync::atomic::AtomicUsize) -> usize {
    n.load(std::sync::atomic::Ordering::SeqCst)
}

// Catches constructing Environment on the caller and losing factory panic payloads.
#[test]
fn worker_factory_runs_on_worker_and_non_string_panic_survives_actual_join() {
    let (_temp, recipe, output) = project();
    let options = RunOptions::all(&recipe);
    let original_thread = std::thread::current().id();
    let payload_drops = drops();
    let witness = Witness(payload_drops.clone());
    let mut worker = EngineWorker::spawn_with_environment(
        handle(),
        recipe,
        options,
        CancellationToken::default(),
        output,
        move || -> EnvironmentFixture {
            assert_ne!(std::thread::current().id(), original_thread);
            std::panic::panic_any(witness);
        },
    )
    .unwrap();
    let WorkerCompletion::Engine(completion) = joined(&mut worker) else {
        panic!("engine completion")
    };
    assert!(worker.try_binding().is_none());
    let owner = completion.result.unwrap_err();
    assert_eq!(count(&payload_drops), 0);
    drop(serde_json::to_vec(owner.view()).unwrap());
    assert_eq!(count(&payload_drops), 0);
    drop(owner);
    assert_eq!(count(&payload_drops), 1);
}

// Catches dropping the original engine result during Environment destructor unwind.
#[test]
fn worker_engine_and_environment_destructor_panics_both_remain_owned() {
    let (_temp, recipe, output) = project();
    let options = RunOptions::all(&recipe);
    let engine_drops = drops();
    let environment_drops = drops();
    let engine_witness = Witness(engine_drops.clone());
    let environment_witness = Witness(environment_drops.clone());
    let mut worker = EngineWorker::spawn_with_environment(
        handle(),
        recipe,
        options,
        CancellationToken::default(),
        output,
        move || EnvironmentFixture {
            prepare_panic: Some(engine_witness),
            drop_panic: Some(environment_witness),
            prepare_calls: None,
            prepare: None,
            dropping: None,
            release: None,
        },
    )
    .unwrap();
    let WorkerCompletion::Engine(completion) = joined(&mut worker) else {
        panic!("engine completion")
    };
    let owner = completion.result.unwrap_err();
    assert_eq!(count(&engine_drops), 0);
    assert_eq!(count(&environment_drops), 0);
    drop(owner);
    assert_eq!(count(&engine_drops), 1);
    assert_eq!(count(&environment_drops), 1);
}

// Catches cancellation being checked after prepare, or using unpinned output paths.
#[test]
fn worker_cancellation_before_prepare_uses_pinned_parent_and_actual_report_summary() {
    let (temp, recipe, output) = project();
    let mut options = RunOptions::all(&recipe);
    options.output_root = temp.path().join("untrusted-unused");
    let cancel = CancellationToken::default();
    cancel.cancel();
    let calls = drops();
    let observed = calls.clone();
    let mut worker = EngineWorker::spawn_with_environment(
        handle(),
        recipe,
        options,
        cancel,
        output.clone(),
        move || EnvironmentFixture {
            prepare_calls: Some(observed),
            prepare_panic: None,
            drop_panic: None,
            prepare: None,
            dropping: None,
            release: None,
        },
    )
    .unwrap();
    let WorkerCompletion::Engine(completion) = joined(&mut worker) else {
        panic!("engine completion")
    };
    let summary = completion.result.unwrap();
    assert_eq!(count(&calls), 0);
    assert_eq!(summary.outcome, RunOutcome::Cancelled);
    assert_eq!(summary.counts.commands, 0);
    let binding = worker.try_binding().unwrap();
    assert_eq!(
        summary.report_path,
        output.root().join(&binding.report_id).join("report.json")
    );
    assert_eq!(
        summary.journal_path,
        output.root().join(&binding.report_id).join("journal.json")
    );
    assert!(summary.journal_path.is_file());
    assert!(!temp.path().join("untrusted-unused").exists());
}

// Catches late cancellation fabricating a different result after run_with_parent returned.
#[test]
fn worker_completed_engine_result_wins_late_cancellation_before_join() {
    let (_temp, recipe, output) = project();
    let options = RunOptions::all(&recipe);
    let cancel = CancellationToken::default();
    let (dropping, dropped) = mpsc::channel();
    let (release, waiting) = mpsc::channel();
    let mut worker = EngineWorker::spawn_with_environment(
        handle(),
        recipe,
        options,
        cancel.clone(),
        output,
        move || EnvironmentFixture {
            prepare: Some(anyhow::anyhow!("preflight fixture unavailable")),
            dropping: Some(dropping),
            release: Some(waiting),
            prepare_calls: None,
            prepare_panic: None,
            drop_panic: None,
        },
    )
    .unwrap();
    dropped.recv_timeout(Duration::from_secs(10)).unwrap();
    cancel.cancel();
    assert!(worker.try_join().is_none());
    release.send(()).unwrap();
    let WorkerCompletion::Engine(completion) = joined(&mut worker) else {
        panic!("engine completion")
    };
    assert_eq!(completion.result.unwrap().outcome, RunOutcome::Incomplete);
}

// Catches closure-drop on Builder spawn failure consuming the entire owner bundle.
#[test]
fn worker_finalizer_spawn_failure_returns_every_original_owner() {
    let witnesses: Vec<_> = (0..3).map(|_| drops()).collect();
    let owners = witnesses
        .iter()
        .map(|witness| {
            OwnedError::capture(failure(crate::runner::process::worker_panic(Box::new(
                Witness(witness.clone()),
            ))))
        })
        .collect();
    let failure = match EngineWorker::start_finalizer(owners, |task| {
        drop(task);
        Err(std::io::Error::other("injected thread creation failure"))
    }) {
        Ok(_) => panic!("spawn unexpectedly succeeded"),
        Err(error) => error,
    };
    assert_eq!(failure.owners.len(), 3);
    assert_eq!(
        failure.source.code,
        super::super::types::ErrorCode::WorkerError
    );
    assert!(witnesses.iter().all(|w| count(w) == 0));
    drop(failure);
    assert!(witnesses.iter().all(|w| count(w) == 1));
}

// Catches conflating mailbox readiness with finalizer thread termination.
#[test]
fn worker_finalizer_completion_waits_for_actual_thread_return() {
    let witness_drops = drops();
    let owners = vec![OwnedError::capture(failure(
        crate::runner::process::worker_panic(Box::new(Witness(witness_drops.clone()))),
    ))];
    let (published, observed) = mpsc::channel();
    let (release, waiting) = mpsc::channel();
    let mut worker = EngineWorker::start_finalizer(owners, move |task| {
        std::thread::Builder::new().spawn(move || {
            let completion = task();
            published.send(()).unwrap();
            waiting.recv_timeout(Duration::from_secs(10)).unwrap();
            completion
        })
    })
    .unwrap();
    observed.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(worker.try_join().is_none());
    assert_eq!(count(&witness_drops), 0);
    release.send(()).unwrap();
    let WorkerCompletion::Finalizer { owners } = joined(&mut worker) else {
        panic!("finalizer completion")
    };
    assert_eq!(owners.len(), 1);
    assert_eq!(count(&witness_drops), 0);
    drop(owners);
    assert_eq!(count(&witness_drops), 1);
}

// Catches counter wrapping and later hints overwriting the reliable first binding.
#[test]
fn worker_saved_hints_coalesce_without_overwriting_started_and_never_wrap() {
    let (_temp, _recipe, output) = project();
    let mailbox = Arc::new(Mutex::new(Bindings::default()));
    let mut observer = Observer {
        handle: handle(),
        output: output.clone(),
        bindings: mailbox.clone(),
        binding: None,
        sequence: 0,
    };
    let event = |kind| EngineEvent {
        kind,
        report_path: output.root().join("actual-child/report.json"),
        scenario_id: None,
        stage: None,
    };
    observer.emit(event(EventKind::Started));
    for _ in 0..1000 {
        observer.emit(event(EventKind::StageFinished));
    }
    assert_eq!(
        mailbox
            .lock()
            .unwrap()
            .first
            .as_ref()
            .unwrap()
            .hint_sequence,
        1
    );
    assert_eq!(
        mailbox
            .lock()
            .unwrap()
            .latest
            .as_ref()
            .unwrap()
            .hint_sequence,
        1001
    );
    observer.sequence = UINT_MAX;
    assert!(
        catch_unwind(AssertUnwindSafe(
            || observer.emit(event(EventKind::Finished))
        ))
        .is_err()
    );
    assert_eq!(observer.sequence, UINT_MAX);
    assert_eq!(
        mailbox
            .lock()
            .unwrap()
            .latest
            .as_ref()
            .unwrap()
            .hint_sequence,
        1001
    );
    assert!(
        !output.root().join("actual-child").exists(),
        "callback performs no filesystem work"
    );
}

mod fixtures;

// Compare every retained field, normalizing only actual per-run identity/time.
// Proof fields, scenario order, counts and cleanup journal stay authoritative.
fn normalize(value: &mut serde_json::Value, run_id: &str) {
    match value {
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                if matches!(key.as_str(), "started_at" | "finished_at") {
                    if !value.is_null() {
                        *value = "<timestamp>".into();
                    }
                } else {
                    normalize(value, run_id);
                }
            }
        }
        serde_json::Value::Array(values) => values.iter_mut().for_each(|v| normalize(v, run_id)),
        serde_json::Value::String(text) => {
            *text = text.replace(run_id, "<run>");
        }
        _ => (),
    }
}
fn compare_saved<E: Environment + 'static>(
    direct: crate::engine::RunResult,
    recipe: ValidatedRecipe,
    root: &std::path::Path,
    factory: impl FnOnce() -> E + Send + 'static,
) {
    assert_eq!(
        direct.report.outcome,
        RunOutcome::Passed,
        "healthy parity fixture"
    );
    let output = Arc::new(ReportStore::safe_root(&root.join("facade"), true).unwrap());
    let options = RunOptions::all(&recipe);
    let mut worker = EngineWorker::spawn_with_environment(
        handle(),
        recipe,
        options,
        CancellationToken::default(),
        output,
        factory,
    )
    .unwrap();
    let WorkerCompletion::Engine(completion) = joined(&mut worker) else {
        panic!("engine completion")
    };
    let summary = completion.result.unwrap();
    assert_eq!(summary.outcome, direct.report.outcome);
    assert_eq!(
        serde_json::to_value(&summary.counts).unwrap(),
        serde_json::to_value(direct.report.counts()).unwrap()
    );
    let facade = crate::report::load(&summary.report_path).unwrap();
    let mut actual = serde_json::to_value(&facade).unwrap();
    let mut expected = serde_json::to_value(&direct.report).unwrap();
    normalize(&mut actual, &facade.run_id);
    normalize(&mut expected, &direct.report.run_id);
    assert_eq!(
        actual, expected,
        "facade must retain every engine proof field"
    );
    let mut actual =
        serde_json::to_value(crate::report::load_journal(&summary.journal_path).unwrap()).unwrap();
    let mut expected = serde_json::to_value(
        crate::report::load_journal(&direct.report_path.with_file_name("journal.json")).unwrap(),
    )
    .unwrap();
    normalize(&mut actual, &facade.run_id);
    normalize(&mut expected, &direct.report.run_id);
    assert_eq!(
        actual, expected,
        "facade must retain the exact cleanup journal"
    );
}
#[test]
fn worker_parity_clean_upgrade_reinstall_interruption_and_downgrade() {
    fixtures::ordinary_parity();
}
#[test]
fn worker_parity_rollback_both_policies_and_profiles() {
    fixtures::rollback_parity();
}
#[test]
fn worker_parity_interrupted_rollback_both_policies_and_profiles() {
    fixtures::interrupted_rollback_parity();
}
#[test]
fn worker_parity_plugin_lifecycle_all_scopes() {
    fixtures::plugin_parity();
}
#[test]
fn worker_parity_guest_mcp_clean_upgrade_and_plugin() {
    fixtures::guest_mcp_parity();
}

// Catches a consuming finalizer dropping the primary on a second Display panic.
#[test]
fn worker_finalizer_projection_panic_returns_original_and_panic_owners() {
    #[derive(Debug)]
    struct Primary {
        calls: std::sync::atomic::AtomicUsize,
        original_drops: Arc<std::sync::atomic::AtomicUsize>,
        panic_drops: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl std::fmt::Display for Primary {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0 {
                std::panic::panic_any(Witness(self.panic_drops.clone()));
            }
            f.write_str("original typed primary")
        }
    }
    impl std::error::Error for Primary {}
    impl Drop for Primary {
        fn drop(&mut self) {
            self.original_drops
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let original_drops = drops();
    let panic_drops = drops();
    let owner = OwnedError::capture(failure(
        Primary {
            calls: std::sync::atomic::AtomicUsize::new(0),
            original_drops: original_drops.clone(),
            panic_drops: panic_drops.clone(),
        }
        .into(),
    ));
    assert!(!owner.has_worker_panic());
    let mut finalizer = EngineWorker::spawn_finalizer(vec![owner]).unwrap();
    let WorkerCompletion::Finalizer { owners } = joined(&mut finalizer) else {
        panic!("finalizer completion")
    };
    assert_eq!(owners.len(), 1);
    assert!(owners[0].primary().is::<Primary>());
    assert!(owners[0].has_worker_panic());
    assert_eq!(count(&original_drops), 0);
    assert_eq!(count(&panic_drops), 0);
    drop(owners);
    assert_eq!(count(&original_drops), 1);
    assert_eq!(count(&panic_drops), 1);
}
