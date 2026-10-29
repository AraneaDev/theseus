use anyhow::{Result, ensure};
use crossterm::{
    cursor::Show,
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use std::io::{self, IsTerminal};
pub type Terminal = ratatui::Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>;
pub fn require_tty() -> Result<()> {
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "interactive mode requires a terminal; use run --headless or report RUN_DIRECTORY --json"
    );
    Ok(())
}
fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
}
pub struct Guard;
impl Guard {
    pub fn enter() -> Result<(Self, Terminal)> {
        require_tty()?;
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore();
            previous(info);
        }));
        enable_raw_mode()?;
        let guard = Self;
        execute!(io::stdout(), EnterAlternateScreen)?;
        let terminal =
            ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(io::stdout()))?;
        Ok((guard, terminal))
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        restore();
    }
}
