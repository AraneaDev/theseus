//! Private native test fixture; no shell parsing or arbitrary executable launch.
//!
//! Commands (arguments are separate native argv entries):
//! - `echo-argv [ARG...]`: each ARG as u64 little-endian byte length followed by
//!   `OsStr::as_encoded_bytes()` bytes; no argument count or terminator.
//! - `echo-env NAME`: the explicitly named value in the same record format.
//!   Missing variables fail; an empty value is a zero-length record.
//! - `echo-bytes HEX`: decode an even-length ASCII hexadecimal string to stdout.
//! - `flood COUNT [STDERR_COUNT]`: write COUNT `O` bytes to stdout, then
//!   STDERR_COUNT `E` bytes to stderr (defaults to COUNT).
//! - `flood-duplex COUNT`: concurrently write COUNT `O` / `E` bytes on stdout/stderr.
//! - `delayed-read-until-eof MAX_BYTES MILLIS`: delay reading, then echo input at
//!   EOF and report `stdin EOF received` on stderr.
//! - `descendant MILLIS`: spawn this fixture's `hold-pipes MILLIS` with inherited
//!   stdout/stderr, print its decimal PID and newline, then exit without waiting.
//! - `hold-pipes MILLIS` / `ignore-stdin MILLIS`: retain streams without reading
//!   stdin until the duration expires, then exit successfully.
//! - `read-until-eof MAX_BYTES`: read until EOF, then echo the exact input bytes;
//!   fail without stdout if input exceeds the bound.
//!
//! - `publication-interruption ROOT before|after`: publish fixed bounded bytes to
//!   ROOT/report.json through HostDir. Before gates inside the writer callback;
//!   after gates only after publish returns, and prints the completed scope.
//!   Each gate prints/flushed readiness, then reads exactly one byte from stdin.
//!   Only byte `!` releases it; EOF/errors abort. The owning test kills/reaps it.
//!
//! - `echo-cwd`: current directory as one native record.
//! - `launch-marker PATH`: write fixed bytes `native fixture ran` to PATH.
//! - `nested-launch PATH`: launch this fixture's marker through OwnedChild.
//! - `delayed-marker READY LATE`: write READY, wait 500ms, then write LATE.
//! - `descendant-marker READY LATE`: spawn delayed-marker then exit immediately.
//! - Windows `close-job-owner READY LATE`: create nested owned delayed-marker,
//!   wait for READY, exit without Rust Drop to close the native job handle.
//! - Windows `probe-handle NUMBER FORBIDDEN_BASENAME`: succeed only if the handle
//!   is absent or names a different file; detects unrelated inherited objects.
//!
//! Counts are decimal, at most 64 MiB; durations are at most 60,000 ms.
//! Hex output is at most 64 MiB. No mode reads unrelated environment variables.

#[path = "fixture/docker.rs"]
mod docker;
#[path = "fixture/duplex.rs"]
mod duplex;
#[cfg(feature = "test-support")]
#[path = "fixture/terminal.rs"]
mod terminal;

use std::env;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

const MAX_BYTES: u64 = 64 * 1024 * 1024;

// The main fixture thread retains typed spawn failures and their cleanup owners.
// io::Error's Send+Sync payload bound is unnecessary for this private boundary.
type FixtureResult = Result<(), Box<dyn std::error::Error>>;

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn number(text: &OsStr, maximum: u64) -> io::Result<u64> {
    let text = text
        .to_str()
        .ok_or_else(|| invalid("expected decimal number"))?;
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid("expected decimal number"));
    }
    let value = text
        .parse::<u64>()
        .map_err(|_| invalid("number too large"))?;
    if value > maximum {
        return Err(invalid("number exceeds fixture bound"));
    }
    Ok(value)
}

fn record(output: &mut impl Write, value: &OsStr) -> io::Result<()> {
    let bytes = value.as_encoded_bytes();
    output.write_all(&(bytes.len() as u64).to_le_bytes())?;
    output.write_all(bytes)
}

fn flood(output: &mut impl Write, byte: u8, mut count: u64) -> io::Result<()> {
    let bytes = [byte; 8192];
    while count > 0 {
        let len = count.min(bytes.len() as u64) as usize;
        output.write_all(&bytes[..len])?;
        count -= len as u64;
    }
    Ok(())
}

