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
mod tests;

#[cfg(test)]
mod review_regressions;

#[cfg(test)]
mod pending_write_regressions;

#[cfg(all(test, windows))]
mod native_launch_failures;

#[cfg(test)]
mod cleanup_clock_tests;
