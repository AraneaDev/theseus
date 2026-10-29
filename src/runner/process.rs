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
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "The planned interruption consumer is introduced in Task 2"
        )
    )]
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
        let result = snapshot::read_archive(&mut out, &root, limits).and_then(|snapshot| {
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
    // These regressions catch ignored Stop, error-as-success, and safety checks
    // skipped after a callback. The child signals only after filling both pipes.
    fn held_child(ready: &std::path::Path) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "head -c 131072 /dev/zero; head -c 131072 /dev/zero >&2; sleep 10 & printf '%s %s\\n' \"$$\" \"$!\" > \"$1\"; wait", "supervised-child"]).arg(ready);
        command
    }
    #[test]
    fn supervision_stops_held_child_and_joins_bounded_drains() {
        let temp = tempfile::tempdir().unwrap();
        let ready = temp.path().join("ready");
        let mut polls = vec![];
        let mut aborted = false;
        let output = run_supervised(
            &mut held_child(&ready),
            Duration::from_secs(1),
            &CancellationToken::default(),
            None,
            128,
            |event| match event {
                SupervisionEvent::Poll => {
                    polls.push(Instant::now());
                    Ok(if ready.exists() {
                        SupervisionDecision::Stop
                    } else {
                        SupervisionDecision::Continue
                    })
                }
                SupervisionEvent::Abort => {
                    aborted = true;
                    Ok(SupervisionDecision::Continue)
                }
            },
        )
        .unwrap();
        assert!(output.supervised_stop);
        assert!(!output.timed_out);
        assert!(!output.cancelled);
        assert!(output.error.is_none());
        assert!(!aborted, "intentional stop is distinct from safety abort");
        assert_eq!(output.stdout.bytes.len(), 128);
        assert_eq!(output.stderr.bytes.len(), 128);
        assert!(output.stdout.truncated && output.stderr.truncated);
        assert!(
            polls
                .windows(2)
                .all(|pair| pair[1].duration_since(pair[0]) >= Duration::from_millis(50))
        );
        let ids = std::fs::read_to_string(&ready).unwrap();
        let ids: Vec<_> = ids.split_whitespace().collect();
        assert!(
            !std::path::Path::new(&format!("/proc/{}", ids[0])).exists(),
            "leader was not reaped"
        );
        let child = std::fs::read_to_string(format!("/proc/{}/stat", ids[1]));
        assert!(
            child.is_err()
                || child
                    .unwrap()
                    .rsplit_once(')')
                    .unwrap()
                    .1
                    .trim_start()
                    .starts_with(['Z', 'X']),
            "descriptor holder survived group cleanup"
        );
        assert!(output.duration_ms < 1000);
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
