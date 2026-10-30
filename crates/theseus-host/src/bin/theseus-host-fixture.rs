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

fn run(args: &[OsString]) -> io::Result<()> {
    let (mode, args) = args
        .split_first()
        .ok_or_else(|| invalid("missing command"))?;
    match (mode.to_str(), args) {
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
