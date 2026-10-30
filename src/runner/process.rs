//! Deadlines cover child processes and pipe lifetime, including inherited handles.
use crate::{
    engine::CancellationToken,
    recipe::Limits,
    snapshot::{self, Snapshot},
};
use anyhow::{Context, Result, anyhow};
use rustix::{
    fs::{OFlags, fcntl_getfl, fcntl_setfl},
    process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid},
};
use std::{
    fs::File,
    io::{Read, Write},
    os::{fd::AsFd, unix::process::CommandExt},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
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
}

// A group is necessary to terminate owned CLI wrappers/helpers. Interruptible
// reads are also necessary: EOF is not guaranteed by killing the direct child or
// even its group (a descriptor could have been inherited by another group).
struct PipeReader<R> {
    reader: R,
    stop: Arc<AtomicBool>,
}
impl<R: Read + AsFd> PipeReader<R> {
    fn new(reader: R, stop: Arc<AtomicBool>) -> Result<Self> {
        fcntl_setfl(&reader, fcntl_getfl(&reader)? | OFlags::NONBLOCK)?;
        Ok(Self { reader, stop })
    }
}
impl<R: Read> Read for PipeReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if self.stop.load(Ordering::SeqCst) {
                return Ok(0);
            }
            match self.reader.read(buffer) {
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                result => return result,
            }
        }
    }
}
struct HostProcess {
    child: Child,
    group: Pid,
    stop: Arc<AtomicBool>,
    started: Instant,
    armed: bool,
}
struct Completion {
    status: Option<ExitStatus>,
    timed_out: bool,
    cancelled: bool,
    supervised_stop: bool,
    error: Option<String>,
    duration_ms: u64,
}
impl HostProcess {
    fn spawn(command: &mut Command) -> Result<Self> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let started = Instant::now();
        let child = command.spawn().context("spawn Docker CLI")?;
        let group = Pid::from_raw(child.id() as i32).context("invalid subprocess group ID")?;
        Ok(Self {
            child,
            group,
            stop: Arc::new(AtomicBool::new(false)),
            started,
            armed: true,
        })
    }
    /// Observe exit while retaining the waitable leader. Its unreaped identity
    /// reserves this PID/PGID until every group signal has finished.
    fn exited_without_reaping(&mut self) -> Result<bool> {
        match waitid(
            WaitId::Pid(self.group),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        ) {
            Ok(status) => Ok(status.is_some()),
            Err(rustix::io::Errno::CHILD) => {
                // An external reaper/SIGCHLD policy consumed ownership. Never
                // send a signal to the now unreserved numeric process/group ID.
                self.armed = false;
                anyhow::bail!("host child identity is no longer reserved");
            }
            Err(error) => Err(error.into()),
        }
    }
    fn terminate(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::SeqCst);
        if self.armed {
            self.exited_without_reaping()?;
            match kill_process_group(self.group, Signal::KILL) {
                Ok(()) | Err(rustix::io::Errno::SRCH) => {}
                Err(error) => {
                    let _ = self.child.kill();
                    return Err(error.into());
                }
            }
        }
        Ok(())
    }
    fn reap_and_disarm(&mut self) -> Result<ExitStatus> {
        // All group signalling must precede this consuming wait. Disarm even if
        // wait fails; no later Drop path may target an unreserved numeric ID.
        let result = self.child.wait();
        self.armed = false;
        result.context("reap owned host child after group cleanup")
    }
    fn monitor(
        &mut self,
        timeout: Duration,
        cancel: &CancellationToken,
        readers_done: impl Fn() -> bool,
        failed_reader: impl Fn() -> bool,
        mut control: impl FnMut(SupervisionEvent) -> Result<SupervisionDecision>,
    ) -> Completion {
        let mut exited = false;
        let mut timed_out;
        let mut cancelled;
        let mut supervised_stop = false;
        let mut last_poll: Option<Instant> = None;
        let mut error = None;
        loop {
            if !exited {
                match self.exited_without_reaping() {
                    Ok(value) => exited = value,
                    Err(failure) => error = Some(format!("observe host child: {failure:#}")),
                }
            }
            cancelled = cancel.is_cancelled();
            timed_out = self.started.elapsed() >= timeout;
            if !cancelled && !timed_out && error.is_none() {
                // Parser rejection stops only acquisition, preserving the container.
                if failed_reader() || (exited && readers_done()) {
                    break;
                }
                // Never turn an already-exited child into an intentional stop.
                // Slow control calls space their successors from completion too.
                if !exited && last_poll.is_none_or(|at| at.elapsed() >= Duration::from_millis(50)) {
                    let decision = control(SupervisionEvent::Poll);
                    last_poll = Some(Instant::now());
                    cancelled = cancel.is_cancelled();
                    timed_out = self.started.elapsed() >= timeout;
                    match decision {
                        Err(failure) => {
                            error = Some(format!("supervise owned environment: {failure:#}"))
                        }
                        Ok(SupervisionDecision::Stop) if !cancelled && !timed_out => {
                            supervised_stop = true;
                            break;
                        }
                        Ok(_) => {}
                    }
                }
            }
            if cancelled || timed_out || error.is_some() {
                self.stop.store(true, Ordering::SeqCst);
                // The exited leader stays unreaped throughout container abort,
                // so even its control-operation deadlines cannot expose reuse.
                if let Err(failure) = control(SupervisionEvent::Abort) {
                    let abort_error = format!("abort owned environment: {failure:#}");
                    error = Some(error.map_or(abort_error.clone(), |prior| {
                        format!("{prior}; {abort_error}")
                    }));
                }
                cancelled |= cancel.is_cancelled();
                timed_out |= self.started.elapsed() >= timeout;
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        // Normal completion, timeout/cancel, parser rejection and errors all share
        // the same ownership order: signal while reserved, reap, then disarm.
        if let Err(failure) = self.terminate() {
            error = Some(format!("terminate host process group: {failure:#}"));
        }
        let status = match self.reap_and_disarm() {
            Ok(status) => Some(status),
            Err(failure) => {
                error = Some(format!("{failure:#}"));
                None
            }
        };
        Completion {
            status,
            timed_out,
            cancelled,
            supervised_stop: supervised_stop && error.is_none(),
            error,
            duration_ms: self.started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        }
    }
}
impl Drop for HostProcess {
    fn drop(&mut self) {
        if self.armed {
            // Early setup/read errors retain the same signal-before-reap order.
            let _ = self.terminate();
            let _ = self.reap_and_disarm();
        }
    }
}

fn drain(mut reader: impl Read, mut file: Option<File>, limit: usize) -> Result<Drain> {
    let mut output = Drain::empty();
    let mut buffer = [0; 32768];
    let mut error = None;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let keep = read.min(limit.saturating_sub(output.stored as usize));
        output.truncated |= keep < read;
        if keep > 0 {
            output.bytes.extend_from_slice(&buffer[..keep]);
            if let Some(f) = file.as_mut()
                && let Err(failure) = f.write_all(&buffer[..keep])
            {
                error = Some(failure);
                file = None;
            }
            output.stored += keep as u64;
        }
    }
    if let Some(error) = error {
        return Err(error.into());
    }
    if let Some(file) = file {
        file.sync_all()?;
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
        let error = control(SupervisionEvent::Abort)
            .err()
            .map(|error| format!("{error:#}"));
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
    let mut process = HostProcess::spawn(command)?;
    let out = PipeReader::new(process.child.stdout.take().unwrap(), process.stop.clone())?;
    let err = PipeReader::new(process.child.stderr.take().unwrap(), process.stop.clone())?;
    let (out_file, err_file) = files.map_or((None, None), |(a, b)| (Some(a), Some(b)));
    let stdout = thread::spawn(move || drain(out, out_file, limit));
    let stderr = thread::spawn(move || drain(err, err_file, limit));
    let completion = process.monitor(
        timeout,
        cancel,
        || stdout.is_finished() && stderr.is_finished(),
        || false,
        control,
    );
    // Monitor either established completion or interrupted the nonblocking pipe
    // readers. Join both even when one fails; no reader thread is detached.
    let stdout = stdout.join();
    let stderr = stderr.join();
    let stdout = stdout.map_err(|_| anyhow!("stdout drain panicked"))??;
    let stderr = stderr.map_err(|_| anyhow!("stderr drain panicked"))??;
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
    let mut process = HostProcess::spawn(command)?;
    let mut out = PipeReader::new(process.child.stdout.take().unwrap(), process.stop.clone())?;
    let err = PipeReader::new(process.child.stderr.take().unwrap(), process.stop.clone())?;
    let rejected = Arc::new(AtomicBool::new(false));
    let failed = rejected.clone();
    let root = root.to_string();
    let parser = thread::spawn(move || {
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
    let stderr = thread::spawn(move || drain(err, None, 65536));
    let completion = process.monitor(
        timeout,
        cancel,
        || parser.is_finished() && stderr.is_finished(),
        || failed.load(Ordering::SeqCst),
        |event| match event {
            SupervisionEvent::Poll => Ok(SupervisionDecision::Continue),
            SupervisionEvent::Abort => abort().map(|()| SupervisionDecision::Continue),
        },
    );
    let parsed = parser.join();
    let stderr = stderr.join();
    let parsed = parsed.map_err(|_| anyhow!("archive parser panicked"))?;
    let stderr = stderr.map_err(|_| anyhow!("archive stderr drain panicked"))??;
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
mod tests {
    use super::*;
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct HolderIdentity {
        pid: u32,
        pgid: u32,
        start_time: u64,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct HolderStat {
        identity: HolderIdentity,
        state: char,
    }

    #[derive(Clone, Debug)]
    struct HolderSample {
        before: Instant,
        after: Instant,
        stat: std::result::Result<Option<String>, String>,
        absent_errno: Option<i32>,
    }

    #[derive(Debug)]
    struct HolderEvidence {
        first: HolderSample,
        last: HolderSample,
    }

    fn sample_holder(
        deadline: Instant,
        now: &mut impl FnMut() -> Instant,
        read: &mut impl FnMut() -> std::io::Result<String>,
    ) -> HolderSample {
        let before = now();
        let mut absent_errno = None;
        let stat = if before >= deadline {
            Err("deadline before observation".into())
        } else {
            match read() {
                Ok(raw) if raw.len() <= 4096 => Ok(Some(raw)),
                Ok(_) => Err("oversized holder stat".into()),
                // proc_single_show returns ESRCH when the task disappears
                // after opening /proc/<pid>/stat. This post-Stop reader alone
                // treats that kernel-documented case as absence.
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound
                        || error.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error()) =>
                {
                    absent_errno = error.raw_os_error();
                    Ok(None)
                }
                Err(error) => Err(format!("I/O: {error}")),
            }
        };
        HolderSample {
            before,
            after: now(),
            stat,
            absent_errno,
        }
    }

    // Read only the captured owned identity. A successful group signal and a
    // reaped leader do not synchronize the holder's terminal transition.
    fn observe_holder_until_terminal(
        expected: HolderIdentity,
        started: Instant,
        deadline: Instant,
        first: HolderSample,
        mut now: impl FnMut() -> Instant,
        mut read: impl FnMut() -> std::io::Result<String>,
        mut wait: impl FnMut(Duration),
    ) -> std::result::Result<HolderEvidence, String> {
        let mut current = first.clone();
        loop {
            let failure = |reason: &str, at: Instant| {
                format!(
                    "{reason}; expected={expected:?}; elapsed={:?}; first={first:?}; current={current:?}",
                    at.saturating_duration_since(started)
                )
            };
            let at = now();
            if current.before >= deadline || current.after >= deadline || at >= deadline {
                return Err(failure("holder observation deadline expired", at));
            }
            let terminal = match &current.stat {
                Err(error) => return Err(failure(error, at)),
                Ok(None) => true,
                Ok(Some(raw)) => {
                    let stat = parse_holder_stat(raw)
                        .map_err(|error| failure(&format!("malformed stat: {error:#}"), now()))?;
                    if stat.identity.pid != expected.pid
                        || stat.identity.start_time != expected.start_time
                    {
                        return Err(failure("holder identity changed", now()));
                    }
                    if stat.identity.pgid != expected.pgid {
                        return Err(failure("holder PGID changed", now()));
                    }
                    matches!(stat.state, 'Z' | 'X')
                }
            };
            let at = now();
            if at >= deadline {
                return Err(failure("holder observation deadline expired", at));
            }
            if terminal {
                return Ok(HolderEvidence {
                    first,
                    last: current,
                });
            }
            // Only a valid, same-identity nonterminal holder earns a pause.
            wait(Duration::from_millis(5).min(deadline.duration_since(at)));
            let at = now();
            if at >= deadline {
                return Err(failure(
                    "holder observation deadline expired after waiting",
                    at,
                ));
            }
            current = sample_holder(deadline, &mut now, &mut read);
        }
    }

    fn positive_pid(raw: &str) -> Result<u32> {
        anyhow::ensure!(
            !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit()),
            "invalid PID"
        );
        let pid: u32 = raw.parse()?;
        anyhow::ensure!(pid > 0 && pid <= i32::MAX as u32, "invalid PID");
        Ok(pid)
    }

    fn parse_holder_stat(raw: &str) -> Result<HolderStat> {
        anyhow::ensure!(
            raw.len() <= 4096 && raw.ends_with('\n'),
            "truncated or oversized stat"
        );
        let (pid, rest) = raw.split_once(" (").context("missing comm field")?;
        let (_, fields) = rest.rsplit_once(") ").context("unterminated comm field")?;
        let fields: Vec<_> = fields.split_whitespace().collect();
        anyhow::ensure!(fields.len() >= 20, "truncated stat fields");
        let state = fields[0];
        anyhow::ensure!(
            state.len() == 1 && "RSDZTtXxKWPI".contains(state),
            "invalid process state"
        );
        anyhow::ensure!(
            fields[19].bytes().all(|b| b.is_ascii_digit()),
            "invalid start time"
        );
        Ok(HolderStat {
            identity: HolderIdentity {
                pid: positive_pid(pid)?,
                pgid: positive_pid(fields[2])?,
                start_time: fields[19].parse()?,
            },
            state: state.chars().next().unwrap(),
        })
    }

    fn parse_holder_ready(raw: &str) -> Result<Option<(u32, u32)>> {
        anyhow::ensure!(raw.len() <= 128, "oversized ready record");
        let Some(line) = raw.strip_suffix('\n') else {
            return Ok(None);
        };
        anyhow::ensure!(!line.contains('\n'), "multiple ready records");
        let fields: Vec<_> = line.split_whitespace().collect();
        anyhow::ensure!(fields.len() == 2, "ready record must contain two PIDs");
        let leader = positive_pid(fields[0])?;
        let holder = positive_pid(fields[1])?;
        anyhow::ensure!(leader != holder, "holder must differ from leader");
        Ok(Some((leader, holder)))
    }

    fn read_bounded_text(path: &std::path::Path, limit: usize) -> std::io::Result<String> {
        let mut text = String::new();
        File::open(path)?
            .take((limit + 1) as u64)
            .read_to_string(&mut text)?;
        if text.len() > limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "oversized process-test record",
            ));
        }
        Ok(text)
    }

    // Literal, hand-checked /proc stat fields through field 22 (starttime).
    // The comm deliberately contains spaces and parentheses.
    const RUNNING_HOLDER: &str =
        "42 (owned (holder)) R 7 7 7 0 -1 0 1 0 0 0 0 0 0 0 20 0 1 0 123\n";
    const ZOMBIE_HOLDER: &str = "42 (owned (holder)) Z 1 7 7 0 -1 0 1 0 0 0 0 0 0 0 20 0 1 0 123\n";
    const DEAD_HOLDER: &str = "42 (owned (holder)) X 1 7 7 0 -1 0 1 0 0 0 0 0 0 0 20 0 1 0 123\n";
    const WRONG_GROUP_HOLDER: &str =
        "42 (owned (holder)) Z 1 8 7 0 -1 0 1 0 0 0 0 0 0 0 20 0 1 0 123\n";
    const REUSED_HOLDER: &str = "42 (owned (holder)) Z 1 7 7 0 -1 0 1 0 0 0 0 0 0 0 20 0 1 0 124\n";
    const EXPECTED_HOLDER: HolderIdentity = HolderIdentity {
        pid: 42,
        pgid: 7,
        start_time: 123,
    };

    // This is a test-only deterministic clock/source, paired below with the real
    // held-child regression. Assertions concern verdicts, evidence and elapsed
    // budget, never mocked call counts.
    fn holder_sequence(
        first_at_ms: u64,
        observations: Vec<(std::io::Result<String>, u64)>,
    ) -> (std::result::Result<HolderEvidence, String>, Duration) {
        let started = Instant::now();
        let elapsed = std::cell::Cell::new(Duration::from_millis(first_at_ms));
        let mut observations = std::collections::VecDeque::from(observations);
        let mut now = || started + elapsed.get();
        let mut read = || {
            let (result, read_ms) = observations
                .pop_front()
                .expect("unexpected extra observation");
            elapsed.set(elapsed.get() + Duration::from_millis(read_ms));
            result
        };
        let first = sample_holder(started + Duration::from_secs(1), &mut now, &mut read);
        let result = observe_holder_until_terminal(
            EXPECTED_HOLDER,
            started,
            started + Duration::from_secs(1),
            first,
            now,
            read,
            |pause| elapsed.set(elapsed.get() + pause),
        );
        (result, elapsed.get())
    }

    #[test]
    fn holder_observation_retains_first_running_sample_then_accepts_same_identity_terminal() {
        let (result, elapsed) = holder_sequence(
            990,
            vec![
                (Ok(RUNNING_HOLDER.into()), 0),
                (Ok(ZOMBIE_HOLDER.into()), 0),
            ],
        );
        let evidence =
            result.expect("pending termination must be observed within the original budget");
        assert_eq!(
            evidence.first.stat.unwrap().as_deref(),
            Some(RUNNING_HOLDER)
        );
        assert_eq!(evidence.last.stat.unwrap().as_deref(), Some(ZOMBIE_HOLDER));
        assert_eq!(elapsed, Duration::from_millis(995));
    }

    #[test]
    fn holder_observation_accepts_initial_terminal_or_absent_without_delay() {
        for observation in [
            Ok(ZOMBIE_HOLDER.into()),
            Ok(DEAD_HOLDER.into()),
            Err(std::io::ErrorKind::NotFound.into()),
        ] {
            let (result, elapsed) = holder_sequence(990, vec![(observation, 0)]);
            assert!(result.is_ok(), "{result:?}");
            assert_eq!(elapsed, Duration::from_millis(990));
        }
    }

    #[test]
    fn holder_observation_accepts_proc_esrch_disappearance_initially_or_after_running() {
        for (later, expected_ms) in [(false, 900), (true, 905)] {
            let mut sequence = vec![];
            if later {
                sequence.push((Ok(RUNNING_HOLDER.into()), 0));
            }
            sequence.push((
                Err(std::io::Error::from_raw_os_error(
                    rustix::io::Errno::SRCH.raw_os_error(),
                )),
                0,
            ));
            let (result, elapsed) = holder_sequence(900, sequence);
            let evidence = result.expect("proc stat ESRCH means the captured task disappeared");
            assert_eq!(evidence.last.stat.unwrap(), None);
            assert_eq!(evidence.last.absent_errno, Some(3));
            if later {
                assert_eq!(
                    evidence.first.stat.unwrap().as_deref(),
                    Some(RUNNING_HOLDER)
                );
            }
            assert_eq!(elapsed, Duration::from_millis(expected_ms));
        }
        let (result, _) = holder_sequence(
            999,
            vec![(
                Err(std::io::Error::from_raw_os_error(
                    rustix::io::Errno::SRCH.raw_os_error(),
                )),
                1,
            )],
        );
        assert!(result.unwrap_err().contains("deadline"));
    }

    #[test]
    fn holder_observation_rejects_identity_group_parse_and_io_failures_initially_or_later() {
        for (raw, category) in [
            (
                Ok::<String, std::io::Error>(WRONG_GROUP_HOLDER.into()),
                "PGID",
            ),
            (Ok(REUSED_HOLDER.into()), "identity"),
            (
                Ok("43 (replacement) Z 1 7 7 0 -1 0 1 0 0 0 0 0 0 0 20 0 1 0 123\n".into()),
                "identity",
            ),
            (Ok("42 (truncated) Z 1 7".into()), "malformed"),
            (Err(std::io::ErrorKind::PermissionDenied.into()), "I/O"),
            (Err(std::io::ErrorKind::InvalidData.into()), "I/O"),
        ] {
            for later in [false, true] {
                let raw = match &raw {
                    Ok(raw) => Ok(raw.clone()),
                    Err(error) => Err(std::io::Error::from(error.kind())),
                };
                let mut sequence = vec![];
                if later {
                    sequence.push((Ok(RUNNING_HOLDER.into()), 0));
                }
                sequence.push((raw, 0));
                // A later disappearance must not conceal an earlier bad sample.
                sequence.push((Err(std::io::ErrorKind::NotFound.into()), 0));
                let (result, _) = holder_sequence(900, sequence);
                let error = result.unwrap_err();
                assert!(error.contains(category), "later={later}: {error}");
                assert!(
                    error.contains("first=") && error.contains("current="),
                    "{error}"
                );
            }
        }
    }

    #[test]
    fn holder_observation_fails_persistent_holder_at_original_deadline() {
        let (result, elapsed) = holder_sequence(
            990,
            vec![
                (Ok(RUNNING_HOLDER.into()), 0),
                (Ok(RUNNING_HOLDER.into()), 0),
            ],
        );
        let error = result.unwrap_err();
        assert!(error.contains("deadline"), "{error}");
        assert!(
            error.contains(RUNNING_HOLDER.trim()),
            "first sample lost: {error}"
        );
        assert_eq!(elapsed, Duration::from_secs(1));
    }

    #[test]
    fn holder_observation_rejects_expired_or_late_terminal_and_absent_samples() {
        for (first_at, reads) in [
            (1000, vec![(Ok(ZOMBIE_HOLDER.into()), 0)]),
            (1001, vec![(Err(std::io::ErrorKind::NotFound.into()), 0)]),
            (999, vec![(Ok(ZOMBIE_HOLDER.into()), 1)]),
            (
                990,
                vec![
                    (Ok(RUNNING_HOLDER.into()), 0),
                    (Ok(ZOMBIE_HOLDER.into()), 5),
                ],
            ),
            (
                990,
                vec![
                    (Ok(RUNNING_HOLDER.into()), 0),
                    (Err(std::io::ErrorKind::NotFound.into()), 6),
                ],
            ),
            (999, vec![(Ok(RUNNING_HOLDER.into()), 0)]),
        ] {
            let (result, _) = holder_sequence(first_at, reads);
            let error = result.unwrap_err();
            assert!(error.contains("deadline"), "{error}");
            assert!(
                error.contains("first=") && error.contains("current="),
                "{error}"
            );
        }
    }

    #[test]
    fn holder_stat_requires_valid_identity_and_handles_parentheses_in_comm() {
        assert_eq!(
            parse_holder_stat(RUNNING_HOLDER).unwrap(),
            HolderStat {
                identity: EXPECTED_HOLDER,
                state: 'R'
            }
        );
        for invalid in [
            "",
            "42 (truncated) Z 1 7",
            "42 missing-parentheses R 1 7",
            "42 (holder) ZZ 1 7 7 0 -1 0 1 0 0 0 0 0 0 0 20 0 1 0 123\n",
            "0 (holder) R 1 7 7 0 -1 0 1 0 0 0 0 0 0 0 20 0 1 0 123\n",
            "42 (holder) ? 1 7 7 0 -1 0 1 0 0 0 0 0 0 0 20 0 1 0 123\n",
            "42 (holder) R 1 bad 7 0 -1 0 1 0 0 0 0 0 0 0 20 0 1 0 123\n",
            "42 (holder) R 1 7 7 0 -1 0 1 0 0 0 0 0 0 0 20 0 1 0 bad\n",
        ] {
            assert!(parse_holder_stat(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn holder_ready_requires_complete_bounded_two_pid_record() {
        for incomplete in ["", "7", "7 4", "7 42"] {
            assert_eq!(parse_holder_ready(incomplete).unwrap(), None);
        }
        assert_eq!(parse_holder_ready("7 42\n").unwrap(), Some((7, 42)));
        for invalid in [
            "\n",
            "7\n",
            "7 x\n",
            "0 42\n",
            "7 0\n",
            "-7 42\n",
            "7 42 9\n",
            "7 42\n8 9\n",
            "7 7\n",
        ] {
            assert!(parse_holder_ready(invalid).is_err(), "{invalid:?}");
        }
        assert!(parse_holder_ready(&"7".repeat(129)).is_err());
    }

    // These regressions catch ignored Stop, error-as-success, and safety checks
    // skipped after a callback. The child signals only after filling both pipes.
    fn held_child(ready: &std::path::Path) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "head -c 131072 /dev/zero; head -c 131072 /dev/zero >&2; sleep 10 & printf '%s %s\\n' \"$$\" \"$!\" > \"$1\"; wait", "supervised-child"]).arg(ready);
        command
    }
    #[test]
    fn supervision_stops_held_child_and_joins_bounded_drains() {
        let test_started = Instant::now();
        let deadline = test_started + Duration::from_secs(1);
        let temp = tempfile::tempdir().unwrap();
        let ready = temp.path().join("ready");
        let mut polls = vec![];
        let mut aborted = false;
        let mut ready_record = None;
        let mut holder = None;
        let output = run_supervised(
            &mut held_child(&ready),
            Duration::from_secs(1),
            &CancellationToken::default(),
            None,
            128,
            |event| match event {
                SupervisionEvent::Poll => {
                    polls.push(Instant::now());
                    anyhow::ensure!(Instant::now() < deadline, "test deadline before readiness");
                    let raw = match read_bounded_text(&ready, 128) {
                        Ok(raw) => raw,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            return Ok(SupervisionDecision::Continue);
                        }
                        Err(error) => return Err(error.into()),
                    };
                    ready_record = Some(raw.clone());
                    let Some((leader, pid)) = parse_holder_ready(&raw)? else {
                        anyhow::ensure!(
                            Instant::now() < deadline,
                            "test deadline during readiness"
                        );
                        return Ok(SupervisionDecision::Continue);
                    };
                    anyhow::ensure!(
                        Instant::now() < deadline,
                        "test deadline before identity capture"
                    );
                    let raw_stat = read_bounded_text(
                        std::path::Path::new(&format!("/proc/{pid}/stat")),
                        4096,
                    )?;
                    let stat = parse_holder_stat(&raw_stat)
                        .with_context(|| format!("pre-Stop holder stat={raw_stat:?}"))?;
                    anyhow::ensure!(
                        stat.identity.pid == pid && stat.identity.pgid == leader,
                        "pre-Stop holder identity/PGID mismatch: ready={raw:?}; stat={raw_stat:?}"
                    );
                    anyhow::ensure!(
                        !matches!(stat.state, 'Z' | 'X' | 'x'),
                        "fixture requires a live holder before Stop: {raw_stat:?}"
                    );
                    holder = Some((stat.identity, raw_stat));
                    anyhow::ensure!(
                        Instant::now() < deadline,
                        "test deadline after identity capture"
                    );
                    Ok(SupervisionDecision::Stop)
                }
                SupervisionEvent::Abort => {
                    aborted = true;
                    Ok(SupervisionDecision::Continue)
                }
            },
        );
        // Preserve the FIRST post-return read before assertions, formatting or
        // further observations. Never discard a running sample on later success.
        let first = holder.as_ref().map(|(identity, _)| {
            sample_holder(deadline, &mut Instant::now, &mut || {
                read_bounded_text(
                    std::path::Path::new(&format!("/proc/{}/stat", identity.pid)),
                    4096,
                )
            })
        });
        let context = format!(
            "ready={ready_record:?}; pre_stop={holder:?}; first={first:?}; elapsed={:?}",
            test_started.elapsed()
        );
        let output = output.unwrap_or_else(|error| panic!("{error:#}; {context}"));
        assert!(
            output.supervised_stop,
            "{context}; error={:?}",
            output.error
        );
        assert!(!output.timed_out, "{context}");
        assert!(!output.cancelled, "{context}");
        assert!(
            output.error.is_none(),
            "{context}; error={:?}",
            output.error
        );
        assert!(
            !aborted,
            "intentional stop is distinct from safety abort; {context}"
        );
        assert_eq!(output.stdout.bytes.len(), 128, "{context}");
        assert_eq!(output.stderr.bytes.len(), 128, "{context}");
        assert!(
            output.stdout.truncated && output.stderr.truncated,
            "{context}"
        );
        assert!(
            polls
                .windows(2)
                .all(|pair| pair[1].duration_since(pair[0]) >= Duration::from_millis(50)),
            "{context}"
        );
        let (identity, _) =
            holder.unwrap_or_else(|| panic!("missing pre-Stop identity; {context}"));
        assert!(
            !std::path::Path::new(&format!("/proc/{}", identity.pgid)).exists(),
            "leader was not reaped; {context}"
        );
        let evidence = observe_holder_until_terminal(
            identity,
            test_started,
            deadline,
            first.unwrap_or_else(|| panic!("missing first observation; {context}")),
            Instant::now,
            || read_bounded_text(std::path::Path::new(&format!("/proc/{}/stat", identity.pid)), 4096),
            thread::sleep,
        ).unwrap_or_else(|error| panic!("descriptor holder survived group cleanup or could not be verified: {error}; {context}"));
        assert!(
            Instant::now() < deadline,
            "test deadline after holder observation; {evidence:?}; {context}"
        );
        assert!(output.duration_ms < 1000, "{context}");
    }
    #[test]
    fn supervision_error_aborts_without_claiming_intentional_stop() {
        let temp = tempfile::tempdir().unwrap();
        let ready = temp.path().join("ready");
        let mut aborted = false;
        let output = run_supervised(
            &mut held_child(&ready),
            Duration::from_secs(1),
            &CancellationToken::default(),
            None,
            128,
            |event| match event {
                SupervisionEvent::Poll if ready.exists() => {
                    anyhow::bail!("readiness verification failed")
                }
                SupervisionEvent::Poll => Ok(SupervisionDecision::Continue),
                SupervisionEvent::Abort => {
                    aborted = true;
                    Ok(SupervisionDecision::Continue)
                }
            },
        )
        .unwrap();
        assert!(aborted);
        assert!(!output.supervised_stop);
        assert!(!output.timed_out && !output.cancelled);
        assert!(
            output
                .error
                .unwrap()
                .contains("readiness verification failed")
        );
    }
    #[test]
    fn cancellation_during_stop_callback_wins_and_aborts() {
        let temp = tempfile::tempdir().unwrap();
        let ready = temp.path().join("ready");
        let cancel = CancellationToken::default();
        let mut aborted = false;
        let output = run_supervised(
            &mut held_child(&ready),
            Duration::from_secs(1),
            &cancel,
            None,
            128,
            |event| match event {
                SupervisionEvent::Poll if ready.exists() => {
                    cancel.cancel();
                    Ok(SupervisionDecision::Stop)
                }
                SupervisionEvent::Poll => Ok(SupervisionDecision::Continue),
                SupervisionEvent::Abort => {
                    aborted = true;
                    Ok(SupervisionDecision::Continue)
                }
            },
        )
        .unwrap();
        assert!(output.cancelled && aborted);
        assert!(!output.supervised_stop);
        assert!(!output.timed_out);
    }
    #[test]
    fn deadline_during_stop_callback_wins_and_aborts() {
        let temp = tempfile::tempdir().unwrap();
        let ready = temp.path().join("ready");
        let mut aborted = false;
        let mut reached_barrier = false;
        let output = run_supervised(
            &mut held_child(&ready),
            Duration::from_millis(150),
            &CancellationToken::default(),
            None,
            128,
            |event| match event {
                SupervisionEvent::Poll if ready.exists() => {
                    reached_barrier = true;
                    thread::sleep(Duration::from_millis(160));
                    Ok(SupervisionDecision::Stop)
                }
                SupervisionEvent::Poll => Ok(SupervisionDecision::Continue),
                SupervisionEvent::Abort => {
                    aborted = true;
                    Ok(SupervisionDecision::Continue)
                }
            },
        )
        .unwrap();
        assert!(
            reached_barrier,
            "deadline must occur inside the stop callback"
        );
        assert!(output.timed_out && aborted);
        assert!(!output.supervised_stop);
        assert!(!output.cancelled);
    }
    #[test]
    fn normal_exit_before_readiness_is_not_supervised_success() {
        let mut polls = 0;
        let output = run_supervised(
            Command::new("/bin/sh").args(["-c", "exit 0"]),
            Duration::from_secs(1),
            &CancellationToken::default(),
            None,
            128,
            |event| match event {
                SupervisionEvent::Poll => {
                    polls += 1;
                    Ok(SupervisionDecision::Continue)
                }
                SupervisionEvent::Abort => panic!("normal exit must not abort"),
            },
        )
        .unwrap();
        assert_eq!(output.status, Some(0));
        assert!(!output.supervised_stop);
        assert!(!output.timed_out && !output.cancelled);
        assert!(output.error.is_none());
        assert!(polls <= 1);
    }
    #[test]
    fn exited_leader_stays_reserved_until_deferred_group_cleanup_then_is_reaped() {
        let temp = tempfile::tempdir().unwrap();
        let identities = temp.path().join("identities");
        let mut leader = 0u32;
        let mut reserved_during_abort = false;
        let mut holder_in_other_group = false;
        let output = run(Command::new("/bin/sh").args(["-c", "/usr/bin/setsid /bin/sleep 0.3 & printf '%s %s\n' \"$$\" \"$!\" > \"$1\"; exit 0", "identity-wrapper"]).arg(&identities), Duration::from_millis(50), &CancellationToken::default(), None, 32, || {
            let text = std::fs::read_to_string(&identities)?; let ids: Vec<u32> = text.split_whitespace().map(str::parse).collect::<std::result::Result<_, _>>()?; leader = ids[0];
            reserved_during_abort = std::fs::read_to_string(format!("/proc/{leader}/stat")).is_ok_and(|s| s.rsplit_once(')').unwrap().1.trim_start().starts_with('Z'));
            holder_in_other_group = std::fs::read_to_string(format!("/proc/{}/stat", ids[1])).is_ok_and(|s| { let fields: Vec<_> = s.rsplit_once(')').unwrap().1.split_whitespace().collect(); fields[2].parse::<u32>().unwrap() != leader });
            Ok(())
        }).unwrap();
        assert!(
            holder_in_other_group,
            "regression requires a descriptor holder in a different group"
        );
        assert!(
            reserved_during_abort,
            "exited leader {leader} was reaped before deferred group signalling"
        );
        assert_eq!(output.status, Some(0));
        assert!(output.timed_out);
        assert!(
            !std::path::Path::new(&format!("/proc/{leader}")).exists(),
            "leader was not reaped after group cleanup"
        );
    }
    #[test]
    fn deadline_includes_inherited_descriptors_after_direct_child_exit() {
        let temp = tempfile::tempdir().unwrap();
        let pid_file = temp.path().join("holder.pid");
        let began = Instant::now();
        let mut aborted = false;
        let output = run(
            Command::new("/bin/sh")
                .args([
                    "-c",
                    "sleep 2 & printf '%s\n' \"$!\" > \"$1\"; exit 0",
                    "holder-wrapper",
                ])
                .arg(&pid_file),
            Duration::from_millis(50),
            &CancellationToken::default(),
            None,
            32,
            || {
                aborted = true;
                Ok(())
            },
        )
        .unwrap();
        assert!(
            output.timed_out,
            "elapsed={}ms status={:?}",
            began.elapsed().as_millis(),
            output.status
        );
        assert!(aborted);
        assert_eq!(
            output.status,
            Some(0),
            "premise requires the direct child to have exited"
        );
        assert!(began.elapsed() < Duration::from_secs(1));
        let pid: u32 = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let mut stopped = false;
        for _ in 0..100 {
            match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    stopped = true;
                    break;
                }
                Ok(stat)
                    if stat
                        .rsplit_once(')')
                        .unwrap()
                        .1
                        .trim_start()
                        .starts_with(['Z', 'X']) =>
                {
                    stopped = true;
                    break;
                }
                _ => thread::sleep(Duration::from_millis(5)),
            }
        }
        assert!(stopped, "owned descriptor holder {pid} survived deadline");
    }
    #[test]
    fn cancellation_after_direct_child_exit_interrupts_inherited_descriptors() {
        let cancel = CancellationToken::default();
        let trigger = cancel.clone();
        let signal = thread::spawn(move || {
            thread::sleep(Duration::from_millis(75));
            trigger.cancel();
        });
        let began = Instant::now();
        let mut aborted = false;
        let output = run(
            Command::new("/bin/sh").args(["-c", "sleep 2 & exit 0"]),
            Duration::from_secs(10),
            &cancel,
            None,
            32,
            || {
                aborted = true;
                Ok(())
            },
        )
        .unwrap();
        signal.join().unwrap();
        assert!(
            output.cancelled,
            "elapsed={}ms status={:?}",
            began.elapsed().as_millis(),
            output.status
        );
        assert!(aborted);
        assert_eq!(
            output.status,
            Some(0),
            "premise requires the direct child to have exited"
        );
        assert!(began.elapsed() < Duration::from_secs(1));
    }
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
