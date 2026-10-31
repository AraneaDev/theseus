//! Bounded duplex pipe acquisition under the shared owned-child supervisor.
use super::*;

pub(in crate::runner) trait DuplexDriver: Send + 'static {
    fn pending_write(&self) -> &[u8];
    fn written(&mut self, count: usize) -> Result<()>;
    fn receive(&mut self, bytes: &[u8]);
    fn eof(&mut self);
    fn cancel(&mut self);
    fn wants_close(&self) -> bool;
}
pub(in crate::runner) struct DuplexOutput<D> {
    /// Historical MayRun phase; does not assert successful acquisition or guest startup.
    pub acquisition_started: bool,
    pub launch_failure: Option<AcquisitionFailure>,
    pub cleanup_failures: CleanupFailures,
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
    cleanup: CleanupFailures,
    driver: D,
    output: Drain,
    stdout_bytes: u64,
    stdout_eof: bool,
    error: Option<String>,
}

// Kept local to the existing exchange loop: a deterministic test endpoint can
// model delayed native completion without changing protocol or supervisor policy.
trait ExchangeRead {
    fn try_read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize>;
    fn cancel_and_complete(&mut self) -> std::io::Result<()>;
    fn retain(self) -> theseus_host::process::OwnedSpawnCleanup;
}
trait ExchangeWrite {
    fn try_write(&mut self, bytes: &[u8]) -> std::io::Result<usize>;
    fn close(&mut self) -> std::io::Result<()>;
    fn retain(self) -> theseus_host::process::OwnedSpawnCleanup;
}
impl ExchangeRead for NativeReader {
    fn try_read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        NativeReader::try_read(self, bytes)
    }
    fn cancel_and_complete(&mut self) -> std::io::Result<()> {
        NativeReader::cancel_and_complete(self)
    }
    fn retain(self) -> theseus_host::process::OwnedSpawnCleanup {
        theseus_host::process::OwnedSpawnCleanup::from_endpoints(vec![self], vec![])
    }
}
impl ExchangeWrite for NativeWriter {
    fn try_write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        NativeWriter::try_write(self, bytes)
    }
    fn close(&mut self) -> std::io::Result<()> {
        NativeWriter::close(self)
    }
    fn retain(self) -> theseus_host::process::OwnedSpawnCleanup {
        theseus_host::process::OwnedSpawnCleanup::from_endpoints(vec![], vec![self])
    }
}

fn advance_write<D: DuplexDriver>(
    input: &mut impl ExchangeWrite,
    prefix: &mut Option<Vec<u8>>,
    driver: &mut D,
    error: &mut Option<String>,
) {
    let Some(bytes) = prefix.as_ref() else { return };
    match input.try_write(bytes) {
        Ok(0) => {
            append_error(error, "server stdin closed while writing".into());
            *prefix = None;
        }
        Ok(count) => {
            // Driver mutations are deferred while this exact prefix is live.
            // Acknowledgement belongs only to the frame that submitted it.
            if let Err(failure) = driver.written(count) {
                append_error(error, format!("advance server stdin: {failure:#}"));
            }
            *prefix = None;
        }
        Err(failure)
            if matches!(
                failure.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) => {}
        Err(failure) => {
            append_error(error, format!("write server stdin: {failure}"));
            *prefix = None;
        }
    }
}

fn close_input(
    input: &mut Option<impl ExchangeWrite>,
    prefix: &mut Option<Vec<u8>>,
    error: &mut Option<String>,
    cleanup: &mut CleanupFailures,
) {
    if let Some(mut endpoint) = input.take()
        && let Err(failure) = endpoint.close()
    {
        append_error(error, format!("close server stdin: {failure}"));
        retain_endpoint(cleanup, endpoint.retain(), failure);
    }
    // Native close observes cancellation/completion before releasing its storage.
    // An abandoned prefix is never acknowledged against a replacement frame.
    input.take();
    *prefix = None;
}

fn retain_endpoint(
    cleanup: &mut CleanupFailures,
    owner: theseus_host::process::OwnedSpawnCleanup,
    error: std::io::Error,
) {
    cleanup.retain(
        &AcquisitionFailure::from_spawn(
            SpawnFailure {
                launch: theseus_host::process::LaunchPhase::MayRun,
                error,
                cleanup_errors: vec![],
                cleanup: theseus_host::process::HostCleanup::Pending(owner),
            },
            Instant::now(),
        )
        .into(),
    );
}

fn exchange<D: DuplexDriver>(
    input: impl ExchangeWrite,
    output: impl ExchangeRead,
    driver: D,
    file: Option<File>,
    limit: usize,
    stop: Arc<AtomicBool>,
    signals: Arc<Signals>,
) -> Acquired<D> {
    exchange_with_clock(
        input,
        output,
        driver,
        file,
        limit,
        stop,
        signals,
        Instant::now,
    )
}

