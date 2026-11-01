use anyhow::{Result, ensure};
use crossterm::{
    cursor::Show,
    event::{DisableBracketedPaste, EnableBracketedPaste},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use std::{
    io::{self, IsTerminal},
    sync::{Arc, Mutex, TryLockError, Weak},
};
pub type Terminal = ratatui::Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>;
pub fn require_tty() -> Result<()> {
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "interactive mode requires a terminal; use run --headless or report RUN_DIRECTORY --json"
    );
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum RestoreStep {
    Raw,
    Paste,
    Alternate,
    Cursor,
}
fn restore_steps(
    completed: &mut [bool; 4],
    mut prepare: impl FnMut() -> Result<()>,
    mut attempt: impl FnMut(RestoreStep) -> io::Result<()>,
) -> Result<()> {
    let mut errors = Vec::new();
    let mut prepared = false;
    for (index, step) in [
        RestoreStep::Raw,
        RestoreStep::Paste,
        RestoreStep::Alternate,
        RestoreStep::Cursor,
    ]
    .into_iter()
    .enumerate()
    {
        if completed[index] {
            continue;
        }
        if index > 0 && !prepared {
            if let Err(error) = prepare() {
                errors.push(format!("prepare escape cleanup: {error:#}"));
                // No escape is safe to mark complete when its prerequisite fails.
                // Raw was attempted, and exact restoration still runs last.
                break;
            }
            prepared = true;
        }
        match attempt(step) {
            Ok(()) => completed[index] = true,
            Err(error) => errors.push(format!("{step:?}: {error}")),
        }
    }
    ensure!(errors.is_empty(), "terminal cleanup: {}", errors.join("; "));
    Ok(())
}
fn restore_step(step: RestoreStep) -> io::Result<()> {
    match step {
        RestoreStep::Raw => disable_raw_mode(),
        RestoreStep::Paste => execute!(io::stdout(), DisableBracketedPaste),
        RestoreStep::Alternate => execute!(io::stdout(), LeaveAlternateScreen),
        RestoreStep::Cursor => execute!(io::stdout(), Show),
    }
}

// Only successful steps are complete. Errors and unwinds leave work retryable;
// successful escapes must not replay after restoring an originally clear VT bit.
struct Restoration {
    step: Box<dyn Fn(RestoreStep) -> io::Result<()> + Send + Sync>,
    prepare: Box<dyn Fn() -> Result<()> + Send + Sync>,
    exact: Box<dyn Fn() -> Result<()> + Send + Sync>,
    completed: Mutex<[bool; 4]>,
}
struct ExactOnExit<'a> {
    callback: &'a (dyn Fn() -> Result<()> + Send + Sync),
    active: bool,
}
impl ExactOnExit<'_> {
    fn finish(mut self) -> Result<()> {
        self.active = false;
        (self.callback)()
    }
}
impl Drop for ExactOnExit<'_> {
    fn drop(&mut self) {
        if self.active {
            let _ = (self.callback)();
        }
    }
}
impl Restoration {
    fn run(&self, wait: bool) -> Result<()> {
        let mut state = if wait {
            Some(
                self.completed
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()),
            )
        } else {
            match self.completed.try_lock() {
                Ok(state) => Some(state),
                Err(TryLockError::Poisoned(error)) => Some(error.into_inner()),
                // A panic hook may run inside a step on this same thread. It may
                // restore exact modes but must not reacquire the progress lock.
                Err(TryLockError::WouldBlock) => None,
            }
        };
        let exact = ExactOnExit {
            callback: &*self.exact,
            active: true,
        };
        let screen = match state.as_mut() {
            Some(state) => restore_steps(state, &self.prepare, &self.step),
            None => Ok(()),
        };
        // Also runs on a preparation/step unwind via ExactOnExit::drop. No panic
        // sentinel can permanently skip the unfinished step on a later attempt.
        match (screen, exact.finish()) {
            (Err(screen), Err(error)) => Err(error.context(format!("{screen:#}"))),
            (Err(screen), Ok(())) => Err(screen),
            (Ok(()), result) => result,
        }
    }
}
fn panic_restore(state: &Weak<Restoration>) {
    if let Some(state) = state.upgrade() {
        let _ = state.run(false);
    }
}
pub struct Guard {
    state: Arc<Restoration>,
}
impl Guard {
    fn initialize<T>(&self, setup: impl FnOnce() -> Result<T>) -> Result<T> {
        setup().map_err(|original| match self.state.run(true) {
            Ok(()) => original,
            Err(error) => original.context(format!("terminal restoration: {error:#}")),
        })
    }
    pub fn enter() -> Result<(Self, Terminal)> {
        require_tty()?;
        #[cfg(windows)]
        let snapshot = Arc::new(theseus_host::console::ConsoleModeSnapshot::capture()?);
        #[cfg(windows)]
        let preparation = snapshot.clone();
        let guard = Self {
            state: Arc::new(Restoration {
                step: Box::new(restore_step),
                prepare: Box::new(move || {
                    #[cfg(windows)]
                    preparation.prepare_escape_cleanup()?;
                    Ok(())
                }),
                exact: Box::new(move || {
                    #[cfg(windows)]
                    snapshot.restore()?;
                    Ok(())
                }),
                completed: Mutex::new([false; 4]),
            }),
        };
        let weak = Arc::downgrade(&guard.state);
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            panic_restore(&weak);
            previous(info);
        }));
        // Ownership and the expiring panic reference exist before the first
        // mutation, including a partially successful initialization error.
        let terminal = guard.initialize(|| {
            enable_raw_mode()?;
            execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste)?;
            Ok(ratatui::Terminal::new(
                ratatui::backend::CrosstermBackend::new(io::stdout()),
            )?)
        })?;
        Ok((guard, terminal))
    }
    pub fn restore(self) -> Result<()> {
        self.state.run(true)
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.state.run(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    fn controlled(screen: Arc<AtomicUsize>, exact: Arc<AtomicUsize>) -> Guard {
        Guard {
            state: Arc::new(Restoration {
                step: Box::new(move |step| {
                    if step == RestoreStep::Raw {
                        screen.fetch_add(1, Ordering::SeqCst);
                    }
                    Ok(())
                }),
                prepare: Box::new(|| Ok(())),
                exact: Box::new(move || {
                    exact.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }),
                completed: Mutex::new([false; 4]),
            }),
        }
    }
    fn controlled_steps(
        attempt: impl Fn(RestoreStep) -> io::Result<()> + Send + Sync + 'static,
        exact: impl Fn() -> Result<()> + Send + Sync + 'static,
    ) -> Guard {
        Guard {
            state: Arc::new(Restoration {
                step: Box::new(attempt),
                prepare: Box::new(|| Ok(())),
                exact: Box::new(exact),
                completed: Mutex::new([false; 4]),
            }),
        }
    }
    #[test]
    fn failed_raw_step_retries_without_replaying_successful_escapes() {
        let attempts = Arc::new(Mutex::new(Vec::new()));
        let raw = Arc::new(AtomicUsize::new(0));
        let guard = controlled_steps(
            {
                let attempts = attempts.clone();
                let raw = raw.clone();
                move |step| {
                    attempts.lock().unwrap().push(step);
                    if step == RestoreStep::Raw && raw.fetch_add(1, Ordering::SeqCst) == 0 {
                        return Err(io::Error::other("recoverable raw failure"));
                    }
                    Ok(())
                }
            },
            || Ok(()),
        );
        assert!(
            guard
                .state
                .run(true)
                .unwrap_err()
                .to_string()
                .contains("recoverable raw failure")
        );
        guard.state.run(true).unwrap();
        assert_eq!(
            *attempts.lock().unwrap(),
            [
                RestoreStep::Raw,
                RestoreStep::Paste,
                RestoreStep::Alternate,
                RestoreStep::Cursor,
                RestoreStep::Raw
            ]
        );
    }
    #[test]
    fn interrupted_step_restores_exact_then_retries_unfinished_work() {
        let attempts = Arc::new(Mutex::new(Vec::new()));
        let interrupted = Arc::new(AtomicUsize::new(0));
        let exact = Arc::new(AtomicUsize::new(0));
        let guard = controlled_steps(
            {
                let attempts = attempts.clone();
                move |step| {
                    attempts.lock().unwrap().push(step);
                    if step == RestoreStep::Paste && interrupted.fetch_add(1, Ordering::SeqCst) == 0
                    {
                        panic!("interrupted paste cleanup");
                    }
                    Ok(())
                }
            },
            {
                let exact = exact.clone();
                move || {
                    exact.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
        );
        let interrupted =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| guard.state.run(true)));
        assert!(interrupted.is_err());
        assert!(
            exact.load(Ordering::SeqCst) > 0,
            "panic skipped exact restoration"
        );
        guard.state.run(true).unwrap();
        assert_eq!(
            *attempts.lock().unwrap(),
            [
                RestoreStep::Raw,
                RestoreStep::Paste,
                RestoreStep::Paste,
                RestoreStep::Alternate,
                RestoreStep::Cursor
            ]
        );
    }
    #[test]
    fn failed_escape_retry_prepares_vt_and_restores_exact_last() {
        use std::sync::atomic::AtomicBool;
        let events = Arc::new(Mutex::new(Vec::new()));
        let vt = Arc::new(AtomicBool::new(false));
        let paste = Arc::new(AtomicUsize::new(0));
        let state = Restoration {
            step: Box::new({
                let events = events.clone();
                let vt = vt.clone();
                move |step| {
                    if step != RestoreStep::Raw {
                        assert!(vt.load(Ordering::SeqCst), "escape written with VT disabled");
                    }
                    events.lock().unwrap().push(match step {
                        RestoreStep::Raw => "raw",
                        RestoreStep::Paste => "paste",
                        RestoreStep::Alternate => "alternate",
                        RestoreStep::Cursor => "cursor",
                    });
                    if step == RestoreStep::Paste && paste.fetch_add(1, Ordering::SeqCst) == 0 {
                        return Err(io::Error::other("paste failed once"));
                    }
                    Ok(())
                }
            }),
            prepare: Box::new({
                let events = events.clone();
                let vt = vt.clone();
                move || {
                    events.lock().unwrap().push("prepare");
                    vt.store(true, Ordering::SeqCst);
                    Ok(())
                }
            }),
            exact: Box::new({
                let events = events.clone();
                let vt = vt.clone();
                move || {
                    events.lock().unwrap().push("exact");
                    vt.store(false, Ordering::SeqCst);
                    Ok(())
                }
            }),
            completed: Mutex::new([false; 4]),
        };
        assert!(state.run(true).is_err());
        assert!(!vt.load(Ordering::SeqCst));
        state.run(true).unwrap();
        assert!(!vt.load(Ordering::SeqCst));
        assert_eq!(
            *events.lock().unwrap(),
            [
                "raw",
                "prepare",
                "paste",
                "alternate",
                "cursor",
                "exact",
                "prepare",
                "paste",
                "exact"
            ]
        );
    }
    #[test]
    fn preparation_failure_keeps_escapes_pending_and_exact_error_visible() {
        let steps = Arc::new(Mutex::new(Vec::new()));
        let prepare = AtomicUsize::new(0);
        let exact = Arc::new(AtomicUsize::new(0));
        let state = Restoration {
            step: Box::new({
                let steps = steps.clone();
                move |step| {
                    steps.lock().unwrap().push(step);
                    Ok(())
                }
            }),
            prepare: Box::new(move || {
                if prepare.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(anyhow::anyhow!("preparation failed once"));
                }
                Ok(())
            }),
            exact: Box::new({
                let exact = exact.clone();
                move || {
                    if exact.fetch_add(1, Ordering::SeqCst) == 0 {
                        return Err(anyhow::anyhow!("exact failed once"));
                    }
                    Ok(())
                }
            }),
            completed: Mutex::new([false; 4]),
        };
        let error = format!("{:#}", state.run(true).unwrap_err());
        assert!(error.contains("preparation failed once") && error.contains("exact failed once"));
        assert_eq!(*steps.lock().unwrap(), [RestoreStep::Raw]);
        assert_eq!(exact.load(Ordering::SeqCst), 1);
        state.run(true).unwrap();
        assert_eq!(
            *steps.lock().unwrap(),
            [
                RestoreStep::Raw,
                RestoreStep::Paste,
                RestoreStep::Alternate,
                RestoreStep::Cursor
            ]
        );
        assert_eq!(exact.load(Ordering::SeqCst), 2);
    }
    #[test]
    fn explicit_error_survives_successful_drop_retry() {
        let raw = Arc::new(AtomicUsize::new(0));
        let guard = controlled_steps(
            {
                let raw = raw.clone();
                move |step| {
                    if step == RestoreStep::Raw && raw.fetch_add(1, Ordering::SeqCst) == 0 {
                        return Err(io::Error::other("original raw failure"));
                    }
                    Ok(())
                }
            },
            || Ok(()),
        );
        let error = guard.restore().unwrap_err();
        assert!(error.to_string().contains("original raw failure"));
        assert_eq!(
            raw.load(Ordering::SeqCst),
            2,
            "Drop did not recover failed raw step"
        );
    }
    #[test]
    fn cleanup_attempts_alternate_and_cursor_after_raw_and_paste_fail() {
        let mut observed = Vec::new();
        let error = restore_steps(
            &mut [false; 4],
            || Ok(()),
            |step| {
                observed.push(step);
                if matches!(step, RestoreStep::Raw | RestoreStep::Paste) {
                    Err(io::Error::other(format!("{step:?} failed")))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();
        assert_eq!(
            observed,
            [
                RestoreStep::Raw,
                RestoreStep::Paste,
                RestoreStep::Alternate,
                RestoreStep::Cursor
            ]
        );
        assert!(error.to_string().contains("Raw failed"));
        assert!(error.to_string().contains("Paste failed"));
    }
    #[test]
    fn partial_initialization_error_and_unwind_keep_cleanup_owner() {
        for panic in [false, true] {
            let screen = Arc::new(AtomicUsize::new(0));
            let exact = Arc::new(AtomicUsize::new(0));
            let result = std::panic::catch_unwind({
                let screen = screen.clone();
                let exact = exact.clone();
                move || {
                    let guard = controlled(screen, exact);
                    guard.initialize::<()>(|| {
                        if panic {
                            panic!("partial native setup");
                        }
                        Err(anyhow::anyhow!("partial native setup"))
                    })
                }
            });
            assert!(result.is_err() || result.unwrap().is_err());
            assert_eq!(screen.load(Ordering::SeqCst), 1);
            assert!(exact.load(Ordering::SeqCst) >= 1);
        }
    }
    #[test]
    fn panic_cleanup_is_once_and_weak_hook_cannot_replay_after_drop() {
        let screen = Arc::new(AtomicUsize::new(0));
        let exact = Arc::new(AtomicUsize::new(0));
        let guard = controlled(screen.clone(), exact.clone());
        let weak = Arc::downgrade(&guard.state);
        panic_restore(&weak);
        guard.restore().unwrap();
        assert_eq!(screen.load(Ordering::SeqCst), 1);
        let final_count = exact.load(Ordering::SeqCst);
        panic_restore(&weak);
        assert_eq!(exact.load(Ordering::SeqCst), final_count);
        assert!(weak.upgrade().is_none());
    }
    #[test]
    fn panic_during_cleanup_cannot_reacquire_its_own_lock() {
        let screen = Arc::new(AtomicUsize::new(0));
        let exact = Arc::new(AtomicUsize::new(0));
        let guard = controlled(screen.clone(), exact.clone());
        let held = guard.state.completed.lock().unwrap();
        panic_restore(&Arc::downgrade(&guard.state));
        assert_eq!(screen.load(Ordering::SeqCst), 0);
        assert_eq!(exact.load(Ordering::SeqCst), 1);
        drop(held);
        guard.restore().unwrap();
        assert_eq!(screen.load(Ordering::SeqCst), 1);
    }
}
