//! Deadlines cover child processes and pipe lifetime, including inherited handles.
mod cleanup;
mod duplex;
use crate::{
    engine::CancellationToken,
    recipe::Limits,
    snapshot::{self, Snapshot},
};
use anyhow::{Context, Result, anyhow};
pub(crate) use cleanup::{AcquisitionFailure, CleanupFailures, worker_panic};
#[cfg(all(test, windows))]
pub(crate) use cleanup::{native_tests, release_test_fault};
pub(super) use duplex::{DuplexDriver, DuplexOutput, run_duplex};
use std::{
    fs::File,
    io::{Read, Write},
    process::ExitStatus,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use theseus_host::process::{
    InputMode, NativeCommand as Command, OwnedChild, PipeReader as NativeReader,
    PipeWriter as NativeWriter, SpawnFailure,
};

pub(super) enum SupervisionEvent {
    Poll,
    Abort,
}
pub(super) enum SupervisionDecision {
    Continue,
    Stop,
}

pub(super) struct Output {
    pub supervised_stop: bool,
    pub status: Option<i32>,
    pub timed_out: bool,
    pub cancelled: bool,
    pub stdout: Drain,
    pub stderr: Drain,
    pub duration_ms: u64,
    pub error: Option<String>,
}
pub(super) struct Drain {
    pub bytes: Vec<u8>,
    pub stored: u64,
    pub truncated: bool,
}
impl Drain {
    fn empty() -> Self {
        Self {
            bytes: vec![],
            stored: 0,
            truncated: false,
        }
    }
    fn record(&mut self, bytes: &[u8], file: &mut Option<File>, limit: usize) -> Result<()> {
        let keep = bytes.len().min(limit.saturating_sub(self.bytes.len()));
        self.truncated |= keep < bytes.len();
        self.bytes.extend_from_slice(&bytes[..keep]);
        self.stored += keep as u64;
        if keep > 0
            && let Some(writer) = file.as_mut()
            && let Err(failure) = writer.write_all(&bytes[..keep])
        {
            *file = None;
            return Err(failure.into());
        }
        Ok(())
    }
}

// A group is necessary to terminate owned CLI wrappers/helpers. Interruptible
// reads are also necessary: EOF is not guaranteed by killing the direct child or
// even its group (a descriptor could have been inherited by another group).
struct PipeReader {
    reader: Option<NativeReader>,
    stop: Arc<AtomicBool>,
}
impl PipeReader {
    fn new(reader: NativeReader, stop: Arc<AtomicBool>) -> Result<Self> {
        Ok(Self {
            reader: Some(reader),
            stop,
        })
    }
    fn read_failed(&mut self, error: std::io::Error) -> std::io::Error {
        if let Some(reader) = self.reader.as_mut()
            && let Err(cleanup_error) = reader.cancel_and_complete()
        {
            let owner = theseus_host::process::OwnedSpawnCleanup::from_endpoints(
                self.reader.take().into_iter().collect(),
                vec![],
            );
            return AcquisitionFailure::from_spawn(
                SpawnFailure {
                    launch: theseus_host::process::LaunchPhase::MayRun,
                    error,
                    cleanup_errors: vec![cleanup_error],
                    cleanup: theseus_host::process::HostCleanup::Pending(owner),
                },
                Instant::now(),
            )
            .into_io_error();
        }
        error
    }
}
impl Read for PipeReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if self.stop.load(Ordering::SeqCst) {
                if let Some(reader) = self.reader.as_mut()
                    && let Err(error) = reader.cancel_and_complete()
                {
                    let owner = theseus_host::process::OwnedSpawnCleanup::from_endpoints(
                        self.reader.take().into_iter().collect(),
                        vec![],
                    );
                    return Err(AcquisitionFailure::from_spawn(
                        SpawnFailure {
                            launch: theseus_host::process::LaunchPhase::MayRun,
                            error,
                            cleanup_errors: vec![],
                            cleanup: theseus_host::process::HostCleanup::Pending(owner),
                        },
                        Instant::now(),
                    )
                    .into_io_error());
                }
                return Ok(0);
            }
            match self
                .reader
                .as_mut()
                .expect("reader active until stop")
                .try_read(buffer)
            {
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(self.read_failed(error)),
                result => return result,
            }
        }
    }
}
enum StdinMode {
    Null,
    Piped,
}
enum MonitorPolicy<'a> {
    Immediate,
    Duplex(&'a duplex::Signals),
}
struct HostProcess {
    child: Option<OwnedChild>,
    stdin: Option<NativeWriter>,
    stdout: Option<NativeReader>,
    stderr: Option<NativeReader>,
    stop: Arc<AtomicBool>,
    started: Instant,
    cleanup_deadline: Option<Instant>,
}
struct Completion {
    panic: Option<Box<dyn std::any::Any + Send>>,
    cleanup: CleanupFailures,
    status: Option<ExitStatus>,
    timed_out: bool,
    cancelled: bool,
    supervised_stop: bool,
    error: Option<String>,
    duration_ms: u64,
}
impl HostProcess {
    fn spawn(command: &Command) -> Result<Self> {
        Self::spawn_with_stdin(command, StdinMode::Null).map_err(|failure| {
            anyhow::Error::new(AcquisitionFailure::from_spawn(
                failure,
                Instant::now() + Duration::from_secs(3),
            ))
            .context("spawn Docker CLI")
        })
    }
    fn spawn_with_stdin(
        command: &Command,
        stdin: StdinMode,
    ) -> std::result::Result<Self, SpawnFailure> {
        let started = Instant::now();
        let acquired = OwnedChild::spawn(
            command,
            match stdin {
                StdinMode::Null => InputMode::Null,
                StdinMode::Piped => InputMode::Pipe,
            },
        )?;
        Ok(Self::acquired(acquired, started))
    }
    fn acquired(acquired: theseus_host::process::AcquiredChild, started: Instant) -> Self {
        Self {
            child: Some(acquired.child),
            stdin: acquired.stdin,
            stdout: Some(acquired.stdout),
            stderr: Some(acquired.stderr),
            stop: Arc::new(AtomicBool::new(false)),
            started,
            cleanup_deadline: None,
        }
    }
    fn begin_cleanup(&mut self) -> Instant {
        self.begin_cleanup_at(Instant::now())
    }
    fn begin_cleanup_at(&mut self, now: Instant) -> Instant {
        *self
            .cleanup_deadline
            .get_or_insert(now + Duration::from_secs(3))
    }
    fn exited_without_reaping(&mut self) -> Result<bool> {
        Ok(self
            .child
            .as_mut()
            .context("owned child already released")?
            .exited_reserved()?)
    }
    fn terminate(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(child) = &mut self.child {
            child.terminate_owned()?;
        }
        Ok(())
    }
    fn reap_and_disarm(&mut self) -> Result<ExitStatus> {
        let deadline = self.begin_cleanup();
        loop {
            match self
                .child
                .as_mut()
                .context("owned child already released")?
                .try_reap_and_release()
            {
                Ok(Some(status)) => {
                    self.child.take();
                    return Ok(status);
                }
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(1)),
                result => {
                    let error = result.err().unwrap_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "owned host child cleanup incomplete",
                        )
                    });
                    let cleanup = self.child.take().unwrap().into_cleanup();
                    return Err(AcquisitionFailure::from_spawn(
                        SpawnFailure {
                            launch: theseus_host::process::LaunchPhase::MayRun,
                            error,
                            cleanup_errors: vec![],
                            cleanup: theseus_host::process::HostCleanup::Pending(cleanup),
                        },
                        deadline,
                    )
                    .into());
                }
            }
        }
    }
    fn monitor(
        &mut self,
        timeout: Duration,
        cancel: &CancellationToken,
        readers_done: impl Fn() -> bool,
        failed_reader: impl Fn() -> bool,
        policy: MonitorPolicy<'_>,
        control: impl FnMut(SupervisionEvent) -> Result<SupervisionDecision>,
    ) -> Completion {
        self.monitor_with_clock(
            timeout,
            cancel,
            readers_done,
            failed_reader,
            policy,
            control,
            Instant::now,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn monitor_with_clock(
        &mut self,
        timeout: Duration,
        cancel: &CancellationToken,
        readers_done: impl Fn() -> bool,
        failed_reader: impl Fn() -> bool,
        policy: MonitorPolicy<'_>,
        mut control: impl FnMut(SupervisionEvent) -> Result<SupervisionDecision>,
        now: impl Fn() -> Instant,
    ) -> Completion {
        let mut cleanup = CleanupFailures::default();
        let mut panic = None;
        let mut exited = false;
        let mut timed_out;
        let mut cancelled;
        let mut supervised_stop = false;
        let mut last_poll: Option<Instant> = None;
        let mut error = None;
        let mut closing_at = None;
        let mut exchange_expired = false;
        loop {
            if !exited {
                match self.exited_without_reaping() {
                    Ok(value) => exited = value,
                    Err(failure) => error = Some(format!("observe host child: {failure:#}")),
                }
            }
            cancelled = cancel.is_cancelled();
            timed_out = now().saturating_duration_since(self.started) >= timeout;
            let mut cancellation_requires_abort = cancelled;
            let mut grace_expired = false;
            if let MonitorPolicy::Duplex(signals) = &policy {
                if cancelled {
                    signals.cancel.store(true, Ordering::SeqCst);
                }
                if closing_at.is_none()
                    && !signals.close.load(Ordering::SeqCst)
                    && now().saturating_duration_since(self.started)
                        >= timeout.saturating_sub(Duration::from_secs(1))
                {
                    exchange_expired = true;
                    signals.close.store(true, Ordering::SeqCst);
                }
                if cancelled || signals.close.load(Ordering::SeqCst) {
                    let at = closing_at.get_or_insert_with(&now);
                    grace_expired = now().saturating_duration_since(*at) >= Duration::from_secs(1);
                }
                cancellation_requires_abort = cancelled && grace_expired;
                if signals.failed.load(Ordering::SeqCst) {
                    append_error(&mut error, "duplex worker failed".into());
                }
            }
            if !cancellation_requires_abort && !timed_out && !grace_expired && error.is_none() {
                // Parser rejection stops only acquisition, preserving the container.
                if failed_reader() {
                    break;
                }
                if exited && readers_done() {
                    // A worker can publish failure after the first flag sample
                    // and then finish. Recheck after observing completion while
                    // the leader is still reserved, before choosing normal exit.
                    if let MonitorPolicy::Duplex(signals) = &policy
                        && signals.failed.load(Ordering::SeqCst)
                    {
                        append_error(&mut error, "duplex worker failed".into());
                    } else {
                        break;
                    }
                }
                // Never turn an already-exited child into an intentional stop.
                // Slow control calls space their successors from completion too.
                if !exited
                    && last_poll.is_none_or(|at| {
                        now().saturating_duration_since(at) >= Duration::from_millis(50)
                    })
                {
                    let decision = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        control(SupervisionEvent::Poll)
                    }));
                    last_poll = Some(now());
                    cancelled = cancel.is_cancelled();
                    timed_out = now().saturating_duration_since(self.started) >= timeout;
                    if matches!(policy, MonitorPolicy::Immediate) {
                        cancellation_requires_abort = cancelled;
                    }
                    match decision {
                        Err(payload) => {
                            panic = Some(payload);
                            error = Some("owned environment supervisor panicked".into());
                        }
                        Ok(Err(failure)) => {
                            cleanup.retain(&failure);
                            error = Some(format!("supervise owned environment: {failure:#}"))
                        }
                        Ok(Ok(SupervisionDecision::Stop)) if !cancelled && !timed_out => {
                            supervised_stop = true;
                            break;
                        }
                        Ok(_) => {}
                    }
                }
            }
            if cancellation_requires_abort || timed_out || grace_expired || error.is_some() {
                // The execution deadline is judged at the abort decision; the
                // emergency cleanup that follows has its own budget.
                let decided = now();
                self.stop.store(true, Ordering::SeqCst);
                self.begin_cleanup_at(decided);
                // The exited leader stays unreaped throughout container abort,
                // so even its control-operation deadlines cannot expose reuse.
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    control(SupervisionEvent::Abort)
                })) {
                    Ok(Err(failure)) => {
                        cleanup.retain(&failure);
                        append_error(&mut error, format!("abort owned environment: {failure:#}"));
                    }
                    Err(payload) => {
                        // Keep the first panic's cause; a second abort panic must
                        // not unwind past owned IO before workers can be joined.
                        if panic.is_none() {
                            panic = Some(payload);
                        } else {
                            cleanup.retain(&worker_panic(payload));
                        }
                        append_error(&mut error, "owned environment abort panicked".into());
                    }
                    Ok(Ok(_)) => {}
                }
                cancelled |= cancel.is_cancelled();
                // Duplex abort owns a separate emergency cleanup budget. Its
                // elapsed time cannot change the execution/shutdown decision.
                if matches!(policy, MonitorPolicy::Immediate) {
                    timed_out |= decided.saturating_duration_since(self.started) >= timeout;
                }
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        // Normal completion, timeout/cancel, parser rejection and errors all share
        // the same ownership order: signal while reserved, reap, then disarm.
        if let Err(failure) = self.terminate() {
            append_error(
                &mut error,
                format!("terminate host process group: {failure:#}"),
            );
        }
        let status = match self.reap_and_disarm() {
            Ok(status) => Some(status),
            Err(failure) => {
                cleanup.retain(&failure);
                append_error(&mut error, format!("{failure:#}"));
                None
            }
        };
        Completion {
            panic,
            cleanup,
            status,
            timed_out: timed_out || exchange_expired,
            cancelled: cancelled || cancel.is_cancelled(),
            supervised_stop: supervised_stop && error.is_none(),
            error,
            duration_ms: now()
                .saturating_duration_since(self.started)
                .as_millis()
                .min(u64::MAX as u128) as u64,
        }
    }
}
impl Drop for HostProcess {
    fn drop(&mut self) {
        if self.child.is_some() {
            // Early setup/read errors retain the same signal-before-reap order.
            let _ = self.terminate();
            let _ = self.reap_and_disarm();
        }
    }
}

