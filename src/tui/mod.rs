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
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
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
            if app.quit_requested {
                if !app.active {
                    if let Some(error) = &app.terminal_error {
                        return Err(anyhow!("{error}"));
                    }
                    if presentation.quit(started.elapsed()) {
                        return Ok(app.exit_code());
                    }
                }
                if app.active {
                    presentation.bypass();
                    cancel.cancel();
                    let deadline = quit_deadline
                        .get_or_insert_with(|| Instant::now() + Duration::from_secs(5));
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
                            if key.code == KeyCode::Char('c')
                                && key.modifiers.contains(KeyModifiers::CONTROL)
                            {
                                terminate.cancel();
                                presentation.bypass();
                            } else {
                                match presentation.key(key.code) {
                                    KeyAction::Browse => input(&mut app, key),
                                    KeyAction::Quit => app.quit_requested = true,
                                    KeyAction::Ignore => {}
                                }
                            }
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
            if app.quit_requested
                && !app.active
                && decorative_frame.is_none()
                && presentation.quit(started.elapsed())
            {
                return Ok(app.exit_code());
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
