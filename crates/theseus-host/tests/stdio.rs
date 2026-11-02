#![cfg(feature = "test-support")]

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
fn fixture() -> std::path::PathBuf {
    std::env::var_os("THESEUS_HOST_TEST_FIXTURE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_theseus-host-fixture").into())
}

// This catches accidentally qualifying the old prototype while production uses
// a different transport, as well as empty-live-input being mistaken for EOF.
#[test]
fn ordinary_pipes_exercise_production_pending_bytes_eof_and_shutdown() {
    let directory = tempfile::tempdir().unwrap();
    let sidecar = directory.path().join("native.jsonl");
    let mut child = Command::new(fixture())
        .args(["stdio-preflight", "owned_io_experiment/partial-input-eof"])
        .arg(&sidecar)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    let status = loop {
        let text = std::fs::read_to_string(&sidecar).unwrap_or_default();
        if text.contains("\"phase\":\"ready_idle\"")
            && let Some(mut input) = child.stdin.take()
        {
            input.write_all(b"abc").unwrap();
        }
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() >= Duration::from_secs(15) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("production stdio child timed out");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let mut errors = Vec::new();
    child
        .stderr
        .take()
        .unwrap()
        .take(1024)
        .read_to_end(&mut errors)
        .unwrap();
    let text = std::fs::read_to_string(&sidecar).unwrap();
    let events: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(status.success(), "{status}: {text}");
    assert!(errors.is_empty(), "unexpected stdio diagnostics");
    assert!(
        events.iter().any(|e| e["phase"] == "production_transport"),
        "ordinary launcher still exercises prototype: {text}"
    );
    assert!(
        events
            .iter()
            .any(|e| e["phase"] == "eof" && e["observed"]["read_bytes"] == 3),
        "{text}"
    );
    assert!(events.iter().any(|e| e["phase"] == "restored"), "{text}");
    for event in events.iter().filter(|e| e["phase"] == "io") {
        let requested = event["observed"]["requested"]
            .as_u64()
            .expect("production native operation size");
        assert!(
            (1..=4096).contains(&requested),
            "unbounded native operation: {event}"
        );
    }
}

// A restore error must retain the actual changed endpoints through the binary
// deadline. The fault is explicitly synthetic; the retained flags/owners are real.
#[test]
fn failed_restore_retains_production_owner_until_binary_exit() {
    let directory = tempfile::tempdir().unwrap();
    let sidecar = directory.path().join("retained.jsonl");
    let mut child = Command::new(fixture())
        .args([
            "stdio-preflight",
            "owned_io_experiment/retained-restore-failure",
        ])
        .arg(&sidecar)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() >= Duration::from_secs(15) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("restore owner watchdog");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let text = std::fs::read_to_string(sidecar).unwrap_or_default();
    let events: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(status.code(), Some(2), "{text}");
    assert!(
        events
            .iter()
            .any(|e| e["phase"] == "retained_restore_failure"
                && e["observed"]["settled"] == false
                && e["observed"]["synthetic_fault"] == true),
        "missing retained restore owner: {text}"
    );
    assert!(
        events
            .iter()
            .any(|e| e["phase"] == "forced_exit"
                && e["observed"]["production_owner_retained"] == true),
        "{text}"
    );
    let native = &events
        .iter()
        .find(|e| e["phase"] == "retained_restore_failure")
        .unwrap()["observed"]["native_ownership"];
    assert_eq!(native["owned_endpoints"], 2);
    #[cfg(unix)]
    {
        assert_eq!(native["saved_modes"], 2);
        assert!(native["actual_input_flags"].as_i64().unwrap() >= 0);
        assert!(native["actual_output_flags"].as_i64().unwrap() >= 0);
        assert_ne!(
            native["actual_input_flags"].as_i64().unwrap() & i64::from(libc::O_NONBLOCK),
            0
        );
        assert_ne!(
            native["actual_output_flags"].as_i64().unwrap() & i64::from(libc::O_NONBLOCK),
            0
        );
    }
    #[cfg(windows)]
    {
        assert_eq!(native["retained_joins"], 0);
        assert_eq!(native["actual_input_type"], 3);
        assert_eq!(native["actual_output_type"], 3);
    }
    assert!(start.elapsed() >= Duration::from_secs(5));
    assert!(
        !events.iter().any(|e| e["phase"] == "restored"),
        "failed restore must not claim success"
    );
}

#[cfg(windows)]
#[test]
fn pending_write_keeps_prefix_after_peer_received_bytes_before_ack() {
    for case in ["pending-write-before-ack", "pending-write-cancel"] {
        let directory = tempfile::tempdir().unwrap();
        let sidecar = directory.path().join("pending-write.jsonl");
        let mut child = Command::new(fixture())
            .arg("stdio-preflight")
            .arg(format!("owned_io_experiment/{case}"))
            .arg(&sidecar)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut pipe = child.stdout.take().unwrap();
        let received = std::thread::spawn(move || {
            let mut bytes = [0; 8];
            pipe.read_exact(&mut bytes).unwrap();
            bytes
        });
        let start = Instant::now();
        while !received.is_finished() {
            if start.elapsed() >= Duration::from_secs(10) {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("pending write did not reach peer");
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(received.join().unwrap(), *b"ORIGINAL");
        child.stdin.as_mut().unwrap().write_all(b"a").unwrap();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if start.elapsed() >= Duration::from_secs(15) {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("write ack watchdog");
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        let text = std::fs::read_to_string(sidecar).unwrap();
        let events: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(status.success(), "{text}");
        assert!(
            events
                .iter()
                .any(|e| e["phase"] == "pending_write_ownership"
                    && e["observed"]["peer_received_before_ack"] == true
                    && e["observed"]["replacement_rejected"] == true
                    && e["observed"]["mutation_rejected"] == true),
            "{text}"
        );
    }
}

#[test]
fn opposite_ends_of_one_pipe_are_rejected_as_stdio_self_loop() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("alias.jsonl");
    let mut child = Command::new(fixture())
        .args(["stdio-preflight", "owned_io_experiment/same-pipe-alias"])
        .arg(&path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(10) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("alias control watchdog");
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    let text = std::fs::read_to_string(path).unwrap_or_default();
    assert!(
        status.success(),
        "actual self-loop profile was not rejected: {text}"
    );
    let events: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        events.iter().any(|e| e["phase"] == "alias_rejected"),
        "self-loop failed before alias comparison: {text}"
    );
    assert!(
        events
            .iter()
            .any(|e| e["phase"] == "rejected" && e["observed"]["actual_self_loop"] == true),
        "{text}"
    );
}

#[cfg(windows)]
#[test]
fn partial_worker_start_failure_retains_and_joins_the_actual_reader() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("worker-start.jsonl");
    let mut child = Command::new(fixture())
        .args([
            "stdio-preflight",
            "owned_io_experiment/worker-startup-rollback",
        ])
        .arg(&path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(10) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("worker startup rollback watchdog");
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    let text = std::fs::read_to_string(path).unwrap_or_default();
    assert!(status.success(), "{text}");
    let events: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let retained = events
        .iter()
        .find(|e| e["phase"] == "retained_start_failure")
        .unwrap();
    assert_eq!(retained["observed"]["retained_joins"], 1);
    assert_eq!(retained["observed"]["owned_endpoints"], 2);
    assert_eq!(
        events
            .iter()
            .filter(|e| e["phase"] == "joined" && e["observed"]["actual_join"] == true)
            .count(),
        1
    );
    assert!(
        events
            .iter()
            .any(|e| e["phase"] == "restored" && e["observed"]["acquired_handles_closed"] == true)
    );
}
