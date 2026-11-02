//! Test-only ordinary-pipe qualification. No protocol or engine is reachable.
#[cfg(unix)]
#[path = "stdio_preflight/unix.rs"]
mod native;
#[cfg(windows)]
#[path = "stdio_preflight/windows.rs"]
mod native;
#[path = "stdio_preflight/owned.rs"]
mod owned;

use serde_json::{Value, json};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, Instant};

pub(super) const CASES: &[&str] = &[
    "idle-then-eof",
    "immediate-eof",
    "zero-byte-write",
    "partial-input-eof",
    "partial-output",
    "full-stdout",
    "slow-drain",
    "stderr-stall",
    "stderr-alias",
    "reduced-access",
    "named-byte",
    "message-pipe",
    "overlapped-pipe",
    "file",
    "terminal",
    "socket",
    "parent-idle-duplicate",
    "mode-sharing",
    "restore-failure",
    "startup-partial-rollback",
    "cancel-before-call",
    "unresolved-shutdown",
];
pub(super) fn failure(message: &str) -> io::Error {
    io::Error::other(message)
}
pub(super) fn tick() {
    std::thread::sleep(Duration::from_millis(2));
}

pub(super) struct Evidence {
    file: File,
    size: usize,
    candidate: String,
    case: String,
    events: usize,
    qualification_failure: Option<usize>,
}
impl Evidence {
    fn new(path: &Path, candidate: &str, case: &str) -> io::Result<Self> {
        Ok(Self {
            file: OpenOptions::new().write(true).create_new(true).open(path)?,
            size: 0,
            candidate: candidate.into(),
            case: case.into(),
            events: 0,
            qualification_failure: None,
        })
    }
    pub(super) fn event(
        &mut self,
        phase: &str,
        endpoint: &str,
        api: &str,
        ok: bool,
        native: (Option<i64>, Option<i32>),
        observed: Value,
    ) -> io::Result<()> {
        let (status, error) = native;
        let event = json!({"format_version":1,"candidate":self.candidate,"case":self.case,
            "phase":phase,"endpoint":endpoint,"api":api,"ok":ok,"raw_status":status,
            "win32_error":if cfg!(windows) {error} else {None},"observed":observed});
        let mut bytes = serde_json::to_vec(&event)?;
        bytes.push(b'\n');
        if self.size + bytes.len() > 65536 {
            return Err(failure("sidecar bound exceeded"));
        }
        self.file.write_all(&bytes)?;
        self.file.flush()?;
        self.size += bytes.len();
        if !ok
            && matches!(
                phase,
                "duplicate" | "query" | "qualified" | "mode" | "set_nowait" | "mode_sharing"
            )
        {
            self.qualification_failure = Some(self.events);
        }
        self.events += 1;
        Ok(())
    }
    pub(super) fn mark(&mut self, phase: &str, observed: Value) -> io::Result<()> {
        self.event(phase, "control", "fixture", true, (None, None), observed)
    }
}

#[derive(Debug)]
pub(super) enum Progress {
    Bytes(usize),
    Pending,
    Eof,
}
pub(super) struct Observation {
    pub progress: io::Result<Progress>,
    pub api: &'static str,
    pub status: i64,
    pub error: Option<i32>,
}
impl Observation {
    pub(super) fn record(&self, evidence: &mut Evidence, endpoint: &str) -> io::Result<()> {
        let (kind, count) = match &self.progress {
            Ok(Progress::Bytes(n)) => ("Bytes", Some(*n)),
            Ok(Progress::Pending) => ("Pending", None),
            Ok(Progress::Eof) => ("Eof", None),
            Err(_) => ("Error", None),
        };
        evidence.event(
            "io",
            endpoint,
            self.api,
            self.progress.is_ok(),
            (Some(self.status), self.error),
            json!({"result":kind,"count":count,"errno":if cfg!(unix) {self.error} else {None}}),
        )
    }
}

