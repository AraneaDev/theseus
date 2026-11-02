//! ConPTY-only acceptance plumbing. All handles keep their existing native owners.
use super::{OwnedChild, launch, pipe};
use crate::process::terminal::{ConsoleProfile, trace, trace_modes};
mod modes;
use crate::process::{NativeCommand, PipeReader, PipeWriter};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::process::{Child, ExitStatus, Stdio};
use windows_sys::Win32::System::Console::*;

struct PseudoConsole(HPCON);
impl Drop for PseudoConsole {
    fn drop(&mut self) {
        trace("close-native-begin");
        unsafe {
            ClosePseudoConsole(self.0);
        }
        trace("close-native-complete");
    }
}
pub(crate) struct Reader(PipeReader);
impl Read for Reader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.0.try_read(bytes)
    }
}
pub(crate) struct Terminal {
    child: Option<OwnedChild>,
    input: PipeWriter,
    console: Option<PseudoConsole>,
    directory: tempfile::TempDir,
    sequence: std::cell::Cell<u32>,
    complete: std::cell::Cell<bool>,
    last: std::cell::RefCell<Option<serde_json::Value>>,
}
fn invalid(message: &str) -> io::Error {
    io::Error::other(message)
}
fn publish(root: &Path, name: &str, value: &serde_json::Value) -> io::Result<()> {
    let temporary = root.join(format!("{name}.pending"));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    serde_json::to_writer(&mut file, value)?;
    file.flush()?;
    drop(file);
    std::fs::rename(temporary, root.join(name))
}
fn read_record(path: &Path) -> io::Result<Option<serde_json::Value>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err(invalid("oversized terminal telemetry"));
    }
    Ok(Some(serde_json::from_slice(&bytes)?))
}
fn validate_response(
    value: &serde_json::Value,
    sequence: u32,
    phase: &str,
) -> io::Result<serde_json::Value> {
    if value["sequence"] != sequence || value["phase"] != phase {
        return Err(invalid("uncorrelated terminal telemetry"));
    }
    let modes = &value["modes"];
    if modes["kind"] != "console" || modes["raw"].as_bool().is_none() {
        return Err(invalid("invalid console mode evidence"));
    }
    for key in ["before", "current", "dimensions"] {
        let values = modes[key]
            .as_array()
            .ok_or_else(|| invalid("missing native mode array"))?;
        if values.len() != 2 || values.iter().any(|v| v.as_u64().is_none()) {
            return Err(invalid("invalid native mode array"));
        }
    }
    Ok(modes.clone())
}
// Closing an initializing/suspended pseudoconsole client can deadlock. The
// read endpoint is already closed by these failure callers; settle retained
// launch cleanup before releasing HPCON. Native completion errors retain owners.
fn settle_failed_launch(error: &mut crate::process::SpawnFailure) {
    trace("failed-launch-cleanup-begin");
    if let crate::process::HostCleanup::Pending(owner) = &mut error.cleanup {
        loop {
            match owner.try_complete() {
                Ok(true) => break,
                Ok(false) => (),
                Err(cleanup) => {
                    if error.cleanup_errors.len() < 8 {
                        error.cleanup_errors.push(cleanup);
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        error.cleanup = crate::process::HostCleanup::Complete;
    }
    trace("failed-launch-cleanup-complete");
}
impl Terminal {
    pub(crate) fn spawn(
        command: &NativeCommand,
        rows: u16,
        columns: u16,
        profile: ConsoleProfile,
    ) -> Result<(Self, Reader), Box<dyn std::error::Error>> {
        Self::spawn_inner(command, rows, columns, profile, false)
    }
    #[cfg(test)]
    pub(crate) fn spawn_legacy(
        profile: ConsoleProfile,
    ) -> Result<(Self, Reader), Box<dyn std::error::Error>> {
        let fixture = std::env::var_os("THESEUS_HOST_TEST_FIXTURE")
            .ok_or_else(|| invalid("required pinned fixture"))?;
        Self::spawn_inner(&NativeCommand::new(fixture), 24, 80, profile, true)
    }
    fn spawn_inner(
        command: &NativeCommand,
        rows: u16,
        columns: u16,
        profile: ConsoleProfile,
        legacy: bool,
    ) -> Result<(Self, Reader), Box<dyn std::error::Error>> {
        let (input, input_peer) = pipe::writer()?;
        let (output, output_peer) = pipe::reader()?;
        let mut hpc = 0;
        trace("native-create-begin");
        let result = unsafe {
            CreatePseudoConsole(
                COORD {
                    X: columns as i16,
                    Y: rows as i16,
                },
                input_peer.as_raw_handle(),
                output_peer.as_raw_handle(),
                0,
                &mut hpc,
            )
        };
        if result < 0 {
            return Err(invalid(&format!("CreatePseudoConsole HRESULT {result:#x}")).into());
        }
        trace("native-created");
        let console = PseudoConsole(hpc);
        let directory = tempfile::tempdir()?;
        let mut wrapper = command.clone();
        wrapper.program = std::env::current_exe()?.into_os_string();
        wrapper.args = vec![
            "terminal-client".into(),
            command.program.clone(),
            directory.path().as_os_str().to_owned(),
            profile.name().into(),
        ];
        wrapper.args.extend(command.args.iter().cloned());
        if legacy {
            wrapper.program = command.program.clone();
            wrapper.args = vec![
                "terminal-legacy-client".into(),
                directory.path().as_os_str().to_owned(),
                profile.name().into(),
            ];
        }
        // No inherited stdio pipe handles: the pseudoconsole supplies console handles.
        trace("native-launch-begin");
        let child = match launch::spawn_terminal(&wrapper, hpc) {
            Ok(child) => child,
            Err(mut error) => {
                // No usable client; close the reader first so older blocking close
                // cannot wait on an output pipe that nobody is servicing.
                drop(output);
                drop(output_peer);
                drop(input_peer);
                settle_failed_launch(&mut error);
                drop(console);
                return Err(error.into());
            }
        };
        trace("native-launched");
        drop(input_peer);
        drop(output_peer);
        Ok((
            Self {
                child: Some(child),
                input,
                console: Some(console),
                directory,
                sequence: std::cell::Cell::new(0),
                complete: std::cell::Cell::new(false),
                last: std::cell::RefCell::new(None),
            },
            Reader(output),
        ))
    }
    fn request(&self, operation: &str) -> io::Result<serde_json::Value> {
        if self.console.is_none() || self.complete.get() {
            if operation != "modes" {
                return Err(invalid("terminal client already completed"));
            }
            return self
                .last
                .borrow()
                .clone()
                .ok_or_else(|| invalid("no terminal mode evidence"));
        }
        trace("request-begin");
        let sequence = self.sequence.get() + 1;
        if sequence > 100_000 {
            return Err(invalid("terminal telemetry request bound"));
        }
        self.sequence.set(sequence);
        publish(
            self.directory.path(),
            &format!("request.{sequence}.json"),
            &serde_json::json!({"sequence":sequence,"operation":operation}),
        )?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(value) = read_record(
                &self
                    .directory
                    .path()
                    .join(format!("response.{sequence}.json")),
            )? {
                let modes = validate_response(&value, sequence, "active")?;
                trace("client-ready");
                trace_modes("first-active-modes", &modes);
                std::fs::remove_file(
                    self.directory
                        .path()
                        .join(format!("response.{sequence}.json")),
                )?;
                self.last.replace(Some(modes.clone()));
                return Ok(modes);
            }
            if let Some(value) = read_record(&self.directory.path().join("complete.json"))? {
                let modes = validate_response(&value, sequence - 1, "complete")?;
                trace("client-ready");
                trace_modes("client-complete-modes", &modes);
                self.complete.set(true);
                self.last.replace(Some(modes.clone()));
                if operation != "modes" {
                    return Err(invalid("terminal completed before control acknowledgement"));
                }
                return Ok(modes);
            }
            if std::time::Instant::now() >= deadline {
                trace("request-deadline");
                return Err(invalid("terminal telemetry deadline"));
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    pub(crate) fn modes(&self) -> io::Result<serde_json::Value> {
        self.request("modes")
    }
    pub(crate) fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.input.try_write(bytes)
    }
    pub(crate) fn resize(&mut self, rows: u16, columns: u16) -> io::Result<()> {
        trace("resize-native-begin");
        let result = unsafe {
            ResizePseudoConsole(
                self.console
                    .as_ref()
                    .ok_or_else(|| invalid("closed terminal"))?
                    .0,
                COORD {
                    X: columns as i16,
                    Y: rows as i16,
                },
            )
        };
        trace("resize-native-returned");
        if result < 0 {
            return Err(invalid(&format!("ResizePseudoConsole HRESULT {result:#x}")));
        }
        Ok(())
    }
    pub(crate) fn signal(&mut self, event: &str) -> io::Result<()> {
        if !matches!(event, "interrupt" | "terminate") {
            return Err(invalid("invalid terminal event"));
        }
        trace("signal-native-begin");
        let result = self.request("signal").map(|_| ());
        trace("signal-native-returned");
        result
    }
    pub(crate) fn exited(&mut self) -> io::Result<bool> {
        self.child
            .as_mut()
            .map_or(Ok(true), OwnedChild::exited_reserved)
    }
    pub(crate) fn close(&mut self, terminate: bool) -> io::Result<ExitStatus> {
        trace("close-begin");
        let observation = self.request("modes");
        trace("close-observation-returned");
        if let Ok(modes) = &observation {
            trace_modes("close-modes", modes);
        }
        trace("input-close-begin");
        let input = self.input.close();
        trace("input-close-returned");
        let mut child = self
            .child
            .take()
            .ok_or_else(|| invalid("terminal child released"))?;
        trace("terminate-begin");
        let termination = if terminate {
            child.terminate_owned()
        } else {
            Ok(())
        };
        trace("terminate-returned");
        // The independent output reader remains live until real peer EOF. This
        // worker is joined even on current builds whose native close returns early.
        let console = self
            .console
            .take()
            .ok_or_else(|| invalid("terminal already closed"))?;
        let closer = close_worker(console, None);
        trace("job-cleanup-begin");
        let status = child.reap_and_release();
        trace("job-cleanup-returned");
        trace("close-join-begin");
        let joined = closer.join();
        trace("close-join-returned");
        if !matches!(joined, Ok(true)) {
            return Err(invalid(
                "ConPTY close worker panic or model lacked reader progress",
            ));
        }
        input?;
        termination?;
        observation?;
        status
    }
}

#[derive(Default)]
struct CloseModel {
    paused: std::sync::atomic::AtomicBool,
    read_bytes: std::sync::atomic::AtomicUsize,
    progress: std::sync::atomic::AtomicBool,
}
fn close_worker(
    console: PseudoConsole,
    model: Option<std::sync::Arc<CloseModel>>,
) -> std::thread::JoinHandle<bool> {
    std::thread::spawn(move || {
        trace("close-worker-start");
        let mut progressed = true;
        if let Some(model) = model {
            use std::sync::atomic::Ordering;
            let before = model.read_bytes.load(Ordering::SeqCst);
            model.paused.store(false, Ordering::SeqCst);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while model.read_bytes.load(Ordering::SeqCst) <= before
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            progressed = model.read_bytes.load(Ordering::SeqCst) > before;
            model.progress.store(progressed, Ordering::SeqCst);
        }
        // Exactly one real HPCON close, with the independent reader still live.
        drop(console);
        progressed
    })
}

fn active_modes() -> io::Result<Vec<u32>> {
    // Reopen CONOUT$ every time: a saved original screen-buffer handle is not
    // evidence of the alternate screen buffer currently used by Theseus.
    let input = OpenOptions::new().read(true).write(true).open("CONIN$")?;
    let output = OpenOptions::new().read(true).write(true).open("CONOUT$")?;
    let mut input_mode = 0;
    let mut output_mode = 0;
    if unsafe { GetConsoleMode(input.as_raw_handle(), &mut input_mode) } == 0
        || unsafe { GetConsoleMode(output.as_raw_handle(), &mut output_mode) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(vec![input_mode, output_mode])
}
fn evidence(before: &[u32]) -> io::Result<serde_json::Value> {
    let current = active_modes()?;
    let output = OpenOptions::new().read(true).write(true).open("CONOUT$")?;
    let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
    if unsafe { GetConsoleScreenBufferInfo(output.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(
        serde_json::json!({"kind":"console", "before":before,"current":current,
        "dimensions":[info.srWindow.Bottom-info.srWindow.Top+1, info.srWindow.Right-info.srWindow.Left+1],
        "raw":current[0] & ENABLE_LINE_INPUT == 0}),
    )
}

unsafe extern "system" fn wrapper_control(event: u32) -> i32 {
    i32::from(event == CTRL_BREAK_EVENT || event == CTRL_C_EVENT)
}
struct Client(Child);
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
/// Only invoked by the trusted fixture's fixed terminal-client entry point.
pub(crate) fn run_client(
    arguments: &[std::ffi::OsString],
) -> Result<i32, Box<dyn std::error::Error>> {
    if arguments.len() < 3 || arguments.len() > 131 {
        return Err(invalid("invalid terminal client arguments").into());
    }
    let program = Path::new(&arguments[0]);
    if !program.is_absolute() || program.file_stem() != Some(std::ffi::OsStr::new("theseus")) {
        return Err(invalid("terminal client only launches Theseus").into());
    }
    let root = Path::new(&arguments[1]);
    let profile = ConsoleProfile::parse(&arguments[2])?;
    let baseline = modes::Baseline::apply(profile)?;
    // Attachment does not establish inherited standard handles. Bind the actual
    // child to owned validated console devices; keep the app IsTerminal check.
    let input = OpenOptions::new().read(true).write(true).open("CONIN$")?;
    let output = OpenOptions::new().read(true).write(true).open("CONOUT$")?;
    modes::query(&input)?;
    modes::query(&output)?;
    if unsafe { SetConsoleCtrlHandler(Some(wrapper_control), 1) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    let mut child = Client(
        std::process::Command::new(program)
            .args(&arguments[3..])
            .stdin(Stdio::from(input))
            .stdout(Stdio::from(output.try_clone()?))
            .stderr(Stdio::from(output))
            .spawn()?,
    );
    let mut sequence = 1;
    loop {
        if let Some(status) = child.0.try_wait()? {
            publish(
                root,
                "complete.json",
                &serde_json::json!({"phase":"complete","sequence":sequence-1,"modes":baseline.evidence()?}),
            )?;
            baseline.restore()?;
            return Ok(status.code().unwrap_or(130));
        }
        if let Some(request) = read_record(&root.join(format!("request.{sequence}.json")))? {
            if request["sequence"] != sequence {
                return Err(invalid("wrong terminal request sequence").into());
            }
            match request["operation"].as_str() {
                Some("modes") => (),
                Some("signal") => {
                    if unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, 0) } == 0 {
                        return Err(io::Error::last_os_error().into());
                    }
                }
                _ => return Err(invalid("unknown terminal client operation").into()),
            }
            publish(
                root,
                &format!("response.{sequence}.json"),
                &serde_json::json!({"phase":"active","sequence":sequence,"modes":baseline.evidence()?}),
            )?;
            std::fs::remove_file(root.join(format!("request.{sequence}.json")))?;
            sequence += 1;
            if sequence > 100_000 {
                return Err(invalid("terminal client request bound").into());
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

pub(crate) fn run_legacy_client(
    arguments: &[std::ffi::OsString],
) -> Result<i32, Box<dyn std::error::Error>> {
    modes::legacy(arguments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::Ordering};

    #[test]
    fn conpty_backpressure_close_model_joins_reader_and_real_native_cleanup() {
        let (mut input, input_peer) = pipe::writer().unwrap();
        let (mut output, output_peer) = pipe::reader().unwrap();
        let mut hpc = 0;
        trace("native-create-begin");
        let result = unsafe {
            CreatePseudoConsole(
                COORD { X: 80, Y: 24 },
                input_peer.as_raw_handle(),
                output_peer.as_raw_handle(),
                0,
                &mut hpc,
            )
        };
        assert!(result >= 0, "CreatePseudoConsole HRESULT {result:#x}");
        trace("native-created");
        let console = PseudoConsole(hpc);
        let mut command = NativeCommand::new(
            std::env::var_os("THESEUS_HOST_TEST_FIXTURE")
                .expect("required Cargo-built native fixture"),
        );
        command.args(["flood", "1048576", "0"]);
        trace("native-launch-begin");
        let child = match launch::spawn_terminal(&command, hpc) {
            Ok(child) => child,
            Err(mut error) => {
                drop(output);
                drop(output_peer);
                drop(input_peer);
                settle_failed_launch(&mut error);
                drop(console);
                panic!("{error}");
            }
        };
        trace("native-launched");
        trace("input-peer-drop-begin");
        drop(input_peer);
        trace("input-peer-drop-returned");
        trace("output-peer-drop-begin");
        drop(output_peer);
        trace("output-peer-drop-returned");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        let pressure = loop {
            trace("pressure-peek-begin");
            let buffered = output.terminal_buffered_bytes();
            trace("pressure-peek-returned");
            match buffered {
                Ok(bytes) if bytes >= 16384 => break Ok(bytes),
                Ok(_) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(2))
                }
                Ok(bytes) => {
                    break Err(format!(
                        "only {bytes} bytes; output did not fill native pipe"
                    ));
                }
                Err(error) => break Err(error.to_string()),
            }
        };
        trace("pressure-loop-complete");
        // Even a failed pressure observation goes through the same real drain,
        // close and child cleanup before any assertion can unwind.
        let model = Arc::new(CloseModel::default());
        model.paused.store(true, Ordering::SeqCst);
        let observer = model.clone();
        trace("reader-spawn-begin");
        let reader = std::thread::spawn(move || -> io::Result<usize> {
            trace("reader-started");
            let mut bytes = [0; 16384];
            loop {
                if observer.paused.load(Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    continue;
                }
                match output.try_read(&mut bytes) {
                    Ok(0) => {
                        trace("reader-eof");
                        return Ok(observer.read_bytes.load(Ordering::SeqCst));
                    }
                    Ok(count) => {
                        observer.read_bytes.fetch_add(count, Ordering::SeqCst);
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(2))
                    }
                    Err(error) => {
                        trace("reader-error");
                        return Err(error);
                    }
                }
            }
        });
        trace("close-begin");
        let closer = close_worker(console, Some(model.clone()));
        trace("input-close-begin");
        let closed_input = input.close();
        trace("input-close-returned");
        trace("job-cleanup-begin");
        let cleanup = child.reap_and_release();
        trace("job-cleanup-returned");
        trace("close-join-begin");
        let closed = closer.join();
        trace("close-join-returned");
        trace("reader-join-begin");
        let drained = reader.join();
        trace("reader-join-returned");
        closed_input.unwrap();
        let status = cleanup.unwrap();
        assert!(
            closed.unwrap(),
            "MODEL close must wait for actual reader byte progress"
        );
        let count = drained.unwrap().unwrap();
        let buffered = pressure.unwrap();
        assert!(
            count >= buffered,
            "accepted output must drain before real EOF"
        );
        assert!(model.progress.load(Ordering::SeqCst));
        println!(
            "ConPTY ACTUAL buffered={buffered} drained={count} EOF=true owned_client_job_cleanup=true close_worker_joined=true reader_joined=true status={status}; blocking-close seam=MODEL; pre26100 kernel execution=PENDING"
        );
    }

    #[test]
    fn terminal_file_records_reject_partial_oversized_and_stale_sequences() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("record.json");
        assert!(read_record(&path).unwrap().is_none());
        std::fs::write(&path, b"{\"sequence\":").unwrap();
        assert!(read_record(&path).is_err());
        std::fs::write(&path, vec![b' '; 4097]).unwrap();
        assert!(read_record(&path).is_err());
        std::fs::remove_file(&path).unwrap();
        publish(
            root.path(),
            "record.json",
            &serde_json::json!({"sequence":7,"phase":"active","modes":{"kind":"console"}}),
        )
        .unwrap();
        let value = read_record(&path).unwrap().unwrap();
        assert!(validate_response(&value, 8, "active").is_err());
        assert!(validate_response(&value, 7, "complete").is_err());
        assert!(
            validate_response(&value, 7, "active").is_err(),
            "missing native modes cannot fabricate zeros"
        );
        let complete = serde_json::json!({"sequence":7,"phase":"active","modes":{"kind":"console","before":[7,3],"current":[0,7],"dimensions":[36,120],"raw":true}});
        assert_eq!(
            validate_response(&complete, 7, "active").unwrap()["current"],
            serde_json::json!([0, 7])
        );
    }
}
