#![cfg(feature = "test-support")]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn ordinary_rust_empty_live_input_is_observed_before_eof() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<theseus_host::fs::HostDir>();
    let temporary = tempfile::tempdir().unwrap();
    let evidence = temporary.path().join("native.jsonl");
    let mut child = Command::new(env!("CARGO_BIN_EXE_theseus-host-fixture"))
        .args(["stdio-preflight", "owned_io_experiment/idle-then-eof"])
        .arg(&evidence)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut ready = false;
    let status = loop {
        let bytes = std::fs::read(&evidence).unwrap_or_default();
        assert!(bytes.len() <= 65536);
        for line in bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            if let Ok(event) = serde_json::from_slice::<serde_json::Value>(line)
                && event["phase"] == "ready_idle"
            {
                ready = true;
            }
        }
        if ready {
            drop(child.stdin.take());
        }
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("ordinary stdio supervisor expired");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let raw = std::fs::read_to_string(evidence).unwrap_or_default();
    assert!(ready, "missing measured empty-open Pending: {raw}");
    assert!(status.success(), "fixture failed: {status}: {raw}");
    let events: Vec<serde_json::Value> = raw
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(events.iter().any(|event| event["phase"] == "eof"));
    assert!(events.iter().any(|event| event["phase"]
        == if cfg!(windows) {
            "joined"
        } else {
            "synchronous_io_stopped"
        }));
    assert!(events.iter().any(|event| event["phase"] == "restored"));
}

