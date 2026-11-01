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
pub(crate) fn trace(phase: &str) {
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
        dimensions(rows, columns)?;
        trace("spawn-begin");
        let (native, mut input) = platform::Terminal::spawn(command, rows, columns)?;
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
        Ok(Self {
            native,
            capture,
            reader: Some(reader),
            status: None,
        })
    }
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
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
            self.finish(false)?;
        }
        let modes = self.native.modes()?;
        let mut capture = self.capture.lock().unwrap();
        if let Some(error) = &capture.error {
            return Err(io::Error::other(error.clone()));
        }
        let bytes = std::mem::take(&mut capture.bytes);
        trace("first-poll-complete");
        Ok(
            serde_json::json!({"bytes":bytes,"status":self.status,"modes":modes,"eof":capture.eof,"total":capture.total}),
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
