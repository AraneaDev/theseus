//! Ordinary inherited-pipe qualification of the actual production transport.
use super::{Evidence, failure, native, tick};
use serde_json::json;
use std::{
    io,
    time::{Duration, Instant},
};
use theseus_host::stdio::{AcquireFailure, ReadStep, Stdio, WriteStep, test_support};

pub(super) fn events(owner: &mut Stdio, evidence: &mut Evidence) -> io::Result<()> {
    record(test_support::take_events(owner), evidence)
}
fn failure_events(owner: &mut AcquireFailure, evidence: &mut Evidence) -> io::Result<()> {
    record(test_support::take_failure_events(owner), evidence)
}
fn record(events: Vec<test_support::Event>, evidence: &mut Evidence) -> io::Result<()> {
    for event in events {
        // Empty polls are represented by the first ready event; retain all
        // positive/native error observations and every ownership transition.
        if event.phase == "observer_overflow" {
            return Err(failure("native observer overflow"));
        }
        evidence.event(
            event.phase,
            event.endpoint,
            event.api,
            event.ok,
            (event.status, event.error),
            event.observed,
        )?;
    }
    Ok(())
}
fn restore_failure(owner: &mut AcquireFailure, evidence: &mut Evidence) -> io::Result<()> {
    owner.request_stop();
    let start = Instant::now();
    loop {
        let result = owner.poll_stop_or_restore();
        failure_events(owner, evidence)?;
        if matches!(result, Ok(true)) {
            return Ok(());
        }
        if start.elapsed() >= Duration::from_secs(5) {
            let _ = evidence.mark(
                "forced_exit",
                json!({"grace_ms":5000,"acquisition_owner":true,"settled":owner.is_settled()}),
            );
            std::process::exit(2);
        }
        tick();
    }
}
fn stop(owner: &mut Stdio, evidence: &mut Evidence) -> io::Result<()> {
    owner.request_stop();
    evidence.mark("stop",json!({"before_further_dispatch":true,"production_owner":true,"write_in_flight":owner.write_in_flight()}))?;
    let start = Instant::now();
    loop {
        let result = owner.poll_stop_or_restore();
        events(owner, evidence)?;
        if matches!(result, Ok(true)) {
            if cfg!(unix) {
                evidence.mark(
                    "synchronous_io_stopped",
                    json!({"outstanding_operations":0,"native_workers":0}),
                )?;
            }
            evidence.mark(
                "cancel_complete",
                json!({"settled":owner.is_settled(),"both_joined":cfg!(windows)}),
            )?;
            evidence.mark(
                "restored",
                json!({"reverse_order":true,"acquired_handles_closed":true}),
            )?;
            return Ok(());
        }
        result?;
        if start.elapsed() >= Duration::from_secs(5) {
            let _ = evidence.mark("forced_exit",json!({"grace_ms":5000,"reader_owner":true,"production_owner_retained":true,"operation_storage":"retained_in_production_owner","synthetic_control":true,"native_worker_barrier":cfg!(windows)}));
            std::process::exit(2);
        }
        tick();
    }
}

