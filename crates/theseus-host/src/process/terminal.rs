//! Private acceptance transport. Not an application supervisor or public stdio API.
use super::NativeCommand;
#[cfg(unix)]
use super::unix::terminal as platform;
#[cfg(windows)]
use super::windows::terminal as platform;
use std::io::{self, Read};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

// Private diagnostics: one CI-scoped switch, fixed phases once per helper
// process, stderr only. Never emit argv, environment, paths or protocol JSON.
const TRACE_PHASES: &[&str] = &[
    "spawn-begin",
    "native-create-begin",
    "native-created",
    "native-launch-begin",
    "native-launched",
    "reader-started",
    "reader-eof",
    "reader-error",
    "first-poll-begin",
    "first-poll-complete",
    "request-begin",
    "client-ready",
    "first-active-modes",
    "client-complete-modes",
    "request-deadline",
    "finish-begin",
    "close-begin",
    "close-observation-returned",
    "close-modes",
    "input-close-returned",
    "terminate-begin",
    "terminate-returned",
    "close-worker-start",
    "close-native-begin",
    "close-native-complete",
    "job-cleanup-begin",
    "job-cleanup-returned",
    "close-join-begin",
    "close-join-returned",
    "reader-join-begin",
    "reader-join-returned",
    "finish-complete",
    "failed-launch-cleanup-begin",
    "failed-launch-cleanup-complete",
    "input-write-complete",
    "reserved-exit-observed",
    "input-write-begin",
    "input-close-begin",
    "resize-native-begin",
    "resize-native-returned",
    "signal-native-begin",
    "signal-native-returned",
    "ready-flush-begin",
    "ready-flush-returned",
    "reply-flush-begin",
    "reply-flush-returned",
    "input-peer-drop-begin",
    "input-peer-drop-returned",
    "output-peer-drop-begin",
    "output-peer-drop-returned",
    "pressure-peek-begin",
    "pressure-peek-returned",
    "pressure-loop-complete",
    "reader-spawn-begin",
];
fn trace_phase(phase: &str) -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    static SEEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if !*ENABLED.get_or_init(|| {
        std::env::var_os("THESEUS_TERMINAL_TRACE").as_deref() == Some(std::ffi::OsStr::new("1"))
    }) {
        return false;
    }
    let Some(index) = TRACE_PHASES.iter().position(|p| *p == phase) else {
        return false;
    };
    let bit = 1_u64 << index;
    SEEN.fetch_or(bit, std::sync::atomic::Ordering::Relaxed) & bit == 0
}
#[doc(hidden)]
pub fn trace(phase: &str) {
    use std::io::Write;
    if trace_phase(phase) {
        let _ = writeln!(
            io::stderr().lock(),
            "theseus-terminal pid={} phase={phase}",
            std::process::id()
        );
    }
}
#[cfg(windows)]
pub(crate) fn trace_modes(phase: &str, modes: &serde_json::Value) {
    use std::io::Write;
    if trace_phase(phase) {
        // Callers supply validated two-element native mode arrays, not general
        // JSON payloads. Even a broken diagnostic cannot grow without a bound.
        let pair = |key: &str| -> Option<[u64; 2]> {
            Some([modes[key].get(0)?.as_u64()?, modes[key].get(1)?.as_u64()?])
        };
        let _ = writeln!(
            io::stderr().lock(),
            "theseus-terminal pid={} phase={phase} before={:?} current={:?}",
            std::process::id(),
            pair("before"),
            pair("current")
        );
    }
}

const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
#[derive(Default)]
struct Capture {
    bytes: Vec<u8>,
    total: usize,
    eof: bool,
    error: Option<String>,
}

/// Fixed adverse baselines, selected once at private test startup only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConsoleProfile {
    #[default]
    Default,
    EchoOffVtOff,
    ProcessedOffVtOn,
}
impl ConsoleProfile {
    #[cfg(windows)]
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::EchoOffVtOff => "echo-off-vt-off",
            Self::ProcessedOffVtOn => "processed-off-vt-on",
        }
    }
    #[cfg(windows)]
    pub(crate) fn parse(value: &std::ffi::OsStr) -> io::Result<Self> {
        match value.to_str() {
            Some("default") => Ok(Self::Default),
            Some("echo-off-vt-off") => Ok(Self::EchoOffVtOff),
            Some("processed-off-vt-on") => Ok(Self::ProcessedOffVtOn),
            _ => Err(io::Error::other("unknown fixed console profile")),
        }
    }
}