fn direct(
    input: &native::Endpoint,
    output: &native::Endpoint,
    case: &str,
    evidence: &mut Evidence,
) -> io::Result<()> {
    let start = Instant::now();
    let mut buffer = [0; 4096];
    let mut read_total = 0;
    let mut written = 0;
    let mut ready = false;
    let mut blocked = false;
    let flood = matches!(case, "full-stdout" | "slow-drain");
    let target = if flood {
        262144
    } else if case == "partial-output" {
        37
    } else {
        0
    };
    let mut last_progress = Instant::now();
    loop {
        if start.elapsed() > Duration::from_secs(10) {
            return Err(failure("direct operation watchdog"));
        }
        let observation = native::read(input, &mut buffer);
        let is_pending = matches!(observation.progress, Ok(Progress::Pending));
        if !is_pending || !ready {
            observation.record(evidence, "stdin")?;
        }
        match observation.progress? {
            Progress::Pending if !ready => {
                evidence.mark(
                    "ready_idle",
                    json!({"result":"Pending","source":"native_nonblocking_read"}),
                )?;
                ready = true;
            }
            Progress::Bytes(n) => {
                read_total += n;
            }
            Progress::Eof => {
                evidence.mark("eof", json!({"read_bytes":read_total}))?;
                if !flood || written == target {
                    break;
                }
            }
            _ => {}
        }
        if written < target {
            let count = (target - written).min(4096);
            let observation = native::write(output, &[b'O'; 4096][..count]);
            if !matches!(observation.progress, Ok(Progress::Pending)) || !blocked {
                observation.record(evidence, "stdout")?;
            }
            match observation.progress? {
                Progress::Bytes(n) => {
                    written += n;
                    last_progress = Instant::now();
                }
                Progress::Pending => {
                    if !blocked {
                        evidence.mark("output_pending", json!({"written":written}))?;
                        blocked = true;
                    }
                }
                Progress::Eof => return Err(failure("unexpected output EOF")),
            }
        }
        if case == "partial-output" && written == target {
            evidence.mark("output_complete", json!({"written":written}))?;
            break;
        }
        if flood && written == target {
            evidence.mark("output_complete", json!({"written":written}))?;
            break;
        }
        if case == "full-stdout" && blocked && last_progress.elapsed() >= Duration::from_secs(5) {
            evidence.mark(
                "stall",
                json!({"positive_progress_timeout_ms":5000,"written":written}),
            )?;
            break;
        }
        if matches!(case, "cancel-before-call" | "unresolved-shutdown") {
            break;
        }
        tick();
    }
    Ok(())
}