pub(super) fn run(case: &str, evidence: &mut Evidence) -> io::Result<()> {
    evidence.mark("production_transport",json!({"implementation":"theseus_host::stdio","mechanism":if cfg!(windows) {"owned_synchronous_workers"} else {"owned_nonblocking_descriptors"},"native_acceptance":"pending_final_batch"}))?;
    if case == "same-pipe-alias" {
        return native::self_loop(evidence);
    }
    if native::control(case, "owned_io_experiment", evidence)? {
        return Ok(());
    }
    let acquired = test_support::acquire_case(case);
    let mut owner = match acquired {
        Ok(owner) => owner,
        Err(mut failure_owner) => {
            let acquisition_events = test_support::take_failure_events(&mut failure_owner);
            let worker_injection = acquisition_events
                .iter()
                .any(|event| event.phase == "worker_start_injection");
            if worker_injection {
                evidence.mark(
                    "retained_start_failure",
                    test_support::failure_ownership(&failure_owner),
                )?;
            }
            let injection = acquisition_events
                .iter()
                .any(|event| event.phase == "rollback_injection");
            record(acquisition_events, evidence)?;
            restore_failure(&mut failure_owner, evidence)?;
            evidence.mark(
                "restored",
                json!({"reverse_order":true,"acquired_handles_closed":failure_owner.is_settled()}),
            )?;
            evidence.mark("qualification_rollback", json!({"complete":true}))?;
            if worker_injection {
                return Ok(());
            }
            if injection {
                evidence.mark("rollback",json!({"injection_reached":true,"obligation":if cfg!(unix) {"mutated_input_mode"} else {"acquired_handles"},"reverse_verified":true,"acquired_endpoints":2,"mutated_endpoints":if cfg!(unix) {1} else {0}}))?;
                return Ok(());
            }
            return Err(failure(failure_owner.error().to_string().as_str()));
        }
    };
    events(&mut owner, evidence)?;
    if case == "retained-restore-failure" {
        return retained_restore(&mut owner, evidence);
    }
    #[cfg(windows)]
    if matches!(case, "pending-write-before-ack" | "pending-write-cancel") {
        let operation = pending_write(&mut owner, evidence, case == "pending-write-cancel");
        let stopped = stop(&mut owner, evidence);
        stopped?;
        return operation;
    }
    let result = exercise(&mut owner, case, evidence);
    let restored = stop(&mut owner, evidence);
    if case == "restore-failure" && result.is_ok() && restored.is_ok() {
        // Preserve the original real invalid-handle control separately. Stronger
        // retained failed-restoration coverage is in host tests/stdio.rs.
        native::restore_fault(evidence)?;
        evidence.mark(
            "expected_restore_failure",
            json!({"native_failure_observed":true,"legitimate_endpoints_restored":true}),
        )?;
        return Err(failure("injected native restoration failure"));
    }
    restored?;
    result
}
fn exercise(owner: &mut Stdio, case: &str, evidence: &mut Evidence) -> io::Result<()> {
    let start = Instant::now();
    let mut buffer = [0; 4096];
    let output = [b'O'; 4096];
    let mut read_total = 0;
    let mut written = 0;
    let target = match case {
        "full-stdout" | "slow-drain" => 262144,
        "partial-output" => 37,
        _ => 0,
    };
    let mut ready = false;
    let mut blocked = false;
    let mut eof = false;
    let mut last_progress = Instant::now();
    loop {
        if start.elapsed() >= Duration::from_secs(10) {
            return Err(failure("production transport fixture watchdog"));
        }
        if !eof {
            let result = owner.try_read(&mut buffer);
            let native_events = test_support::take_events(owner);
            // Only omit redundant native Pending polls, never status errors.
            record(
                native_events
                    .into_iter()
                    .filter(|event| {
                        event.phase != "io" || event.observed["result"] != "Pending" || !ready
                    })
                    .collect(),
                evidence,
            )?;
            match result? {
                ReadStep::Pending => {
                    if !ready && !matches!(case, "cancel-before-call" | "unresolved-shutdown") {
                        evidence.mark("ready_idle",json!({"result":"Pending","source":if cfg!(windows) {"control_mailbox"} else {"native_nonblocking_read"},"production_owner":true}))?;
                        ready = true;
                    }
                }
                ReadStep::Bytes(n) => read_total += n,
                ReadStep::Eof => {
                    eof = true;
                    evidence.mark("eof", json!({"read_bytes":read_total}))?;
                    if target == 0 {
                        return Ok(());
                    }
                }
            }
        }
        if matches!(case, "cancel-before-call" | "unresolved-shutdown")
            && test_support::before_call(owner)
        {
            evidence.mark("before_native_call",json!({"barrier_held":true,"worker_owner_retained":cfg!(windows),"synthetic_control":true,"native_worker_barrier":cfg!(windows)}))?;
            if cfg!(unix) {
                evidence.mark("cancel_request",json!({"api":"stop_before_next_nonblocking_call","native_cancellation_required":false,"synthetic_control":true}))?;
                if case == "cancel-before-call" {
                    test_support::release_control(owner);
                }
            }
            return Ok(());
        }
        if written < target {
            let count = (target - written).min(4096);
            let result = owner.try_write(&output[..count]);
            let native_events = test_support::take_events(owner);
            record(
                native_events
                    .into_iter()
                    .filter(|event| {
                        event.phase != "io" || event.observed["result"] != "Pending" || !blocked
                    })
                    .collect(),
                evidence,
            )?;
            match result? {
                WriteStep::Bytes(n) => {
                    written += n;
                    last_progress = Instant::now();
                }
                WriteStep::Pending
                    if !blocked && last_progress.elapsed() >= Duration::from_millis(30) =>
                {
                    blocked = true;
                    evidence.mark("output_pending",json!({"written":written,"source":if cfg!(windows) {"control_mailbox"} else {"native_nonblocking_write"},"write_in_flight":owner.write_in_flight()}))?;
                }
                WriteStep::Pending => {}
            }
        }
        if target > 0 && written == target {
            evidence.mark("output_complete", json!({"written":written}))?;
            return Ok(());
        }
        if case == "full-stdout" && blocked && last_progress.elapsed() >= Duration::from_secs(5) {
            evidence.mark(
                "stall",
                json!({"positive_progress_timeout_ms":5000,"written":written}),
            )?;
            return Ok(());
        }
        tick();
    }
}