#[allow(clippy::too_many_arguments)]
fn exchange_with_clock<D: DuplexDriver>(
    input: impl ExchangeWrite,
    mut output: impl ExchangeRead,
    mut driver: D,
    mut file: Option<File>,
    limit: usize,
    stop: Arc<AtomicBool>,
    signals: Arc<Signals>,
    now: impl Fn() -> Instant,
) -> Acquired<D> {
    let _guard = WorkerGuard(signals.clone());
    let mut input = Some(input);
    let mut cleanup = CleanupFailures::default();
    let mut prefix = None;
    let mut acquired = Drain::empty();
    // Deferred driver input reuses the bounded capture; only a cursor is added.
    let mut delivered = 0;
    let mut stdout_bytes = 0u64;
    let mut stdout_eof = false;
    let mut error = None;
    let mut cancellation_at = None;
    let mut cancellation_position = None;
    let mut cancelled_driver = false;
    let mut buffer = [0; 4096];
    let exchanged = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        while !stop.load(Ordering::SeqCst) {
            if signals.cancel.load(Ordering::SeqCst) && cancellation_at.is_none() {
                cancellation_at = Some(now());
                cancellation_position = Some(acquired.bytes.len());
            }
            if let Some(input) = input.as_mut() {
                advance_write(input, &mut prefix, &mut driver, &mut error);
            }
            let cancel_expired = cancellation_at.is_some_and(|at| {
                now().saturating_duration_since(at) >= Duration::from_millis(100)
            });
            if signals.close.load(Ordering::SeqCst) || cancel_expired || error.is_some() {
                close_input(&mut input, &mut prefix, &mut error, &mut cleanup);
                signals.close.store(true, Ordering::SeqCst);
            }
            if prefix.is_none() {
                // Preserve the observed ordering of received bytes and cancellation
                // even if a native write delayed their delivery to the pure driver.
                if let Some(position) = cancellation_position.take() {
                    if delivered < position {
                        driver.receive(&acquired.bytes[delivered..position]);
                        delivered = position;
                    }
                    driver.cancel();
                    cancelled_driver = true;
                }
                if delivered < acquired.bytes.len() {
                    driver.receive(&acquired.bytes[delivered..]);
                    delivered = acquired.bytes.len();
                }
                if (cancelled_driver && driver.pending_write().is_empty())
                    || (cancellation_at.is_none() && driver.wants_close())
                {
                    close_input(&mut input, &mut prefix, &mut error, &mut cleanup);
                    signals.close.store(true, Ordering::SeqCst);
                }
                if let Some(input) = input.as_mut() {
                    let pending = driver.pending_write();
                    if !pending.is_empty() {
                        prefix = Some(pending[..pending.len().min(4096)].to_vec());
                        advance_write(input, &mut prefix, &mut driver, &mut error);
                    }
                }
            }
            match output.try_read(&mut buffer) {
                Ok(0) => stdout_eof = true,
                Ok(count) => {
                    stdout_bytes = stdout_bytes.saturating_add(count as u64);
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
            // A peer may respond after kernel completion but before our next poll.
            // Reconcile the same prefix before exposing those bytes or EOF.
            if let Some(input) = input.as_mut() {
                advance_write(input, &mut prefix, &mut driver, &mut error);
            }
            if stdout_eof {
                close_input(&mut input, &mut prefix, &mut error, &mut cleanup);
            }
            if prefix.is_none()
                && cancellation_position.is_none()
                && delivered < acquired.bytes.len()
            {
                driver.receive(&acquired.bytes[delivered..]);
                delivered = acquired.bytes.len();
            }
            if stdout_eof {
                if let Some(position) = cancellation_position.take() {
                    if delivered < position {
                        driver.receive(&acquired.bytes[delivered..position]);
                        delivered = position;
                    }
                    driver.cancel();
                }
                if delivered < acquired.bytes.len() {
                    driver.receive(&acquired.bytes[delivered..]);
                }
                driver.eof();
                break;
            }
            if error.is_some() {
                signals.failed.store(true, Ordering::SeqCst);
            }
            thread::sleep(Duration::from_millis(1));
        }
    }));
    if exchanged.is_err() {
        signals.failed.store(true, Ordering::SeqCst);
    }
    close_input(&mut input, &mut prefix, &mut error, &mut cleanup);
    if let Err(failure) = output.cancel_and_complete() {
        append_error(&mut error, format!("complete server stdout: {failure}"));
        retain_endpoint(&mut cleanup, output.retain(), failure);
    }
    if let Err(original) = exchanged {
        cleanup.resume_unwind(original);
    }
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
        cleanup,
        driver,
        output: acquired,
        stdout_bytes,
        stdout_eof,
        error,
    }
}