const CASES: &[&str] = &[
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

struct Supervised(std::process::Child);
impl Drop for Supervised {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
fn read_events(path: &std::path::Path) -> Vec<serde_json::Value> {
    let bytes = std::fs::read(path).unwrap_or_default();
    assert!(bytes.len() <= 65536, "native sidecar bound");
    let mut lines = bytes.split(|byte| *byte == b'\n').collect::<Vec<_>>();
    lines.pop(); // an in-flight incomplete trailing line is not evidence
    lines
        .into_iter()
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect()
}
fn has(events: &[serde_json::Value], phase: &str) -> bool {
    events.iter().any(|event| event["phase"] == phase)
}
fn peer_write(child: &std::process::ChildStdin, bytes: &[u8]) {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        assert_eq!(
            unsafe { libc::write(child.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) },
            bytes.len() as isize
        );
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        let mut count = 0;
        assert_ne!(
            unsafe {
                windows_sys::Win32::Storage::FileSystem::WriteFile(
                    child.as_raw_handle(),
                    bytes.as_ptr(),
                    bytes.len() as u32,
                    &mut count,
                    std::ptr::null_mut(),
                )
            },
            0
        );
        assert_eq!(count as usize, bytes.len());
    }
}
fn idle_duplicate(child: &std::process::ChildStdin) -> std::fs::File {
    #[cfg(unix)]
    {
        use std::os::fd::{AsFd, OwnedFd};
        let owned: OwnedFd = child.as_fd().try_clone_to_owned().unwrap();
        owned.into()
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::{AsHandle, OwnedHandle};
        let owned: OwnedHandle = child.as_handle().try_clone_to_owned().unwrap();
        owned.into()
    }
}
fn ordinary_case(
    fixture: &std::path::Path,
    root: &std::path::Path,
    candidate: &str,
    case: &str,
) -> serde_json::Value {
    use std::io::Read;
    let path = root.join(format!("rust-{candidate}-{case}.native.jsonl"));
    let started = Instant::now();
    // No alternate rights, custom pipes, shells, PTYs or named-pipe substitution.
    let mut child = Supervised(
        Command::new(fixture)
            .arg("stdio-preflight")
            .arg(format!("{candidate}/{case}"))
            .arg(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut duplicate =
        (case == "parent-idle-duplicate").then(|| idle_duplicate(child.0.stdin.as_ref().unwrap()));
    if case == "immediate-eof" {
        drop(child.0.stdin.take());
    }
    let mut zero_at = None;
    let mut drainer = None;
    let mut timed_out = false;
    let status = loop {
        let events = read_events(&path);
        if has(&events, "ready_idle")
            && child.0.stdin.is_some()
            && !matches!(case, "full-stdout" | "slow-drain" | "partial-output")
        {
            if case == "zero-byte-write" {
                if zero_at.is_none() {
                    peer_write(child.0.stdin.as_ref().unwrap(), b"");
                    zero_at = Some(Instant::now());
                }
                assert!(!has(&events, "eof"), "zero write fabricated EOF");
            }
            if zero_at.is_none_or(|time| time.elapsed() >= Duration::from_millis(100)) {
                if case == "partial-input-eof" {
                    peer_write(child.0.stdin.as_ref().unwrap(), b"abc");
                }
                drop(child.0.stdin.take());
                drop(duplicate.take());
            }
        }
        if case == "slow-drain" && has(&events, "output_pending") && drainer.is_none() {
            let mut pipe = child.0.stdout.take().unwrap();
            drainer = Some(std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let mut block = [0; 1024];
                loop {
                    let count = pipe.read(&mut block).unwrap();
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&block[..count]);
                    assert!(bytes.len() <= 262144);
                    std::thread::sleep(Duration::from_millis(1));
                }
                bytes
            }));
        }
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() >= Duration::from_secs(15) {
            timed_out = true;
            child.0.kill().unwrap();
            break child.0.wait().unwrap();
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    drop(child.0.stdin.take());
    drop(duplicate.take());
    let output = if let Some(join) = drainer {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !join.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        if !join.is_finished() {
            std::process::exit(3);
        }
        join.join().unwrap()
    } else {
        let mut bytes = Vec::new();
        child
            .0
            .stdout
            .take()
            .unwrap()
            .take(262145)
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    };
    let mut errors = Vec::new();
    child
        .0
        .stderr
        .take()
        .unwrap()
        .take(513)
        .read_to_end(&mut errors)
        .unwrap();
    let events = read_events(&path);
    let measurement_complete = std::fs::read(&path).is_ok_and(|raw| raw.ends_with(b"\n"))
        && events.first().is_some_and(|e| e["phase"] == "started")
        && events.iter().all(|e| {
            e.as_object().is_some_and(|o| o.len() == 10)
                && e["format_version"] == 1
                && e["candidate"] == candidate
                && e["case"] == case
        })
        && (events
            .last()
            .is_some_and(|e| e["phase"] == "result" && e["observed"]["pass"].is_boolean())
            || (candidate == "owned_io_experiment"
                && case == "unresolved-shutdown"
                && has(&events, "forced_exit")));
    let result_consistent = events.last().is_some_and(|e| {
        e["phase"] == "result"
            && e["ok"] == status.success()
            && e["observed"]["pass"] == status.success()
    });
    let native_incompatibility = measurement_complete
        && result_consistent
        && status.code() == Some(2)
        && output.is_empty()
        && errors.is_empty()
        && is_native_incompatibility(&events);
    let normal = status.success() && result_consistent;
    let fault = case == "restore-failure"
        && status.code() == Some(2)
        && has(&events, "expected_restore_failure");
    let forced = candidate == "owned_io_experiment"
        && case == "unresolved-shutdown"
        && status.code() == Some(2)
        && events.iter().any(|e| {
            e["phase"] == "forced_exit"
                && e["observed"]["reader_owner"] == true
                && e["observed"]["grace_ms"] == 5000
        })
        && output.is_empty()
        && started.elapsed() >= Duration::from_secs(5);
    let production_measured = candidate != "owned_io_experiment"
        || (has(&events, "production_transport")
            && events
                .iter()
                .filter(|e| {
                    e["phase"] == "io" && matches!(e["endpoint"].as_str(), Some("stdin" | "stdout"))
                })
                .all(|e| {
                    e["observed"]["requested"]
                        .as_u64()
                        .is_some_and(|n| (1..=4096).contains(&n))
                }));
    let passed = measurement_complete
        && production_measured
        && !timed_out
        && errors.is_empty()
        && output.iter().all(|byte| *byte == b'O')
        && output.len() <= 262144
        && ((normal && normal_case_contract(&events, candidate, case, &output))
            || (fault
                && result_consistent
                && has(&events, "restore_fault")
                && has(&events, "eof"))
            || forced);
    let result = serde_json::json!({"launcher":"rust_command_stdio_piped","candidate":candidate,"case":case,"fixture":fixture,
        "source_sha":option_env!("GITHUB_SHA"),"exit_code":status.code(),"watchdog":if timed_out {"expired"}else{"completed"},"test_pass":passed,
        "elapsed_ms":started.elapsed().as_millis(),"stdout_bytes":output.len(),"native_result":events.iter().rev().find(|e|e["phase"]=="result"),
        "native_incompatibility":native_incompatibility,"measurement_complete":measurement_complete,"expected_fatal_control":fault||forced});
    std::fs::write(
        root.join(format!("rust-{candidate}-{case}.supervisor.json")),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    result
}

#[test]
fn ordinary_rust_complete_qualification_matrix_retains_both_candidates() {
    let temporary = if let Some(root) = std::env::var_os("THESEUS_HOST_EVIDENCE") {
        tempfile::Builder::new()
            .prefix("mcp-stdio-rust-")
            .tempdir_in(root)
            .unwrap()
    } else {
        tempfile::tempdir().unwrap()
    };
    let fixture = std::env::var_os("THESEUS_HOST_TEST_FIXTURE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_theseus-host-fixture").into());
    let python = if cfg!(windows) { "python" } else { "python3" };
    let metadata = Command::new(python).args(["-c", "import hashlib,json,pathlib,platform,subprocess,sys; p=pathlib.Path(sys.argv[1]); print(json.dumps({'fixture':str(p),'sha256':hashlib.sha256(p.read_bytes()).hexdigest(),'source_sha':subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip(),'rustc':subprocess.check_output(['rustc','-Vv'],text=True),'os':platform.platform(),'machine':platform.machine(),'launcher':'rust_command_stdio_piped'}))"])
        .arg(&fixture).output().unwrap();
    assert!(
        metadata.status.success(),
        "supervisor identity collection failed"
    );
    std::fs::write(temporary.path().join("identity.json"), metadata.stdout).unwrap();
    let mut results = Vec::new();
    for case in CASES {
        for candidate in ["direct_nowait", "owned_io_experiment"] {
            let result = ordinary_case(&fixture, temporary.path(), candidate, case);
            println!("{result}");
            results.push(result);
        }
    }
    let passed = results.iter().all(qualification_case_pass);
    std::fs::write(
        temporary.path().join("summary.json"),
        serde_json::to_vec_pretty(&results).unwrap(),
    )
    .unwrap();
    let retained = temporary.keep();
    assert!(
        passed,
        "owned compatibility or supervision failed; evidence: {}",
        retained.display()
    );
}

fn qualification_case_pass(result: &serde_json::Value) -> bool {
    result["watchdog"] == "completed"
        && result["measurement_complete"] == true
        && (result["test_pass"] == true
            || (result["candidate"] == "direct_nowait" && result["native_incompatibility"] == true))
}
#[test]
fn direct_failures_are_not_blanket_waived() {
    for reason in [
        "missing_eof",
        "missing_restore",
        "wrong_bytes",
        "internal_watchdog",
        "trailing_evidence",
        "corrupt_evidence",
    ] {
        let result = serde_json::json!({"candidate":"direct_nowait","watchdog":"completed",
            "measurement_complete":true,"test_pass":false,"native_incompatibility":false,"error":reason});
        assert!(!qualification_case_pass(&result), "waived {reason}");
    }
}

#[test]
fn early_qualification_failure_is_measured_without_claiming_rollback_injection() {
    let temporary = tempfile::tempdir().unwrap();
    let result = ordinary_case(
        std::path::Path::new(env!("CARGO_BIN_EXE_theseus-host-fixture")),
        temporary.path(),
        "direct_nowait",
        "startup-early-failure",
    );
    assert_eq!(
        result["measurement_complete"], true,
        "missing native early-failure measurement"
    );
    assert_eq!(
        result["test_pass"], false,
        "early failure must not claim support"
    );
    let events = read_events(
        &temporary
            .path()
            .join("rust-direct_nowait-startup-early-failure.native.jsonl"),
    );
    assert!(!has(&events, "rollback_injection"));
    assert!(!has(&events, "rollback"));
    assert!(has(&events, "rollback_unexercised"));
    assert_eq!(result["native_incompatibility"], true);
}

fn is_native_incompatibility(events: &[serde_json::Value]) -> bool {
    if events.last().is_none_or(|e| {
        e["phase"] != "result" || e["ok"] != false || e["observed"]["pass"] != false
    }) {
        return false;
    }
    if events
        .last()
        .and_then(|e| e["observed"]["error"].as_str())
        .is_some_and(|message| message.to_ascii_lowercase().contains("watchdog"))
    {
        return false;
    }
    let forbidden = [
        "io",
        "ready_idle",
        "eof",
        "output_pending",
        "output_complete",
        "stall",
        "stop",
        "joined",
        "before_native_call",
        "forced_exit",
        "rollback_injection",
        "rollback",
    ];
    if events.iter().any(|e| {
        e["phase"]
            .as_str()
            .is_some_and(|phase| forbidden.contains(&phase))
    }) {
        return false;
    }
    let markers: Vec<_> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| e["phase"] == "native_incompatibility")
        .collect();
    let [(marker_index, marker)] = markers.as_slice() else {
        return false;
    };
    let Some(index) = marker["observed"]["failure_event"]
        .as_u64()
        .and_then(|i| usize::try_from(i).ok())
    else {
        return false;
    };
    if index >= *marker_index
        || marker["observed"]["rollback_complete"] != true
        || marker["observed"]["before_io"] != true
    {
        return false;
    }
    let failed = &events[index];
    let phases = [
        "duplicate",
        "query",
        "qualified",
        "mode",
        "set_nowait",
        "mode_sharing",
    ];
    let apis = [
        "DuplicateHandle",
        "fcntl(F_DUPFD_CLOEXEC)",
        "NtQueryInformationFile",
        "GetFileType",
        "fstat",
        "fstat/F_GETFL",
        "native pipe profile",
        "GetNamedPipeHandleStateW",
        "SetNamedPipeHandleState",
        "fcntl(F_SETFL)",
        "GetNamedPipeHandleStateW duplicate",
        "fcntl(F_GETFL duplicate)",
    ];
    if failed["ok"] != false
        || !failed["phase"]
            .as_str()
            .is_some_and(|p| phases.contains(&p))
        || !failed["api"]
            .as_str()
            .is_some_and(|api| apis.contains(&api))
    {
        return false;
    }
    let restored: Vec<_> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            e["phase"] == "restored"
                && e["observed"]["reverse_order"] == true
                && e["observed"]["acquired_handles_closed"] == true
        })
        .map(|(i, _)| i)
        .collect();
    let rolled: Vec<_> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            e["phase"] == "qualification_rollback" && e["observed"]["complete"] == true
        })
        .map(|(i, _)| i)
        .collect();
    matches!((restored.as_slice(),rolled.as_slice()),([r],[b]) if index<*r && r<b && *b<*marker_index && *marker_index<events.len()-1)
}

