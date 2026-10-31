//! Private acceptance transport. Not an application supervisor or public stdio API.
use super::NativeCommand;
#[cfg(unix)]
use super::unix::terminal as platform;
#[cfg(windows)]
use super::windows::terminal as platform;
use std::io::{self, Read};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

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
        let (native, mut input) = platform::Terminal::spawn(command, rows, columns)?;
        let capture = Arc::new(Mutex::new(Capture::default()));
        let output = capture.clone();
        let reader = std::thread::spawn(move || {
            let mut buffer = [0; 16384];
            loop {
                match input.read(&mut buffer) {
                    Ok(0) => {
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
        if self.status.is_none() && self.native.exited()? {
            self.finish(false)?;
        }
        let modes = self.native.modes()?;
        let mut capture = self.capture.lock().unwrap();
        if let Some(error) = &capture.error {
            return Err(io::Error::other(error.clone()));
        }
        let bytes = std::mem::take(&mut capture.bytes);
        Ok(
            serde_json::json!({"bytes":bytes,"status":self.status,"modes":modes,"eof":capture.eof,"total":capture.total}),
        )
    }
    pub fn finish(&mut self, terminate: bool) -> io::Result<()> {
        if self.status.is_some() {
            return Ok(());
        }
        let status = self.native.close(terminate);
        let joined = self.reader.take().map(|reader| reader.join());
        if matches!(joined, Some(Err(_))) {
            return Err(io::Error::other("terminal reader panicked"));
        }
        self.status = Some(status?.code().unwrap_or(130));
        if !self.capture.lock().unwrap().eof {
            return Err(io::Error::other("terminal drain did not observe peer EOF"));
        }
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