fn configure_pipes(
    process: &mut HostProcess,
    abort: &mut impl FnMut() -> Result<()>,
) -> Result<(NativeWriter, NativeReader, PipeReader)> {
    let setup = (|| {
        let input = process.stdin.take().context("missing child stdin")?;
        let output = process.stdout.take().context("missing child stdout")?;
        let stderr = PipeReader::new(
            process.stderr.take().context("missing child stderr")?,
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
    launch_failure: Option<AcquisitionFailure>,
    cancel: &CancellationToken,
    started: Instant,
    abort: &mut impl FnMut() -> Result<()>,
) -> DuplexOutput<D> {
    let mut cleanup_failures = CleanupFailures::default();
    if let Some(failure) = &launch_failure {
        cleanup_failures.retain(&failure.clone().into());
    }
    let mut cancelled = cancel.is_cancelled();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if let Err(failure) = abort() {
            cleanup_failures.retain(&failure);
            append_error(&mut error, format!("abort owned environment: {failure:#}"));
        }
        cancelled |= cancel.is_cancelled();
        if cancelled {
            driver.cancel();
        }
    }));
    if let Err(panic) = result {
        cleanup_failures.resume_unwind(panic);
    }
    DuplexOutput {
        acquisition_started: launch_failure
            .as_ref()
            .is_some_and(|failure| failure.launch() == theseus_host::process::LaunchPhase::MayRun),
        launch_failure,
        cleanup_failures,
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
    abort: impl FnMut() -> Result<()>,
) -> Result<DuplexOutput<D>> {
    run_duplex_with(
        command,
        timeout,
        cancel,
        files,
        stdout_limit,
        stderr_limit,
        driver,
        abort,
        |command| HostProcess::spawn_with_stdin(command, StdinMode::Piped),
    )
}

#[allow(clippy::too_many_arguments)]
fn run_duplex_with<D: DuplexDriver>(
    command: &mut Command,
    timeout: Duration,
    cancel: &CancellationToken,
    files: Option<(File, File)>,
    stdout_limit: usize,
    stderr_limit: usize,
    driver: D,
    mut abort: impl FnMut() -> Result<()>,
    acquire: impl FnOnce(&Command) -> std::result::Result<HostProcess, SpawnFailure>,
) -> Result<DuplexOutput<D>> {
    let started = Instant::now();
    if cancel.is_cancelled() {
        return Ok(unstarted(driver, None, None, cancel, started, &mut abort));
    }
    let mut process = match acquire(command) {
        Ok(process) => process,
        Err(error) => {
            let failure =
                AcquisitionFailure::from_spawn(error, Instant::now() + Duration::from_secs(3));
            return Ok(unstarted(
                driver,
                Some(format!("spawn Docker CLI: {failure:#}")),
                Some(failure),
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
                let setup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    setup_failure(
                        &mut process,
                        &mut abort,
                        format!("start stderr worker: {failure}"),
                    )
                }));
                let mut retained = CleanupFailures::default();
                let mut error = None;
                if let Ok(failure) = &setup {
                    retained.retain(failure);
                    append_error(&mut error, format!("{failure:#}"));
                }
                match stdout.join() {
                    Ok(acquired) => {
                        if let Err(failure) = acquired.cleanup.finish() {
                            retained.retain(&failure);
                        }
                        if let Some(failure) = acquired.error {
                            append_error(&mut error, failure);
                        }
                    }
                    Err(payload) => {
                        retained.retain(&worker_panic(payload));
                        append_error(&mut error, "duplex stdout worker panicked".into());
                    }
                }
                if let Err(panic) = setup {
                    retained.resume_unwind(panic);
                }
                retained.finish()?;
                return Err(AcquisitionFailure::after_launch(error.unwrap()).into());
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
        let mut retained = CleanupFailures::default();
        if let Err(error) = completion.cleanup.finish() {
            retained.retain(&error);
        }
        let mut io_error = None;
        let stdout = match stdout {
            Ok(mut acquired) => {
                if let Err(error) = std::mem::take(&mut acquired.cleanup).finish() {
                    retained.retain(&error);
                }
                if let Some(error) = &acquired.error {
                    append_error(&mut io_error, error.clone());
                }
                Some(acquired)
            }
            Err(payload) => {
                retained.retain(&worker_panic(payload));
                append_error(&mut io_error, "duplex stdout worker panicked".into());
                None
            }
        };
        let stderr = match stderr {
            Ok(Ok(drain)) => drain,
            Ok(Err(failure)) => {
                retained.retain(&failure);
                append_error(&mut io_error, format!("stderr drain: {failure:#}"));
                Drain::empty()
            }
            Err(payload) => {
                retained.retain(&worker_panic(payload));
                append_error(&mut io_error, "stderr worker panicked".into());
                Drain::empty()
            }
        };
        if let Some(panic) = completion.panic {
            retained.resume_unwind(panic);
        }
        retained.finish().map_err(|error| {
            error.context(AcquisitionFailure::after_launch(
                "duplex worker cleanup".into(),
            ))
        })?;
        let mut process_error = completion.error;
        if let Some(error) = &io_error {
            append_error(&mut process_error, error.clone());
        }
        let stdout = stdout
            .ok_or_else(|| AcquisitionFailure::after_launch(process_error.clone().unwrap()))?;
        Ok(DuplexOutput {
            acquisition_started: true,
            launch_failure: None,
            cleanup_failures: CleanupFailures::default(),
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
    use super::super::test_support::fixture;
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
    fn fix_round_one_failed_acquisition_observes_cancellation_requested_by_abort() {
        let cancel = CancellationToken::default();
        let out = run_duplex(
            &mut Command::new("/missing-duplex-executable"),
            Duration::from_secs(3),
            &cancel,
            None,
            32,
            32,
            OneRound {
                write: b"PING\n".to_vec(),
                ..Default::default()
            },
            || {
                cancel.cancel();
                Ok(())
            },
        )
        .unwrap();
        assert!(!out.acquisition_started);
        assert!(
            out.process.cancelled,
            "abort requested cancellation after acquisition failed"
        );
        assert_eq!(
            out.driver.offset, 5,
            "driver must receive cancellation after abort"
        );
        assert!(!out.stdout_eof);
    }

    #[test]
    fn duplex_closes_stdin_after_reply_and_joins_both_streams() {
        let mut command = fixture("duplex-case", &["ping"]);
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
    #[cfg(target_os = "linux")]
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

    #[test]
    fn actual_native_peer_completes_mcp_session_and_observes_stdin_eof() {
        use crate::mcp::{McpProbe, McpProtocol, McpSession, McpShutdown};
        let session = McpSession::new(McpProbe {
            argv: vec!["/fixture".into()],
            env: Default::default(),
            timeout_seconds: 3,
            protocol: McpProtocol::Modern20260728,
            server_name: "fixture".into(),
            server_version: "2".into(),
            tools: vec!["hello".into()],
        })
        .unwrap();
        let mut aborted = false;
        let output = run_duplex(
            &mut fixture("duplex-case", &["mcp-modern"]),
            Duration::from_secs(3),
            &CancellationToken::default(),
            None,
            4096,
            4096,
            session,
            || {
                aborted = true;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(output.process.status, Some(0), "{:?}", output.process.error);
        assert!(!aborted && !output.forced_abort && output.stdin_closed && output.stdout_eof);
        assert!(output.io_error.is_none());
        let evidence = output.driver.finish(McpShutdown::Graceful);
        assert!(evidence.protocol_complete, "{:?}", evidence.issue);
        assert_eq!(evidence.sent_ids.len(), 2);
    }

    fn heartbeat_advances(
        path: &std::path::Path,
        before: u64,
        deadline: Instant,
    ) -> std::io::Result<bool> {
        while Instant::now() < deadline {
            if std::fs::metadata(path)?.len() > before {
                return Ok(true);
            }
            thread::sleep(
                Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
        Ok(false)
    }
    #[test]
    fn heartbeat_oracle_accepts_live_growth_and_rejects_retained_dead_child() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let ready = temp.path().join("ready");
        let heartbeat = temp.path().join("heartbeat");
        let mut command = fixture("ready-hold", &[]);
        command.arg(&ready).arg(&heartbeat).arg("30000");
        let mut child = HostProcess::spawn(&command)?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while !ready.exists() {
            anyhow::ensure!(
                Instant::now() < deadline,
                "native heartbeat fixture not ready"
            );
            thread::sleep(Duration::from_millis(5));
        }
        let before = std::fs::metadata(&heartbeat)?.len();
        let live = heartbeat_advances(&heartbeat, before, deadline)?;
        let expired_baseline = std::fs::metadata(&heartbeat)?.len();
        let expired = heartbeat_advances(&heartbeat, expired_baseline, Instant::now())?;
        child.terminate()?;
        while !child.exited_without_reaping()? {
            anyhow::ensure!(Instant::now() < deadline, "owned native child did not exit");
            thread::sleep(Duration::from_millis(5));
        }
        // Only sample the negative baseline after confirmed reserved exit.
        let stopped_baseline = std::fs::metadata(&heartbeat)?.len();
        let stopped = heartbeat_advances(
            &heartbeat,
            stopped_baseline,
            Instant::now() + Duration::from_millis(80),
        )?;
        let retained = child.child.is_some();
        child.reap_and_disarm()?;
        assert!(
            live,
            "same oracle must witness growth from a live native child"
        );
        assert!(
            !expired,
            "an expired observation deadline must not be restarted"
        );
        assert!(
            retained && !stopped,
            "same oracle must reject an exited child while identity is retained"
        );
        Ok(())
    }

    // A blocking stdin write, or treating forced termination as exit 0, breaks this.
    #[test]
    fn unread_stdin_is_bounded_and_aborts_before_group_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let pid_file = temp.path().join("leader");
        let heartbeat = temp.path().join("heartbeat");
        let mut command = fixture("ready-hold", &[]);
        command.arg(&pid_file).arg(&heartbeat).arg("30000");
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
                let before = std::fs::metadata(&heartbeat)?.len();
                assert!(heartbeat_advances(
                    &heartbeat,
                    before,
                    began + Duration::from_millis(1850)
                )?);
                // The fixture writes its complete PID before the first heartbeat.
                leader = std::fs::read_to_string(&pid_file)?.parse::<u32>()?;
                assert!(leader > 0);
                #[cfg(target_os = "linux")]
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
        let after = std::fs::metadata(&heartbeat).unwrap().len();
        thread::sleep(Duration::from_millis(40));
        assert_eq!(std::fs::metadata(&heartbeat).unwrap().len(), after);
        #[cfg(target_os = "linux")]
        assert!(!std::path::Path::new(&format!("/proc/{leader}")).exists());
    }
    #[test]
    fn ignored_eof_gets_one_second_grace_and_unknown_status() {
        let began = Instant::now();
        let mut aborted = false;
        let out = run_duplex(
            &mut fixture("duplex-case", &["reply-hold"]),
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
            &mut fixture("flood-duplex", &["65536"]),
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
            &mut fixture("exit", &["23"]),
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
    #[cfg(target_os = "linux")]
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
            &mut fixture("hold-pipes", &["30000"]),
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
            &mut fixture("hold-pipes", &["30000"]),
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
        let stdout = super::super::test_support::failing_log();
        let stderr = super::super::test_support::failing_log();
        let out = run_duplex(
            &mut fixture("duplex-case", &["output"]),
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
            &mut fixture("duplex-case", &["stdout-eof-hold"]),
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
        let stderr = super::super::test_support::failing_log();
        let began = Instant::now();
        let mut aborted = false;
        let error = run_duplex(
            &mut fixture("duplex-case", &["output-hold"]),
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
            &mut fixture("duplex-case", &["reply-delay-eof"]),
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
            &mut fixture("duplex-case", &["blocked-read"]),
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
            &mut fixture("duplex-case", &["reply-hold"]),
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
                &mut fixture("hold-pipes", &["30000"]),
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
        let out = run_duplex(
            &mut fixture("duplex-case", &["notify"]),
            Duration::from_secs(3),
            &cancel,
            None,
            32,
            32,
            NotifyCancel {
                offset: 0,
                cancelled: false,
            },
            || Ok(()),
        )
        .unwrap();
        cancellation.join().unwrap();
        assert_eq!(out.process.status, Some(7));
        assert_eq!(out.driver.offset, 5);
        assert!(out.process.cancelled && out.stdout_eof && !out.forced_abort);
    }
    struct SetupReady(std::sync::mpsc::Sender<()>);
    impl DuplexDriver for SetupReady {
        fn pending_write(&self) -> &[u8] {
            &[]
        }
        fn written(&mut self, _: usize) -> Result<()> {
            Ok(())
        }
        fn receive(&mut self, _: &[u8]) {
            let _ = self.0.send(());
        }
        fn eof(&mut self) {}
        fn cancel(&mut self) {}
        fn wants_close(&self) -> bool {
            false
        }
    }

    #[test]
    fn second_worker_setup_failure_releases_stdout_before_abort_panic() {
        second_worker_setup_failure(true);
    }

    #[test]
    fn second_worker_setup_failure_preserves_abort_error_and_reaps_host() {
        second_worker_setup_failure(false);
    }

    fn second_worker_setup_failure(panic_in_abort: bool) {
        let temp = tempfile::tempdir().unwrap();
        let ready_path = temp.path().join("ready");
        let heartbeat = temp.path().join("heartbeat");
        let mut leader = 0;
        let mut released_before_abort = false;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // This owned inert child holds stdout independently of stdin EOF.
            // Its lifetime is ended only by HostProcess cleanup, never a timer.
            let mut process = HostProcess::spawn_with_stdin(
                Command::new(
                    std::env::var_os("THESEUS_HOST_TEST_FIXTURE")
                        .expect("build trusted host fixture and set THESEUS_HOST_TEST_FIXTURE"),
                )
                .arg("ready-hold")
                .arg(&ready_path)
                .arg(&heartbeat)
                .arg("30000"),
                StdinMode::Piped,
            )
            .unwrap();
            let growth_deadline = process.started + Duration::from_secs(2);
            let (input, output, _stderr) = configure_pipes(&mut process, &mut || Ok(())).unwrap();
            let stop = process.stop.clone();
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            thread::scope(|scope| {
                let worker_stop = stop.clone();
                scope.spawn(move || {
                    let acquired = exchange(
                        input,
                        output,
                        SetupReady(ready_tx),
                        None,
                        32,
                        worker_stop,
                        Arc::new(Signals::default()),
                    );
                    done_tx.send((acquired.stdout_eof, acquired.error)).unwrap();
                });
                // Prove the real exchange worker has read from the live child
                // before injecting the second spawn's setup_failure seam.
                let ready = ready_rx.recv_timeout(Duration::from_secs(2));
                if ready.is_err() {
                    stop.store(true, Ordering::SeqCst);
                }
                ready.unwrap();
                leader = std::fs::read_to_string(&ready_path)
                    .unwrap()
                    .parse()
                    .unwrap();
                setup_failure(
                    &mut process,
                    &mut || {
                        let done = done_rx.recv_timeout(Duration::from_secs(2));
                        released_before_abort = done.is_ok();
                        // A bounded test-only rescue permits a failing RED run
                        // to join safely without global limits or PID signals.
                        if !released_before_abort {
                            stop.store(true, Ordering::SeqCst);
                        }
                        if let Ok((eof, error)) = done {
                            assert!(!eof && error.is_none(), "stop must release quiet stdout");
                        }
                        let before = std::fs::metadata(&heartbeat)?.len();
                        assert!(
                            heartbeat_advances(&heartbeat, before, growth_deadline)?,
                            "native fixture must remain alive during guest abort"
                        );
                        #[cfg(target_os = "linux")]
                        {
                            let stat = std::fs::read_to_string(format!("/proc/{leader}/stat"))?;
                            let state = stat.rsplit_once(')').unwrap().1.trim_start();
                            assert!(
                                !state.starts_with('Z'),
                                "host must remain alive during abort"
                            );
                        }
                        if panic_in_abort {
                            panic!("setup abort panic");
                        }
                        anyhow::bail!("setup owned cleanup refused")
                    },
                    "start stderr worker: injected spawn failure".into(),
                )
            })
        }));
        let after = std::fs::metadata(&heartbeat).unwrap().len();
        thread::sleep(Duration::from_millis(40));
        assert_eq!(
            std::fs::metadata(&heartbeat).unwrap().len(),
            after,
            "owned native fixture must stop after cleanup"
        );
        #[cfg(target_os = "linux")]
        assert!(!std::path::Path::new(&format!("/proc/{leader}")).exists());
        eprintln!(
            "setup seam panic={panic_in_abort} owned_leader={leader} reaped=true worker_released_before_abort={released_before_abort}"
        );
        if panic_in_abort {
            let panic = result.expect_err("abort panic must propagate");
            assert_eq!(panic.downcast_ref::<&str>(), Some(&"setup abort panic"));
        } else {
            let error = result.unwrap().to_string();
            assert!(error.contains("start stderr worker: injected spawn failure"));
            assert!(error.contains("abort owned environment: setup owned cleanup refused"));
        }
        assert!(
            released_before_abort,
            "setup abort waited for the stdout worker rescue"
        );
    }

    #[test]
    fn pipe_setup_failure_aborts_while_leader_is_reserved() {
        let temp = tempfile::tempdir().unwrap();
        let ready = temp.path().join("ready");
        let heartbeat = temp.path().join("heartbeat");
        let mut command = Command::new(
            std::env::var_os("THESEUS_HOST_TEST_FIXTURE")
                .expect("build trusted host fixture and set THESEUS_HOST_TEST_FIXTURE"),
        );
        command
            .arg("ready-hold")
            .arg(&ready)
            .arg(&heartbeat)
            .arg("30000");
        let mut process = HostProcess::spawn(&command).unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        while !ready.exists() {
            assert!(Instant::now() < until);
            thread::sleep(Duration::from_millis(1));
        }
        let mut leader = 0u32;
        let mut aborted = false;
        let error = configure_pipes(&mut process, &mut || {
            let before = std::fs::metadata(&heartbeat)?.len();
            assert!(heartbeat_advances(&heartbeat, before, until)?);
            // Existence alone can expose the PID file before its write completes.
            leader = std::fs::read_to_string(&ready)?.parse::<u32>()?;
            assert!(leader > 0);
            #[cfg(target_os = "linux")]
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
        let stopped = std::fs::metadata(&heartbeat).unwrap().len();
        thread::sleep(Duration::from_millis(40));
        assert_eq!(std::fs::metadata(&heartbeat).unwrap().len(), stopped);
        #[cfg(target_os = "linux")]
        assert!(!std::path::Path::new(&format!("/proc/{leader}")).exists());
    }
    // Publish the final failure only when the monitor observes worker completion:
    // its first failure sample has already happened, so accepting completion here
    // would bypass owned abort and kill the local descriptor holder first.
    #[test]
    fn final_worker_failure_aborts_before_cleanup_after_completion_observation() {
        let temp = tempfile::tempdir().unwrap();
        let identities = temp.path().join("identities");
        let heartbeat = temp.path().join("heartbeat");
        let mut command = fixture("tree-exit", &[]);
        command.arg(&identities).arg(&heartbeat);
        let mut process = HostProcess::spawn(&command).unwrap();
        let began = Instant::now();
        let growth_deadline = process.started + Duration::from_secs(3);
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
        assert!(leader > 0 && holder > 0 && leader != holder);
        #[cfg(target_os = "linux")]
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
                    let before = std::fs::metadata(&heartbeat)?.len();
                    assert!(
                        heartbeat_advances(&heartbeat, before, growth_deadline)?,
                        "owned native descendant must remain alive through guest abort"
                    );
                    #[cfg(target_os = "linux")]
                    {
                        assert_eq!(state(leader), Some('Z'), "leader must remain reserved");
                        assert!(
                            state(holder).is_some_and(|state| !matches!(state, 'Z' | 'X')),
                            "local group must remain alive until owned abort"
                        );
                    }
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
        let stopped = std::fs::metadata(&heartbeat).unwrap().len();
        thread::sleep(Duration::from_millis(40));
        assert_eq!(std::fs::metadata(&heartbeat).unwrap().len(), stopped);
        #[cfg(target_os = "linux")]
        {
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
            let mut command = super::super::test_support::fixture("duplex-case", &["reply-hold"]);
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

#[cfg(test)]
mod pending_write_regressions {
    use super::*;
    use std::{collections::VecDeque, sync::Mutex};

    #[derive(Default)]
    struct Schedule {
        prefix: Option<Vec<u8>>,
        complete: bool,
        submissions: Vec<Vec<u8>>,
        replies: VecDeque<Vec<u8>>,
        reads: usize,
        cancel_on_first_read: bool,
        never_complete: bool,
        elapsed: Duration,
        closed_at: Option<Duration>,
        closed: bool,
    }
    struct DelayedWrite(Arc<Mutex<Schedule>>);
    struct ScheduledRead(Arc<Mutex<Schedule>>, Arc<Signals>);
    impl ExchangeWrite for DelayedWrite {
        fn retain(self) -> theseus_host::process::OwnedSpawnCleanup {
            theseus_host::process::OwnedSpawnCleanup::from_endpoints(vec![], vec![])
        }
        fn try_write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let mut state = self.0.lock().unwrap();
            if let Some(prefix) = &state.prefix {
                if prefix != bytes {
                    return Err(std::io::Error::other("retry replaced in-flight prefix"));
                }
                if !state.complete {
                    return Err(std::io::ErrorKind::WouldBlock.into());
                }
                let count = prefix.len();
                state.prefix = None;
                state.complete = false;
                return Ok(count);
            }
            state.prefix = Some(bytes.to_vec());
            state.submissions.push(bytes.to_vec());
            Err(std::io::ErrorKind::WouldBlock.into())
        }
        fn close(&mut self) -> std::io::Result<()> {
            let mut state = self.0.lock().unwrap();
            state.prefix = None;
            state.closed = true;
            state.closed_at = Some(state.elapsed);
            Ok(())
        }
    }
    impl ExchangeRead for ScheduledRead {
        fn cancel_and_complete(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn retain(self) -> theseus_host::process::OwnedSpawnCleanup {
            theseus_host::process::OwnedSpawnCleanup::from_endpoints(vec![], vec![])
        }
        fn try_read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let mut state = self.0.lock().unwrap();
            state.reads += 1;
            state.elapsed += Duration::from_millis(20);
            assert!(state.reads < 20, "exchange failed to make bounded progress");
            if state.cancel_on_first_read && state.reads == 1 {
                self.1.cancel.store(true, Ordering::SeqCst);
            }
            if state.prefix.is_some() {
                // The peer has consumed the native bytes and can already reply,
                // but the exchange has not acknowledged this write to its driver.
                state.complete = !state.never_complete;
                if let Some(reply) = state.replies.pop_front() {
                    out[..reply.len()].copy_from_slice(&reply);
                    return Ok(reply.len());
                }
                return Err(std::io::ErrorKind::WouldBlock.into());
            }
            if state.closed || state.replies.is_empty() {
                Ok(0)
            } else {
                Err(std::io::ErrorKind::WouldBlock.into())
            }
        }
    }
    fn scheduled<D: DuplexDriver>(driver: D, schedule: Schedule) -> (Acquired<D>, Vec<Vec<u8>>) {
        let state = Arc::new(Mutex::new(schedule));
        let signals = Arc::new(Signals::default());
        let acquired = exchange(
            DelayedWrite(state.clone()),
            ScheduledRead(state.clone(), signals.clone()),
            driver,
            None,
            4096,
            Arc::new(AtomicBool::new(false)),
            signals,
        );
        let writes = state.lock().unwrap().submissions.clone();
        (acquired, writes)
    }
    #[test]
    fn abandoned_write_and_cancel_frame_share_one_request_clock() {
        let state = Arc::new(Mutex::new(Schedule {
            cancel_on_first_read: true,
            never_complete: true,
            ..Default::default()
        }));
        let signals = Arc::new(Signals::default());
        let start = Instant::now();
        let acquired = exchange_with_clock(
            DelayedWrite(state.clone()),
            ScheduledRead(state.clone(), signals.clone()),
            ReplaceFrame::new(false),
            None,
            8,
            Arc::new(AtomicBool::new(false)),
            signals,
            || start + state.lock().unwrap().elapsed,
        );
        let state = state.lock().unwrap();
        assert_eq!(
            state.closed_at,
            Some(Duration::from_millis(120)),
            "request observed at 20ms; old write settlement and cancel flush share 100ms"
        );
        assert_eq!(state.submissions, vec![b"old\n".to_vec()]);
        assert!(
            acquired.driver.acknowledgements.is_empty(),
            "abandoned bytes must not be acknowledged as cancellation"
        );
        assert!(acquired.stdout_eof);
    }

    #[test]
    fn pending_write_keeps_read_progress_and_bounded_capture() {
        let state = Arc::new(Mutex::new(Schedule {
            never_complete: true,
            replies: VecDeque::from([b"12345".to_vec(), b"67890".to_vec()]),
            ..Default::default()
        }));
        let signals = Arc::new(Signals::default());
        let acquired = exchange(
            DelayedWrite(state.clone()),
            ScheduledRead(state.clone(), signals.clone()),
            ReplaceFrame::new(true),
            None,
            8,
            Arc::new(AtomicBool::new(false)),
            signals,
        );
        assert_eq!(acquired.output.bytes, b"12345678");
        assert!(acquired.output.truncated);
        assert_eq!(acquired.stdout_bytes, 10);
        assert!(acquired.driver.acknowledgements.is_empty());
        assert!(
            state.lock().unwrap().closed,
            "bounded capture closes an unresponsive writer"
        );
    }

    #[test]
    fn response_after_native_completion_before_ack_is_not_an_unsent_mcp_id() {
        use crate::mcp::{McpProbe, McpProtocol, McpSession, McpShutdown};
        let session = McpSession::new(McpProbe {
            argv: vec!["/fixture".into()],
            env: Default::default(),
            timeout_seconds: 3,
            protocol: McpProtocol::Modern20260728,
            server_name: "fixture".into(),
            server_version: "2".into(),
            tools: vec!["hello".into()],
        })
        .unwrap();
        let replies = VecDeque::from([
            br#"{"jsonrpc":"2.0","id":"theseus-discover-1","result":{"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"public","_meta":{"io.modelcontextprotocol/serverInfo":{"name":"fixture","version":"2"}}}}
"#.to_vec(),
            br#"{"jsonrpc":"2.0","id":1,"result":{"resultType":"complete","tools":[{"name":"hello","inputSchema":{"type":"object"}}],"ttlMs":0,"cacheScope":"public"}}
"#.to_vec(),
        ]);
        let (acquired, writes) = scheduled(
            session,
            Schedule {
                replies,
                ..Default::default()
            },
        );
        assert!(acquired.error.is_none(), "{:?}", acquired.error);
        let evidence = acquired.driver.finish(McpShutdown::Graceful);
        assert!(evidence.protocol_complete, "{:?}", evidence.issue);
        assert_eq!(evidence.sent_ids.len(), 2);
        assert_eq!(writes.len(), 2);
    }

    #[test]
    fn invalid_reply_clearing_pending_does_not_erase_or_reassign_native_ack() {
        use crate::mcp::{McpIssueKind, McpProbe, McpProtocol, McpSession, McpShutdown};
        for (reply, message) in [
            (b"not JSON\n".to_vec(), "invalid JSON"),
            (
                br#"{"jsonrpc":"2.0","id":999,"result":{}}
"#
                .to_vec(),
                "unmatched or premature RPC response ID",
            ),
        ] {
            let session = McpSession::new(McpProbe {
                argv: vec!["/fixture".into()],
                env: Default::default(),
                timeout_seconds: 3,
                protocol: McpProtocol::Modern20260728,
                server_name: "fixture".into(),
                server_version: "2".into(),
                tools: vec!["hello".into()],
            })
            .unwrap();
            let (acquired, writes) = scheduled(
                session,
                Schedule {
                    replies: VecDeque::from([reply]),
                    ..Default::default()
                },
            );
            assert!(acquired.error.is_none(), "{:?}", acquired.error);
            let evidence = acquired.driver.finish(McpShutdown::Graceful);
            assert!(!evidence.protocol_complete);
            let issue = evidence.issue.unwrap();
            assert_eq!(issue.kind, McpIssueKind::Protocol);
            assert!(issue.message.contains(message), "{}", issue.message);
            assert_eq!(
                evidence.sent_ids.len(),
                1,
                "only the actually completed request is sent"
            );
            assert_eq!(writes.len(), 1);
        }
    }

    struct ReplaceFrame {
        pending: Vec<u8>,
        offset: usize,
        acknowledgements: Vec<(Vec<u8>, usize)>,
        replace_on_receive: bool,
        mutated_before_ack: bool,
    }
    impl ReplaceFrame {
        fn new(replace_on_receive: bool) -> Self {
            Self {
                pending: b"old\n".to_vec(),
                offset: 0,
                acknowledgements: vec![],
                replace_on_receive,
                mutated_before_ack: false,
            }
        }
        fn replace(&mut self, bytes: &[u8]) {
            self.mutated_before_ack |= self.offset != self.pending.len();
            self.pending = bytes.to_vec();
            self.offset = 0;
        }
    }
    impl DuplexDriver for ReplaceFrame {
        fn pending_write(&self) -> &[u8] {
            &self.pending[self.offset..]
        }
        fn written(&mut self, count: usize) -> Result<()> {
            self.acknowledgements.push((self.pending.clone(), count));
            self.offset += count;
            Ok(())
        }
        fn receive(&mut self, _: &[u8]) {
            if self.replace_on_receive {
                self.replace(b"new\n");
                self.replace_on_receive = false;
            }
        }
        fn eof(&mut self) {}
        fn cancel(&mut self) {
            self.replace(b"cancel\n");
        }
        fn wants_close(&self) -> bool {
            false
        }
    }
    #[test]
    fn cancellation_cannot_acknowledge_old_native_bytes_as_cancel_frame() {
        let (acquired, _) = scheduled(
            ReplaceFrame::new(false),
            Schedule {
                cancel_on_first_read: true,
                ..Default::default()
            },
        );
        assert!(acquired.error.is_none(), "{:?}", acquired.error);
        assert!(!acquired.driver.mutated_before_ack);
        assert_eq!(
            acquired.driver.acknowledgements,
            vec![(b"old\n".to_vec(), 4), (b"cancel\n".to_vec(), 7)]
        );
    }
    #[test]
    fn receive_replacing_pending_frame_preserves_exact_ack_binding() {
        let (acquired, _) = scheduled(
            ReplaceFrame::new(true),
            Schedule {
                replies: VecDeque::from([b"reply".to_vec()]),
                ..Default::default()
            },
        );
        assert!(acquired.error.is_none(), "{:?}", acquired.error);
        assert!(!acquired.driver.mutated_before_ack);
        assert_eq!(
            acquired.driver.acknowledgements,
            vec![(b"old\n".to_vec(), 4), (b"new\n".to_vec(), 4)]
        );
    }
}

#[cfg(all(test, windows))]
mod native_launch_failures {
    use super::*;
    use theseus_host::process::{
        HostCleanup, LaunchPhase,
        test_support::{self, LaunchFault},
    };
    #[derive(Default)]
    struct Idle {
        cancelled: bool,
    }
    impl DuplexDriver for Idle {
        fn pending_write(&self) -> &[u8] {
            &[]
        }
        fn written(&mut self, _: usize) -> Result<()> {
            Ok(())
        }
        fn receive(&mut self, _: &[u8]) {}
        fn eof(&mut self) {}
        fn cancel(&mut self) {
            self.cancelled = true;
        }
        fn wants_close(&self) -> bool {
            false
        }
    }
    #[test]
    fn actual_native_failed_launch_preserves_phase_owner_and_no_execution_before_resume() {
        for (fault, expected, pending) in [
            (LaunchFault::BeforeCreate, LaunchPhase::NeverCreated, false),
            (
                LaunchFault::CreatedSuspended,
                LaunchPhase::CreatedSuspended,
                false,
            ),
            (LaunchFault::ResumeUncertain, LaunchPhase::MayRun, false),
            (
                LaunchFault::CleanupPending,
                LaunchPhase::CreatedSuspended,
                true,
            ),
            (
                LaunchFault::BeforeCreatePending,
                LaunchPhase::NeverCreated,
                true,
            ),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let marker = temp.path().join("ran");
            let mut command = super::super::test_support::fixture("launch-marker", &[]);
            command.arg(&marker);
            let mut aborted = 0;
            let cancel = CancellationToken::default();
            let cleanup_deadline = Instant::now() + Duration::from_secs(3);
            let output = run_duplex_with(
                &mut command,
                Duration::from_secs(3),
                &cancel,
                None,
                32,
                32,
                Idle::default(),
                || {
                    aborted += 1;
                    cancel.cancel();
                    Ok(())
                },
                |command| {
                    test_support::spawn(command, InputMode::Pipe, fault)
                        .map(|child| HostProcess::acquired(child, Instant::now()))
                        .map_err(|mut failure| {
                            if pending {
                                let HostCleanup::Pending(owner) = &mut failure.cleanup else {
                                    panic!("native pending fault lost actual cleanup owner")
                                };
                                test_support::fail_cleanup_attempts(owner, 4);
                            }
                            failure
                        })
                },
            )
            .unwrap();
            assert_eq!(aborted, 1, "guest preparation always requires owned abort");
            assert!(
                output.process.cancelled && output.driver.cancelled,
                "native failed-acquisition abort cancellation reaches both output and driver"
            );
            assert_eq!(output.acquisition_started, expected == LaunchPhase::MayRun);
            let failure = output.launch_failure.unwrap();
            assert_eq!(failure.launch(), expected);
            if expected != LaunchPhase::MayRun {
                assert!(!marker.exists());
            }
            eprintln!(
                "native failure phase={expected:?} injected_pending={pending} initial_pending={}",
                failure.pending()
            );
            if pending {
                assert!(
                    failure.pending(),
                    "injected pending fault must retain actual authority"
                );
                assert!(failure.retry_cleanup().is_err());
                assert!(failure.pending());
                assert!(!failure.retry_cleanup().unwrap());
                assert!(failure.pending());
                let mut retained = CleanupFailures::default();
                retained.retain(&anyhow::Error::new(failure.clone()));
                retained.retain(&anyhow::Error::new(failure.clone()));
                let error = retained
                    .finish()
                    .expect_err("injected native cleanup still pending");
                assert!(error.downcast_ref::<CleanupFailures>().is_some());
                super::super::release_test_fault(&failure);
            }
            // Ordinary eager cleanup can still be Pending on return: termination
            // requests are asynchronous. Observe real completion using one clock.
            while !failure.retry_cleanup().unwrap() {
                assert!(Instant::now() < cleanup_deadline);
                thread::sleep(Duration::from_millis(1));
            }
            assert!(!failure.pending());
            if expected != LaunchPhase::MayRun {
                assert!(
                    !marker.exists(),
                    "pre-resume fixture must never execute, including after cleanup"
                );
            }
        }
    }
}

#[cfg(test)]
mod cleanup_clock_tests {
    use super::*;
    use std::cell::Cell;
    #[test]
    fn expired_exchange_still_starts_independent_cleanup_clock_without_retry_reset() {
        let command = super::super::test_support::fixture("exit", &["0"]);
        let mut process = HostProcess::spawn(&command).unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        while !process.exited_without_reaping().unwrap() {
            assert!(Instant::now() < until);
            thread::sleep(Duration::from_millis(1));
        }
        let now = Cell::new(process.started + Duration::from_secs(9));
        let expected = now.get() + Duration::from_secs(3);
        let completion = process.monitor_with_clock(
            Duration::from_secs(2),
            &CancellationToken::default(),
            || false,
            || false,
            MonitorPolicy::Duplex(&Signals::default()),
            |event| {
                assert!(matches!(event, SupervisionEvent::Abort));
                now.set(now.get() + Duration::from_secs(10));
                Ok(SupervisionDecision::Continue)
            },
            || now.get(),
        );
        assert!(completion.timed_out);
        assert_eq!(process.cleanup_deadline, Some(expected));
        assert_eq!(
            process.begin_cleanup_at(now.get()),
            expected,
            "retry must not mint another cleanup budget"
        );
    }
    #[test]
    fn cancellation_during_cleanup_does_not_reclassify_prior_grace_decision_as_timeout() {
        let command = super::super::test_support::fixture("exit", &["0"]);
        let mut process = HostProcess::spawn(&command).unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        while !process.exited_without_reaping().unwrap() {
            assert!(Instant::now() < until);
            thread::sleep(Duration::from_millis(1));
        }
        let cancel = CancellationToken::default();
        let now = Cell::new(process.started + Duration::from_millis(100));
        let signals = Signals::default();
        signals.close.store(true, Ordering::SeqCst);
        let completion = process.monitor_with_clock(
            Duration::from_secs(5),
            &cancel,
            || {
                now.set(now.get() + Duration::from_millis(1100));
                false
            },
            || false,
            MonitorPolicy::Duplex(&signals),
            |event| {
                assert!(matches!(event, SupervisionEvent::Abort));
                now.set(now.get() + Duration::from_secs(20));
                cancel.cancel();
                Ok(SupervisionDecision::Continue)
            },
            || now.get(),
        );
        assert!(completion.cancelled);
        assert!(
            !completion.timed_out,
            "time spent in separately bounded cleanup is not execution time"
        );
        assert_eq!(
            process.cleanup_deadline,
            Some(process.started + Duration::from_millis(4200))
        );
    }
}