#[test]
fn native_incompatibility_requires_exact_failure_and_reversal() {
    let mut events = vec![
        serde_json::json!({"phase":"started"}),
        serde_json::json!({"phase":"qualified","api":"fstat/F_GETFL","ok":false}),
        serde_json::json!({"phase":"restored","observed":{"reverse_order":true,"acquired_handles_closed":true}}),
        serde_json::json!({"phase":"qualification_rollback","observed":{"complete":true}}),
        serde_json::json!({"phase":"native_incompatibility","observed":{"failure_event":1,"rollback_complete":true,"before_io":true}}),
        serde_json::json!({"phase":"result","ok":false,"observed":{"pass":false,"error":"endpoint access is incompatible"}}),
    ];
    assert!(is_native_incompatibility(&events));
    for phase in [
        "qualified",
        "restored",
        "qualification_rollback",
        "native_incompatibility",
    ] {
        let broken: Vec<_> = events
            .iter()
            .filter(|e| e["phase"] != phase)
            .cloned()
            .collect();
        assert!(
            !is_native_incompatibility(&broken),
            "missing {phase} was waived"
        );
    }
    events.last_mut().unwrap()["observed"]["error"] = "direct operation watchdog".into();
    assert!(!is_native_incompatibility(&events));
    events.last_mut().unwrap()["observed"]["error"] = "endpoint access is incompatible".into();
    events.insert(5, serde_json::json!({"phase":"io"}));
    assert!(!is_native_incompatibility(&events));
}

