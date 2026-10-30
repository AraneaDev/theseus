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

use std::env;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

const MAX_BYTES: u64 = 64 * 1024 * 1024;

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

fn nested_launch(path: &OsStr) -> io::Result<()> {
    use theseus_host::process::{ChildEnvironment, NativeCommand, OwnedChild};
    let command = NativeCommand {
        program: env::current_exe()?.into(),
        args: vec!["launch-marker".into(), path.into()],
        cwd: None,
        environment: ChildEnvironment::Inherit,
    };
    let mut child =
        OwnedChild::spawn_files(&command, None, None, None).map_err(io::Error::other)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !child.exited_reserved()? {
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "nested fixture"));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    if child.reap_and_release()?.success() {
        Ok(())
    } else {
        Err(io::Error::other("nested launch failed"))
    }
}

// Exiting the owning process bypasses Rust Drop. Its non-inherited job handle is
// closed by the kernel, independently proving KILL_ON_JOB_CLOSE for the child.
#[cfg(windows)]
fn close_job_owner(ready: &OsStr, late: &OsStr) -> io::Result<()> {
    use theseus_host::process::{ChildEnvironment, NativeCommand, OwnedChild};
    let command = NativeCommand {
        program: env::current_exe()?.into(),
        args: vec!["delayed-marker".into(), ready.into(), late.into()],
        cwd: None,
        environment: ChildEnvironment::Inherit,
    };
    let _child = OwnedChild::spawn_files(&command, None, None, None).map_err(io::Error::other)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !std::path::Path::new(ready).exists() {
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "nested ready marker",
            ));
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

fn run(args: &[OsString]) -> io::Result<()> {
    let (mode, args) = args
        .split_first()
        .ok_or_else(|| invalid("missing command"))?;
    match (mode.to_str(), args) {
        (Some("launch-marker"), [path]) => marker(path),
        (Some("nested-launch"), [path]) => nested_launch(path),
        (Some("delayed-marker"), [ready, late]) => delayed_marker(ready, late),
        (Some("descendant-marker"), [ready, late]) => descendant_marker(ready, late),
        #[cfg(windows)]
        (Some("close-job-owner"), [ready, late]) => close_job_owner(ready, late),
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
        (Some("echo-env"), [name]) => {
            if name.is_empty()
                || name
                    .as_encoded_bytes()
                    .iter()
                    .any(|b| matches!(b, b'=' | 0))
            {
                return Err(invalid("invalid environment name"));
            }
            let value = env::var_os(name).ok_or_else(|| invalid("named variable is absent"))?;
            record(&mut io::stdout().lock(), &value)
        }
        (Some("echo-bytes"), [hex]) => {
            let hex = hex
                .to_str()
                .ok_or_else(|| invalid("expected hexadecimal bytes"))?;
            if hex.len() % 2 != 0 || hex.len() as u64 / 2 > MAX_BYTES {
                return Err(invalid("invalid hexadecimal byte count"));
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
        (Some("hold-pipes" | "ignore-stdin"), [millis]) => {
            std::thread::sleep(Duration::from_millis(number(millis, 60_000)?));
            Ok(())
        }
        (Some("read-until-eof"), [maximum]) => {
            let maximum = number(maximum, MAX_BYTES)?;
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
        _ => Err(invalid("unknown command or wrong argument count")),
    }
}

fn main() {
    if let Err(error) = run(&env::args_os().skip(1).collect::<Vec<_>>()) {
        eprintln!("theseus-host-fixture: {error}");
        std::process::exit(2);
    }
}
