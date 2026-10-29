//! Responsive live and offline evidence UI; one blocking engine runs on a worker.
mod animation;
mod app;
mod input;
mod render;
pub mod terminal;
use crate::{
    engine::{self, CancellationToken, RunResult},
    report,
};
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
    let mut result_rx: Option<oneshot::Receiver<Result<RunResult>>> = None;
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
                let event: engine::EngineEvent = event;
                last_path = Some(event.report_path.clone());
                if let Ok(report) = report::load(&event.report_path) {
                    app.set_report(report, event.report_path.parent().unwrap().to_path_buf());
                    app.status = format!(
                        "Running {:?} {} {}",
                        event.kind,
                        event.scenario_id.unwrap_or_default(),
                        event.stage.unwrap_or_default()
                    );
                }
            }
            if let Some(receiver) = &mut result_rx {
                match receiver.try_recv() {
                    Ok(result) => {
                        complete_run(&mut app, result)?;
                        result_rx = None;
                        app.refresh_logs();
                    }
                    Err(oneshot::error::TryRecvError::Closed) => {
                        return Err(anyhow!("engine worker ended without delivering its result"));
                    }
                    Err(oneshot::error::TryRecvError::Empty) => {}
                }
            }
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
            if let Some(result) = quit_result(&app, &mut presentation, started.elapsed()) {
                return result;
            }
            if app.quit_requested && app.active {
                presentation.bypass();
                cancel.cancel();
                let deadline =
                    quit_deadline.get_or_insert_with(|| Instant::now() + Duration::from_secs(5));
                if Instant::now() >= *deadline {
                    return Err(anyhow!(
                        "cleanup window expired; worker may still be cleaning. Inspect saved report/journal and run theseus cleanup {}. Unresolved resources: {}",
                        last_path
                            .as_ref()
                            .and_then(|p| p.parent())
                            .map_or("<pending report>".into(), |p| p.display().to_string()),
                        unresolved(&app)
                    ));
                }
            }
            if app.start_requested && !app.quit_requested {
                app.start_requested = false;
                let recipe = app.recipe.as_ref().unwrap().clone();
                let options = app.options.as_ref().unwrap().clone();
                if let Err(error) = recipe.validate_selection(&options.profiles, &options.scenarios)
                {
                    app.status = format!("Cannot start: {error:#}");
                } else {
                    let (tx, rx) = oneshot::channel();
                    result_rx = Some(rx);
                    let events = events_tx.clone();
                    let token = cancel.clone();
                    app.active = true;
                    app.status = "Starting engine".into();
                    runtime.spawn_blocking(move || {
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            engine::run(&recipe, options, token, |e| {
                                let _ = events.try_send(e);
                            })
                        }))
                        .unwrap_or_else(|_| {
                            Err(anyhow!("engine worker panicked; inspect recovery journal"))
                        });
                        let _ = tx.send(result);
                    });
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
    drop(terminal);
    drop(guard);
    drop(signals);
    // Dropping a runtime normally waits indefinitely for blocking work. The engine
    // retains ownership of its journal; forced quit must restore the terminal now.
    runtime.shutdown_timeout(Duration::ZERO);
    result
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

fn unresolved(app: &App) -> String {
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
struct Signals(Vec<signal_hook::SigId>);
impl Signals {
    fn register(cancel: &CancellationToken) -> Result<Self> {
        let mut signals = Self(Vec::new());
        for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
            signals
                .0
                .push(signal_hook::flag::register(signal, cancel.flag())?);
        }
        Ok(signals)
    }
}
impl Drop for Signals {
    fn drop(&mut self) {
        for id in self.0.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

// Shared completion boundary, also exercised without a terminal in regressions.
fn complete_run(app: &mut App, result: Result<RunResult>) -> Result<()> {
    app.active = false;
    match result {
        Ok(result) => {
            let root = result.report_path.parent().unwrap().to_path_buf();
            app.set_report(result.report, root);
            app.status = format!("Finished | {}", result.report_path.display());
        }
        Err(error) => {
            let diagnostic = format!("Engine error: {error:#}");
            app.terminal_error = Some(if app.root.as_os_str().is_empty() {
                diagnostic.clone()
            } else {
                format!(
                    "{diagnostic}\nSaved report: {}\nRecovery: theseus cleanup {}",
                    app.root.join("report.json").display(),
                    app.root.display()
                )
            });
            app.status =
                "Engine error; incomplete result — inspect evidence and recovery path".into();
            app.evidence_index = 0;
            app.scroll = 0;
            app.refresh_logs();
            if app.report.is_none() {
                return Err(error);
            }
        }
    }
    Ok(())
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
