//! Responsive live and offline evidence UI; one blocking engine runs on a worker.
mod app;
mod input;
mod render;
pub mod terminal;
use crate::{
    engine::{self, CancellationToken, RunResult},
    report,
};
use anyhow::{Result, anyhow};
pub use app::{App, Focus, Tab, safe_text};
use crossterm::event::{self, Event};
pub use input::input;
pub use render::render;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};

/// Interactive exit reports the selected run's outcome; preview exit is successful.
pub fn run(mut app: App) -> Result<i32> {
    terminal::require_tty()?;
    let runtime = tokio::runtime::Runtime::new()?;
    let cancel = CancellationToken::default();
    let signals = Signals::register(&cancel)?;
    let (guard, mut terminal) = terminal::Guard::enter()?;
    let (events_tx, mut events_rx) = mpsc::channel(32);
    let mut result_rx: Option<oneshot::Receiver<Result<RunResult>>> = None;
    let mut quit_deadline = None;
    let mut last_path: Option<PathBuf> = None;
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
                        app.active = false;
                        match result {
                            Ok(result) => {
                                let root = result.report_path.parent().unwrap().to_path_buf();
                                app.set_report(result.report, root);
                                app.status = format!("Finished | {}", result.report_path.display());
                            }
                            Err(error) => {
                                app.status = format!("Engine error: {error:#}");
                                if app.report.is_none() {
                                    return Err(error);
                                }
                            }
                        }
                        result_rx = None;
                        app.refresh_logs();
                    }
                    Err(oneshot::error::TryRecvError::Closed) => {
                        return Err(anyhow!("engine worker ended without delivering its result"));
                    }
                    Err(oneshot::error::TryRecvError::Empty) => {}
                }
            }
            if cancel.is_cancelled() && app.active {
                app.cancel_requested = true;
            }
            if app.cancel_requested && app.active {
                cancel.cancel();
                app.status = "Cancellation requested; waiting for owned-resource teardown".into();
            }
            if app.quit_requested {
                if !app.active {
                    return Ok(app.report.as_ref().map_or(0, |r| r.outcome.exit_code()));
                }
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
            if app.start_requested {
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
            terminal.draw(|f| render(f, &app))?;
            if event::poll(Duration::from_millis(50))?
                && let Event::Key(key) = event::read()?
            {
                input(&mut app, key);
            }
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