pub struct Session {
    native: platform::Terminal,
    capture: Arc<Mutex<Capture>>,
    reader: Option<JoinHandle<()>>,
    status: Option<i32>,
}
impl Session {
    pub fn spawn(
        command: &NativeCommand,
        rows: u16,
        columns: u16,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Self::spawn_with_profile(command, rows, columns, ConsoleProfile::Default)
    }
    pub fn spawn_with_profile(
        command: &NativeCommand,
        rows: u16,
        columns: u16,
        profile: ConsoleProfile,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        dimensions(rows, columns)?;
        trace("spawn-begin");
        let (native, input) = platform::Terminal::spawn(command, rows, columns, profile)?;
        Ok(Self::from_native(native, input))
    }
    #[cfg(all(test, windows))]
    pub(crate) fn fixture_control(
        command: &NativeCommand,
        rows: u16,
        columns: u16,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        dimensions(rows, columns)?;
        trace("spawn-begin");
        let (native, input) = platform::Terminal::spawn_fixture(command, rows, columns)?;
        Ok(Self::from_native(native, input))
    }
    #[cfg(all(test, windows))]
    pub(crate) fn legacy_control(
        profile: ConsoleProfile,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (native, input) = platform::Terminal::spawn_legacy(profile)?;
        Ok(Self::from_native(native, input))
    }
    fn from_native(native: platform::Terminal, mut input: platform::Reader) -> Self {
        let capture = Arc::new(Mutex::new(Capture::default()));
        let output = capture.clone();
        let reader = std::thread::spawn(move || {
            trace("reader-started");
            let mut buffer = [0; 16384];
            loop {
                match input.read(&mut buffer) {
                    Ok(0) => {
                        trace("reader-eof");
                        output.lock().unwrap().eof = true;
                        break;
                    }
                    Ok(count) => {
                        let mut capture = output.lock().unwrap();
                        capture.total += count;
                        if capture.total <= OUTPUT_LIMIT {
                            capture.bytes.extend_from_slice(&buffer[..count]);
                        } else {
                            capture
                                .error
                                .get_or_insert("terminal output exceeded 4 MiB".into());
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(2))
                    }
                    Err(error) => {
                        trace("reader-error");
                        output.lock().unwrap().error = Some(error.to_string());
                        break;
                    }
                }
            }
        });
        Self {
            native,
            capture,
            reader: Some(reader),
            status: None,
        }
    }
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        trace("input-write-begin");
        if bytes.len() > 65536 {
            return Err(io::Error::other("terminal input exceeded 64 KiB"));
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut offset = 0;
        while offset < bytes.len() {
            if std::time::Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "terminal input deadline",
                ));
            }
            match self.native.write(&bytes[offset..]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => offset += n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(2))
                }
                Err(e) => return Err(e),
            }
        }
        trace("input-write-complete");
        Ok(())
    }
    pub fn resize(&mut self, rows: u16, columns: u16) -> io::Result<()> {
        dimensions(rows, columns)?;
        self.native.resize(rows, columns)
    }
    pub fn signal(&mut self, event: &str) -> io::Result<()> {
        self.native.signal(event)
    }
    pub fn poll(&mut self) -> io::Result<serde_json::Value> {
        trace("first-poll-begin");
        if self.status.is_none() && self.native.exited()? {
            trace("reserved-exit-observed");
            self.finish(false)?;
        }
        let capture = &self.capture;
        let native = &self.native;
        let ((bytes, eof, total), modes) = observe(
            || {
                let mut capture = capture.lock().unwrap();
                if let Some(error) = &capture.error {
                    return Err(io::Error::other(error.clone()));
                }
                Ok((
                    std::mem::take(&mut capture.bytes),
                    capture.eof,
                    capture.total,
                ))
            },
            || native.modes(),
        )?;
        trace("first-poll-complete");
        Ok(
            serde_json::json!({"bytes":bytes,"status":self.status,"modes":modes,"eof":eof,"total":total}),
        )
    }
    pub fn finish(&mut self, terminate: bool) -> io::Result<()> {
        if self.status.is_some() {
            return Ok(());
        }
        trace("finish-begin");
        let status = self.native.close(terminate);
        trace("reader-join-begin");
        let joined = self.reader.take().map(|reader| reader.join());
        trace("reader-join-returned");
        if matches!(joined, Some(Err(_))) {
            return Err(io::Error::other("terminal reader panicked"));
        }
        self.status = Some(status?.code().unwrap_or(130));
        if !self.capture.lock().unwrap().eof {
            return Err(io::Error::other("terminal drain did not observe peer EOF"));
        }
        trace("finish-complete");
        Ok(())
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if self.reader.is_some() {
            let _ = self.finish(true);
        }
    }
}
fn dimensions(rows: u16, columns: u16) -> io::Result<()> {
    if !(2..=500).contains(&rows) || !(2..=500).contains(&columns) {
        return Err(io::Error::other("terminal dimensions outside 2..500"));
    }
    Ok(())
}