// The first scoped worker is explicitly joined even when starting its peer fails.
fn join_failed_setup<T>(
    setup: std::thread::Result<anyhow::Error>,
    worker: Result<T>,
) -> anyhow::Error {
    let mut cleanup = CleanupFailures::default();
    if let Err(error) = &worker {
        cleanup.retain(error);
    }
    let error = match setup {
        Ok(error) => {
            cleanup.retain(&error);
            error
        }
        Err(panic) => cleanup.resume_unwind(panic),
    };
    match cleanup.finish() {
        Ok(()) => error,
        Err(pending) => pending.context(format!("{error:#}")),
    }
}

fn failed_acquisition(error: anyhow::Error, abort: impl FnOnce() -> Result<()>) -> anyhow::Error {
    let mut cleanup = CleanupFailures::default();
    cleanup.retain(&error);
    let aborted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(abort));
    let error = match aborted {
        Err(panic) => cleanup.resume_unwind(panic),
        Ok(Ok(())) => error,
        Ok(Err(abort)) => {
            cleanup.retain(&abort);
            error.context(format!("abort owned environment: {abort:#}"))
        }
    };
    match cleanup.finish() {
        Ok(()) => error,
        Err(pending) => pending.context(format!("{error:#}")),
    }
}

fn setup_failure(
    process: &mut HostProcess,
    abort: &mut impl FnMut() -> Result<()>,
    message: String,
) -> anyhow::Error {
    // Scoped workers must be able to join even if owned abort unwinds.
    // Stop I/O first, but keep host signals/reaping after the owned abort.
    process.stop.store(true, Ordering::SeqCst);
    process.begin_cleanup();
    let mut error = Some(message);
    let mut cleanup = CleanupFailures::default();
    let abort_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(abort));
    if let Ok(Err(failure)) = &abort_result {
        cleanup.retain(failure);
        append_error(&mut error, format!("abort owned environment: {failure:#}"));
    }
    if let Err(failure) = process.terminate() {
        append_error(
            &mut error,
            format!("terminate host process group: {failure:#}"),
        );
    }
    if let Err(failure) = process.reap_and_disarm() {
        cleanup.retain(&failure);
        append_error(&mut error, format!("{failure:#}"));
    }
    if let Err(panic) = abort_result {
        cleanup.resume_unwind(panic);
    }
    let context = AcquisitionFailure::after_launch(error.unwrap());
    match cleanup.finish() {
        Ok(()) => context.into(),
        Err(error) => error.context(context),
    }
}
fn append_error(errors: &mut Option<String>, error: String) {
    match errors {
        Some(prior) => {
            prior.push_str("; ");
            prior.push_str(&error);
        }
        None => *errors = Some(error),
    }
}

