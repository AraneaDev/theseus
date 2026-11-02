//! Fixed acceptance protocol, intentionally not a general command broker.
//! Startup <= 256 KiB; each operation <= 512 KiB; <= 100,000 operations,
//! <= 1 MiB aggregate input, <= 64 KiB per input, 2..500 rows/columns.
//! Output is separately bounded to 4 MiB by the owning terminal session.
use serde::Deserialize;
use std::io::{self, BufRead, Write};
use theseus_host::process::{ChildEnvironment, NativeCommand, terminal::Session};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Startup {
    program: std::path::PathBuf,
    args: Vec<String>,
    cwd: std::path::PathBuf,
    environment: std::collections::BTreeMap<String, String>,
    rows: u16,
    columns: u16,
    #[serde(default)]
    console_profile: theseus_host::process::terminal::ConsoleProfile,
}
#[derive(Deserialize)]
#[serde(tag = "op", deny_unknown_fields)]
enum Operation {
    #[serde(rename = "poll")]
    Poll {},
    #[serde(rename = "input")]
    Input { bytes: Vec<u8> },
    #[serde(rename = "resize")]
    Resize { rows: u16, columns: u16 },
    #[serde(rename = "signal")]
    Signal { event: String },
    #[serde(rename = "finish")]
    Finish {},
}
fn record(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    std::io::Read::take(&mut *reader, 524289).read_until(b'\n', &mut bytes)?;
    if bytes.is_empty() {
        return Ok(None);
    }
    if bytes.len() > 524288 || bytes.last() != Some(&b'\n') {
        return Err(io::Error::other("incomplete or oversized terminal record"));
    }
    Ok(Some(bytes))
}
fn reply(value: serde_json::Value) -> io::Result<()> {
    let mut output = io::stdout().lock();
    serde_json::to_writer(&mut output, &value)?;
    output.write_all(b"\n")?;
    output.flush()
}
pub(super) fn run() -> super::FixtureResult {
    let mut input = io::stdin().lock();
    let startup_bytes =
        record(&mut input)?.ok_or_else(|| io::Error::other("missing terminal startup"))?;
    if startup_bytes.len() > 262144 {
        return Err(io::Error::other("oversized terminal startup").into());
    }
    let startup: Startup = serde_json::from_slice(&startup_bytes)?;
    if !startup.program.is_absolute()
        || !startup.program.is_file()
        || startup.program.file_stem() != Some(std::ffi::OsStr::new("theseus"))
        || !startup.cwd.is_absolute()
        || startup.args.len() > 128
        || startup.environment.len() > 512
    {
        return Err(io::Error::other(
            "terminal startup requires the pinned Theseus binary and bounded native arguments",
        )
        .into());
    }
    let mut command = NativeCommand::new(&startup.program);
    command.args(startup.args).current_dir(startup.cwd);
    command.environment = ChildEnvironment::Replace(
        startup
            .environment
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect(),
    );
    let mut terminal = Session::spawn_with_profile(
        &command,
        startup.rows,
        startup.columns,
        startup.console_profile,
    )?;
    theseus_host::process::terminal::trace("ready-flush-begin");
    reply(serde_json::json!({"ready":true}))?;
    theseus_host::process::terminal::trace("ready-flush-returned");
    let mut total = 0;
    for _ in 0..100_000 {
        let Some(bytes) = record(&mut input)? else {
            terminal.finish(true)?;
            return Ok(());
        };
        let operation: Operation = serde_json::from_slice(&bytes)?;
        match operation {
            Operation::Input { bytes } => {
                total += bytes.len();
                if total > 1024 * 1024 {
                    return Err(io::Error::other("terminal aggregate input exceeded 1 MiB").into());
                }
                terminal.write(&bytes)?;
            }
            Operation::Resize { rows, columns } => terminal.resize(rows, columns)?,
            Operation::Signal { event } => terminal.signal(&event)?,
            Operation::Finish {} => terminal.finish(true)?,
            Operation::Poll {} => (),
        }
        let observed = terminal.poll()?;
        theseus_host::process::terminal::trace("reply-flush-begin");
        reply(observed)?;
        theseus_host::process::terminal::trace("reply-flush-returned");
    }
    Err(io::Error::other("terminal operation count exceeded").into())
}