// This mode deliberately leaves a bounded native descendant after the root exits.
#[allow(clippy::zombie_processes)]
fn descendant(millis: u64) -> io::Result<()> {
    let child = Command::new(env::current_exe()?)
        .arg("hold-pipes")
        .arg(millis.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    writeln!(io::stdout().lock(), "{}", child.id())
}

fn publication_gate(label: &str) -> io::Result<()> {
    let mut output = io::stdout().lock();
    writeln!(output, "{label}")?;
    output.flush()?;
    let mut release = [0];
    io::stdin().lock().read_exact(&mut release)?;
    if release != *b"!" {
        return Err(invalid("publication gate requires explicit release"));
    }
    Ok(())
}
fn publication_interruption(root: &OsStr, gate: &OsStr) -> io::Result<()> {
    use theseus_host::fs::{HostDir, RootPolicy};
    let before = match gate.to_str() {
        Some("before") => true,
        Some("after") => false,
        _ => return Err(invalid("expected before or after publication gate")),
    };
    let root = HostDir::open(std::path::Path::new(root), RootPolicy::LeafNoFollow)?;
    let scope = root.publish(OsStr::new("report.json"), &mut |file| {
        file.write_all(b"complete new bytes")?;
        if before {
            publication_gate("before:writer-callback")?;
        }
        Ok(())
    })?;
    if !before {
        publication_gate(&format!("after:{scope:?}"))?;
    }
    Ok(())
}

fn marker(path: &OsStr) -> io::Result<()> {
    std::fs::write(path, b"native fixture ran")
}

fn delayed_marker(ready: &OsStr, late: &OsStr) -> io::Result<()> {
    marker(ready)?;
    std::thread::sleep(Duration::from_millis(500));
    marker(late)
}

#[allow(clippy::zombie_processes)]
fn descendant_marker(ready: &OsStr, late: &OsStr) -> io::Result<()> {
    Command::new(env::current_exe()?)
        .arg("delayed-marker")
        .arg(ready)
        .arg(late)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    Ok(())
}

fn nested_launch(path: &OsStr) -> FixtureResult {
    use theseus_host::process::{ChildEnvironment, NativeCommand, OwnedChild};
    let command = NativeCommand {
        program: env::current_exe()?.into(),
        args: vec!["launch-marker".into(), path.into()],
        cwd: None,
        environment: ChildEnvironment::Inherit,
    };
    let mut child = OwnedChild::spawn_files(&command, None, None, None)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !child.exited_reserved()? {
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "nested fixture").into());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    if child.reap_and_release()?.success() {
        Ok(())
    } else {
        Err(io::Error::other("nested launch failed").into())
    }
}

// Exiting the owning process bypasses Rust Drop. Its non-inherited job handle is
// closed by the kernel, independently proving KILL_ON_JOB_CLOSE for the child.
#[cfg(windows)]
fn close_job_owner(ready: &OsStr, late: &OsStr) -> FixtureResult {
    use theseus_host::process::{ChildEnvironment, NativeCommand, OwnedChild};
    let command = NativeCommand {
        program: env::current_exe()?.into(),
        args: vec!["delayed-marker".into(), ready.into(), late.into()],
        cwd: None,
        environment: ChildEnvironment::Inherit,
    };
    let _child = OwnedChild::spawn_files(&command, None, None, None)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !std::path::Path::new(ready).exists() {
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "nested ready marker").into());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    std::process::exit(0)
}

#[cfg(windows)]
fn probe_handle(value: &OsStr, forbidden: &OsStr) -> io::Result<()> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;
    let handle = number(value, usize::MAX as u64)? as usize as *mut std::ffi::c_void;
    let mut name = vec![0u16; 32768];
    // An absent handle can be reused for another object in the child. Compare the
    // observed object name, rather than assuming matching numeric values identify it.
    let count =
        unsafe { GetFinalPathNameByHandleW(handle, name.as_mut_ptr(), name.len() as u32, 0) };
    if count == 0 {
        return Ok(());
    }
    if count as usize >= name.len() {
        return Err(io::Error::other("unexpected handle path length"));
    }
    let path = OsString::from_wide(&name[..count as usize]);
    if std::path::Path::new(&path).file_name() == Some(forbidden) {
        return Err(io::Error::other(
            "unrelated inheritable file leaked to child",
        ));
    }
    Ok(())
}

fn read_until_eof(maximum: u64) -> io::Result<()> {
    let mut bytes = Vec::new();
    io::stdin()
        .lock()
        .take(maximum + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        return Err(invalid("input exceeds fixture bound"));
    }
    io::stdout().lock().write_all(&bytes)
}

// The Windows parent creates a new console for this fixture only. Event group 0
// therefore cannot reach the test runner or an unrelated console process.
// Fixed owned descendant tree: heartbeat proves liveness without PID reopening.
#[allow(clippy::zombie_processes)]
fn owned_tree(mode: &str, ready: &OsStr, heartbeat: &OsStr) -> io::Result<()> {
    if mode == "tree-held" {
        flood(&mut io::stdout(), 0, 131072)?;
        flood(&mut io::stderr(), 0, 131072)?;
    }
    let child_ready = std::path::Path::new(ready).with_extension("child");
    let mut child = Command::new(env::current_exe()?)
        .arg("ready-hold")
        .arg(&child_ready)
        .arg(heartbeat)
        .arg("30000")
        .stdin(Stdio::null())
        .spawn()?;
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !child_ready.exists() {
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("fixture descendant not ready"));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    std::fs::write(ready, format!("{} {}\n", std::process::id(), child.id()))?;
    if mode == "tree-held" {
        child.wait()?;
    } else {
        std::process::exit(if mode == "tree-exit" { 17 } else { 0 });
    }
    Ok(())
}