fn drain(reader: impl Read, file: Option<File>, limit: usize) -> Result<Drain> {
    drain_reported(reader, file, limit, || {})
}
fn drain_reported(
    mut reader: impl Read,
    mut file: Option<File>,
    limit: usize,
    failed: impl Fn(),
) -> Result<Drain> {
    let mut output = Drain::empty();
    let mut buffer = [0; 4096];
    let mut error = None;
    let mut cleanup = CleanupFailures::default();
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                if let Err(failure) = output.record(&buffer[..read], &mut file, limit) {
                    append_error(&mut error, format!("write stream log: {failure:#}"));
                    failed();
                }
            }
            Err(failure) => {
                let failure = anyhow::Error::new(failure);
                cleanup.retain(&failure);
                append_error(&mut error, format!("read stream: {failure}"));
                failed();
                break;
            }
        }
    }
    if let Some(file) = file
        && let Err(failure) = file.sync_all()
    {
        append_error(&mut error, format!("sync stream log: {failure}"));
        failed();
    }
    cleanup.finish()?;
    if let Some(error) = error {
        return Err(anyhow!(error));
    }
    Ok(output)
}
pub(super) fn run(
    command: &mut Command,
    timeout: Duration,
    cancel: &CancellationToken,
    files: Option<(File, File)>,
    limit: usize,
    mut abort: impl FnMut() -> Result<()>,
) -> Result<Output> {
    run_supervised(
        command,
        timeout,
        cancel,
        files,
        limit,
        |event| match event {
            SupervisionEvent::Poll => Ok(SupervisionDecision::Continue),
            SupervisionEvent::Abort => abort().map(|()| SupervisionDecision::Continue),
        },
    )
}