fn normal_case_contract(
    events: &[serde_json::Value],
    candidate: &str,
    case: &str,
    output: &[u8],
) -> bool {
    match case {
        "file" | "terminal" | "socket" | "reduced-access" => {
            return output.is_empty() && has(events, "rejected");
        }
        "message-pipe" | "overlapped-pipe" => {
            return output.is_empty()
                && (has(events, "rejected")
                    || events.iter().any(|e| {
                        e["phase"] == "platform_scope" && e["observed"]["applicable"] == false
                    }));
        }
        "named-byte" => {
            return output.is_empty()
                && has(events, "restored")
                && events
                    .iter()
                    .filter(|e| {
                        e["phase"] == "io" && e["observed"]["count"] == 8 && e["ok"] == true
                    })
                    .count()
                    == 2;
        }
        "startup-partial-rollback" => {
            let injection = events
                .iter()
                .find(|e| e["phase"] == "rollback_injection")
                .map(|e| &e["observed"]);
            let rollback = events
                .iter()
                .find(|e| e["phase"] == "rollback")
                .map(|e| &e["observed"]);
            return output.is_empty()
                && match (injection, rollback) {
                    (Some(i), Some(r)) => {
                        r["injection_reached"] == true
                            && r["reverse_verified"] == true
                            && i["obligation"] == r["obligation"]
                            && i["acquired_endpoints"] == 2
                            && r["acquired_endpoints"] == 2
                            && i["mutated_endpoints"] == r["mutated_endpoints"]
                            && has(events, "qualification_rollback")
                            && has(events, "restored")
                    }
                    _ => false,
                };
        }
        _ => {}
    }
    if !has(events, "restored")
        || (candidate == "owned_io_experiment"
            && if cfg!(windows) {
                events
                    .iter()
                    .filter(|e| e["phase"] == "joined" && e["observed"]["actual_join"] == true)
                    .count()
                    != 2
            } else {
                !events.iter().any(|e| {
                    e["phase"] == "synchronous_io_stopped"
                        && e["observed"]["outstanding_operations"] == 0
                        && e["observed"]["native_workers"] == 0
                })
            })
    {
        return false;
    }
    match case {
        "full-stdout" => {
            !output.is_empty() && has(events, "output_pending") && has(events, "stall")
        }
        "slow-drain" => {
            has(events, "output_complete")
                && output.len() == 262144
                && output.iter().all(|byte| *byte == b'O')
        }
        "partial-output" => has(events, "output_complete") && output == [b'O'; 37],
        "cancel-before-call" if candidate == "owned_io_experiment" => {
            output.is_empty()
                && has(events, "before_native_call")
                && has(events, "cancel_request")
                && has(events, "cancel_complete")
                && (!cfg!(windows) || {
                    let cancellations: Vec<_> = events
                        .iter()
                        .filter(|e| e["api"] == "CancelSynchronousIo")
                        .collect();
                    cancellations
                        .first()
                        .is_some_and(|e| e["win32_error"] == 1168)
                        && cancellations.iter().skip(1).any(|e| e["ok"] == true)
                        && events
                            .iter()
                            .any(|e| e["phase"] == "io" && e["win32_error"] == 995)
                })
        }
        "cancel-before-call" | "unresolved-shutdown" => output.is_empty(),
        _ => {
            output.is_empty()
                && (case == "immediate-eof" || has(events, "ready_idle"))
                && events.iter().any(|e| {
                    e["phase"] == "eof"
                        && e["observed"]["read_bytes"]
                            == if case == "partial-input-eof" { 3 } else { 0 }
                })
                && (case != "mode-sharing" || has(events, "mode_sharing"))
                && (case != "stderr-alias"
                    || events.iter().any(|e| {
                        e["phase"] == "stderr_policy"
                            && (e["observed"]["same_object"] == true
                                || e["observed"]["numeric_equal"] == true)
                            && e["observed"]["stderr_selected"] == false
                    }))
        }
    }
}
#[test]
fn direct_success_requires_eof_restoration_and_exact_output() {
    let events = vec![
        serde_json::json!({"phase":"ready_idle"}),
        serde_json::json!({"phase":"eof","observed":{"read_bytes":0}}),
        serde_json::json!({"phase":"restored"}),
    ];
    assert!(normal_case_contract(
        &events,
        "direct_nowait",
        "idle-then-eof",
        b""
    ));
    for missing in ["ready_idle", "eof", "restored"] {
        let broken: Vec<_> = events
            .iter()
            .filter(|e| e["phase"] != missing)
            .cloned()
            .collect();
        assert!(!normal_case_contract(
            &broken,
            "direct_nowait",
            "idle-then-eof",
            b""
        ));
    }
    assert!(!normal_case_contract(
        &events,
        "direct_nowait",
        "idle-then-eof",
        b"O"
    ));
}
