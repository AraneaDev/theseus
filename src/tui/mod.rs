//! Responsive live and offline evidence UI; one blocking engine runs on a worker.
mod animation;
mod app;
mod comparison;
mod editor;
pub mod evidence;
mod input;
pub mod paging;
mod preview_edit;
mod refresh;
mod render;
pub mod terminal;
mod view;
use crate::engine::{self, CancellationToken, RunResult};
use animation::{KeyAction, Presentation};
use anyhow::{Result, anyhow};
pub use app::{App, Focus, Tab, safe_text};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
pub use input::input;
pub use render::render;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};

/// Interactive exit reports the selected run's outcome; preview exit is successful.
pub fn run(app: App) -> Result<i32> {
    run_with_options(app, AnimationOptions::default())
}

#[derive(Clone, Copy, Debug)]
pub struct AnimationOptions {
    pub enabled: bool,
}
impl Default for AnimationOptions {
    fn default() -> Self {
        Self { enabled: true }
    }
}

pub fn run_with_options(mut app: App, options: AnimationOptions) -> Result<i32> {
    terminal::require_tty()?;
    let runtime = tokio::runtime::Runtime::new()?;
    let cancel = CancellationToken::default();
    let terminate = CancellationToken::default();
    let signals = Signals::register(&terminate)?;
    let (guard, mut terminal) = terminal::Guard::enter()?;
    let (events_tx, mut events_rx) = mpsc::channel(32);
    let mut result_rx: Option<oneshot::Receiver<Result<WorkResult>>> = None;
    let mut refresh = refresh::Coordinator::live(runtime.handle().clone());
    let mut quit_deadline = None;
    let mut last_path: Option<PathBuf> = None;
    let started = Instant::now();
    let mut presentation = Presentation::new(
        options.enabled && std::env::var_os("TERM").is_none_or(|term| term != "dumb"),
        Duration::ZERO,
    );
    let result = (|| -> Result<i32> {
        loop {
            let mut latest = None;
            while let Ok(event) = events_rx.try_recv() {
                latest = Some(event);
            }
            if let Some(event) = latest {
                refresh.event(&mut app, event, &mut last_path);
            }
            service_control(
                &mut app,
                &mut presentation,
                &cancel,
                &terminate,
                &mut quit_deadline,
                Instant::now(),
            );
            if let Some(receiver) = &mut result_rx {
                match receiver.try_recv() {
                    Ok(result) => {
                        result_rx = None;
                        refresh.complete(&mut app, result)?;
                    }
                    Err(oneshot::error::TryRecvError::Closed) => {
                        return Err(anyhow!("engine worker ended without delivering its result"));
                    }
                    Err(oneshot::error::TryRecvError::Empty) => {}
                }
            }
            if let Some(result) = quit_result(&app, &mut presentation, started.elapsed()) {
                return result;
            }
            if let Some(error) =
                cleanup_timeout(&app, quit_deadline, last_path.as_ref(), Instant::now())
            {
                return Err(error);
            }
            refresh.poll(&mut app);
            if app.start_requested && !app.quit_requested {
                match take_work(&mut app) {
                    Err(error) => app.status = format!("Cannot start: {error:#}"),
                    Ok(None) => {}
                    Ok(Some(work)) => {
                        let (tx, rx) = oneshot::channel();
                        result_rx = Some(rx);
                        let events = events_tx.clone();
                        let token = cancel.clone();
                        app.active = true;
                        app.status = "Starting engine".into();
                        runtime.spawn_blocking(move || {
                            let result =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    work.run(token, events)
                                }))
                                .unwrap_or_else(|payload| {
                                    Err(crate::runner::process::worker_panic(payload))
                                });
                            let _ = tx.send(result);
                        });
                    }
                }
            }
            if event::poll(Duration::from_millis(50))? {
                for _ in 0..64 {
                    match event::read()? {
                        Event::Key(key) if key.kind != KeyEventKind::Release => {
                            handle_key(
                                &mut app,
                                &mut presentation,
                                key,
                                &cancel,
                                &terminate,
                                &mut quit_deadline,
                                Instant::now(),
                            );
                        }
                        Event::Paste(text)
                            if presentation.key(KeyCode::Null) == KeyAction::Browse =>
                        {
                            preview_edit::paste(&mut app, &text)
                        }
                        _ => {}
                    }
                    // Drain keys already received during the scene before advancing it.
                    // Bound each batch so a pasted input stream cannot starve cancellation.
                    if app.quit_requested
                        || terminate.is_cancelled()
                        || !event::poll(Duration::ZERO)?
                    {
                        break;
                    }
                }
            }
            // Keep the scene's input policy until all its queued keys have been
            // consumed. Returning to the loop between batches still services
            // signals, worker results and the cleanup deadline under input load.
            if event::poll(Duration::ZERO)? && !app.quit_requested && !terminate.is_cancelled() {
                continue;
            }
            if terminate.is_cancelled() {
                presentation.bypass();
                if !app.quit_requested {
                    continue;
                }
            }
            let size = terminal.size()?;
            let decorative_frame = presentation.frame(started.elapsed(), size.width, size.height);
            // A completed outro exits without flashing the workbench underneath it.
            if decorative_frame.is_none()
                && let Some(result) = quit_result(&app, &mut presentation, started.elapsed())
            {
                return result;
            }
            terminal.draw(|f| {
                if let Some((direction, index)) = decorative_frame {
                    animation::draw(f, index, direction);
                } else {
                    render(f, &app);
                }
            })?;
        }
    })();
    if app.active {
        cancel.cancel();
    }
    let result = finish_runtime(runtime, &cancel, result_rx, result, || {
        drop(terminal);
        drop(guard);
    });
    drop(signals);
    result
}
fn finish_runtime<T>(
    runtime: tokio::runtime::Runtime,
    cancel: &CancellationToken,
    receiver: Option<oneshot::Receiver<Result<T>>>,
    result: Result<i32>,
    restore: impl FnOnce(),
) -> Result<i32> {
    if receiver.is_some() {
        cancel.cancel();
    }
    // The terminal is restored promptly even when native cleanup cannot yet
    // finish. A delivered result is not a join: Drop waits for all blocking work.
    restore();
    let final_worker = receiver.map(|receiver| {
        receiver
            .blocking_recv()
            .map_err(|_| anyhow!("engine worker ended without delivering its result"))
            .and_then(|result| result)
    });
    drop(runtime);
    let mut retained = crate::runner::process::CleanupFailures::default();
    if let Err(error) = &result {
        retained.retain(error);
    }
    if let Some(Err(error)) = &final_worker {
        retained.retain(error);
    }
    if let Err(error) = retained.finish() {
        return Err(match &result {
            Err(original) => error.context(format!("{original:#}")),
            Ok(_) => error,
        });
    }
    match (result, final_worker) {
        (Err(original), _) => Err(original),
        (Ok(_), Some(Err(error))) => Err(error),
        (Ok(code), _) => Ok(code),
    }
}

