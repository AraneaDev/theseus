//! One bounded operation per retained worker; mailbox readiness is not native IO_PENDING.
use super::{Evidence, Observation, Progress, failure, native, tick};
use serde_json::json;
use std::io;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, SyncSender, TryRecvError},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

struct Completion {
    observation: Observation,
    checksum: u64,
}
struct Worker {
    command: SyncSender<usize>,
    completion: Receiver<Completion>,
    join: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    entered: Arc<AtomicBool>,
    barrier: Arc<AtomicBool>,
    release: Arc<AtomicBool>,
}
impl Worker {
    fn spawn(
        endpoint: native::Endpoint,
        reading: bool,
        race: bool,
        unresolved: bool,
    ) -> io::Result<Self> {
        let (command, commands) = mpsc::sync_channel::<usize>(1);
        let (completed, completion) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(AtomicBool::new(false));
        let barrier = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let (worker_stop, worker_entered, worker_barrier, worker_release) = (
            stop.clone(),
            entered.clone(),
            barrier.clone(),
            release.clone(),
        );
        let join = thread::Builder::new()
            .name(
                if reading {
                    "preflight-reader"
                } else {
                    "preflight-writer"
                }
                .into(),
            )
            .spawn(move || {
                let mut buffer = [if reading { 0 } else { b'O' }; 4096];
                loop {
                    if worker_stop.load(Ordering::Acquire) {
                        break;
                    }
                    let size = match commands.try_recv() {
                        Ok(n) => n,
                        Err(TryRecvError::Empty) => {
                            tick();
                            continue;
                        }
                        Err(TryRecvError::Disconnected) => break,
                    };
                    // Stop is checked before every dispatch. The test barrier sits
                    // immediately after this check, exposing the unavoidable race.
                    if worker_stop.load(Ordering::Acquire) {
                        break;
                    }
                    if race || unresolved {
                        worker_barrier.store(true, Ordering::Release);
                        while !worker_release.load(Ordering::Acquire) {
                            tick();
                        }
                    }
                    worker_entered.store(true, Ordering::Release);
                    let observation = if reading {
                        native::read(&endpoint, &mut buffer[..size])
                    } else {
                        native::write(&endpoint, &buffer[..size])
                    };
                    let checksum = if reading {
                        buffer.iter().map(|b| u64::from(*b)).sum()
                    } else {
                        0
                    };
                    // There is only one submitted operation; a full completion
                    // slot is an invariant violation, never a blocking send.
                    if completed
                        .try_send(Completion {
                            observation,
                            checksum,
                        })
                        .is_err()
                    {
                        break;
                    }
                    worker_entered.store(false, Ordering::Release);
                }
            })?;
        Ok(Self {
            command,
            completion,
            join: Some(join),
            stop,
            entered,
            barrier,
            release,
        })
    }
    fn dispatch(&self, size: usize) -> io::Result<()> {
        if self.stop.load(Ordering::Acquire) {
            return Err(failure("dispatch after stop"));
        }
        self.command
            .try_send(size)
            .map_err(|_| failure("bounded operation mailbox occupied"))
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        // Even panic/error unwinding cannot detach a JoinHandle or free storage
        // still on its worker's stack. This is a binary fixture boundary only.
        if self.join.is_some() {
            std::process::exit(2);
        }
    }
}
fn stop_and_join(
    reader: &mut Worker,
    writer: &mut Worker,
    evidence: &mut Evidence,
    unresolved: bool,
) -> io::Result<()> {
    reader.stop.store(true, Ordering::Release);
    writer.stop.store(true, Ordering::Release);
    evidence.mark("stop",json!({"before_further_dispatch":true,"reader_owner":reader.join.is_some(),"writer_owner":writer.join.is_some()}))?;
    let start = Instant::now();
    let mut attempts = 0;
    loop {
        for worker in [&mut *reader, &mut *writer] {
            if let Some(join) = &worker.join {
                if join.is_finished() {
                    worker
                        .join
                        .take()
                        .unwrap()
                        .join()
                        .map_err(|_| failure("worker panic"))?;
                    if let Ok(completed) = worker.completion.try_recv() {
                        completed.observation.record(evidence, "stopped_worker")?;
                    }
                    evidence.mark("joined", json!({"actual_join":true}))?;
                } else {
                    native::cancel(join, evidence, attempts < 8)?;
                }
            }
        }
        attempts += 1;
        // First cancellation has happened while the race worker is held before
        // its syscall. Only now release it; subsequent cancellation must cover it.
        if !unresolved {
            reader.release.store(true, Ordering::Release);
            writer.release.store(true, Ordering::Release);
        }
        if reader.join.is_none() && writer.join.is_none() {
            evidence.mark(
                "cancel_complete",
                json!({"control_rounds":attempts,"both_joined":true}),
            )?;
            return Ok(());
        }
        if start.elapsed() >= Duration::from_secs(5) {
            let _ = evidence.mark("forced_exit",json!({"grace_ms":5000,"reader_owner":reader.join.is_some(),"writer_owner":writer.join.is_some(),"operation_storage":"retained_on_worker_stack","cancel_rounds":attempts}));
            std::process::exit(2);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
pub(super) fn run(
    input: &native::Endpoint,
    output: &native::Endpoint,
    case: &str,
    evidence: &mut Evidence,
) -> io::Result<()> {
    // Copy before starting either worker so a duplication failure cannot detach.
    let read_endpoint = input.copy()?;
    let write_endpoint = output.copy()?;
    let race = case == "cancel-before-call";
    let unresolved = case == "unresolved-shutdown";
    let mut reader = Worker::spawn(read_endpoint, true, race, unresolved)?;
    let mut writer = Worker::spawn(write_endpoint, false, false, false)?;
    let result = exercise(&reader, &writer, case, evidence);
    let joined = stop_and_join(&mut reader, &mut writer, evidence, unresolved);
    result.and(joined)
}
fn exercise(
    reader: &Worker,
    writer: &Worker,
    case: &str,
    evidence: &mut Evidence,
) -> io::Result<()> {
    let started = Instant::now();
    let mut ready = false;
    let mut bytes = 0;
    let flood = matches!(case, "full-stdout" | "slow-drain");
    let target = if flood {
        262144
    } else if case == "partial-output" {
        37
    } else {
        0
    };
    let mut written = 0;
    let mut outstanding = false;
    let mut output_pending = false;
    let mut last_progress = Instant::now();
    reader.dispatch(4096)?;
    loop {
        if started.elapsed() >= Duration::from_secs(10) {
            return Err(failure("owned experiment control watchdog"));
        }
        if matches!(case, "cancel-before-call" | "unresolved-shutdown") {
            if reader.barrier.load(Ordering::Acquire) {
                evidence.mark("before_native_call",json!({"stop_check_passed":true,"barrier_held":true,"worker_owner_retained":true}))?;
                return Ok(());
            }
            tick();
            continue;
        }
        match reader.completion.try_recv() {
            Ok(completed) => {
                if !ready || !matches!(completed.observation.progress, Ok(Progress::Pending)) {
                    completed.observation.record(evidence, "stdin")?;
                }
                match completed.observation.progress? {
                    Progress::Bytes(n) => {
                        bytes += n;
                        evidence.mark(
                            "input_bytes",
                            json!({"count":n,"checksum":completed.checksum}),
                        )?;
                        reader.dispatch(4096)?;
                    }
                    Progress::Pending => {
                        if !ready {
                            evidence.mark("ready_idle",json!({"result":"Pending","source":"completed_nonblocking_read","workers_retained":true}))?;
                            ready = true;
                        }
                        reader.dispatch(4096)?;
                    }
                    Progress::Eof => {
                        evidence.mark("eof", json!({"read_bytes":bytes}))?;
                        if target == 0 {
                            return Ok(());
                        }
                    }
                }
            }
            Err(TryRecvError::Empty) => {
                if !ready
                    && reader.entered.load(Ordering::Acquire)
                    && started.elapsed() >= Duration::from_millis(20)
                {
                    evidence.event("ready_idle", "stdin", "completion_mailbox.try_recv", true, (None, None), json!({"result":"Pending","source":"control_mailbox","native_read_status":null,"workers_retained":true}))?;
                    ready = true;
                }
            }
            Err(TryRecvError::Disconnected) => {
                return Err(failure("reader stopped before shutdown"));
            }
        }
        if target > 0 && !outstanding && written < target {
            writer.dispatch((target - written).min(4096))?;
            outstanding = true;
        }
        if outstanding {
            match writer.completion.try_recv() {
                Ok(completed) => {
                    if !output_pending
                        || !matches!(completed.observation.progress, Ok(Progress::Pending))
                    {
                        completed.observation.record(evidence, "stdout")?;
                    }
                    match completed.observation.progress? {
                        Progress::Bytes(n) => {
                            written += n;
                            last_progress = Instant::now();
                        }
                        Progress::Pending => {
                            if !output_pending {
                                evidence.mark(
                                    "output_pending",
                                    json!({"source":"native_nonblocking_write","written":written}),
                                )?;
                                output_pending = true;
                            }
                        }
                        Progress::Eof => return Err(failure("writer EOF")),
                    }
                    outstanding = false;
                }
                Err(TryRecvError::Empty) => {
                    if !output_pending
                        && writer.entered.load(Ordering::Acquire)
                        && last_progress.elapsed() >= Duration::from_millis(30)
                    {
                        evidence.mark("output_pending",json!({"source":"control_mailbox","native_write_status":null,"written":written}))?;
                        output_pending = true;
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    return Err(failure("writer stopped before shutdown"));
                }
            }
        }
        if target > 0 && written == target {
            evidence.mark("output_complete", json!({"written":written}))?;
            return Ok(());
        }
        if case == "full-stdout"
            && output_pending
            && last_progress.elapsed() >= Duration::from_secs(5)
        {
            evidence.mark(
                "stall",
                json!({"positive_progress_timeout_ms":5000,"written":written}),
            )?;
            return Ok(());
        }
        tick();
    }
}
