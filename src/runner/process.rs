//! Bounded independent pipe drains. Container abort runs before killing docker exec.
use crate::engine::CancellationToken;
use anyhow::{Context, Result, anyhow};
use std::{
    fs::File,
    io::{Read, Write},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub(super) struct Output {
    pub status: Option<i32>,
    pub timed_out: bool,
    pub cancelled: bool,
    pub stdout: Drain,
    pub stderr: Drain,
    pub duration_ms: u64,
}
pub(super) struct Drain {
    pub bytes: Vec<u8>,
    pub stored: u64,
    pub truncated: bool,
}
fn drain(mut reader: impl Read, mut file: Option<File>, limit: usize) -> Result<Drain> {
    let mut bytes = Vec::new();
    let mut stored = 0;
    let mut truncated = false;
    let mut buffer = [0; 32768];
    let mut error = None;
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        let keep = read.min(limit.saturating_sub(stored));
        truncated |= keep < read;
        if keep > 0 {
            bytes.extend_from_slice(&buffer[..keep]);
            if let Some(f) = file.as_mut()
                && let Err(e) = f.write_all(&buffer[..keep])
            {
                error = Some(e);
                file = None;
            }
            stored += keep;
        }
    }
    if let Some(e) = error {
        return Err(e.into());
    }
    if let Some(f) = file {
        f.sync_all()?;
    }
    Ok(Drain {
        bytes,
        stored: stored as u64,
        truncated,
    })
}
pub(super) fn run(
    command: &mut Command,
    timeout: Duration,
    cancel: &CancellationToken,
    files: Option<(File, File)>,
    limit: usize,
    mut abort: impl FnMut() -> Result<()>,
) -> Result<Output> {
    if cancel.is_cancelled() {
        abort()?;
        return Ok(Output {
            status: None,
            timed_out: false,
            cancelled: true,
            stdout: Drain {
                bytes: vec![],
                stored: 0,
                truncated: false,
            },
            stderr: Drain {
                bytes: vec![],
                stored: 0,
                truncated: false,
            },
            duration_ms: 0,
        });
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let started = Instant::now();
    let mut child = command.spawn().context("spawn Docker CLI")?;
    let out = child.stdout.take().unwrap();
    let err = child.stderr.take().unwrap();
    let (out_file, err_file) = files.map_or((None, None), |(a, b)| (Some(a), Some(b)));
    let stdout = thread::spawn(move || drain(out, out_file, limit));
    let stderr = thread::spawn(move || drain(err, err_file, limit));
    let mut timed_out = false;
    let mut cancelled = false;
    let mut abort_error = None;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(error) => {
                abort_error = Some(anyhow!(error));
                let _ = abort();
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
        cancelled = cancel.is_cancelled();
        timed_out = started.elapsed() >= timeout;
        if cancelled || timed_out {
            if let Err(error) = abort() {
                abort_error = Some(error);
            }
            let _ = child.kill();
            break child.wait().ok();
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = stdout
        .join()
        .map_err(|_| anyhow!("stdout drain panicked"))??;
    let stderr = stderr
        .join()
        .map_err(|_| anyhow!("stderr drain panicked"))??;
    // Preserve timeout/cancel as terminal evidence even if abort failed; the engine
    // retries verified teardown. A plain utility failure includes the abort error.
    if let Some(error) = abort_error
        && !timed_out
        && !cancelled
    {
        return Err(error);
    }
    Ok(Output {
        status: status.and_then(|s| s.code()),
        timed_out,
        cancelled,
        stdout,
        stderr,
        duration_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
    })
}
pub(super) fn utility(command: &mut Command, cancel: &CancellationToken) -> Result<String> {
    let output = run(
        command,
        Duration::from_secs(120),
        cancel,
        None,
        1_048_576,
        || Ok(()),
    )?;
    anyhow::ensure!(
        !output.timed_out && !output.cancelled,
        "Docker operation timed out or cancelled"
    );
    anyhow::ensure!(
        output.status == Some(0),
        "Docker operation failed (status {:?}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    anyhow::ensure!(
        !output.stdout.truncated,
        "Docker metadata exceeds bounded output limit"
    );
    Ok(String::from_utf8(output.stdout.bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn already_cancelled_execution_does_not_spawn_any_program() {
        let cancel = CancellationToken::default();
        cancel.cancel();
        let output = run(
            &mut Command::new("/theseus-nonexistent-program"),
            Duration::from_secs(1),
            &cancel,
            None,
            16,
            || Ok(()),
        )
        .unwrap();
        assert!(output.cancelled);
        assert_eq!(output.status, None);
    }
    #[test]
    fn timeout_invokes_abort_before_terminal_return() {
        let mut aborted = false;
        let output = run(
            Command::new("/bin/sleep").arg("2"),
            Duration::from_millis(30),
            &CancellationToken::default(),
            None,
            32,
            || {
                aborted = true;
                Ok(())
            },
        )
        .unwrap();
        assert!(aborted);
        assert!(output.timed_out);
        assert!(output.duration_ms < 1000);
    }
    #[test]
    fn independently_caps_both_streams_while_draining_large_output() {
        let output = run(
            Command::new("/bin/sh").args([
                "-c",
                "head -c 131072 /dev/zero; head -c 131072 /dev/zero >&2",
            ]),
            Duration::from_secs(2),
            &CancellationToken::default(),
            None,
            256,
            || Ok(()),
        )
        .unwrap();
        assert_eq!(output.status, Some(0));
        assert_eq!(output.stdout.bytes.len(), 256);
        assert_eq!(output.stderr.bytes.len(), 256);
        assert!(output.stdout.truncated && output.stderr.truncated);
    }
}