fn quit_result(app: &App, presentation: &mut Presentation, now: Duration) -> Option<Result<i32>> {
    if !app.quit_requested || app.active {
        return None;
    }
    if let Some(error) = &app.terminal_error {
        return Some(Err(anyhow!("{error}")));
    }
    presentation.quit(now).then(|| Ok(app.exit_code()))
}

fn handle_key(
    app: &mut App,
    presentation: &mut Presentation,
    key: KeyEvent,
    cancel: &CancellationToken,
    terminate: &CancellationToken,
    quit_deadline: &mut Option<Instant>,
    now: Instant,
) {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        terminate.cancel();
        presentation.bypass();
    } else {
        match presentation.key(key.code) {
            KeyAction::Browse => input(app, key),
            KeyAction::Quit => app.quit_requested = true,
            KeyAction::Ignore => {}
        }
    }
    // Latch the active-input boundary before a ready completion can clear active.
    // The persisted result remains authoritative if completion already won.
    if app.active && (app.quit_requested || terminate.is_cancelled()) {
        app.cancel_requested = true;
        cancel.cancel();
        presentation.bypass();
        quit_deadline.get_or_insert(now + Duration::from_secs(5));
    }
}

// Service signals before completion can clear active, just like the key boundary.
fn service_control(
    app: &mut App,
    presentation: &mut Presentation,
    cancel: &CancellationToken,
    terminate: &CancellationToken,
    quit_deadline: &mut Option<Instant>,
    now: Instant,
) {
    if terminate.is_cancelled() {
        presentation.bypass();
        app.quit_requested = true;
        if app.active {
            app.cancel_requested = true;
        }
    }
    if app.cancel_requested && app.active {
        cancel.cancel();
        app.status = "Cancellation requested; waiting for owned-resource teardown".into();
    }
    if app.quit_requested && app.active {
        presentation.bypass();
        cancel.cancel();
        quit_deadline.get_or_insert(now + Duration::from_secs(5));
    }
}

fn cleanup_timeout(
    app: &App,
    deadline: Option<Instant>,
    last_path: Option<&PathBuf>,
    now: Instant,
) -> Option<anyhow::Error> {
    (app.quit_requested && app.active && deadline.is_some_and(|deadline| now >= deadline)).then(|| anyhow!(
        "cleanup window expired; worker may still be cleaning. Inspect saved report/journal and run theseus cleanup {}. Unresolved resources: {}",
        last_path.and_then(|p| p.parent()).map_or("<pending report>".into(), crate::shell::path_argument),
        unresolved(app)
    ))
}

