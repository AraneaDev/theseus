//! Fixed qualification controls, compiled only for the native fixture/tests.
//! Synthetic fault/barrier controls are distinct from real kernel observations.
use super::{AcquireFailure, Native, Stdio};
use serde_json::Value;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Control {
    #[default]
    Normal,
    PartialAcquire,
    HoldRestore,
    BeforeCall,
    HoldBeforeCall,
    WriterSpawnFailure,
}
#[derive(Debug)]
pub struct Event {
    pub phase: &'static str,
    pub endpoint: &'static str,
    pub api: &'static str,
    pub ok: bool,
    pub status: Option<i64>,
    pub error: Option<i32>,
    pub observed: Value,
}
pub(crate) struct Trace {
    events: [Option<Event>; 128],
    length: usize,
    overflow: bool,
}
impl Default for Trace {
    fn default() -> Self {
        Self {
            events: std::array::from_fn(|_| None),
            length: 0,
            overflow: false,
        }
    }
}
impl Trace {
    pub(crate) fn push(&mut self, event: Event) {
        if self.length == self.events.len() {
            self.overflow = true;
            return;
        }
        self.events[self.length] = Some(event);
        self.length += 1;
    }
    fn take(&mut self) -> Vec<Event> {
        let mut result = Vec::with_capacity(self.length + usize::from(self.overflow));
        for event in &mut self.events[..self.length] {
            result.push(event.take().unwrap());
        }
        self.length = 0;
        if self.overflow {
            self.overflow = false;
            result.push(Event {
                phase: "observer_overflow",
                endpoint: "control",
                api: "bounded observer",
                ok: false,
                status: None,
                error: None,
                observed: Value::Null,
            });
        }
        result
    }
}
/// Only the fixed qualification cases can change acquisition behavior.
pub fn acquire_case(case: &str) -> Result<Stdio, AcquireFailure> {
    let mut native = Native::new();
    native.control = match case {
        "startup-partial-rollback" => Control::PartialAcquire,
        "retained-restore-failure" => Control::HoldRestore,
        "cancel-before-call" => Control::BeforeCall,
        "unresolved-shutdown" => Control::HoldBeforeCall,
        "worker-startup-rollback" => Control::WriterSpawnFailure,
        _ => Control::Normal,
    };
    Stdio::acquire_native(native)
}
pub fn take_events(owner: &mut Stdio) -> Vec<Event> {
    owner.inner().trace.take()
}
pub fn take_failure_events(failure: &mut AcquireFailure) -> Vec<Event> {
    take_events(&mut failure.owner)
}
pub fn before_call(owner: &Stdio) -> bool {
    owner.native.as_ref().unwrap().before_call()
}
pub fn release_control(owner: &mut Stdio) {
    owner.inner().release_control();
}

pub fn ownership(owner: &Stdio) -> Value {
    owner.native.as_ref().unwrap().ownership()
}

pub fn failure_ownership(failure: &AcquireFailure) -> Value {
    ownership(&failure.owner)
}

// This is the actual Windows cancellation observation path. It is also built
// for unit tests on other hosts; those tests simulate statuses, not native I/O.
#[cfg(any(windows, test))]
#[derive(Default)]
pub(crate) struct CancellationEvidence {
    workers: [CancellationSeen; 2],
}
#[cfg(any(windows, test))]
#[derive(Default)]
struct CancellationSeen {
    not_found: bool,
    success: bool,
    other_failure: bool,
}
#[cfg(any(windows, test))]
impl CancellationEvidence {
    pub(crate) fn record(
        &mut self,
        trace: &mut Trace,
        reading: bool,
        status: i32,
        error: Option<i32>,
    ) {
        // Win32 ERROR_NOT_FOUND is 1168. Keep the first outcome of each
        // decisive class per worker regardless of how late it occurs: at most
        // three events per worker, including its initial outcome. Poll counts
        // and scheduling cannot suppress the first successful cancellation.
        let worker = &mut self.workers[usize::from(!reading)];
        let seen = if status != 0 {
            &mut worker.success
        } else if error == Some(1168) {
            &mut worker.not_found
        } else {
            &mut worker.other_failure
        };
        if !std::mem::replace(seen, true) {
            trace.push(Event {
                phase: "cancel_request",
                endpoint: if reading { "stdin" } else { "stdout" },
                api: "CancelSynchronousIo",
                ok: status != 0,
                status: Some(status as i64),
                error,
                observed: serde_json::json!({"real_thread_handle":true,"completion_proven":false}),
            });
        }
    }
}
#[cfg(test)]
mod cancellation_tests {
    use super::*;
    #[test]
    fn late_success_after_sixty_four_not_found_polls_is_recorded_per_worker() {
        let mut evidence = CancellationEvidence::default();
        let mut trace = Trace::default();
        for _ in 0..64 {
            evidence.record(&mut trace, true, 0, Some(1168));
        }
        evidence.record(&mut trace, false, 1, None);
        evidence.record(&mut trace, true, 1, None);
        let events = trace.take();
        assert!(
            events.iter().any(|e| e.endpoint == "stdin" && e.ok),
            "late reader success was lost"
        );
        assert!(
            events.iter().any(|e| e.endpoint == "stdout" && e.ok),
            "writer's first success was lost"
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| e.endpoint == "stdin" && e.error == Some(1168))
                .count(),
            1
        );
        assert_eq!(events.len(), 3);
    }
    #[test]
    fn cancellation_evidence_retains_first_error_classes_without_unbounded_repeats() {
        let mut evidence = CancellationEvidence::default();
        let mut trace = Trace::default();
        for _ in 0..1000 {
            for reading in [true, false] {
                evidence.record(&mut trace, reading, 0, Some(5));
                evidence.record(&mut trace, reading, 0, Some(87));
                evidence.record(&mut trace, reading, 0, Some(1168));
                evidence.record(&mut trace, reading, 1, None);
            }
        }
        let events = trace.take();
        assert_eq!(
            events.len(),
            6,
            "decisive observations must be bounded per worker"
        );
        for endpoint in ["stdin", "stdout"] {
            let worker: Vec<_> = events.iter().filter(|e| e.endpoint == endpoint).collect();
            assert_eq!(
                worker.iter().map(|e| e.error).collect::<Vec<_>>(),
                [Some(5), Some(1168), None]
            );
            assert_eq!(
                worker.iter().map(|e| e.status).collect::<Vec<_>>(),
                [Some(0), Some(0), Some(1)]
            );
            assert!(
                worker
                    .iter()
                    .all(|e| e.observed["completion_proven"] == false)
            );
        }
    }
}