fn cancellation_event() -> io::Result<()> {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use theseus_host::cancel::CancellationRegistration;
    for _ in 0..32 {
        let live = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicBool::new(false));
        let registration = CancellationRegistration::install(live.clone())?;
        let obsolete = CancellationRegistration::install(dropped.clone())?;
        drop(obsolete);
        #[cfg(unix)]
        signal_hook::low_level::raise(signal_hook::consts::SIGINT)?;
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
            if unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, 0) } == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !live.load(Ordering::SeqCst) {
            if std::time::Instant::now() >= deadline {
                return Err(io::Error::other(
                    "control event did not reach live registration",
                ));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        if dropped.load(Ordering::SeqCst) {
            return Err(io::Error::other(
                "control event reached dropped registration",
            ));
        }
        drop(registration);
    }
    writeln!(
        io::stdout().lock(),
        "32 owned control events: live=true dropped=false"
    )
}

fn run(args: &[OsString]) -> FixtureResult {
    let (mode, args) = args
        .split_first()
        .ok_or_else(|| invalid("missing command"))?;
    match (mode.to_str(), args) {
        (Some("launch-marker"), [path]) => marker(path),
        (Some("nested-launch"), [path]) => return nested_launch(path),
        (Some("delayed-marker"), [ready, late]) => delayed_marker(ready, late),
        (Some("descendant-marker"), [ready, late]) => descendant_marker(ready, late),
        #[cfg(windows)]
        (Some("close-job-owner"), [ready, late]) => return close_job_owner(ready, late),
        #[cfg(windows)]
        (Some("probe-handle"), [value, forbidden]) => probe_handle(value, forbidden),
        (Some("publication-interruption"), [root, gate]) => publication_interruption(root, gate),
        (Some("echo-cwd"), []) => record(&mut io::stdout().lock(), env::current_dir()?.as_os_str()),
        (Some("echo-argv"), args) => {
            let mut output = io::stdout().lock();
            for arg in args {
                record(&mut output, arg)?;
            }
            Ok(())
        }
        (Some(mode @ ("tree-held" | "tree-exit" | "tree-exit0")), [ready, heartbeat]) => {
            owned_tree(mode, ready, heartbeat)
        }
        (Some("archive-holder"), [ready]) => {
            let mut byte = [0];
            if io::stdin().read(&mut byte)? != 0 {
                return Err(invalid("archive exit gate must contain no data").into());
            }
            std::fs::write(ready, b"leader exited; inherited streams held")?;
            std::thread::sleep(Duration::from_secs(30));
            Ok(())
        }
        (Some("duplex-case"), [case]) => duplex::run(
            case.to_str()
                .ok_or_else(|| invalid("invalid duplex case"))?,
        ),
        (Some("exit"), [status]) => std::process::exit(number(status, i32::MAX as u64)? as i32),
        (Some("cancellation-event"), []) => cancellation_event(),
        (Some("echo-env"), [name]) => {
            if name.is_empty()
                || name
                    .as_encoded_bytes()
                    .iter()
                    .any(|b| matches!(b, b'=' | 0))
            {
                return Err(invalid("invalid environment name").into());
            }
            let value = env::var_os(name).ok_or_else(|| invalid("named variable is absent"))?;
            record(&mut io::stdout().lock(), &value)
        }
        (Some("echo-bytes"), [hex]) => {
            let hex = hex
                .to_str()
                .ok_or_else(|| invalid("expected hexadecimal bytes"))?;
            if hex.len() % 2 != 0 || hex.len() as u64 / 2 > MAX_BYTES {
                return Err(invalid("invalid hexadecimal byte count").into());
            }
            let bytes = hex
                .as_bytes()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| {
                    let digit = |b: u8| {
                        (b as char)
                            .to_digit(16)
                            .ok_or_else(|| invalid("invalid hexadecimal digit"))
                    };
                    Ok((digit(pair[0])? * 16 + digit(pair[1])?) as u8)
                })
                .collect::<io::Result<Vec<_>>>()?;
            io::stdout().lock().write_all(&bytes)
        }
        (Some("flood-duplex"), [count]) => {
            let count = number(count, MAX_BYTES)?;
            std::thread::scope(|scope| {
                let errors = scope.spawn(|| flood(&mut io::stderr().lock(), b'E', count));
                let output = flood(&mut io::stdout().lock(), b'O', count);
                let errors = errors
                    .join()
                    .map_err(|_| io::Error::other("flood worker panicked"))?;
                output.and(errors)
            })
        }
        (Some("flood"), [count]) => {
            let count = number(count, MAX_BYTES)?;
            flood(&mut io::stdout().lock(), b'O', count)?;
            flood(&mut io::stderr().lock(), b'E', count)
        }
        (Some("flood"), [out, err]) => {
            let out = number(out, MAX_BYTES)?;
            let err = number(err, MAX_BYTES)?;
            flood(&mut io::stdout().lock(), b'O', out)?;
            flood(&mut io::stderr().lock(), b'E', err)
        }
        (Some("descendant"), [millis]) => descendant(number(millis, 60_000)?),
        (Some("ready-hold"), [ready, heartbeat, millis]) => {
            let duration = Duration::from_millis(number(millis, 60_000)?);
            let mut heartbeat = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(heartbeat)?;
            std::fs::write(ready, std::process::id().to_string())?;
            io::stdout().lock().write_all(b"ready")?;
            io::stdout().lock().flush()?;
            let until = std::time::Instant::now() + duration;
            while std::time::Instant::now() < until {
                heartbeat.write_all(b".")?;
                heartbeat.flush()?;
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(())
        }
        (Some("hold-pipes" | "ignore-stdin"), [millis]) => {
            std::thread::sleep(Duration::from_millis(number(millis, 60_000)?));
            Ok(())
        }
        (Some("read-until-eof"), [maximum]) => read_until_eof(number(maximum, MAX_BYTES)?),
        (Some("delayed-read-until-eof"), [maximum, millis]) => {
            let maximum = number(maximum, MAX_BYTES)?;
            std::thread::sleep(Duration::from_millis(number(millis, 60_000)?));
            read_until_eof(maximum)?;
            writeln!(io::stderr().lock(), "stdin EOF received")
        }
        _ => Err(invalid("unknown command or wrong argument count")),
    }?;
    Ok(())
}

