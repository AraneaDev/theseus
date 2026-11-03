//! Bounded, read-only live evidence loading. The engine owns every lifecycle action.
use super::{App, Tab, WorkEvent, WorkResult};
use anyhow::{Result, anyhow};
use std::{path::PathBuf, sync::Arc};
use tokio::{runtime::Handle, sync::oneshot};

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Run,
    Native,
    Suite,
}
#[derive(Clone)]
struct Request {
    kind: Kind,
    path: PathBuf,
    generation: u64,
}
enum Loaded {
    Run(Box<crate::model::RunReport>),
    Native(Box<crate::native::model::NativeReport>),
    Suite(Box<crate::comparison::LoadedComparison>),
}
impl Loaded {
    fn read(request: &Request) -> Result<Self> {
        match request.kind {
            Kind::Native => match crate::native::storage::load_run(&request.path)? {
                crate::engine::domain::StoredRun::Native(report) => {
                    Ok(Self::Native(Box::new(report)))
                }
                _ => anyhow::bail!("native refresh family mismatch"),
            },
            Kind::Run => crate::report::load(&request.path).map(|r| Self::Run(Box::new(r))),
            Kind::Suite => {
                crate::comparison::load_document(&request.path).map(|r| Self::Suite(Box::new(r)))
            }
        }
    }
}
struct Context {
    root: PathBuf,
    scenario: usize,
    stage: usize,
    tab: Tab,
    cell: Option<usize>,
    evidence: usize,
    filter: String,
}
impl Context {
    fn new(app: &App) -> Self {
        Self {
            root: app.root.clone(),
            scenario: app.scenario_index,
            stage: app.stage_index,
            tab: app.tab,
            cell: app.comparison.as_ref().map(|v| v.row),
            evidence: app.evidence_index,
            filter: app.filter.clone(),
        }
    }
    fn matches(&self, app: &App) -> bool {
        self.root == app.root
            && self.scenario == app.scenario_index
            && self.stage == app.stage_index
            && self.tab == app.tab
            && self.cell == app.comparison.as_ref().map(|v| v.row)
            && self.evidence == app.evidence_index
            && self.filter == app.filter
    }
}
struct Completion {
    request: Request,
    result: Result<Loaded>,
}
type Loader = dyn Fn(&Request) -> Result<Loaded> + Send + Sync;
pub(super) struct Coordinator {
    runtime: Handle,
    loader: Arc<Loader>,
    generation: u64,
    latest: Option<Request>,
    context: Option<Context>,
    pending: Option<Request>,
    in_flight: Option<oneshot::Receiver<Completion>>,
    sealed: bool,
}
impl Coordinator {
    pub fn live(runtime: Handle) -> Self {
        Self::new(runtime, Loaded::read)
    }
    fn new(
        runtime: Handle,
        loader: impl Fn(&Request) -> Result<Loaded> + Send + Sync + 'static,
    ) -> Self {
        Self {
            runtime,
            loader: Arc::new(loader),
            generation: 0,
            latest: None,
            context: None,
            pending: None,
            in_flight: None,
            sealed: false,
        }
    }
    pub fn event(&mut self, app: &mut App, event: WorkEvent, last_path: &mut Option<PathBuf>) {
        if self.sealed || app.terminal_error.is_some() {
            return;
        }
        let (kind, path) = super::apply_event(app, event, last_path);
        self.request(app, kind, path);
    }
    fn request(&mut self, app: &App, kind: Kind, path: PathBuf) {
        self.generation = self.generation.wrapping_add(1);
        let request = Request {
            kind,
            path,
            generation: self.generation,
        };
        self.latest = Some(request.clone());
        self.context = Some(Context::new(app));
        self.pending = Some(request);
        self.start_pending();
    }
    fn start_pending(&mut self) {
        if self.in_flight.is_some() || self.sealed {
            return;
        }
        let Some(request) = self.pending.take() else {
            return;
        };
        let (tx, rx) = oneshot::channel();
        self.in_flight = Some(rx);
        let loader = Arc::clone(&self.loader);
        self.runtime.spawn_blocking(move || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| loader(&request)))
                    .unwrap_or_else(|_| Err(anyhow!("evidence refresh worker panicked")));
            let _ = tx.send(Completion { request, result });
        });
    }
    fn sync_context(&mut self, app: &App) {
        if self.sealed || app.terminal_error.is_some() {
            return;
        }
        if self.context.as_ref().is_some_and(|c| !c.matches(app))
            && let Some(latest) = &self.latest
        {
            let kind = latest.kind;
            let path = app.root.join(match kind {
                Kind::Run => "report.json",
                Kind::Native => crate::native::model::REPORT_FILENAME,
                Kind::Suite => "comparison.json",
            });
            self.request(app, kind, path);
        }
    }
    pub fn poll(&mut self, app: &mut App) {
        self.sync_context(app);
        let Some(receiver) = &mut self.in_flight else {
            return;
        };
        match receiver.try_recv() {
            Ok(completion) => self.finish_load(app, completion),
            Err(oneshot::error::TryRecvError::Empty) => {}
            Err(oneshot::error::TryRecvError::Closed) => {
                self.in_flight = None;
                if !self.sealed && app.terminal_error.is_none() {
                    app.status =
                        "Evidence refresh unavailable: worker ended without a result".into();
                }
                self.start_pending();
            }
        }
    }
    fn finish_load(&mut self, app: &mut App, completion: Completion) {
        self.sync_context(app);
        self.in_flight = None;
        if !self.sealed
            && app.terminal_error.is_none()
            && completion.request.generation == self.generation
        {
            let root = completion
                .request
                .path
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .to_path_buf();
            match completion.result {
                Ok(Loaded::Native(report)) => app.set_native_report(*report, root),
                Ok(Loaded::Run(report)) => app.set_report(*report, root),
                Ok(Loaded::Suite(document)) => {
                    if let Some(view) = &mut app.comparison {
                        view.manifest = document.manifest;
                    }
                    app.set_comparison_report(document.report, root);
                }
                Err(error) => app.status = format!("Evidence refresh unavailable: {error:#}"),
            }
            // Setters may clamp selection. Adoption is not a new read request.
            self.context = Some(Context::new(app));
        }
        self.start_pending();
    }
    pub fn complete(&mut self, app: &mut App, result: Result<WorkResult>) -> Result<()> {
        self.sealed = true;
        self.generation = self.generation.wrapping_add(1);
        self.pending = None;
        super::complete_work(app, result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine,
        model::{RunOutcome, RunReport},
    };
    use std::sync::mpsc;

    fn run_event(path: &str) -> super::super::WorkEvent {
        super::super::WorkEvent::Run(engine::EngineEvent {
            kind: engine::EventKind::StageStarted,
            report_path: path.into(),
            scenario_id: Some("user-clean".into()),
            stage: Some("install".into()),
        })
    }
    // Only the safe disk loader is replaced. Real coordinator/jobs/adoption are used.
    fn blocked() -> (
        tokio::runtime::Runtime,
        Coordinator,
        mpsc::Receiver<Request>,
        mpsc::Sender<Result<Loaded>>,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = std::sync::Mutex::new(release_rx);
        let coordinator = Coordinator::new(runtime.handle().clone(), move |request| {
            entered_tx.send(request.clone()).unwrap();
            release_rx.lock().unwrap().recv().unwrap()
        });
        (runtime, coordinator, entered_rx, release_tx)
    }
    fn loaded(id: &str) -> Result<Loaded> {
        let mut report = RunReport::new(id);
        report.outcome = RunOutcome::Passed;
        Ok(Loaded::Run(Box::new(report)))
    }
    fn wait_next(runtime: &tokio::runtime::Runtime, coordinator: &mut Coordinator, app: &mut App) {
        let result = runtime
            .block_on(coordinator.in_flight.as_mut().unwrap())
            .unwrap();
        coordinator.finish_load(app, result);
    }
    fn app() -> App {
        let mut app = App::offline(RunReport::new("cached"), "/run".into());
        app.active = true;
        app
    }
    #[allow(dead_code)]
    mod fixtures {
        use crate as theseus;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/comparison.rs"
        ));
    }
    #[test]
    fn safe_suite_load_adopts_manifest_with_recomputed_report_at_new_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = fixtures::saved(temp.path());
        let mut document = crate::comparison::load_document(&root).unwrap();
        document.manifest.name = "old name".into();
        document.manifest.scopes[0].path = "/old-scope".into();
        let mut app = App::comparison_offline(document, "/old-root".into());
        app.active = true;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let mut coordinator = Coordinator::live(runtime.handle().clone());
        coordinator.event(
            &mut app,
            super::super::WorkEvent::Suite(crate::comparison::ComparisonEvent {
                report_path: root.join("comparison.json"),
                variant_id: Some("archive".into()),
                source: None,
            }),
            &mut None,
        );
        assert!(app.comparison.as_ref().unwrap().report.is_none());
        wait_next(&runtime, &mut coordinator, &mut app);
        let view = app.comparison.as_ref().unwrap();
        assert_eq!(view.manifest.name, "methods");
        assert_eq!(view.manifest.scopes[0].path, "/app");
        assert_eq!(
            app.outcome(),
            RunOutcome::Failed,
            "stored artifact says Passed, safe loader must recompute"
        );
    }
    #[test]
    fn safe_run_loader_adopts_owned_stored_report() {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::report::create(&temp.path().join("run")).unwrap();
        store.save(&RunReport::new("stored-live-run")).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let mut coordinator = Coordinator::live(runtime.handle().clone());
        let mut app = app();
        coordinator.event(
            &mut app,
            run_event(store.report_path().to_str().unwrap()),
            &mut None,
        );
        wait_next(&runtime, &mut coordinator, &mut app);
        assert_eq!(app.report.as_ref().unwrap().run_id, "stored-live-run");
    }
    #[test]
    fn blocked_read_does_not_delay_runtime_shutdown() {
        let (runtime, mut coordinator, entered, release) = blocked();
        let mut app = app();
        coordinator.event(&mut app, run_event("/run/report.json"), &mut None);
        entered.recv().unwrap();
        drop(coordinator);
        runtime.shutdown_timeout(std::time::Duration::ZERO);
        // Reaching this send proves shutdown returned before read completion.
        release.send(loaded("unused")).unwrap();
    }
    #[test]
    fn missing_live_evidence_is_pending_not_preview_or_passed() {
        use ratatui::{Terminal, backend::TestBackend};
        let temp = tempfile::tempdir().unwrap();
        let suite = crate::comparison::load(&fixtures::recipe_suite(temp.path())).unwrap();
        for mut app in [
            App::offline(RunReport::new("old"), "/old".into()),
            App::comparison_preview(
                suite,
                crate::comparison::SuiteRunOptions {
                    output_root: temp.path().join("output"),
                    pull: false,
                },
            ),
        ] {
            app.active = true;
            app.pending_root("/pending".into());
            let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
            terminal.draw(|f| super::super::render(f, &app)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(!text.contains("no commands executed"), "{text}");
            assert!(text.contains("pending"), "{text}");
            assert_eq!(app.outcome(), RunOutcome::Incomplete);
        }
    }
    #[test]
    fn blocked_load_returns_and_burst_coalesces_to_latest_path() {
        let (runtime, mut coordinator, entered, release) = blocked();
        let mut app = app();
        let mut last = None;
        coordinator.event(&mut app, run_event("/run/report.json"), &mut last);
        let first = entered.recv().unwrap();
        for n in 0..100 {
            coordinator.event(
                &mut app,
                run_event(&format!("/run-{n}/report.json")),
                &mut last,
            );
        }
        assert_eq!(app.root, PathBuf::from("/run-99"));
        assert_eq!(last, Some(PathBuf::from("/run-99/report.json")));
        assert!(app.report.is_none());
        assert!(app.status.contains("install"));
        assert!(
            entered.try_recv().is_err(),
            "one in-flight job while blocked"
        );
        release.send(loaded("obsolete")).unwrap();
        wait_next(&runtime, &mut coordinator, &mut app);
        assert!(app.report.is_none());
        let latest = entered.recv().unwrap();
        assert_eq!(latest.path, PathBuf::from("/run-99/report.json"));
        assert!(latest.generation > first.generation);
        release.send(loaded("latest")).unwrap();
        wait_next(&runtime, &mut coordinator, &mut app);
        assert_eq!(app.report.as_ref().unwrap().run_id, "latest");
        coordinator.poll(&mut app);
        assert!(
            coordinator.in_flight.is_none(),
            "accepted setters must not reschedule themselves"
        );
        assert!(entered.try_recv().is_err());
    }
    #[test]
    fn each_changed_context_discards_old_load_and_retries_latest_once() {
        for changed in ["scenario", "stage", "tab", "record", "filter", "root"] {
            let (runtime, mut coordinator, entered, release) = blocked();
            let mut app = app();
            coordinator.event(&mut app, run_event("/run/report.json"), &mut None);
            entered.recv().unwrap();
            match changed {
                "scenario" => app.scenario_index = 1,
                "stage" => app.stage_index = 1,
                "tab" => app.tab = super::super::Tab::Findings,
                "record" => app.evidence_index = 1,
                "filter" => app.filter = "selected-now".into(),
                "root" => app.pending_root("/new-root".into()),
                _ => unreachable!(),
            }
            coordinator.poll(&mut app);
            release.send(loaded("old-selection")).unwrap();
            wait_next(&runtime, &mut coordinator, &mut app);
            assert_eq!(
                app.report.as_ref().map(|r| r.run_id.as_str()),
                if changed == "root" {
                    None
                } else {
                    Some("cached")
                },
                "{changed}"
            );
            let latest = entered.recv().unwrap();
            assert_eq!(latest.path, app.root.join("report.json"));
            release.send(loaded("new-selection")).unwrap();
            wait_next(&runtime, &mut coordinator, &mut app);
            if changed == "filter" {
                assert_eq!(app.filter, "selected-now");
            }
            assert_eq!(app.report.as_ref().unwrap().run_id, "new-selection");
            coordinator.poll(&mut app);
            assert!(coordinator.in_flight.is_none());
        }
    }
    #[test]
    fn first_blocked_read_cannot_hide_final_error_or_recovery_root() {
        let (runtime, mut coordinator, entered, release) = blocked();
        let mut app = app();
        let mut last = None;
        coordinator.event(&mut app, run_event("/first/report.json"), &mut last);
        entered.recv().unwrap();
        assert!(app.report.is_none());
        let error = coordinator
            .complete(&mut app, Err(anyhow!("first final save failed")))
            .unwrap_err();
        assert!(error.to_string().contains("first final save failed"));
        assert!(
            app.terminal_error
                .as_ref()
                .unwrap()
                .contains("theseus cleanup /first")
        );
        release.send(loaded("stale-passed")).unwrap();
        wait_next(&runtime, &mut coordinator, &mut app);
        assert!(app.report.is_none());
        assert_eq!(app.exit_code(), 2);
        assert_eq!(last, Some(PathBuf::from("/first/report.json")));
        assert!(!app.active);
    }
    #[test]
    fn final_result_and_engine_error_invalidate_blocked_success_and_pending_events() {
        for failure in [false, true] {
            let (runtime, mut coordinator, entered, release) = blocked();
            let mut app = app();
            coordinator.event(&mut app, run_event("/run/report.json"), &mut None);
            entered.recv().unwrap();
            coordinator.event(&mut app, run_event("/run/report.json"), &mut None);
            let result = if failure {
                Err(anyhow::anyhow!("original final error"))
            } else {
                let mut report = RunReport::new("final");
                report.outcome = RunOutcome::Failed;
                Ok(super::super::WorkResult::Run(Box::new(engine::RunResult {
                    report,
                    report_path: "/run/report.json".into(),
                })))
            };
            coordinator.complete(&mut app, result).unwrap();
            release.send(loaded("stale-passed")).unwrap();
            wait_next(&runtime, &mut coordinator, &mut app);
            coordinator.event(&mut app, run_event("/late/report.json"), &mut None);
            assert!(!app.active);
            assert_eq!(app.root, PathBuf::from("/run"));
            assert!(coordinator.in_flight.is_none());
            assert!(entered.try_recv().is_err());
            if failure {
                assert!(
                    app.terminal_error
                        .as_ref()
                        .unwrap()
                        .contains("original final error")
                );
                assert_eq!(app.exit_code(), 2);
            } else {
                assert_eq!(app.report.as_ref().unwrap().run_id, "final");
                assert_eq!(app.exit_code(), 1);
            }
        }
    }
    #[test]
    fn blocked_load_never_delays_active_quit_signal_or_cleanup_deadline() {
        use super::super::{animation::Presentation, handle_key, quit_result, service_control};
        use crossterm::event::KeyCode;
        use std::time::{Duration, Instant};
        for signal in [false, true] {
            let (runtime, mut coordinator, entered, release) = blocked();
            let mut app = app();
            coordinator.event(&mut app, run_event("/run/report.json"), &mut None);
            entered.recv().unwrap();
            let cancel = engine::CancellationToken::default();
            let terminate = engine::CancellationToken::default();
            let mut presentation = Presentation::new(true, Duration::ZERO);
            let now = Instant::now();
            let mut deadline = None;
            if signal {
                terminate.cancel();
            } else {
                handle_key(
                    &mut app,
                    &mut presentation,
                    KeyCode::Char('q').into(),
                    &cancel,
                    &terminate,
                    &mut deadline,
                    now,
                );
            }
            service_control(
                &mut app,
                &mut presentation,
                &cancel,
                &terminate,
                &mut deadline,
                now,
            );
            assert!(cancel.is_cancelled());
            assert!(app.cancel_requested);
            assert!(app.quit_requested);
            assert_eq!(deadline, Some(now + Duration::from_secs(5)));
            let path = PathBuf::from("/run/report.json");
            assert!(super::super::cleanup_timeout(&app, deadline, Some(&path), now).is_none());
            let expired = super::super::cleanup_timeout(
                &app,
                deadline,
                Some(&path),
                now + Duration::from_secs(5),
            )
            .unwrap();
            assert!(expired.to_string().contains("theseus cleanup /run"));
            coordinator
                .complete(
                    &mut app,
                    Err(anyhow::anyhow!("authoritative error after cancellation")),
                )
                .unwrap();
            let error = quit_result(&app, &mut presentation, Duration::ZERO)
                .unwrap()
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("authoritative error after cancellation")
            );
            // The quit result was available while the reader was still held.
            release.send(loaded("old-success")).unwrap();
            wait_next(&runtime, &mut coordinator, &mut app);
            assert!(
                app.terminal_error
                    .as_ref()
                    .unwrap()
                    .contains("authoritative error after cancellation")
            );
            assert!(cancel.is_cancelled());
        }
    }

    #[test]
    fn refresh_failure_is_visible_preserves_outcome_and_only_new_event_retries() {
        let (runtime, mut coordinator, entered, release) = blocked();
        let mut app = app();
        coordinator.event(&mut app, run_event("/run/report.json"), &mut None);
        entered.recv().unwrap();
        let outcome = app.outcome();
        release
            .send(Err(anyhow::anyhow!("transient read failed")))
            .unwrap();
        wait_next(&runtime, &mut coordinator, &mut app);
        assert!(app.status.contains("transient read failed"));
        assert_eq!(app.outcome(), outcome);
        assert!(app.terminal_error.is_none());
        for _ in 0..10 {
            coordinator.poll(&mut app);
        }
        assert!(coordinator.in_flight.is_none());
        coordinator.event(&mut app, run_event("/run/report.json"), &mut None);
        entered.recv().unwrap();
        release.send(loaded("recovered")).unwrap();
        wait_next(&runtime, &mut coordinator, &mut app);
        assert_eq!(app.report.as_ref().unwrap().run_id, "recovered");
        assert!(!app.status.contains("transient read failed"));
    }
}
