//! Responsive live and offline evidence UI; one blocking engine runs on a worker.
mod animation;
mod app;
mod comparison;
mod editor;
pub mod evidence;
mod input;
mod native;
pub mod paging;
mod preview_doc;
mod preview_edit;
mod refresh;
mod render;
pub mod terminal;
mod theme;
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
        guard.restore()
    });
    drop(signals);
    result
}
#[derive(Debug)]
struct SecondaryNativeFailures(Vec<anyhow::Error>);
impl std::fmt::Display for SecondaryNativeFailures {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} secondary native cleanup failure(s) retained",
            self.0.len()
        )
    }
}
fn finish_runtime<T>(
    runtime: tokio::runtime::Runtime,
    cancel: &CancellationToken,
    receiver: Option<oneshot::Receiver<Result<T>>>,
    result: Result<i32>,
    restore: impl FnOnce() -> Result<()>,
) -> Result<i32> {
    if receiver.is_some() {
        cancel.cancel();
    }
    // The terminal is restored promptly even when native cleanup cannot yet
    // finish. A delivered result is not a join: Drop waits for all blocking work.
    let restoration = restore();
    let final_worker = receiver.map(|receiver| {
        receiver
            .blocking_recv()
            .map_err(|_| anyhow!("engine worker ended without delivering its result"))
            .and_then(|result| result)
    });
    drop(runtime);
    let mut retained = crate::runner::process::CleanupFailures::default();
    let mut diagnostics = Vec::new();
    for (stage, error) in [
        ("UI", result.as_ref().err()),
        ("terminal restoration", restoration.as_ref().err()),
        (
            "worker",
            final_worker
                .as_ref()
                .and_then(|result| result.as_ref().err()),
        ),
    ] {
        if let Some(error) = error {
            retained.retain(error);
            diagnostics.push(format!("{stage}: {error:#}"));
        }
    }
    let retained = retained.settled();
    let pending = retained.pending();
    let mut secondary = Vec::new();
    let outcome = match (result, final_worker, restoration) {
        (Err(original), worker, restore) => {
            if let Some(Err(error)) = worker
                && engine::native_cleanup_pending(&error)
            {
                secondary.push(error);
            }
            if let Err(error) = restore
                && engine::native_cleanup_pending(&error)
            {
                secondary.push(error);
            }
            Err(original)
        }
        (Ok(_), Some(Err(error)), restore) => {
            if let Err(other) = restore
                && engine::native_cleanup_pending(&other)
            {
                secondary.push(other);
            }
            Err(error)
        }
        (Ok(_), _, Err(error)) => Err(error),
        (Ok(code), _, Ok(())) => Ok(code),
    };
    outcome.map_err(|error| {
        let error = if secondary.is_empty() {
            error
        } else {
            error.context(SecondaryNativeFailures(secondary))
        };
        // Typed context retains every pending/panic owner while preserving the
        // primary error's downcast and original source chain.
        let error = if pending {
            error.context(retained)
        } else {
            error
        };
        if pending || diagnostics.len() > 1 {
            error.context(diagnostics.join("; "))
        } else {
            error
        }
    })
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
    Native(
        Box<(
            engine::domain::SelectedRecipe,
            engine::domain::NativeRunOptions,
            crate::native::launch::LaunchAuthority,
        )>,
    ),
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
    Native(Box<engine::domain::SelectedRunResult>),
    Run(Box<RunResult>),
    Suite(crate::comparison::ComparisonResult),
}
impl Work {
    fn run(self, token: CancellationToken, events: mpsc::Sender<WorkEvent>) -> Result<WorkResult> {
        match self {
            Self::Native(input) => {
                let (recipe, options, launch) = *input;
                let options =
                    if matches!(recipe, engine::domain::SelectedRecipe::NativeIntegration(_)) {
                        engine::domain::SelectedRunOptions::NativeIntegration(options)
                    } else {
                        engine::domain::SelectedRunOptions::Native(options)
                    };
                engine::run_selected(&recipe, options, Some(&launch), token, |e| {
                    let _ = events.try_send(WorkEvent::Run(e));
                })
                .map(|result| WorkResult::Native(Box::new(result)))
            }
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
    if let Some(view) = &mut app.native {
        let work = view
            .take_execution()?
            .map(|input| Work::Native(Box::new(input)));
        app.work_consumed = work.is_some();
        return Ok(work);
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
            if event
                .report_path
                .file_name()
                .is_some_and(|n| n == crate::native::model::REPORT_FILENAME)
            {
                refresh::Kind::Native
            } else {
                refresh::Kind::Run
            },
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
        Ok(WorkResult::Native(result)) => {
            let root = result.report_path.parent().unwrap().to_path_buf();
            match result.report {
                engine::domain::StoredRun::Native(report) => app.set_native_report(report, root),
                engine::domain::StoredRun::NativeIntegration(report) => {
                    app.set_integration_report(report, root)
                }
                _ => anyhow::bail!("native worker returned another backend"),
            }
            app.status = format!("Finished | {}", result.report_path.display());
        }
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
            if cleanup.pending() || engine::native_cleanup_pending(&error) {
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
                        } else if app.native.is_some() {
                            crate::native::model::REPORT_FILENAME
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
            if app.report.is_none()
                && app.native.as_ref().is_none_or(|v| v.report.is_none())
                && app.comparison.as_ref().is_none_or(|v| v.report.is_none())
            {
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
mod tests;

#[cfg(test)]
mod comparison_tests;

#[cfg(all(test, windows))]
mod native_cleanup_tests;

#[cfg(test)]
mod worker_join_regressions;
