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
    let mut command = shell("/usr/bin/setsid /bin/sleep 0.8 & printf '%s' \"$$\" > \"$1\"; exit 0");
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
        aborted && error.contains("missing child stdin") && error.contains("setup cleanup refused")
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
    // Group termination can land asynchronously (macOS): a heartbeat byte
    // already in flight may follow the cleanup return, but the holder must
    // then stop writing within a bounded window rather than keep running.
    let mut stopped = std::fs::metadata(&heartbeat).unwrap().len();
    let settle_deadline = Instant::now() + Duration::from_secs(1);
    loop {
        thread::sleep(Duration::from_millis(40));
        let now = std::fs::metadata(&heartbeat).unwrap().len();
        if now == stopped {
            break;
        }
        assert!(
            Instant::now() < settle_deadline,
            "owned descriptor holder kept writing after group cleanup"
        );
        stopped = now;
    }
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