/// Control callbacks must honor their own bounded deadlines; the monitor checks
/// cancellation and its execution deadline both before and after each callback.
pub(super) fn run_supervised(
    command: &mut Command,
    timeout: Duration,
    cancel: &CancellationToken,
    files: Option<(File, File)>,
    limit: usize,
    mut control: impl FnMut(SupervisionEvent) -> Result<SupervisionDecision>,
) -> Result<Output> {
    if cancel.is_cancelled() {
        let abort = control(SupervisionEvent::Abort);
        let mut cleanup = CleanupFailures::default();
        if let Err(error) = &abort {
            cleanup.retain(error);
        }
        cleanup.finish()?;
        let error = abort.err().map(|error| format!("{error:#}"));
        return Ok(Output {
            supervised_stop: false,
            status: None,
            timed_out: false,
            cancelled: true,
            stdout: Drain::empty(),
            stderr: Drain::empty(),
            duration_ms: 0,
            error,
        });
    }
    let mut process = match HostProcess::spawn(command) {
        Ok(process) => process,
        Err(error) => {
            return Err(failed_acquisition(error, || {
                control(SupervisionEvent::Abort).map(|_| ())
            }));
        }
    };
    let out = PipeReader::new(process.stdout.take().unwrap(), process.stop.clone())?;
    let err = PipeReader::new(process.stderr.take().unwrap(), process.stop.clone())?;
    let (out_file, err_file) = files.map_or((None, None), |(a, b)| (Some(a), Some(b)));
    thread::scope(|scope| {
        let stdout = thread::Builder::new()
            .name("process-stdout".into())
            .spawn_scoped(scope, move || drain(out, out_file, limit));
        let stdout = match stdout {
            Ok(worker) => worker,
            Err(error) => {
                return Err(setup_failure(
                    &mut process,
                    &mut || control(SupervisionEvent::Abort).map(|_| ()),
                    format!("start stdout worker: {error}"),
                ));
            }
        };
        let stderr = thread::Builder::new()
            .name("process-stderr".into())
            .spawn_scoped(scope, move || drain(err, err_file, limit));
        let stderr = match stderr {
            Ok(worker) => worker,
            Err(error) => {
                let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    setup_failure(
                        &mut process,
                        &mut || control(SupervisionEvent::Abort).map(|_| ()),
                        format!("start stderr worker: {error}"),
                    )
                }));
                return Err(join_failed_setup(
                    failure,
                    stdout
                        .join()
                        .map_err(worker_panic)
                        .and_then(|result| result),
                ));
            }
        };
        let completion = process.monitor(
            timeout,
            cancel,
            || stdout.is_finished() && stderr.is_finished(),
            || false,
            MonitorPolicy::Immediate,
            control,
        );
        // Monitor either established completion or interrupted the nonblocking pipe
        // readers. Join both even when one fails; no reader thread is detached.
        let stdout = stdout
            .join()
            .map_err(worker_panic)
            .context("stdout drain panicked")
            .and_then(|result| result);
        let stderr = stderr
            .join()
            .map_err(worker_panic)
            .context("stderr drain panicked")
            .and_then(|result| result);
        let mut retained = CleanupFailures::default();
        if let Err(error) = completion.cleanup.finish() {
            retained.retain(&error);
        }
        if let Err(error) = &stdout {
            retained.retain(error);
        }
        if let Err(error) = &stderr {
            retained.retain(error);
        }
        if let Some(panic) = completion.panic {
            retained.resume_unwind(panic);
        }
        retained.finish()?;
        let stdout = stdout?;
        let stderr = stderr?;
        Ok(Output {
            supervised_stop: completion.supervised_stop,
            status: completion.status.and_then(|s| s.code()),
            timed_out: completion.timed_out,
            cancelled: completion.cancelled,
            stdout,
            stderr,
            duration_ms: completion.duration_ms,
            error: completion.error,
        })
    })
}