// Native fixture endpoint construction stays test-only; production still acquires
// actual inherited std handles through precisely its normal qualification path.
pub(super) fn probe(
    endpoint: &native::Endpoint,
    evidence: &mut Evidence,
    positive: bool,
) -> io::Result<bool> {
    let inherited = endpoint.install_input()?;
    let acquired = Stdio::acquire();
    drop(inherited);
    match acquired {
        Err(mut owner) => {
            failure_events(&mut owner, evidence)?;
            restore_failure(&mut owner, evidence)?;
            evidence.mark("restored",json!({"reverse_order":true,"acquired_handles_closed":owner.is_settled(),"control_profile":true}))?;
            Ok(false)
        }
        Ok(mut owner) => {
            events(&mut owner, evidence)?;
            let mut buffer = [0; 8];
            let start = Instant::now();
            if positive {
                loop {
                    let read = owner.try_read(&mut buffer);
                    events(&mut owner, evidence)?;
                    match read? {
                        ReadStep::Bytes(8) if buffer == *b"positive" => break,
                        ReadStep::Pending if start.elapsed() < Duration::from_secs(5) => tick(),
                        _ => {
                            stop(&mut owner, evidence)?;
                            return Err(failure("production positive control read"));
                        }
                    }
                }
            }
            stop(&mut owner, evidence)?;
            Ok(true)
        }
    }
}

fn retained_restore(owner: &mut Stdio, evidence: &mut Evidence) -> io::Result<()> {
    owner.request_stop();
    let start = Instant::now();
    let mut recorded = false;
    loop {
        let result = owner.poll_stop_or_restore();
        events(owner, evidence)?;
        if result.is_err() && !recorded {
            recorded = true;
            evidence.mark("retained_restore_failure",json!({"settled":owner.is_settled(),"synthetic_fault":true,"native_ownership":test_support::ownership(owner)}))?;
        }
        if matches!(result, Ok(true)) {
            return Err(failure("held restoration unexpectedly settled"));
        }
        if start.elapsed() >= Duration::from_secs(5) {
            let _=evidence.mark("forced_exit",json!({"grace_ms":5000,"production_owner_retained":true,"native_ownership":test_support::ownership(owner)}));
            std::process::exit(2);
        }
        tick();
    }
}
#[cfg(windows)]
fn pending_write(owner: &mut Stdio, evidence: &mut Evidence, cancel: bool) -> io::Result<()> {
    let mut frame = *b"ORIGINAL";
    if owner.try_write(&frame)? != WriteStep::Pending || !owner.write_in_flight() {
        return Err(failure("write was not retained pending"));
    }
    let start = Instant::now();
    let mut ack = [0; 1];
    loop {
        match owner.try_read(&mut ack)? {
            ReadStep::Bytes(1) if ack[0] == b'a' => break,
            ReadStep::Pending if start.elapsed() < Duration::from_secs(5) => tick(),
            _ => return Err(failure("peer receipt not observed")),
        }
    }
    // Parent confirms actual received bytes while completion remains uncollected.
    if !owner.write_in_flight() {
        return Err(failure("write receipt prematurely released"));
    }
    let replacement_rejected = owner.try_write(b"REPLACED").is_err() && owner.write_in_flight();
    frame[0] = b'X';
    let mutation_rejected = owner.try_write(&frame).is_err() && owner.write_in_flight();
    frame[0] = b'O';
    if !replacement_rejected || !mutation_rejected {
        return Err(failure("pending write replacement accepted"));
    }
    evidence.mark("pending_write_ownership",json!({"peer_received_before_ack":true,"replacement_rejected":replacement_rejected,"mutation_rejected":mutation_rejected,"unknown_progress_cancel":cancel}))?;
    if !cancel && (owner.try_write(&frame)? != WriteStep::Bytes(8) || owner.write_in_flight()) {
        return Err(failure("original write acknowledgment lost"));
    }
    owner.request_stop();
    if owner.try_write(&frame).is_ok() {
        return Err(failure("dispatch after stop"));
    }
    Ok(())
}
