//! Bounded duplex pipe acquisition under the shared owned-child supervisor.
use super::*;
use std::process::{ChildStderr, ChildStdin, ChildStdout};

pub(in crate::runner) trait DuplexDriver: Send + 'static {
    fn pending_write(&self) -> &[u8];
    fn written(&mut self, count: usize) -> Result<()>;
    fn receive(&mut self, bytes: &[u8]);
    fn eof(&mut self);
    fn cancel(&mut self);
    fn wants_close(&self) -> bool;
}
pub(in crate::runner) struct DuplexOutput<D> {
    /// The interactive CLI child was successfully spawned.
    pub acquisition_started: bool,
    pub process: Output,
    pub driver: D,
    pub stdin_closed: bool,
    pub stdout_eof: bool,
    pub io_error: Option<String>,
    pub forced_abort: bool,
    pub stdout_bytes: u64,
}
#[derive(Default)]
pub(super) struct Signals {
    pub close: AtomicBool,
    pub cancel: AtomicBool,
    pub failed: AtomicBool,
}
// Unwinding must notify the monitor before a failed worker can look complete.
struct WorkerGuard(Arc<Signals>);
impl Drop for WorkerGuard {
    fn drop(&mut self) {
        if thread::panicking() {
            self.0.failed.store(true, Ordering::SeqCst);
        }
    }
}
struct Acquired<D> {
    driver: D,
    output: Drain,
    stdout_bytes: u64,
    stdout_eof: bool,
    error: Option<String>,
}