fn unresolved(app: &App) -> String {
    if app.comparison.is_some() {
        return format!(
            "inspect suite {} and contained source journals; source teardown may be pending",
            app.root.display()
        );
    }
    app.report
        .as_ref()
        .map(|r| {
            r.scenarios
                .iter()
                .filter(|s| !matches!(s.teardown, crate::model::TeardownStatus::Complete))
                .map(|s| {
                    format!(
                        "{} container={:?} teardown={:?}",
                        s.id, s.container_id, s.teardown
                    )
                })
                .collect::<Vec<_>>()
                .join("; ")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "setup may be pending; inspect journal".into())
}
pub(crate) struct Signals {
    _registration: theseus_host::cancel::CancellationRegistration,
}
impl Signals {
    pub(crate) fn register(cancel: &CancellationToken) -> Result<Self> {
        Ok(Self {
            _registration: theseus_host::cancel::CancellationRegistration::install(cancel.flag())?,
        })
    }
}

enum Work {
    Run(crate::recipe::ValidatedRecipe, engine::RunOptions),
    Suite(
        crate::comparison::ValidatedSuite,
        crate::comparison::SuiteRunOptions,
    ),
}
enum WorkEvent {
    Run(engine::EngineEvent),
    Suite(crate::comparison::ComparisonEvent),
}
enum WorkResult {
    Run(Box<RunResult>),
    Suite(crate::comparison::ComparisonResult),
}
impl Work {
    fn run(self, token: CancellationToken, events: mpsc::Sender<WorkEvent>) -> Result<WorkResult> {
        match self {
            Self::Run(recipe, options) => engine::run(&recipe, options, token, |e| {
                let _ = events.try_send(WorkEvent::Run(e));
            })
            .map(|result| WorkResult::Run(Box::new(result))),
            Self::Suite(suite, options) => crate::comparison::run(&suite, options, token, |e| {
                let _ = events.try_send(WorkEvent::Suite(e));
            })
            .map(WorkResult::Suite),
        }
    }
}
fn take_work(app: &mut App) -> Result<Option<Work>> {
    if preview_edit::blocks_work(app) {
        app.start_requested = false;
        return Ok(None);
    }
    if !std::mem::take(&mut app.start_requested)
        || app.active
        || app.quit_requested
        || app.work_consumed
    {
        return Ok(None);
    }
    if let Some(view) = &app.comparison {
        if view.report.is_some() {
            return Ok(None);
        }
        let work = view
            .suite
            .clone()
            .zip(view.options.clone())
            .map(|(s, o)| Work::Suite(s, o));
        app.work_consumed = work.is_some();
        if work.is_some() {
            app.edit_session = None;
        }
        return Ok(work);
    }
    if app.report.is_some() {
        return Ok(None);
    }
    let Some((recipe, options)) = app.recipe.clone().zip(app.options.clone()) else {
        return Ok(None);
    };
    recipe.validate_selection(&options.profiles, &options.scenarios)?;
    app.work_consumed = true;
    app.edit_session = None;
    Ok(Some(Work::Run(recipe, options)))
}
// Latch recovery context before scheduling any disk work.
fn apply_event(
    app: &mut App,
    event: WorkEvent,
    last_path: &mut Option<PathBuf>,
) -> (refresh::Kind, PathBuf) {
    let (kind, path, status) = match event {
        WorkEvent::Run(event) => (
            refresh::Kind::Run,
            event.report_path,
            format!(
                "Running {:?} {} {}",
                event.kind,
                event.scenario_id.unwrap_or_default(),
                event.stage.unwrap_or_default()
            ),
        ),
        WorkEvent::Suite(event) => (
            refresh::Kind::Suite,
            event.report_path,
            format!(
                "Running suite | {} {}",
                event.variant_id.unwrap_or_default(),
                event.source.map_or(String::new(), |e| format!(
                    "{:?} {}",
                    e.kind,
                    e.stage.unwrap_or_default()
                ))
            ),
        ),
    };
    *last_path = Some(path.clone());
    app.pending_root(
        path.parent()
            .unwrap_or(std::path::Path::new("."))
            .to_path_buf(),
    );
    app.status = status;
    (kind, path)
}
// Shared completion boundary, also exercised without a terminal in regressions.
fn complete_work(app: &mut App, result: Result<WorkResult>) -> Result<()> {
    app.active = false;
    match result {
        Ok(WorkResult::Run(result)) => {
            let root = result.report_path.parent().unwrap().to_path_buf();
            app.set_report(result.report, root);
            app.status = format!("Finished | {}", result.report_path.display());
        }
        Ok(WorkResult::Suite(result)) => {
            app.set_comparison_report(
                result.report,
                result.report_path.parent().unwrap().to_path_buf(),
            );
            app.status = format!("Finished | {}", result.report_path.display());
        }
        Err(error) => {
            let mut cleanup = crate::runner::process::CleanupFailures::default();
            cleanup.retain(&error);
            // An unresolved native owner must leave the UI with its typed error;
            // the outer terminal guard restores modes before propagating it.
            if cleanup.pending() {
                return Err(error);
            }
            let diagnostic = format!("Engine error: {error:#}");
            app.terminal_error = Some(if app.root.as_os_str().is_empty() {
                diagnostic.clone()
            } else {
                format!(
                    "{diagnostic}\nSaved report: {}\nRecovery: theseus cleanup {}",
                    app.root
                        .join(if app.comparison.is_some() {
                            "comparison.json"
                        } else {
                            "report.json"
                        })
                        .display(),
                    crate::shell::path_argument(&app.root)
                )
            });
            app.status =
                "Engine error; incomplete result — inspect evidence and recovery path".into();
            app.evidence_index = 0;
            app.scroll = 0;
            app.refresh_logs();
            if app.report.is_none() && app.comparison.as_ref().is_none_or(|v| v.report.is_none()) {
                return Err(error);
            }
        }
    }
    Ok(())
}
#[cfg(test)]
fn complete_run(app: &mut App, result: Result<RunResult>) -> Result<()> {
    complete_work(app, result.map(|result| WorkResult::Run(Box::new(result))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        ProfileName, RunOutcome, RunReport, ScenarioKind, ScenarioReport, StageOutcome,
        StageReport, TeardownStatus,
    };
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn event_latches_recovery_and_status_even_when_report_cannot_be_loaded() {
        let mut app = App::offline(RunReport::new("old-root-evidence"), PathBuf::from("/old"));
        app.active = true;
        let mut last = None;
        apply_event(
            &mut app,
            WorkEvent::Run(engine::EngineEvent {
                kind: engine::EventKind::StageStarted,
                report_path: PathBuf::from("/missing-new-root/report.json"),
                scenario_id: Some("user-clean".into()),
                stage: Some("install".into()),
            }),
            &mut last,
        );
        assert!(
            app.status.contains("StageStarted"),
            "event status must not wait for a successful read"
        );
        assert_eq!(last, Some(PathBuf::from("/missing-new-root/report.json")));
        assert_eq!(app.root, PathBuf::from("/missing-new-root"));
        assert!(
            app.report.is_none(),
            "old document must not acquire a different root"
        );
        assert!(complete_run(&mut app, Err(anyhow!("final persistence failed"))).is_err());
        assert!(
            app.terminal_error
                .as_ref()
                .unwrap()
                .contains("/missing-new-root/report.json")
        );
    }

    #[test]
    fn cached_worker_error_quit_keeps_diagnostic_when_animation_is_disabled_dumb_or_small() {
        for mode in ["disabled", "dumb", "small"] {
            let mut report = RunReport::new("cached-before-error");
            report.outcome = RunOutcome::Passed;
            let mut app = App::offline(report, PathBuf::from("/tmp/cached-worker-error"));
            app.active = true;
            complete_run(&mut app, Err(anyhow!("final persistence failed"))).unwrap();
            let mut presentation = Presentation::new(mode == "small", Duration::ZERO);
            let size = if mode == "small" { (39, 17) } else { (80, 24) };
            presentation.frame(Duration::ZERO, size.0, size.1);
            handle_key(
                &mut app,
                &mut presentation,
                KeyCode::Char('q').into(),
                &CancellationToken::default(),
                &CancellationToken::default(),
                &mut None,
                Instant::now(),
            );
            let error = quit_result(&app, &mut presentation, Duration::ZERO)
                .unwrap()
                .unwrap_err()
                .to_string();
            for expected in [
                "Engine error: final persistence failed",
                "Saved report: /tmp/cached-worker-error/report.json",
                "Recovery: theseus cleanup /tmp/cached-worker-error",
            ] {
                assert!(error.contains(expected), "{mode}: {error}");
            }
        }
    }

    #[test]
    fn active_quit_latches_cancel_deadline_and_outro_bypass_before_ready_completion() {
        for outcome in [
            RunOutcome::Passed,
            RunOutcome::Failed,
            RunOutcome::Cancelled,
            RunOutcome::Incomplete,
        ] {
            let mut app = App::offline(
                RunReport::new("cached-running"),
                PathBuf::from("/tmp/ready-completion"),
            );
            app.active = true;
            let mut presentation = Presentation::new(true, Duration::ZERO);
            assert_eq!(
                presentation.frame(Duration::from_millis(5400), 80, 24),
                None
            );
            let cancel = CancellationToken::default();
            let terminate = CancellationToken::default();
            let now = Instant::now();
            let mut deadline = None;
            let (sender, mut receiver) = oneshot::channel();
            let mut persisted = RunReport::new("persisted-completion");
            persisted.outcome = outcome;
            persisted.cancelled = outcome == RunOutcome::Cancelled;
            assert!(
                sender
                    .send(Ok(RunResult {
                        report: persisted,
                        report_path: PathBuf::from("/tmp/ready-completion/report.json")
                    }))
                    .is_ok()
            );
            handle_key(
                &mut app,
                &mut presentation,
                KeyCode::Char('q').into(),
                &cancel,
                &terminate,
                &mut deadline,
                now,
            );
            assert!(cancel.is_cancelled(), "q must cancel at input time");
            assert_eq!(deadline, Some(now + Duration::from_secs(5)));
            complete_run(&mut app, receiver.try_recv().unwrap()).unwrap();
            assert!(!app.active);
            assert_eq!(
                presentation.frame(Duration::from_millis(5401), 80, 24),
                None
            );
            let result = quit_result(&app, &mut presentation, Duration::from_millis(5401));
            assert!(
                result.is_some(),
                "active q must not start an outro after ready completion"
            );
            assert_eq!(result.unwrap().unwrap(), outcome.exit_code());
            assert_eq!(app.report.as_ref().unwrap().outcome, outcome);
            assert_eq!(
                app.report.as_ref().unwrap().cancelled,
                outcome == RunOutcome::Cancelled
            );
        }
    }

    #[test]
    fn terminal_worker_error_overrides_cached_passed_and_failed_results() {
        for cached in [RunOutcome::Passed, RunOutcome::Failed] {
            let mut report = RunReport::new("cached-before-final-save");
            report.outcome = cached;
            let mut scenario =
                ScenarioReport::new("user-clean", ProfileName::User, ScenarioKind::Clean);
            scenario.outcome = cached;
            scenario.teardown = TeardownStatus::Complete;
            let mut stage = StageReport::new("final");
            stage.outcome = if cached == RunOutcome::Passed {
                StageOutcome::Passed
            } else {
                StageOutcome::Failed
            };
            scenario.stages.push(stage);
            report.scenarios.push(scenario);
            let mut app = App::offline(report, PathBuf::from("/tmp/recovery-run"));
            app.active = true;
            app.filter = "unrelated old filter".into();
            complete_run(&mut app, Err(anyhow!("final persistence failed"))).unwrap();
            assert!(!app.active);
            input(
                &mut app,
                crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char('q')),
            );
            assert!(app.quit_requested);
            assert_eq!(
                app.exit_code(),
                2,
                "cached {cached} must not mask worker failure"
            );
            assert_eq!(
                app.report.as_ref().unwrap().outcome,
                cached,
                "retain the last saved evidence"
            );
            for tab in Tab::ALL {
                app.tab = tab;
                let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
                terminal.draw(|frame| render(frame, &app)).unwrap();
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                for expected in [
                    "cached-before-final-save | incomplete |",
                    "final persistence failed",
                    "/tmp/recovery-run/report.json",
                    "theseus cleanup /tmp/recovery-run",
                ] {
                    assert!(text.contains(expected), "missing {expected}");
                }
            }
        }
    }
}

#[cfg(test)]
mod comparison_tests {
    use super::*;
    use crate::comparison::{self as suites, ComparisonResult, LoadedComparison, SuiteRunOptions};
    use ratatui::{Terminal, backend::TestBackend};
    fn screen(app: &App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
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
    fn suite_start_is_consumed_once_and_original_preview_matrix_is_unchanged() {
        let t = tempfile::tempdir().unwrap();
        let path = fixtures::recipe_suite(t.path());
        let suite = suites::load(&path).unwrap();
        let mut app = App::comparison_preview(
            suite,
            SuiteRunOptions {
                output_root: t.path().join("output"),
                pull: true,
            },
        );
        for k in ['p', 's', 'r', 'r'] {
            input(&mut app, KeyCode::Char(k).into());
        }
        let work = take_work(&mut app).unwrap().unwrap();
        let Work::Suite(suite, options) = work else {
            panic!("suite must dispatch through suite worker");
        };
        assert_eq!(
            suite.manifest.profiles,
            vec![crate::model::ProfileName::User]
        );
        assert_eq!(
            suite.manifest.scenarios,
            vec![crate::model::ScenarioKind::Clean]
        );
        assert!(options.pull);
        assert!(!app.start_requested);
        assert!(take_work(&mut app).unwrap().is_none());
        app.active = true;
        input(&mut app, KeyCode::Char('r').into());
        assert!(take_work(&mut app).unwrap().is_none());
        assert!(!t.path().join("output").exists());
    }
    #[test]
    fn editing_recipe_retains_dirty_draft_and_blocks_stale_start() {
        let t = tempfile::tempdir().unwrap();
        fixtures::recipe_suite(t.path());
        let recipe = crate::recipe::load(&t.path().join("copy.toml")).unwrap();
        let mut app = App::preview(recipe.clone(), engine::RunOptions::all(&recipe));
        input(&mut app, KeyCode::Char('r').into());
        input(&mut app, KeyCode::Char('e').into());
        assert!(take_work(&mut app).unwrap().is_none());
        input(&mut app, KeyCode::Char('q').into());
        input(&mut app, KeyCode::Char('r').into());
        assert!(!app.quit_requested);
        assert!(!app.start_requested);
        input(&mut app, KeyCode::Esc.into());
        input(&mut app, KeyCode::Char('r').into());
        assert!(take_work(&mut app).unwrap().is_none());
        input(&mut app, KeyCode::Char('d').into());
        input(&mut app, KeyCode::Esc.into());
        assert!(take_work(&mut app).unwrap().is_none());
        input(&mut app, KeyCode::Char('d').into());
        input(&mut app, KeyCode::Enter.into());
        assert!(!app.start_requested);
        input(&mut app, KeyCode::Char('r').into());
        assert!(take_work(&mut app).unwrap().is_some());
    }
    #[test]
    fn unchanged_editor_closes_before_launch_and_cannot_resume_after_launch() {
        let t = tempfile::tempdir().unwrap();
        fixtures::recipe_suite(t.path());
        let recipe = crate::recipe::load(&t.path().join("copy.toml")).unwrap();
        let mut app = App::preview(recipe.clone(), engine::RunOptions::all(&recipe));
        input(&mut app, KeyCode::Char('e').into());
        input(&mut app, KeyCode::Esc.into());
        input(&mut app, KeyCode::Char('r').into());
        assert!(take_work(&mut app).unwrap().is_some());
        input(&mut app, KeyCode::Char('e').into());
        input(&mut app, KeyCode::Char('q').into());
        assert!(app.quit_requested);
    }
    #[test]
    fn recipe_validation_quit_confirmation_and_failed_save_retain_draft() {
        let t = tempfile::tempdir().unwrap();
        fixtures::recipe_suite(t.path());
        let recipe = crate::recipe::load(&t.path().join("copy.toml")).unwrap();
        let mut app = App::preview(recipe.clone(), engine::RunOptions::all(&recipe));
        input(&mut app, KeyCode::Char('e').into());
        preview_edit::paste(&mut app, "\n[");
        input(
            &mut app,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
        );
        assert!(screen(&app).contains("Invalid draft"));
        input(
            &mut app,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        );
        input(&mut app, KeyCode::Enter.into());
        assert!(screen(&app).contains("Save Copy failed"));
        input(&mut app, KeyCode::Esc.into());
        input(&mut app, KeyCode::Esc.into());
        input(&mut app, KeyCode::Char('q').into());
        assert!(!app.quit_requested);
        input(&mut app, KeyCode::Esc.into());
        assert!(take_work(&mut app).unwrap().is_none());
        input(&mut app, KeyCode::Char('q').into());
        input(&mut app, KeyCode::Enter.into());
        assert!(app.quit_requested);
        assert!(!t.path().join("copy.edited.toml").exists());
    }
    #[test]
    fn recipe_save_copy_adopts_bytes_and_requires_fresh_run() {
        let t = tempfile::tempdir().unwrap();
        fixtures::recipe_suite(t.path());
        let recipe = crate::recipe::load(&t.path().join("copy.toml")).unwrap();
        let old = std::fs::read(&recipe.path).unwrap();
        let mut app = App::preview(recipe.clone(), engine::RunOptions::all(&recipe));
        input(&mut app, KeyCode::Char('r').into());
        input(&mut app, KeyCode::Char('e').into());
        preview_edit::paste(&mut app, "\n# café\n");
        input(
            &mut app,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        );
        input(&mut app, KeyCode::Enter.into());
        let saved = t.path().join("copy.edited.toml");
        assert_eq!(
            std::fs::read(&saved).unwrap(),
            [old.clone(), b"\n# caf\xc3\xa9\n".to_vec()].concat()
        );
        assert_eq!(std::fs::read(&recipe.path).unwrap(), old);
        assert_eq!(app.recipe.as_ref().unwrap().path, saved);
        assert!(!app.start_requested);
        assert!(take_work(&mut app).unwrap().is_none());
        input(&mut app, KeyCode::Char('r').into());
        let Some(Work::Run(adopted, _)) = take_work(&mut app).unwrap() else {
            panic!("saved recipe must launch")
        };
        assert_eq!(adopted.path, saved);
    }
    #[test]
    fn save_copy_falls_back_to_newly_declared_recipe_selection() {
        let t = tempfile::tempdir().unwrap();
        fixtures::recipe_suite(t.path());
        let recipe = crate::recipe::load(&t.path().join("copy.toml")).unwrap();
        let mut options = engine::RunOptions::all(&recipe);
        options.profiles = vec![crate::model::ProfileName::System];
        options.scenarios = vec![crate::model::ScenarioKind::Upgrade];
        options.pull = true;
        let mut app = App::preview(recipe, options);
        input(&mut app, KeyCode::Char('e').into());
        input(
            &mut app,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        );
        input(&mut app, KeyCode::Enter.into());
        let selected = app.options.as_ref().unwrap();
        assert_eq!(selected.profiles, vec![crate::model::ProfileName::User]);
        assert_eq!(selected.scenarios, vec![crate::model::ScenarioKind::Clean]);
        assert!(selected.pull);
        assert!(!app.start_requested);
    }
    #[test]
    fn suite_save_copy_adopts_manifest_and_paste_outside_editor_is_inert() {
        let t = tempfile::tempdir().unwrap();
        let path = fixtures::recipe_suite(t.path());
        let mut app = App::comparison_preview(
            suites::load(&path).unwrap(),
            SuiteRunOptions {
                output_root: t.path().join("out"),
                pull: true,
            },
        );
        preview_edit::paste(&mut app, "rq\n");
        assert!(!app.start_requested && !app.quit_requested);
        input(&mut app, KeyCode::Char('e').into());
        preview_edit::paste(&mut app, "\n# suite edit\n");
        input(&mut app, KeyCode::Esc.into());
        input(&mut app, KeyCode::Char('r').into());
        assert!(take_work(&mut app).unwrap().is_none());
        input(&mut app, KeyCode::Char('e').into());
        input(
            &mut app,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        );
        input(&mut app, KeyCode::Enter.into());
        let saved = t.path().join("comparison.edited.toml");
        assert!(saved.exists());
        assert_eq!(
            app.comparison
                .as_ref()
                .unwrap()
                .suite
                .as_ref()
                .unwrap()
                .path,
            saved
        );
        assert!(!app.start_requested);
        input(&mut app, KeyCode::Char('r').into());
        let Some(Work::Suite(suite, options)) = take_work(&mut app).unwrap() else {
            panic!("saved suite must launch")
        };
        assert_eq!(suite.path, saved);
        assert!(options.pull);
    }
    #[test]
    fn save_copy_name_collision_and_invalid_name_keep_editor_and_original() {
        let t = tempfile::tempdir().unwrap();
        fixtures::recipe_suite(t.path());
        let recipe = crate::recipe::load(&t.path().join("copy.toml")).unwrap();
        let mut app = App::preview(recipe.clone(), engine::RunOptions::all(&recipe));
        input(&mut app, KeyCode::Char('e').into());
        preview_edit::paste(&mut app, "\n# retained\n");
        input(
            &mut app,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        );
        for _ in 0..128 {
            input(&mut app, KeyCode::Backspace.into());
        }
        preview_edit::paste(&mut app, "copy.toml");
        input(&mut app, KeyCode::Enter.into());
        assert!(screen(&app).contains("Save Copy failed"));
        assert_eq!(app.recipe.as_ref().unwrap().path, recipe.path);
        for _ in 0..128 {
            input(&mut app, KeyCode::Backspace.into());
        }
        preview_edit::paste(&mut app, "../escape.toml");
        input(&mut app, KeyCode::Enter.into());
        assert!(screen(&app).contains("Save Copy failed"));
        assert!(!t.path().join("escape.toml").exists());
        for _ in 0..128 {
            input(&mut app, KeyCode::Backspace.into());
        }
        preview_edit::paste(&mut app, "copy.edited.toml");
        std::fs::write(t.path().join("copy.edited.toml"), b"existing").unwrap();
        input(&mut app, KeyCode::Enter.into());
        assert!(screen(&app).contains("Save Copy failed"));
        assert_eq!(
            std::fs::read(t.path().join("copy.edited.toml")).unwrap(),
            b"existing"
        );
        input(&mut app, KeyCode::Esc.into());
        assert!(screen(&app).contains("UNSAVED"));
    }
    #[test]
    fn failed_save_copy_names_attempted_leaf_even_under_long_source_path() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("a".repeat(140)).join("b".repeat(140));
        std::fs::create_dir_all(&root).unwrap();
        fixtures::recipe_suite(&root);
        let recipe = crate::recipe::load(&root.join("copy.toml")).unwrap();
        let mut app = App::preview(recipe.clone(), engine::RunOptions::all(&recipe));
        std::fs::write(root.join("copy.edited.toml"), b"existing").unwrap();
        input(&mut app, KeyCode::Char('e').into());
        input(
            &mut app,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        );
        input(&mut app, KeyCode::Enter.into());
        assert!(screen(&app).contains("Save Copy failed: copy.edited.toml"));
        assert_eq!(app.recipe.as_ref().unwrap().path, recipe.path);
        assert_eq!(
            std::fs::read(root.join("copy.edited.toml")).unwrap(),
            b"existing"
        );
    }
    #[test]
    fn dirty_preview_labels_accepted_document_and_offline_preview_cannot_edit() {
        let t = tempfile::tempdir().unwrap();
        fixtures::recipe_suite(t.path());
        let recipe = crate::recipe::load(&t.path().join("copy.toml")).unwrap();
        let mut app = App::preview(recipe.clone(), engine::RunOptions::all(&recipe));
        input(&mut app, KeyCode::Char('e').into());
        preview_edit::paste(&mut app, "\n# draft\n");
        input(&mut app, KeyCode::Esc.into());
        let text = screen(&app);
        assert!(text.contains("UNSAVED draft"));
        assert!(text.contains("ACCEPTED recipe preview"));
        let mut offline = App::offline(
            crate::model::RunReport::new("offline"),
            t.path().to_path_buf(),
        );
        input(&mut offline, KeyCode::Char('e').into());
        assert!(offline.edit_session.is_none());
        app.set_report(crate::model::RunReport::new("done"), t.path().to_path_buf());
        input(&mut app, KeyCode::Char('e').into());
        assert!(app.edit_session.is_none());
    }
    #[test]
    fn filter_keeps_literal_e_and_ctrl_c_bypasses_editor_modal() {
        let t = tempfile::tempdir().unwrap();
        fixtures::recipe_suite(t.path());
        let recipe = crate::recipe::load(&t.path().join("copy.toml")).unwrap();
        let mut app = App::preview(recipe.clone(), engine::RunOptions::all(&recipe));
        input(&mut app, KeyCode::Char('/').into());
        input(&mut app, KeyCode::Char('e').into());
        assert_eq!(app.filter_draft, "e");
        assert!(app.edit_session.is_none());
        input(&mut app, KeyCode::Esc.into());
        input(&mut app, KeyCode::Char('e').into());
        let terminate = CancellationToken::default();
        let mut presentation = Presentation::new(false, Duration::ZERO);
        handle_key(
            &mut app,
            &mut presentation,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &CancellationToken::default(),
            &terminate,
            &mut None,
            Instant::now(),
        );
        assert!(terminate.is_cancelled());
    }
    #[test]
    fn invalid_run_selection_does_not_consume_the_single_start() {
        let t = tempfile::tempdir().unwrap();
        fixtures::recipe_suite(t.path());
        let recipe = crate::recipe::load(&t.path().join("copy.toml")).unwrap();
        let mut options = engine::RunOptions::all(&recipe);
        options.profiles = vec![crate::model::ProfileName::System];
        let mut app = App::preview(recipe, options);
        app.start_requested = true;
        assert!(take_work(&mut app).is_err());
        app.options.as_mut().unwrap().profiles = vec![crate::model::ProfileName::User];
        app.start_requested = true;
        assert!(take_work(&mut app).unwrap().is_some());
        app.start_requested = true;
        assert!(take_work(&mut app).unwrap().is_none());
    }
    #[test]
    fn suite_without_first_saved_report_cannot_launch_a_second_engine() {
        let t = tempfile::tempdir().unwrap();
        let path = fixtures::recipe_suite(t.path());
        let mut app = App::comparison_preview(
            suites::load(&path).unwrap(),
            SuiteRunOptions {
                output_root: t.path().join("output"),
                pull: false,
            },
        );
        app.start_requested = true;
        assert!(take_work(&mut app).unwrap().is_some());
        app.active = true;
        assert!(complete_work(&mut app, Err(anyhow!("first save failed"))).is_err());
        app.start_requested = true;
        assert!(
            take_work(&mut app).unwrap().is_none(),
            "an App must consume its engine start once even without a saved report"
        );
    }
    #[test]
    fn suite_ready_completion_retains_cancel_deadline_and_persisted_failed_outcome() {
        let t = tempfile::tempdir().unwrap();
        let root = fixtures::saved(t.path());
        let document = suites::load_document(&root).unwrap();
        let report = document.report.clone();
        let mut app = App::comparison_offline(document, root.clone());
        app.active = true;
        let cancel = CancellationToken::default();
        let now = Instant::now();
        let mut deadline = None;
        let mut presentation = Presentation::new(true, Duration::ZERO);
        presentation.frame(Duration::from_secs(6), 100, 30);
        handle_key(
            &mut app,
            &mut presentation,
            KeyCode::Char('q').into(),
            &cancel,
            &CancellationToken::default(),
            &mut deadline,
            now,
        );
        assert!(cancel.is_cancelled());
        assert_eq!(deadline, Some(now + Duration::from_secs(5)));
        complete_work(
            &mut app,
            Ok(WorkResult::Suite(ComparisonResult {
                report,
                report_path: root.join("comparison.json"),
            })),
        )
        .unwrap();
        assert_eq!(
            quit_result(&app, &mut presentation, Duration::from_secs(7))
                .unwrap()
                .unwrap(),
            1
        );
    }
    #[test]
    fn suite_error_quit_prioritizes_diagnostic_and_suite_recovery_over_cached_outcome() {
        let t = tempfile::tempdir().unwrap();
        let parent = t.path().join("space ' $(touch INJECTED) ;");
        std::fs::create_dir(&parent).unwrap();
        let root = fixtures::saved(&parent);
        let mut app = App::comparison_offline(suites::load_document(&root).unwrap(), root.clone());
        app.active = true;
        complete_work(&mut app, Err(anyhow!("suite final save failed"))).unwrap();
        input(&mut app, KeyCode::Char('q').into());
        let mut p = Presentation::new(false, Duration::ZERO);
        let error = quit_result(&app, &mut p, Duration::ZERO)
            .unwrap()
            .unwrap_err()
            .to_string();
        assert!(error.contains("suite final save failed"));
        assert!(error.contains(&root.join("comparison.json").display().to_string()));
        let command = error
            .lines()
            .find_map(|line| line.strip_prefix("Recovery: "))
            .unwrap();
        let bin = t.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        #[cfg(unix)]
        let out = {
            let receiver = bin.join("theseus");
            std::fs::write(
                &receiver,
                "#!/bin/sh\nprintf '%s\\n' \"$#\" \"$1\" \"$2\"\n",
            )
            .unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&receiver, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::process::Command::new("/bin/sh")
                .args(["-c", command])
                .env("PATH", &bin)
                .current_dir(t.path())
                .output()
                .unwrap()
        };
        #[cfg(windows)]
        let out = {
            let receiver = bin.join("theseus.exe");
            std::fs::copy(
                std::env::var_os("THESEUS_HOST_TEST_FIXTURE")
                    .expect("build trusted host fixture and set THESEUS_HOST_TEST_FIXTURE"),
                &receiver,
            )
            .unwrap();
            let powershell = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
                .join("System32/WindowsPowerShell/v1.0/powershell.exe");
            std::process::Command::new(powershell)
                .args(["-NoProfile", "-NonInteractive", "-Command", command])
                .env("PATH", &bin)
                .current_dir(t.path())
                .output()
                .unwrap()
        };
        assert!(out.status.success());
        assert_eq!(
            String::from_utf8(out.stdout).unwrap().replace("\r\n", "\n"),
            format!("2\ncleanup\n{}\n", root.display())
        );
        assert!(!t.path().join("INJECTED").exists());
        assert_eq!(app.exit_code(), 2);
        assert!(unresolved(&app).contains("source journals"));
        let document = LoadedComparison {
            manifest: fixtures::manifest(),
            report: app.comparison.as_ref().unwrap().report.clone().unwrap(),
        };
        assert_eq!(document.report.outcome, crate::model::RunOutcome::Failed);
    }
    #[test]
    fn suite_startup_paste_cannot_start_and_live_event_retains_validated_recovery_context() {
        let t = tempfile::tempdir().unwrap();
        let path = fixtures::recipe_suite(t.path());
        let mut app = App::comparison_preview(
            suites::load(&path).unwrap(),
            SuiteRunOptions {
                output_root: t.path().join("output"),
                pull: false,
            },
        );
        let mut presentation = Presentation::new(true, Duration::ZERO);
        let cancel = CancellationToken::default();
        let terminate = CancellationToken::default();
        let mut deadline = None;
        for k in ['r', 'p', 's', 'r'] {
            handle_key(
                &mut app,
                &mut presentation,
                KeyCode::Char(k).into(),
                &cancel,
                &terminate,
                &mut deadline,
                Instant::now(),
            );
        }
        assert!(!app.start_requested);
        assert!(take_work(&mut app).unwrap().is_none());
        assert!(!t.path().join("output").exists());
        app.start_requested = true;
        assert!(take_work(&mut app).unwrap().is_some());
        app.active = true;
        let root = fixtures::saved(t.path());
        let mut last = None;
        apply_event(
            &mut app,
            WorkEvent::Suite(suites::ComparisonEvent {
                report_path: root.join("comparison.json"),
                variant_id: Some("archive".into()),
                source: None,
            }),
            &mut last,
        );
        assert_eq!(last, Some(root.join("comparison.json")));
        assert_eq!(app.root, root);
        assert!(app.comparison.as_ref().unwrap().report.is_none());
        assert_eq!(app.outcome(), crate::model::RunOutcome::Incomplete);
        assert_eq!(app.comparison.as_ref().unwrap().manifest.baseline, "copy");
        input(&mut app, KeyCode::Char('r').into());
        assert!(take_work(&mut app).unwrap().is_none());
    }
}

#[cfg(all(test, windows))]
mod native_cleanup_tests {
    use super::*;
    #[test]
    fn cached_report_cannot_hide_live_native_cleanup_owner() {
        use crate::runner::process::native_tests::{finish_native, suspended_failure};
        let (temp, failure) = suspended_failure();
        let mut app = App::offline(
            crate::model::RunReport::new("cached"),
            temp.path().to_path_buf(),
        );
        app.active = true;
        let error = complete_run(&mut app, Err(failure.clone().into()))
            .expect_err("cached evidence must not discard native cleanup authority");
        let mut retained = crate::runner::process::CleanupFailures::default();
        retained.retain(&error);
        assert!(retained.pending());
        assert!(!app.active);
        finish_native(&temp, &failure);
        retained.finish().unwrap();
    }
    #[test]
    fn restored_runtime_return_preserves_worker_panic_owner_over_original_ui_error() {
        use crate::runner::process::{
            CleanupFailures,
            native_tests::{finish_native, suspended_failure},
        };
        let (temp, failure) = suspended_failure();
        let mut cleanup = CleanupFailures::default();
        cleanup.retain(&failure.clone().into());
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (tx, rx) = oneshot::channel::<Result<()>>();
        runtime.spawn_blocking(move || {
            let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                cleanup.resume_unwind(Box::new("original native worker panic"))
            }))
            .unwrap_err();
            tx.send(Err(crate::runner::process::worker_panic(panic)))
                .unwrap();
        });
        let restored = std::cell::Cell::new(false);
        let error = finish_runtime(
            runtime,
            &CancellationToken::default(),
            Some(rx),
            Err(anyhow!("original UI timeout")),
            || restored.set(true),
        )
        .unwrap_err();
        assert!(restored.get());
        assert!(error.to_string().contains("original UI timeout"));
        let mut retained = CleanupFailures::default();
        retained.retain(&error);
        assert!(retained.pending());
        finish_native(&temp, &failure);
        retained.finish().unwrap();
    }
}