fn main() {
    #[cfg(all(windows, feature = "test-support"))]
    if env::args_os().nth(1).as_deref() == Some(OsStr::new("terminal-client")) {
        let result = theseus_host::process::terminal::run_client(
            &env::args_os().skip(2).collect::<Vec<_>>(),
        );
        match result {
            Ok(code) => std::process::exit(code),
            Err(error) => {
                eprintln!("terminal client: {error}");
                drop(error);
                std::process::exit(2);
            }
        }
    }
    #[cfg(feature = "test-support")]
    if env::args_os().nth(1).as_deref() == Some(OsStr::new("terminal")) {
        if let Err(error) = terminal::run() {
            eprintln!("terminal: {error}");
            drop(error);
            std::process::exit(2);
        }
        return;
    }
    let executable = env::current_exe().expect("fixture executable path");
    if executable.file_stem() == Some(OsStr::new("docker")) {
        #[cfg(feature = "test-support")]
        if env::var_os("THESEUS_DOCKER_TEST_MODE").as_deref() == Some(OsStr::new("cached-error")) {
            std::fs::write("prepare-ready", b"ready").expect("prepare readiness");
            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            while std::time::Instant::now() < deadline {
                if std::fs::read("prepare-gate").is_ok_and(|bytes| bytes == b"x") {
                    std::process::exit(1);
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            std::process::exit(98);
        }
        let sentinel = env::var_os("THESEUS_DOCKER_SENTINEL").expect("native Docker sentinel path");
        std::fs::write(sentinel, b"called").expect("native Docker sentinel write");
        std::process::exit(111);
    }
    if executable.file_stem() == Some(OsStr::new("theseus")) {
        let args: Vec<_> = env::args_os().skip(1).collect();
        if args.len() != 2 || args[0] != "cleanup" {
            std::process::exit(99);
        }
        println!("2\ncleanup\n{}", args[1].to_string_lossy());
        return;
    }
    if let Some(mode) = executable
        .file_stem()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("theseus-docker-"))
    {
        let result = docker::run(
            mode,
            executable.parent().unwrap(),
            &env::args_os().skip(1).collect::<Vec<_>>(),
        );
        match result {
            Ok(code) => std::process::exit(code),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(99);
            }
        }
    }
    if let Err(error) = run(&env::args_os().skip(1).collect::<Vec<_>>()) {
        eprintln!("theseus-host-fixture: {error}");
        // Ordinary failure runs retained cleanup destructors before process exit.
        // close-job-owner's intentional successful exit remains a separate proof.
        drop(error);
        std::process::exit(2);
    }
}
