//! Fixed adverse console baselines and actual locked-library negative control.
use super::*;
use crate::console::ConsoleModeSnapshot;

pub(super) fn query(file: &File) -> io::Result<u32> {
    let mut mode = 0;
    if unsafe { GetConsoleMode(file.as_raw_handle(), &mut mode) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(mode)
}
fn set(name: &str, mode: u32) -> io::Result<()> {
    let file = OpenOptions::new().read(true).write(true).open(name)?;
    if unsafe { SetConsoleMode(file.as_raw_handle(), mode) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
pub(super) struct Baseline {
    original: Vec<u32>,
    before: Vec<u32>,
    profile: ConsoleProfile,
    restore: ConsoleModeSnapshot,
}
impl Baseline {
    pub(super) fn apply(profile: ConsoleProfile) -> io::Result<Self> {
        let restore = ConsoleModeSnapshot::capture()?;
        let original = active_modes()?;
        let mut baseline = Self {
            before: original.clone(),
            original,
            profile,
            restore,
        };
        let expected = match profile {
            ConsoleProfile::Default => baseline.original.clone(),
            ConsoleProfile::EchoOffVtOff => vec![
                (baseline.original[0] | ENABLE_LINE_INPUT) & !ENABLE_ECHO_INPUT,
                (baseline.original[1] | ENABLE_PROCESSED_OUTPUT)
                    & !ENABLE_VIRTUAL_TERMINAL_PROCESSING,
            ],
            ConsoleProfile::ProcessedOffVtOn => vec![
                (baseline.original[0] | ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT)
                    & !ENABLE_PROCESSED_INPUT,
                baseline.original[1] | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
            ],
        };
        if profile != ConsoleProfile::Default {
            set("CONIN$", expected[0])?;
            set("CONOUT$", expected[1])?;
        }
        baseline.before = active_modes()?;
        if baseline.before != expected {
            return Err(invalid("console profile read-back differs"));
        }
        Ok(baseline)
    }
    pub(super) fn evidence(&self) -> io::Result<serde_json::Value> {
        let mut value = evidence(&self.before)?;
        value["profile"] = self.profile.name().into();
        value["original"] = serde_json::json!(self.original);
        Ok(value)
    }
    pub(super) fn restore(&self) -> io::Result<()> {
        self.restore.restore()
    }
}
impl Drop for Baseline {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

pub(super) fn legacy(arguments: &[std::ffi::OsString]) -> Result<i32, Box<dyn std::error::Error>> {
    use crossterm::{
        cursor::Show,
        event::{DisableBracketedPaste, EnableBracketedPaste},
        execute,
        terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    };
    let [root, profile] = arguments else {
        return Err(invalid("fixed legacy control arguments").into());
    };
    let profile = ConsoleProfile::parse(profile)?;
    if profile == ConsoleProfile::Default {
        return Err(invalid("legacy control requires adverse baseline").into());
    }
    let baseline = Baseline::apply(profile)?;
    let mut output = io::BufWriter::new(OpenOptions::new().read(true).write(true).open("CONOUT$")?);
    // Actual locked crossterm calls, not a model or an old Theseus executable.
    enable_raw_mode()?;
    execute!(output, EnterAlternateScreen, EnableBracketedPaste)?;
    let live = active_modes()?;
    disable_raw_mode()?;
    execute!(output, DisableBracketedPaste, LeaveAlternateScreen, Show)?;
    let mut observed = baseline.evidence()?;
    observed["control"] = "actual-legacy-crossterm".into();
    observed["live"] = serde_json::json!(live);
    publish(
        Path::new(root),
        "complete.json",
        &serde_json::json!({"phase":"complete", "sequence":0, "modes":observed}),
    )?;
    let after: Vec<u32> = serde_json::from_value(observed["current"].clone())?;
    let expected = vec![baseline.before[0] | 7, baseline.before[1] | 4];
    let demonstrated = after == expected && after != baseline.before && live[0] & 7 == 0;
    // The complete record precedes harness reset; the snapshot is cleanup only.
    baseline.restore()?;
    if !demonstrated {
        return Err(invalid("actual legacy crossterm drift was not demonstrated").into());
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conpty_legacy_crossterm_controls_demonstrate_exact_mode_drift() {
        for profile in [
            ConsoleProfile::EchoOffVtOff,
            ConsoleProfile::ProcessedOffVtOn,
        ] {
            let mut session = crate::process::terminal::Session::legacy_control(profile).unwrap();
            let result = (|| -> io::Result<serde_json::Value> {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
                loop {
                    let value = session.poll()?;
                    if !value["status"].is_null() {
                        return Ok(value);
                    }
                    if std::time::Instant::now() >= deadline {
                        return Err(invalid("legacy control exit deadline"));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
            })();
            let cleanup = session.finish(true);
            println!(
                "ACTUAL NATIVE LEGACY-CROSSTERM RED CONTROL profile={} result={result:?}",
                profile.name()
            );
            cleanup.unwrap();
            let value = result.unwrap();
            assert_eq!(value["status"], 0);
            assert_eq!(value["eof"], true);
            assert_ne!(value["modes"]["before"], value["modes"]["current"]);
            let before: Vec<u32> =
                serde_json::from_value(value["modes"]["before"].clone()).unwrap();
            assert_eq!(
                value["modes"]["current"],
                serde_json::json!([before[0] | 7, before[1] | 4])
            );
        }
    }
}