#[cfg(windows)]
pub fn run_client(arguments: &[std::ffi::OsString]) -> Result<i32, Box<dyn std::error::Error>> {
    platform::run_client(arguments)
}

#[cfg(unix)]
#[doc(hidden)]
pub fn run_mode_fixture(restore: bool) -> io::Result<()> {
    platform::run_mode_fixture(restore)
}

#[cfg(unix)]
#[doc(hidden)]
pub fn run_event_fixture(root: &std::path::Path, mode: &str) -> io::Result<()> {
    platform::run_event_fixture(root, mode)
}

#[cfg(windows)]
#[doc(hidden)]
pub fn run_vt_event_fixture(root: &std::path::Path, mode: &str) -> io::Result<()> {
    platform::run_vt_event_fixture(root, mode)
}

#[cfg(windows)]
#[doc(hidden)]
pub fn run_legacy_client(
    arguments: &[std::ffi::OsString],
) -> Result<i32, Box<dyn std::error::Error>> {
    platform::run_legacy_client(arguments)
}

/// One poll's output and console modes: the output first, then the modes, so
/// the modes a poll reports are never older than the output it returns.
///
/// Sampled the other way round, a client that switches modes and then draws
/// between the two halves gets its drawing reported with the modes from
/// before the switch. On Windows the sample is a round trip to the telemetry
/// process, so that window is wide enough to hit.
fn observe<B, M>(
    take_output: impl FnOnce() -> io::Result<B>,
    sample_modes: impl FnOnce() -> io::Result<M>,
) -> io::Result<(B, M)> {
    let output = take_output()?;
    let modes = sample_modes()?;
    Ok((output, modes))
}

#[cfg(test)]
mod tests {
    use super::observe;
    use std::cell::RefCell;

    /// The client switches to raw mode before it draws anything. A response
    /// that carries the drawing must therefore report raw mode, wherever the
    /// switch falls between the two halves of a poll.
    #[test]
    fn a_poll_never_reports_output_with_older_modes() {
        #[derive(Default)]
        struct Client {
            raw: bool,
            drawn: Vec<u8>,
        }
        let client = RefCell::new(Client::default());
        // The client switches and draws right after the first half runs.
        let act = || {
            let mut client = client.borrow_mut();
            client.raw = true;
            client.drawn.extend_from_slice(b"Preview");
        };
        let first = RefCell::new(true);
        let after = |step: &dyn Fn()| {
            if first.replace(false) {
                step();
            }
        };
        let (output, raw) = observe(
            || {
                let output = std::mem::take(&mut client.borrow_mut().drawn);
                after(&act);
                Ok(output)
            },
            || {
                let raw = client.borrow().raw;
                after(&act);
                Ok(raw)
            },
        )
        .unwrap();
        assert!(
            output.is_empty() || raw,
            "output {output:?} reported with raw={raw}"
        );
    }
}