/// Typed evidence rejection, distinct from process/pipe acquisition failure.
#[derive(Debug)]
pub(super) struct ArchiveRejected;
impl std::fmt::Display for ArchiveRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("archive observation rejected")
    }
}
impl std::error::Error for ArchiveRejected {}

/// Streaming archive acquisition uses the same child-and-pipes deadline as logs.
pub(super) fn archive(
    command: &mut Command,
    root: &str,
    limits: Limits,
    timeout: Duration,
    cancel: &CancellationToken,
    mut abort: impl FnMut() -> Result<()>,
) -> Result<Snapshot> {
    if cancel.is_cancelled() {
        abort()?;
        anyhow::bail!("archive operation cancelled");
    }
    let mut process = match HostProcess::spawn(command) {
        Ok(process) => process,
        Err(error) => {
            return Err(failed_acquisition(error, abort));
        }
    };
    let mut out = PipeReader::new(process.stdout.take().unwrap(), process.stop.clone())?;
    let err = PipeReader::new(process.stderr.take().unwrap(), process.stop.clone())?;
    let rejected = Arc::new(AtomicBool::new(false));
    let failed = rejected.clone();
    let root = root.to_string();
    thread::scope(|scope| {
        let parser = thread::Builder::new()
            .name("archive-parser".into())
            .spawn_scoped(scope, move || {
                let result = snapshot::read_archive(&mut out, &root, limits)
                    .map_err(|error| {
                        // A bounded/unsafe archive rejection is expected incomplete evidence.
                        // Actual pipe/file I/O errors remain infrastructure failures.
                        if error.downcast_ref::<std::io::Error>().is_some() {
                            error
                        } else {
                            error.context(ArchiveRejected)
                        }
                    })
                    .and_then(|snapshot| {
                        std::io::copy(&mut out, &mut std::io::sink())?;
                        Ok(snapshot)
                    });
                if result.is_err() {
                    rejected.store(true, Ordering::SeqCst);
                }
                result
            });
        let parser = match parser {
            Ok(worker) => worker,
            Err(error) => {
                return Err(setup_failure(
                    &mut process,
                    &mut abort,
                    format!("start archive worker: {error}"),
                ));
            }
        };
        let stderr = thread::Builder::new()
            .name("archive-stderr".into())
            .spawn_scoped(scope, move || drain(err, None, 65536));
        let stderr = match stderr {
            Ok(worker) => worker,
            Err(error) => {
                let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    setup_failure(
                        &mut process,
                        &mut abort,
                        format!("start archive stderr worker: {error}"),
                    )
                }));
                return Err(join_failed_setup(
                    failure,
                    parser
                        .join()
                        .map_err(worker_panic)
                        .and_then(|result| result),
                ));
            }
        };
        let completion = process.monitor(
            timeout,
            cancel,
            || parser.is_finished() && stderr.is_finished(),
            || failed.load(Ordering::SeqCst),
            MonitorPolicy::Immediate,
            |event| match event {
                SupervisionEvent::Poll => Ok(SupervisionDecision::Continue),
                SupervisionEvent::Abort => abort().map(|()| SupervisionDecision::Continue),
            },
        );
        let parsed = parser.join();
        let stderr = stderr.join();
        finish_archive_workers(completion, parsed, stderr)
    })
}