#[cfg(test)]
mod worker_join_regressions {
    use super::*;
    #[test]
    fn terminal_restoration_precedes_join_and_delivery_does_not_detach_blocking_worker() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        };
        let restored = Arc::new(AtomicBool::new(false));
        let observed = restored.clone();
        let (release_tx, release_rx) = mpsc::channel();
        let (worker_done_tx, worker_done_rx) = mpsc::channel();
        let (returned_tx, returned_rx) = mpsc::channel();
        let owner = std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let (result_tx, result_rx) = oneshot::channel();
            let (delivered_tx, delivered_rx) = mpsc::channel();
            runtime.spawn_blocking(move || {
                result_tx.send(Ok(())).unwrap();
                delivered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                worker_done_tx.send(()).unwrap();
            });
            delivered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            let result = finish_runtime(
                runtime,
                &CancellationToken::default(),
                Some(result_rx),
                Err(anyhow!("original UI cleanup deadline")),
                || observed.store(true, Ordering::SeqCst),
            );
            returned_tx.send(result).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while !restored.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        let early = returned_rx.recv_timeout(Duration::from_millis(50));
        // Rescue the controlled peer before assertions, even for the RED run.
        release_tx.send(()).unwrap();
        worker_done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        owner.join().unwrap();
        assert!(
            early.is_err(),
            "ordinary return detached a still-running blocking worker"
        );
        let result = returned_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("original UI cleanup deadline"),
            "late worker success must not erase original UI timeout"
        );
    }
    #[test]
    fn disconnected_worker_result_restores_terminal_then_returns_error() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (tx, rx) = oneshot::channel::<Result<()>>();
        runtime.spawn_blocking(move || drop(tx));
        let restored = std::cell::Cell::new(false);
        let error = finish_runtime(
            runtime,
            &CancellationToken::default(),
            Some(rx),
            Ok(0),
            || restored.set(true),
        )
        .unwrap_err();
        assert!(restored.get());
        assert!(error.to_string().contains("without delivering"));
    }
}