pub(super) fn main() -> ! {
    // This fixture never diagnoses through either pipe under qualification.
    std::panic::set_hook(Box::new(|_| {}));
    let args: Vec<_> = std::env::args_os().skip(2).collect();
    let [case_arg, path] = args.as_slice() else {
        std::process::exit(2)
    };
    let Some((candidate, case)) = case_arg.to_str().and_then(|s| s.split_once('/')) else {
        std::process::exit(2)
    };
    if !matches!(candidate, "direct_nowait" | "owned_io_experiment")
        || (!CASES.contains(&case) && case != "startup-early-failure")
    {
        std::process::exit(2)
    }
    let Ok(mut evidence) = Evidence::new(Path::new(path), candidate, case) else {
        std::process::exit(2)
    };
    let result = run(candidate, case, &mut evidence);
    let passed = result.is_ok();
    let _ = evidence.event(
        "result",
        "control",
        "fixture",
        result.is_ok(),
        (
            None,
            result.as_ref().err().and_then(io::Error::raw_os_error),
        ),
        json!({"pass":result.is_ok(),"error":result.err().map(|e|e.to_string())}),
    );
    std::process::exit(if passed { 0 } else { 2 })
}
fn reverse(
    input: &mut Option<native::Endpoint>,
    output: &mut Option<native::Endpoint>,
    evidence: &mut Evidence,
    fault: bool,
) -> io::Result<()> {
    let out = output
        .as_mut()
        .map_or(Ok(()), |endpoint| endpoint.restore(evidence, fault));
    let read = input
        .as_mut()
        .map_or(Ok(()), |endpoint| endpoint.restore(evidence, false));
    out.and(read)?;
    // Release acquired handles before recording completed rollback/normal close.
    drop(output.take());
    drop(input.take());
    evidence.mark(
        "restored",
        json!({"reverse_order":true,"acquired_handles_closed":true}),
    )
}
fn run(candidate: &str, case: &str, evidence: &mut Evidence) -> io::Result<()> {
    evidence.mark("started", json!({"pid":std::process::id(),"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"stderr":"omitted_unqualified"}))?;
    if native::control(case, candidate, evidence)? {
        return Ok(());
    }
    let (mut input, mut output) = (None, None);
    let mut injection = None;
    let qualified = (|| {
        input = Some(native::Endpoint::standard(
            if case == "startup-early-failure" {
                1
            } else {
                0
            },
            evidence,
        )?);
        if case == "startup-early-failure" {
            input.as_mut().unwrap().expect_read_access();
        }
        output = Some(native::Endpoint::standard(1, evidence)?);
        input.as_ref().unwrap().qualify(evidence)?;
        output.as_ref().unwrap().qualify(evidence)?;
        if cfg!(windows) && candidate == "owned_io_experiment" && case == "startup-partial-rollback"
        {
            injection = Some("acquired_handles");
            evidence.mark("rollback_injection",json!({"obligation":"acquired_handles","acquired_endpoints":2,"mutated_endpoints":0}))?;
            return Err(failure("injected failure after owned endpoint acquisition"));
        }
        if candidate == "direct_nowait" || cfg!(unix) {
            input.as_mut().unwrap().nowait(evidence)?;
            if case == "startup-partial-rollback" {
                injection = Some("mutated_input_mode");
                evidence.mark("rollback_injection",json!({"obligation":"mutated_input_mode","acquired_endpoints":2,"mutated_endpoints":1}))?;
                return Err(failure("injected failure after first mode mutation"));
            }
            output.as_mut().unwrap().nowait(evidence)?;
        }
        if case == "mode-sharing" {
            input.as_ref().unwrap().verify_sharing(evidence)?;
        }
        Ok(())
    })();
    if let Err(error) = qualified {
        let native_failure = evidence.qualification_failure;
        let acquired = usize::from(input.is_some()) + usize::from(output.is_some());
        let mutated = input.as_ref().is_some_and(native::Endpoint::mode_changed) as usize
            + output.as_ref().is_some_and(native::Endpoint::mode_changed) as usize;
        reverse(&mut input, &mut output, evidence, false)?;
        evidence.mark(
            "qualification_rollback",
            json!({"complete":true,"acquired_endpoints":acquired,"mutated_endpoints":mutated}),
        )?;
        if let Some(obligation) = injection {
            if acquired != 2 || (obligation == "mutated_input_mode" && mutated != 1) {
                return Err(failure("rollback injection obligation was not retained"));
            }
            evidence.mark("rollback",json!({"injection_reached":true,"obligation":obligation,"reverse_verified":true,"acquired_endpoints":acquired,"mutated_endpoints":mutated}))?;
            return Ok(());
        }
        if matches!(case, "startup-partial-rollback" | "startup-early-failure") {
            evidence.mark("rollback_unexercised", json!({"injection_reached":false}))?;
        }
        if let Some(index) = native_failure {
            evidence.mark(
                "native_incompatibility",
                json!({"failure_event":index,"rollback_complete":true,"before_io":true}),
            )?;
        }
        return Err(error);
    }
    let operation = if candidate == "owned_io_experiment" {
        owned::run(
            input.as_ref().unwrap(),
            output.as_ref().unwrap(),
            case,
            evidence,
        )
    } else {
        direct(
            input.as_ref().unwrap(),
            output.as_ref().unwrap(),
            case,
            evidence,
        )
    };
    let restored = reverse(
        &mut input,
        &mut output,
        evidence,
        case == "restore-failure" && operation.is_ok(),
    );
    if case == "restore-failure" && operation.is_ok() {
        if restored.is_ok() {
            return Err(failure("restore fault was not exercised"));
        }
        evidence.mark(
            "expected_restore_failure",
            json!({"native_failure_observed":true}),
        )?;
        return Err(failure(
            "restoration intentionally failed; fatal boundary required",
        ));
    }
    restored?;
    operation
}
