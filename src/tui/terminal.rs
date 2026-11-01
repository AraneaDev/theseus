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
fn restore_steps(mut attempt: impl FnMut(RestoreStep) -> io::Result<()>) -> Result<()> {
    let mut errors = Vec::new();
    for step in [
        RestoreStep::Raw,
        RestoreStep::Paste,
        RestoreStep::Alternate,
        RestoreStep::Cursor,
    ] {
        if let Err(error) = attempt(step) {
            errors.push(format!("{step:?}: {error}"));
        }
    }
    ensure!(errors.is_empty(), "terminal cleanup: {}", errors.join("; "));
    Ok(())
}
fn restore_screen() -> Result<()> {
    restore_steps(|step| match step {
        RestoreStep::Raw => disable_raw_mode(),
        RestoreStep::Paste => execute!(io::stdout(), DisableBracketedPaste),
        RestoreStep::Alternate => execute!(io::stdout(), LeaveAlternateScreen),
        RestoreStep::Cursor => execute!(io::stdout(), Show),
    })
}

// No global strong owner. Screen escapes run once: after restoring an originally
// clear Windows VT bit, replaying those escapes could print them as literal text.
// Exact native modes can safely be retried after Drop or a hook interruption.
struct Restoration {
    screen: Box<dyn Fn() -> Result<()> + Send + Sync>,
    exact: Box<dyn Fn() -> Result<()> + Send + Sync>,
    screen_result: Mutex<Option<Option<String>>>,
}
impl Restoration {
    fn run(&self, wait: bool) -> Result<()> {
        let mut state = if wait {
            Some(
                self.screen_result
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()),
            )
        } else {
            match self.screen_result.try_lock() {
                Ok(state) => Some(state),
                Err(TryLockError::Poisoned(error)) => Some(error.into_inner()),
                // The hook may run on the thread already performing cleanup.
                // Never reacquire its lock; exact mode restoration is independent.
                Err(TryLockError::WouldBlock) => None,
            }
        };
        let screen_error = state.as_mut().and_then(|state| {
            if state.is_none() {
                **state = Some(Some("terminal cleanup interrupted by panic".into()));
                **state = Some((self.screen)().err().map(|error| format!("{error:#}")));
            }
            state.as_ref().and_then(Clone::clone)
        });
        let exact = (self.exact)();
        match (screen_error, exact) {
            (Some(screen), Err(error)) => Err(error.context(screen)),
            (Some(screen), Ok(())) => Err(anyhow::anyhow!(screen)),
            (None, result) => result,
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
        let snapshot = theseus_host::console::ConsoleModeSnapshot::capture()?;
        let guard = Self {
            state: Arc::new(Restoration {
                screen: Box::new(restore_screen),
                exact: Box::new(move || {
                    #[cfg(windows)]
                    snapshot.restore()?;
                    Ok(())
                }),
                screen_result: Mutex::new(None),
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
                screen: Box::new(move || {
                    screen.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }),
                exact: Box::new(move || {
                    exact.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }),
                screen_result: Mutex::new(None),
            }),
        }
    }
    #[test]
    fn cleanup_attempts_alternate_and_cursor_after_raw_and_paste_fail() {
        let mut observed = Vec::new();
        let error = restore_steps(|step| {
            observed.push(step);
            if matches!(step, RestoreStep::Raw | RestoreStep::Paste) {
                Err(io::Error::other(format!("{step:?} failed")))
            } else {
                Ok(())
            }
        })
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
        let held = guard.state.screen_result.lock().unwrap();
        panic_restore(&Arc::downgrade(&guard.state));
        assert_eq!(screen.load(Ordering::SeqCst), 0);
        assert_eq!(exact.load(Ordering::SeqCst), 1);
        drop(held);
        guard.restore().unwrap();
        assert_eq!(screen.load(Ordering::SeqCst), 1);
    }
}