fn exchange<D: DuplexDriver>(
    input: ChildStdin,
    mut output: ChildStdout,
    mut driver: D,
    mut file: Option<File>,
    limit: usize,
    stop: Arc<AtomicBool>,
    signals: Arc<Signals>,
) -> Acquired<D> {
    let _guard = WorkerGuard(signals.clone());
    let mut input = Some(input);
    let mut acquired = Drain::empty();
    let mut stdout_bytes = 0u64;
    let mut stdout_eof = false;
    let mut error = None;
    let mut cancellation_at = None;
    let mut buffer = [0; 4096];
    while !stop.load(Ordering::SeqCst) {
        if signals.cancel.load(Ordering::SeqCst) && cancellation_at.is_none() {
            cancellation_at = Some(Instant::now());
            driver.cancel();
        }
        let cancelling = cancellation_at.is_some();
        let cancel_flushed = cancellation_at.is_some_and(|at| {
            at.elapsed() >= Duration::from_millis(100) || driver.pending_write().is_empty()
        });
        if signals.close.load(Ordering::SeqCst)
            || cancel_flushed
            || (!cancelling && driver.wants_close())
            || error.is_some()
        {
            input.take();
            signals.close.store(true, Ordering::SeqCst);
        }
        if let Some(input) = input.as_mut() {
            let pending = driver.pending_write();
            if !pending.is_empty() {
                match input.write(&pending[..pending.len().min(4096)]) {
                    Ok(0) => append_error(&mut error, "server stdin closed while writing".into()),
                    Ok(count) => {
                        if let Err(failure) = driver.written(count) {
                            append_error(&mut error, format!("advance server stdin: {failure:#}"));
                        }
                    }
                    Err(failure)
                        if matches!(
                            failure.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(failure) => {
                        append_error(&mut error, format!("write server stdin: {failure}"))
                    }
                }
            }
        }
        match output.read(&mut buffer) {
            Ok(0) => {
                stdout_eof = true;
                driver.eof();
                break;
            }
            Ok(count) => {
                stdout_bytes = stdout_bytes.saturating_add(count as u64);
                let keep = count.min(limit.saturating_sub(acquired.bytes.len()));
                if keep > 0 {
                    driver.receive(&buffer[..keep]);
                }
                if let Err(failure) = acquired.record(&buffer[..count], &mut file, limit) {
                    append_error(&mut error, format!("write stdout log: {failure:#}"));
                }
                if acquired.truncated {
                    signals.close.store(true, Ordering::SeqCst);
                }
            }
            Err(failure)
                if matches!(
                    failure.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(failure) => {
                append_error(&mut error, format!("read server stdout: {failure}"));
                break;
            }
        }
        if error.is_some() {
            signals.failed.store(true, Ordering::SeqCst);
        }
        thread::sleep(Duration::from_millis(1));
    }
    drop(input);
    signals.close.store(true, Ordering::SeqCst);
    if let Some(file) = file
        && let Err(failure) = file.sync_all()
    {
        append_error(&mut error, format!("sync stdout log: {failure}"));
    }
    if error.is_some() {
        signals.failed.store(true, Ordering::SeqCst);
    }
    Acquired {
        driver,
        output: acquired,
        stdout_bytes,
        stdout_eof,
        error,
    }
}

fn setup_failure(
    process: &mut HostProcess,
    abort: &mut impl FnMut() -> Result<()>,
    message: String,
) -> anyhow::Error {
    let mut error = Some(message);
    if let Err(failure) = abort() {
        append_error(&mut error, format!("abort owned environment: {failure:#}"));
    }
    if let Err(failure) = process.terminate() {
        append_error(
            &mut error,
            format!("terminate host process group: {failure:#}"),
        );
    }
    if let Err(failure) = process.reap_and_disarm() {
        append_error(&mut error, format!("{failure:#}"));
    }
    anyhow!(error.unwrap())
}
fn configure_pipes(
    process: &mut HostProcess,
    abort: &mut impl FnMut() -> Result<()>,
) -> Result<(ChildStdin, ChildStdout, PipeReader<ChildStderr>)> {
    let setup = (|| {
        let input = process.child.stdin.take().context("missing child stdin")?;
        let output = process
            .child
            .stdout
            .take()
            .context("missing child stdout")?;
        fcntl_setfl(&input, fcntl_getfl(&input)? | OFlags::NONBLOCK)?;
        fcntl_setfl(&output, fcntl_getfl(&output)? | OFlags::NONBLOCK)?;
        let stderr = PipeReader::new(
            process
                .child
                .stderr
                .take()
                .context("missing child stderr")?,
            process.stop.clone(),
        )?;
        Ok::<_, anyhow::Error>((input, output, stderr))
    })();
    match setup {
        Ok(pipes) => Ok(pipes),
        Err(failure) => Err(setup_failure(
            process,
            abort,
            format!("set up duplex pipes: {failure:#}"),
        )),
    }
}

// No interactive child exists; preparation may still require owned cleanup.
fn unstarted<D: DuplexDriver>(
    mut driver: D,
    mut error: Option<String>,
    cancel: &CancellationToken,
    started: Instant,
    abort: &mut impl FnMut() -> Result<()>,
) -> DuplexOutput<D> {
    if let Err(failure) = abort() {
        append_error(&mut error, format!("abort owned environment: {failure:#}"));
    }
    let cancelled = cancel.is_cancelled();
    if cancelled {
        driver.cancel();
    }
    DuplexOutput {
        acquisition_started: false,
        process: Output {
            supervised_stop: false,
            status: None,
            timed_out: false,
            cancelled,
            stdout: Drain::empty(),
            stderr: Drain::empty(),
            duration_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            error,
        },
        driver,
        stdin_closed: true,
        stdout_eof: false,
        io_error: None,
        forced_abort: true,
        stdout_bytes: 0,
    }
}

/// `timeout` excludes the caller's receipt reserve. The last second is for EOF
/// and draining. The owned abort callback must enforce its own cleanup deadline.
/// Errors are post-spawn setup/worker failures; pre-spawn failures retain the
/// driver in a structured output with acquisition_started=false.
#[allow(clippy::too_many_arguments)]
pub(in crate::runner) fn run_duplex<D: DuplexDriver>(
    command: &mut Command,
    timeout: Duration,
    cancel: &CancellationToken,
    files: Option<(File, File)>,
    stdout_limit: usize,
    stderr_limit: usize,
    driver: D,
    mut abort: impl FnMut() -> Result<()>,
) -> Result<DuplexOutput<D>> {
    let started = Instant::now();
    if cancel.is_cancelled() {
        return Ok(unstarted(driver, None, cancel, started, &mut abort));
    }
    let mut process = match HostProcess::spawn_with_stdin(command, StdinMode::Piped) {
        Ok(process) => process,
        Err(error) => {
            return Ok(unstarted(
                driver,
                Some(format!("{error:#}")),
                cancel,
                started,
                &mut abort,
            ));
        }
    };
    let (input, output, stderr) = configure_pipes(&mut process, &mut abort)?;
    let signals = Arc::new(Signals::default());
    let (out_file, err_file) = files.map_or((None, None), |(a, b)| (Some(a), Some(b)));
    // Scoped workers ensure that setup errors cannot detach the first worker.
    thread::scope(|scope| {
        let stop = process.stop.clone();
        let worker_signals = signals.clone();
        let stdout = thread::Builder::new()
            .name("duplex-stdout".into())
            .spawn_scoped(scope, move || {
                exchange(
                    input,
                    output,
                    driver,
                    out_file,
                    stdout_limit,
                    stop,
                    worker_signals,
                )
            });
        let stdout = match stdout {
            Ok(worker) => worker,
            Err(failure) => {
                return Err(setup_failure(
                    &mut process,
                    &mut abort,
                    format!("start duplex worker: {failure}"),
                ));
            }
        };
        let err_signals = signals.clone();
        let stderr = thread::Builder::new()
            .name("duplex-stderr".into())
            .spawn_scoped(scope, move || {
                let _guard = WorkerGuard(err_signals.clone());
                let result = drain_reported(stderr, err_file, stderr_limit, || {
                    err_signals.failed.store(true, Ordering::SeqCst);
                });
                if result.is_err() {
                    err_signals.failed.store(true, Ordering::SeqCst);
                }
                result
            });
        let stderr = match stderr {
            Ok(worker) => worker,
            Err(failure) => {
                let mut error = Some(format!(
                    "{:#}",
                    setup_failure(
                        &mut process,
                        &mut abort,
                        format!("start stderr worker: {failure}")
                    )
                ));
                match stdout.join() {
                    Ok(acquired) => {
                        if let Some(failure) = acquired.error {
                            append_error(&mut error, failure);
                        }
                    }
                    Err(_) => append_error(&mut error, "duplex stdout worker panicked".into()),
                }
                return Err(anyhow!(error.unwrap()));
            }
        };
        let mut forced_abort = false;
        let completion = process.monitor(
            timeout,
            cancel,
            || stdout.is_finished() && stderr.is_finished(),
            || false,
            MonitorPolicy::Duplex(&signals),
            |event| match event {
                SupervisionEvent::Poll => Ok(SupervisionDecision::Continue),
                SupervisionEvent::Abort => {
                    forced_abort = true;
                    abort().map(|()| SupervisionDecision::Continue)
                }
            },
        );
        let stdout = stdout.join();
        let stderr = stderr.join();
        let mut io_error = None;
        let stdout = match stdout {
            Ok(acquired) => {
                if let Some(error) = &acquired.error {
                    append_error(&mut io_error, error.clone());
                }
                Some(acquired)
            }
            Err(_) => {
                append_error(&mut io_error, "duplex stdout worker panicked".into());
                None
            }
        };
        let stderr = match stderr {
            Ok(Ok(drain)) => drain,
            Ok(Err(failure)) => {
                append_error(&mut io_error, format!("stderr drain: {failure:#}"));
                Drain::empty()
            }
            Err(_) => {
                append_error(&mut io_error, "stderr worker panicked".into());
                Drain::empty()
            }
        };
        let mut process_error = completion.error;
        if let Some(error) = &io_error {
            append_error(&mut process_error, error.clone());
        }
        let stdout = stdout.ok_or_else(|| anyhow!(process_error.clone().unwrap()))?;
        Ok(DuplexOutput {
            acquisition_started: true,
            process: Output {
                supervised_stop: false,
                status: if forced_abort {
                    None
                } else {
                    completion.status.and_then(|s| s.code())
                },
                timed_out: completion.timed_out,
                cancelled: completion.cancelled,
                stdout: stdout.output,
                stderr,
                duration_ms: completion.duration_ms,
                error: process_error,
            },
            driver: stdout.driver,
            stdin_closed: true,
            stdout_eof: stdout.stdout_eof,
            io_error,
            forced_abort,
            stdout_bytes: stdout.stdout_bytes,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::CancellationToken;

    #[derive(Default)]
    struct OneRound {
        write: Vec<u8>,
        offset: usize,
        got: Vec<u8>,
        eof: bool,
    }
    impl DuplexDriver for OneRound {
        fn pending_write(&self) -> &[u8] {
            &self.write[self.offset..]
        }
        fn written(&mut self, n: usize) -> anyhow::Result<()> {
            self.offset += n;
            Ok(())
        }
        fn receive(&mut self, bytes: &[u8]) {
            self.got.extend_from_slice(bytes);
        }
        fn eof(&mut self) {
            self.eof = true;
        }
        fn cancel(&mut self) {
            self.offset = self.write.len();
        }
        fn wants_close(&self) -> bool {
            self.got == b"PONG\n"
        }
    }
    #[test]
    fn duplex_closes_stdin_after_reply_and_joins_both_streams() {
        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", "IFS= read -r line; test \"$line\" = PING || exit 31; printf 'PONG\\n'; if IFS= read -r extra; then exit 32; fi; printf 'diagnostic\\n' >&2; exit 17"]);
        let mut aborted = false;
        let out = run_duplex(
            &mut command,
            std::time::Duration::from_secs(3),
            &CancellationToken::default(),
            None,
            128,
            128,
            OneRound {
                write: b"PING\n".to_vec(),
                ..Default::default()
            },
            || {
                aborted = true;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(out.process.status, Some(17));
        assert_eq!(out.process.stderr.bytes, b"diagnostic\n");
        assert!(out.stdin_closed && out.stdout_eof && out.driver.eof);
        assert_eq!(out.driver.got, b"PONG\n");
        assert!(!aborted && !out.forced_abort && out.io_error.is_none());
    }
    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        command
    }
    fn ping() -> OneRound {
        OneRound {
            write: b"PING\n".to_vec(),
            ..Default::default()
        }
    }

    // A blocking stdin write, or treating forced termination as exit 0, breaks this.
    #[test]
    fn unread_stdin_is_bounded_and_aborts_before_group_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let pid_file = temp.path().join("leader");
        let mut command = shell("printf '%s' \"$$\" > \"$1\"; sleep 30");
        command.arg("unread").arg(&pid_file);
        let began = Instant::now();
        let mut leader = 0;
        let mut aborted = false;
        let out = run_duplex(
            &mut command,
            Duration::from_millis(1500),
            &CancellationToken::default(),
            None,
            16,
            16,
            OneRound {
                write: vec![b'x'; 16384],
                ..Default::default()
            },
            || {
                // The worker must have filled the pipe before the cutoff starts EOF.
                leader = std::fs::read_to_string(&pid_file)?.parse::<u32>()?;
                assert!(std::path::Path::new(&format!("/proc/{leader}")).exists());
                aborted = true;
                Ok(())
            },
        )
        .unwrap();
        assert!(aborted && out.forced_abort && out.process.timed_out);
        assert_eq!(out.process.status, None);
        assert!(out.driver.offset > 0);
        assert!(began.elapsed() < Duration::from_secs(2));
        assert!(!std::path::Path::new(&format!("/proc/{leader}")).exists());
    }
    #[test]
    fn ignored_eof_gets_one_second_grace_and_unknown_status() {
        let began = Instant::now();
        let mut aborted = false;
        let out = run_duplex(
            &mut shell("printf 'PONG\\n'; while :; do sleep 30; done"),
            Duration::from_secs(5),
            &CancellationToken::default(),
            None,
            128,
            128,
            ping(),
            || {
                aborted = true;
                Ok(())
            },
        )
        .unwrap();
        assert!(out.forced_abort && aborted && out.stdin_closed);
        assert_eq!(out.process.status, None);
        assert!(!out.stdout_eof && !out.driver.eof);
        assert!(began.elapsed() < Duration::from_secs(2));
    }
    #[test]
    fn flooding_both_streams_retains_only_prefix_and_counts_discarded_stdout() {
        let out = run_duplex(
            &mut shell("head -c 65536 /dev/zero & head -c 65536 /dev/zero >&2 & wait"),
            Duration::from_secs(5),
            &CancellationToken::default(),
            None,
            127,
            63,
            OneRound::default(),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(out.stdout_bytes, 65536);
        assert_eq!(out.driver.got.len(), 127);
        assert_eq!(out.process.stdout.bytes.len(), 127);
        assert_eq!(out.process.stderr.bytes.len(), 63);
        assert!(out.process.stdout.truncated && out.process.stderr.truncated);
        assert!(out.stdout_eof && !out.forced_abort);
    }
    #[test]
    fn early_exit_reports_actual_eof_without_forced_success() {
        let out = run_duplex(
            &mut shell("exit 23"),
            Duration::from_secs(3),
            &CancellationToken::default(),
            None,
            32,
            32,
            OneRound::default(),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(out.process.status, Some(23));
        assert!(out.stdout_eof && out.driver.eof && out.stdin_closed && !out.forced_abort);
    }
    #[test]
    fn inherited_stdout_keeps_leader_reserved_until_abort() {
        let temp = tempfile::tempdir().unwrap();
        let pid_file = temp.path().join("leader");
        let mut command =
            shell("/usr/bin/setsid /bin/sleep 0.8 & printf '%s' \"$$\" > \"$1\"; exit 0");
        command.arg("inherited").arg(&pid_file);
        let mut leader = 0;
        let out = run_duplex(
            &mut command,
            Duration::from_millis(200),
            &CancellationToken::default(),
            None,
            32,
            32,
            OneRound::default(),
            || {
                leader = std::fs::read_to_string(&pid_file)?.parse::<u32>()?;
                let stat = std::fs::read_to_string(format!("/proc/{leader}/stat"))?;
                assert!(
                    stat.rsplit_once(')')
                        .unwrap()
                        .1
                        .trim_start()
                        .starts_with('Z')
                );
                Ok(())
            },
        )
        .unwrap();
        assert!(out.forced_abort && !out.stdout_eof);
        assert_eq!(out.process.status, None);
        assert!(!std::path::Path::new(&format!("/proc/{leader}")).exists());
    }
    #[test]
    fn already_cancelled_duplex_aborts_without_spawn() {
        let cancel = CancellationToken::default();
        cancel.cancel();
        let mut aborted = false;
        let out = run_duplex(
            &mut Command::new("/no-such-duplex-program"),
            Duration::from_secs(3),
            &cancel,
            None,
            32,
            32,
            ping(),
            || {
                aborted = true;
                Ok(())
            },
        )
        .unwrap();
        assert!(aborted && out.process.cancelled);
        assert_eq!(out.process.status, None);
        assert_eq!(out.driver.offset, 5);
    }
    #[test]
    fn cancellation_while_writing_never_waits_for_protocol_progress() {
        let cancel = CancellationToken::default();
        let trigger = cancel.clone();
        let cancellation = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            trigger.cancel();
        });
        let began = Instant::now();
        let out = run_duplex(
            &mut shell("sleep 30"),
            Duration::from_secs(10),
            &cancel,
            None,
            32,
            32,
            OneRound {
                write: vec![b'x'; 16384],
                ..Default::default()
            },
            || Ok(()),
        )
        .unwrap();
        cancellation.join().unwrap();
        assert!(out.process.cancelled && out.forced_abort);
        assert_eq!(out.driver.offset, 16384); // cancel was called, discarding the pending frame.
        assert!(began.elapsed() < Duration::from_secs(2));
    }
    #[test]
    fn cancellation_during_abort_remains_cancelled_and_preserves_abort_error() {
        let cancel = CancellationToken::default();
        let out = run_duplex(
            &mut shell("sleep 30"),
            Duration::from_millis(100),
            &cancel,
            None,
            32,
            32,
            OneRound::default(),
            || {
                cancel.cancel();
                anyhow::bail!("owned cleanup refused")
            },
        )
        .unwrap();
        assert!(out.process.cancelled && out.forced_abort);
        assert!(out.process.error.unwrap().contains("owned cleanup refused"));
    }
    #[test]
    fn log_failures_from_both_workers_are_preserved() {
        let stdout = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap();
        let stderr = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap();
        let out = run_duplex(
            &mut shell("printf 'out'; printf 'err' >&2"),
            Duration::from_secs(3),
            &CancellationToken::default(),
            Some((stdout, stderr)),
            32,
            32,
            OneRound::default(),
            || anyhow::bail!("cleanup failed too"),
        )
        .unwrap();
        let error = out.process.error.unwrap();
        assert!(
            error.contains("stdout log") && error.contains("stderr drain"),
            "{error}"
        );
        assert!(out.forced_abort && error.contains("cleanup failed too"));
        assert_eq!(out.process.status, None);
    }
    // An EOF on stdout must start shutdown even if inherited stderr is still held.
    #[test]
    fn stdout_eof_starts_grace_before_exchange_cutoff() {
        let began = Instant::now();
        let out = run_duplex(
            &mut shell("exec 1>&-; sleep 30"),
            Duration::from_secs(5),
            &CancellationToken::default(),
            None,
            32,
            32,
            OneRound::default(),
            || Ok(()),
        )
        .unwrap();
        assert!(out.stdout_eof && out.forced_abort);
        assert!(
            began.elapsed() < Duration::from_secs(2),
            "elapsed {:?}",
            began.elapsed()
        );
    }

    struct PanicOnReceive;
    impl DuplexDriver for PanicOnReceive {
        fn pending_write(&self) -> &[u8] {
            &[]
        }
        fn written(&mut self, _: usize) -> Result<()> {
            Ok(())
        }
        fn receive(&mut self, _: &[u8]) {
            panic!("toy parser panic");
        }
        fn eof(&mut self) {}
        fn cancel(&mut self) {}
        fn wants_close(&self) -> bool {
            false
        }
    }
    #[test]
    fn panicked_worker_aborts_promptly_and_joins_failing_stderr() {
        let stdout = tempfile::tempfile().unwrap();
        let stderr = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap();
        let began = Instant::now();
        let mut aborted = false;
        let error = run_duplex(
            &mut shell("printf 'err' >&2; printf 'out'; sleep 30"),
            Duration::from_secs(5),
            &CancellationToken::default(),
            Some((stdout, stderr)),
            32,
            32,
            PanicOnReceive,
            || {
                aborted = true;
                Ok(())
            },
        )
        .err()
        .unwrap()
        .to_string();
        assert!(
            error.contains("stdout worker panicked") && error.contains("stderr drain"),
            "{error}"
        );
        assert!(aborted && began.elapsed() < Duration::from_secs(2));
    }
    #[test]
    fn graceful_shutdown_may_cross_exchange_cutoff_without_being_a_timeout() {
        let out = run_duplex(
            &mut shell("printf 'PONG\\n'; sleep 0.4; cat >/dev/null; exit 17"),
            Duration::from_millis(1300),
            &CancellationToken::default(),
            None,
            32,
            32,
            ping(),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(out.process.status, Some(17));
        assert!(!out.process.timed_out && !out.forced_abort);
    }
    struct CancelFrame {
        pending: Vec<u8>,
        offset: usize,
        at_cancel: Option<usize>,
        closed_after: bool,
    }
    impl DuplexDriver for CancelFrame {
        fn pending_write(&self) -> &[u8] {
            &self.pending[self.offset..]
        }
        fn written(&mut self, count: usize) -> Result<()> {
            self.offset += count;
            Ok(())
        }
        fn receive(&mut self, _: &[u8]) {}
        fn eof(&mut self) {
            self.closed_after = true;
        }
        fn cancel(&mut self) {
            self.at_cancel = Some(self.offset);
            self.pending = vec![b'c'; 1048576];
            self.offset = 0;
        }
        fn wants_close(&self) -> bool {
            self.at_cancel.is_some()
        }
    }
    #[test]
    fn blocked_writes_cancel_with_bounded_flush_before_eof() {
        let cancel = CancellationToken::default();
        let trigger = cancel.clone();
        let cancellation = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            trigger.cancel();
        });
        let out = run_duplex(
            &mut shell("sleep 0.4; cat >/dev/null; printf done"),
            Duration::from_secs(5),
            &cancel,
            None,
            32,
            32,
            CancelFrame {
                pending: vec![b'x'; 1048576],
                offset: 0,
                at_cancel: None,
                closed_after: false,
            },
            || Ok(()),
        )
        .unwrap();
        cancellation.join().unwrap();
        let before_cancel = out.driver.at_cancel.unwrap();
        assert!(
            before_cancel > 0 && before_cancel < 1048576,
            "test must actually fill the pipe"
        );
        assert!(
            out.driver.offset < 1048576,
            "cancellation cannot wait for a blocked frame"
        );
        assert!(out.driver.closed_after && out.process.cancelled && !out.forced_abort);
        assert!(out.process.duration_ms < 1000);
    }

    #[test]
    fn cancellation_during_existing_close_grace_does_not_extend_it() {
        let cancel = CancellationToken::default();
        let trigger = cancel.clone();
        let cancellation = thread::spawn(move || {
            thread::sleep(Duration::from_millis(600));
            trigger.cancel();
        });
        let out = run_duplex(
            &mut shell("printf 'PONG\\n'; sleep 30"),
            Duration::from_secs(5),
            &cancel,
            None,
            32,
            32,
            ping(),
            || Ok(()),
        )
        .unwrap();
        cancellation.join().unwrap();
        assert!(out.process.cancelled && out.forced_abort);
        assert!(out.process.duration_ms < 1400);
    }
    #[test]
    fn abort_callback_panic_still_stops_and_joins_workers() {
        let began = Instant::now();
        let result = std::panic::catch_unwind(|| {
            let _ = run_duplex(
                &mut shell("sleep 30"),
                Duration::from_millis(100),
                &CancellationToken::default(),
                None,
                32,
                32,
                OneRound::default(),
                || panic!("abort callback panicked"),
            );
        });
        assert!(result.is_err());
        assert!(began.elapsed() < Duration::from_secs(2));
    }
    struct NotifyCancel {
        offset: usize,
        cancelled: bool,
    }
    impl DuplexDriver for NotifyCancel {
        fn pending_write(&self) -> &[u8] {
            if self.cancelled {
                &b"stop\n"[self.offset..]
            } else {
                &[]
            }
        }
        fn written(&mut self, count: usize) -> Result<()> {
            self.offset += count;
            Ok(())
        }
        fn receive(&mut self, _: &[u8]) {}
        fn eof(&mut self) {}
        fn cancel(&mut self) {
            self.cancelled = true;
        }
        fn wants_close(&self) -> bool {
            self.cancelled
        }
    }
    #[test]
    fn cancellation_flushes_permitted_notification_then_closes_stdin() {
        let cancel = CancellationToken::default();
        let trigger = cancel.clone();
        let cancellation = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            trigger.cancel();
        });
        let out = run_duplex(&mut shell("IFS= read -r line; test \"$line\" = stop || exit 31; if IFS= read -r extra; then exit 32; fi; exit 7"),
            Duration::from_secs(3), &cancel, None, 32, 32,
            NotifyCancel { offset: 0, cancelled: false }, || Ok(())).unwrap();
        cancellation.join().unwrap();
        assert_eq!(out.process.status, Some(7));
        assert_eq!(out.driver.offset, 5);
        assert!(out.process.cancelled && out.stdout_eof && !out.forced_abort);
    }
    #[test]
    fn pipe_setup_failure_aborts_while_leader_is_reserved() {
        let mut process = HostProcess::spawn(&mut shell("sleep 30")).unwrap();
        let leader = process.child.id();
        let mut aborted = false;
        let error = configure_pipes(&mut process, &mut || {
            assert!(std::path::Path::new(&format!("/proc/{leader}")).exists());
            aborted = true;
            anyhow::bail!("setup cleanup refused")
        })
        .err()
        .unwrap()
        .to_string();
        assert!(
            aborted
                && error.contains("missing child stdin")
                && error.contains("setup cleanup refused")
        );
        assert!(!std::path::Path::new(&format!("/proc/{leader}")).exists());
    }
    // Publish the final failure only when the monitor observes worker completion:
    // its first failure sample has already happened, so accepting completion here
    // would bypass owned abort and kill the local descriptor holder first.
    #[test]
    fn final_worker_failure_aborts_before_cleanup_after_completion_observation() {
        let temp = tempfile::tempdir().unwrap();
        let identities = temp.path().join("identities");
        let mut command = shell("sleep 30 & printf '%s %s\\n' \"$$\" \"$!\" > \"$1\"; exit 17");
        command.arg("completion-race").arg(&identities);
        let mut process = HostProcess::spawn(&mut command).unwrap();
        let began = Instant::now();
        while !process.exited_without_reaping().unwrap() {
            assert!(began.elapsed() < Duration::from_secs(2));
            thread::sleep(Duration::from_millis(5));
        }
        let ids = std::fs::read_to_string(identities).unwrap();
        let ids: Vec<u32> = ids
            .split_whitespace()
            .map(|id| id.parse().unwrap())
            .collect();
        let leader = ids[0];
        let holder = ids[1];
        let state = |pid| {
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .map(|stat| {
                    stat.rsplit_once(')')
                        .unwrap()
                        .1
                        .trim_start()
                        .chars()
                        .next()
                        .unwrap()
                })
        };
        let signals = Signals::default();
        let mut forced_abort = false;
        let completion = process.monitor(
            Duration::from_secs(3),
            &CancellationToken::default(),
            || {
                assert!(!signals.failed.swap(true, Ordering::SeqCst));
                true
            },
            || false,
            MonitorPolicy::Duplex(&signals),
            |event| match event {
                SupervisionEvent::Poll => panic!("leader is already exited"),
                SupervisionEvent::Abort => {
                    assert_eq!(state(leader), Some('Z'), "leader must remain reserved");
                    assert!(
                        state(holder).is_some_and(|state| !matches!(state, 'Z' | 'X')),
                        "local group must remain alive until owned abort"
                    );
                    forced_abort = true;
                    anyhow::bail!("owned abort diagnostic")
                }
            },
        );
        assert!(forced_abort, "late worker failure must invoke owned abort");
        let error = completion.error.unwrap();
        assert!(
            error.contains("duplex worker failed") && error.contains("owned abort diagnostic"),
            "{error}"
        );
        assert_eq!(state(leader), None, "leader must be reaped after abort");
        for _ in 0..100 {
            if state(holder).is_none_or(|state| matches!(state, 'Z' | 'X')) {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("owned descriptor holder survived group cleanup");
    }
}

#[cfg(test)]
mod review_regressions {
    use super::*;
    #[derive(Default)]
    struct Reply {
        received: bool,
    }
    impl DuplexDriver for Reply {
        fn pending_write(&self) -> &[u8] {
            &[]
        }
        fn written(&mut self, _: usize) -> Result<()> {
            Ok(())
        }
        fn receive(&mut self, _: &[u8]) {
            self.received = true;
        }
        fn eof(&mut self) {}
        fn cancel(&mut self) {}
        fn wants_close(&self) -> bool {
            self.received
        }
    }
    #[test]
    fn early_interactive_shutdown_keeps_timeout_decision_before_emergency_cleanup() {
        for cancel_during_cleanup in [false, true] {
            let cancel = CancellationToken::default();
            let began = Instant::now();
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "printf reply; while :; do sleep 30; done"]);
            let mut aborted = false;
            let out = run_duplex(
                &mut command,
                Duration::from_secs(2),
                &cancel,
                None,
                128,
                128,
                Reply::default(),
                || {
                    assert!(
                        began.elapsed() < Duration::from_secs(2),
                        "shutdown decision precedes execution deadline"
                    );
                    aborted = true;
                    thread::sleep(Duration::from_millis(1200));
                    if cancel_during_cleanup {
                        cancel.cancel();
                    }
                    Ok(())
                },
            )
            .unwrap();
            assert!(aborted && out.forced_abort);
            assert!(began.elapsed() > Duration::from_secs(2));
            assert!(
                !out.process.timed_out,
                "separate cleanup elapsed must not fabricate execution timeout"
            );
            assert_eq!(out.process.cancelled, cancel_during_cleanup);
            assert_eq!(out.process.status, None);
        }
    }
    #[test]
    fn pre_spawn_cancellation_and_spawn_error_retain_explicit_not_started_driver() {
        for cancelled in [true, false] {
            let cancel = CancellationToken::default();
            if cancelled {
                cancel.cancel();
            }
            let mut aborted = false;
            let out = run_duplex(
                &mut Command::new("/missing-duplex-executable"),
                Duration::from_secs(3),
                &cancel,
                None,
                32,
                32,
                Reply::default(),
                || {
                    aborted = true;
                    Ok(())
                },
            )
            .unwrap();
            assert!(!out.acquisition_started);
            assert!(aborted && out.forced_abort);
            assert!(!out.driver.received);
            assert_eq!(out.process.cancelled, cancelled);
            assert_eq!(out.process.status, None);
            assert_eq!(out.process.error.is_some(), !cancelled);
        }
    }
}