fn finish_archive_workers(
    completion: Completion,
    parsed: std::thread::Result<Result<Snapshot>>,
    stderr: std::thread::Result<Result<Drain>>,
) -> Result<Snapshot> {
    let parsed = parsed
        .map_err(worker_panic)
        .context("archive parser panicked");
    let stderr = stderr
        .map_err(worker_panic)
        .context("archive stderr drain panicked")
        .and_then(|result| result);
    let mut retained = CleanupFailures::default();
    if let Err(error) = completion.cleanup.finish() {
        retained.retain(&error);
    }
    match &parsed {
        Err(error) | Ok(Err(error)) => retained.retain(error),
        Ok(Ok(_)) => {}
    }
    if let Err(error) = &stderr {
        retained.retain(error);
    }
    if let Some(panic) = completion.panic {
        retained.resume_unwind(panic);
    }
    retained.finish()?;
    let parsed = parsed?;
    let stderr = stderr?;
    anyhow::ensure!(
        !completion.timed_out && !completion.cancelled,
        "archive operation timed out or cancelled"
    );
    if let Some(error) = completion.error {
        anyhow::bail!("archive operation failed: {error}");
    }
    let snapshot = parsed?;
    anyhow::ensure!(
        completion.status.is_some_and(|s| s.success()),
        "Docker archive failed: {}",
        String::from_utf8_lossy(&stderr.bytes)
    );
    Ok(snapshot)
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
    if let Some(error) = output.error {
        anyhow::bail!("Docker operation failed: {error}");
    }
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
#[path = "../../tests/support/native.rs"]
mod test_native;
#[cfg(test)]
pub(crate) mod test_support {
    use super::Command;
    pub(crate) use super::cleanup::io_error_boundary_tests::PendingFixture;
    pub(crate) use super::test_native::docker;
    pub(crate) fn fixture(mode: &str, args: &[&str]) -> Command {
        let mut command =
            Command::new(std::env::var_os("THESEUS_HOST_TEST_FIXTURE").expect(
                "build tests/support/build_host_fixture.py and set THESEUS_HOST_TEST_FIXTURE",
            ));
        command.arg(mode).args(args);
        command
    }
    pub(crate) fn failing_log() -> std::fs::File {
        // Both writing and final synchronization fail, so the second diagnostic
        // does not depend on which worker the monitor observes first.
        #[cfg(unix)]
        {
            std::fs::File::open("/dev/null").unwrap()
        }
        #[cfg(windows)]
        {
            let temporary = tempfile::NamedTempFile::new().unwrap();
            std::fs::File::open(temporary.path()).unwrap()
        }
    }
    #[cfg(unix)]
    #[test]
    fn immutable_docker_alias_preserves_pin_and_native_dispatch() {
        use std::os::unix::fs::MetadataExt;
        let source =
            std::path::PathBuf::from(std::env::var_os("THESEUS_HOST_TEST_FIXTURE").unwrap());
        let before = std::fs::symlink_metadata(&source).unwrap();
        let root = tempfile::tempdir().unwrap();
        let alias = docker(root.path(), "metadata");
        let linked = std::fs::symlink_metadata(&alias).unwrap();
        assert!(linked.is_file());
        assert_eq!((linked.dev(), linked.ino()), (before.dev(), before.ino()));
        assert_eq!(docker(root.path(), "metadata"), alias);
        let output = std::process::Command::new(&alias)
            .args(["version", "--format", "{{.Server.Version}}"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"29.9.2\n");
        assert!(output.stderr.is_empty());
        std::fs::remove_file(alias).unwrap();
        let output = std::process::Command::new(&source)
            .args(["echo-bytes", "41"])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"A");
        let after = std::fs::symlink_metadata(source).unwrap();
        assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
        assert_eq!((after.mode(), after.len()), (before.mode(), before.len()));
        println!(
            "ACTUAL immutable fixture alias: same dev/inode, idempotent publication, native alias dispatch, source executes after unlink, mode/length unchanged"
        );
    }
    #[cfg(unix)]
    #[test]
    fn immutable_docker_alias_rejects_unrelated_file_and_symlink() {
        let root = tempfile::tempdir().unwrap();
        let alias = root.path().join("theseus-docker-metadata");
        std::fs::write(&alias, b"unrelated").unwrap();
        assert!(std::panic::catch_unwind(|| docker(root.path(), "metadata")).is_err());
        assert_eq!(std::fs::read(&alias).unwrap(), b"unrelated");
        std::fs::remove_file(&alias).unwrap();
        let outside = root.path().join("outside");
        std::fs::write(&outside, b"untouched").unwrap();
        std::os::unix::fs::symlink(&outside, &alias).unwrap();
        assert!(std::panic::catch_unwind(|| docker(root.path(), "metadata")).is_err());
        assert!(
            std::fs::symlink_metadata(&alias)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(outside).unwrap(), b"untouched");
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod native_contract_tests;

#[cfg(test)]
mod monitor_panic_regressions;

#[cfg(test)]
mod native_read_error_regression;

#[cfg(test)]
mod archive_worker_precedence;
