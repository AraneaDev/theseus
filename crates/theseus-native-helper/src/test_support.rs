//! Per-invocation fixture tracing. These observations are not native receipts.
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixtureMode {
    Observe,
    OutputPressure,
    HoldInit,
    HoldBeforeSpawn,
    PanicAfterOwnedSpawn,
}
#[derive(Clone)]
pub struct FixtureControl {
    state: Arc<State>,
}
struct State {
    mode: FixtureMode,
    events: AtomicU64,
    released: AtomicBool,
    control_write_acks: AtomicU64,
    saturation_wire_bytes: AtomicU64,
    saturation_published_frames: AtomicU64,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FixtureSnapshot {
    pub init_reached: bool,
    pub before_spawn_reached: bool,
    pub acquire_committed: bool,
    pub accepted_committed: bool,
    pub terminal_committed: bool,
    pub spawn_attempted: bool,
    pub spawn_owned: bool,
    pub write_pending: bool,
    pub cancel_requested: bool,
    pub panic_injected: bool,
    pub capture_saturated: bool,
    pub control_write_acks: u64,
    pub saturation_wire_bytes: u64,
    pub saturation_published_frames: u64,
}
const INIT_REACHED: u64 = 1 << 0;
const BEFORE_SPAWN_REACHED: u64 = 1 << 1;
const ACQUIRE_COMMITTED: u64 = 1 << 2;
const ACCEPTED_COMMITTED: u64 = 1 << 3;
const TERMINAL_COMMITTED: u64 = 1 << 4;
const SPAWN_ATTEMPTED: u64 = 1 << 5;
const SPAWN_OWNED: u64 = 1 << 6;
const WRITE_PENDING: u64 = 1 << 7;
const CANCEL_REQUESTED: u64 = 1 << 8;
const PANIC_INJECTED: u64 = 1 << 9;
const CAPTURE_SATURATED: u64 = 1 << 10;

impl FixtureControl {
    /// Create an independent observation family for one fixture invocation.
    pub fn new(mode: FixtureMode) -> Self {
        Self {
            state: Arc::new(State {
                mode,
                events: AtomicU64::new(0),
                released: AtomicBool::new(false),
                control_write_acks: AtomicU64::new(0),
                saturation_wire_bytes: AtomicU64::new(0),
                saturation_published_frames: AtomicU64::new(0),
            }),
        }
    }

    /// Release a worker gate, including a gate it has not yet reached.
    pub fn release(&self) {
        self.state.released.store(true, Ordering::Release);
    }

    /// Sample phase observations once. The acknowledgment counter is independent.
    pub fn snapshot(&self) -> FixtureSnapshot {
        let events = self.state.events.load(Ordering::Acquire);
        FixtureSnapshot {
            init_reached: events & INIT_REACHED != 0,
            before_spawn_reached: events & BEFORE_SPAWN_REACHED != 0,
            acquire_committed: events & ACQUIRE_COMMITTED != 0,
            accepted_committed: events & ACCEPTED_COMMITTED != 0,
            terminal_committed: events & TERMINAL_COMMITTED != 0,
            spawn_attempted: events & SPAWN_ATTEMPTED != 0,
            spawn_owned: events & SPAWN_OWNED != 0,
            write_pending: events & WRITE_PENDING != 0,
            cancel_requested: events & CANCEL_REQUESTED != 0,
            panic_injected: events & PANIC_INJECTED != 0,
            capture_saturated: events & CAPTURE_SATURATED != 0,
            control_write_acks: self.state.control_write_acks.load(Ordering::Acquire),
            saturation_wire_bytes: if events & CAPTURE_SATURATED != 0 {
                self.state.saturation_wire_bytes.load(Ordering::Relaxed)
            } else {
                0
            },
            saturation_published_frames: if events & CAPTURE_SATURATED != 0 {
                self.state
                    .saturation_published_frames
                    .load(Ordering::Relaxed)
            } else {
                0
            },
        }
    }

    pub(crate) fn is_output_pressure(&self) -> bool {
        self.state.mode == FixtureMode::OutputPressure
    }
    /// One worker writer prepares first-saturation facts; the existing worker
    /// discard hook releases CAPTURE_SATURATED after this returns.
    pub(crate) fn prepare_capture_saturated_facts(&self, wire_bytes: u32, published_frames: u32) {
        if self.state.events.load(Ordering::Relaxed) & CAPTURE_SATURATED == 0 {
            self.state
                .saturation_wire_bytes
                .store(u64::from(wire_bytes), Ordering::Relaxed);
            self.state
                .saturation_published_frames
                .store(u64::from(published_frames), Ordering::Relaxed);
        }
    }
    fn mark(&self, event: u64) {
        self.state.events.fetch_or(event, Ordering::Release);
    }

    fn wait_for_release(&self) {
        while !self.state.released.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
    }

    /// Worker only: before Init opens any store. Intentionally ignores stop.
    pub(crate) fn init_reached(&self) {
        self.mark(INIT_REACHED);
        if self.state.mode == FixtureMode::HoldInit {
            self.wait_for_release();
        }
    }

    /// Worker only: after Accepted commits, before spawn. Recheck cancellation,
    /// stop and deadline after this returns, before any actual spawn attempt.
    pub(crate) fn before_spawn(&self) {
        self.mark(BEFORE_SPAWN_REACHED);
        if self.state.mode == FixtureMode::HoldBeforeSpawn {
            self.wait_for_release();
        }
    }

    /// Worker only: the original AcquiredChild must already live in worker-owned
    /// state outside the unwind closure. Never call from inside host acquisition.
    pub(crate) fn after_owned_spawn(&self) {
        if self.state.mode == FixtureMode::PanicAfterOwnedSpawn {
            self.mark(PANIC_INJECTED);
            panic!("fixture panic after owned spawn");
        }
    }

    pub(crate) fn mark_acquire_committed(&self) {
        self.mark(ACQUIRE_COMMITTED);
    }
    pub(crate) fn mark_accepted_committed(&self) {
        self.mark(ACCEPTED_COMMITTED);
    }
    pub(crate) fn mark_terminal_committed(&self) {
        self.mark(TERMINAL_COMMITTED);
    }
    pub(crate) fn mark_spawn_attempted(&self) {
        self.mark(SPAWN_ATTEMPTED);
    }
    pub(crate) fn mark_spawn_owned(&self) {
        self.mark(SPAWN_OWNED);
    }
    pub(crate) fn mark_write_pending(&self) {
        self.mark(WRITE_PENDING);
    }
    pub(crate) fn mark_cancel_requested(&self) {
        self.mark(CANCEL_REQUESTED);
    }
    pub(crate) fn mark_capture_saturated(&self) {
        self.mark(CAPTURE_SATURATED);
    }

    /// Dispatcher only: once per whole control frame acknowledged by host Stdio.
    pub(crate) fn acknowledge_control_write(&self) {
        let _ = self.state.control_write_acks.try_update(
            Ordering::Release,
            Ordering::Relaxed,
            |count| Some(count.saturating_add(1)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };

    fn wait_until(mut reached: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !reached() {
            assert!(
                Instant::now() < deadline,
                "fixture hook did not reach boundary"
            );
            thread::yield_now();
        }
    }

    #[test]
    fn observations_latch_across_clones_without_cross_invocation_leakage() {
        let control = FixtureControl::new(FixtureMode::Observe);
        let independent = FixtureControl::new(FixtureMode::Observe);
        let observer = control.clone();
        control.init_reached();
        control.before_spawn();
        control.mark_acquire_committed();
        control.mark_accepted_committed();
        control.mark_terminal_committed();
        control.mark_spawn_attempted();
        control.mark_spawn_owned();
        control.mark_write_pending();
        control.mark_cancel_requested();
        control.mark_capture_saturated();
        assert_eq!(
            observer.snapshot(),
            FixtureSnapshot {
                init_reached: true,
                before_spawn_reached: true,
                acquire_committed: true,
                accepted_committed: true,
                terminal_committed: true,
                spawn_attempted: true,
                spawn_owned: true,
                write_pending: true,
                cancel_requested: true,
                capture_saturated: true,
                panic_injected: false,
                control_write_acks: 0,
                saturation_wire_bytes: 0,
                saturation_published_frames: 0,
            }
        );
        assert_eq!(independent.snapshot(), FixtureSnapshot::default());
    }

    #[test]
    fn init_gate_requires_release_from_same_clone_family() {
        let control = FixtureControl::new(FixtureMode::HoldInit);
        let worker = control.clone();
        let (sent, received) = mpsc::channel();
        let join = thread::spawn(move || {
            worker.init_reached();
            sent.send(()).unwrap();
        });
        wait_until(|| control.snapshot().init_reached);
        let blocked = received.try_recv().is_err();
        control.release();
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        join.join().unwrap();
        assert!(blocked, "init gate returned before release");
        assert!(!control.snapshot().before_spawn_reached);
    }

    #[test]
    fn before_spawn_gate_requires_release_without_fabricating_spawn() {
        let control = FixtureControl::new(FixtureMode::HoldBeforeSpawn);
        let worker = control.clone();
        let (sent, received) = mpsc::channel();
        let join = thread::spawn(move || {
            worker.before_spawn();
            sent.send(()).unwrap();
        });
        wait_until(|| control.snapshot().before_spawn_reached);
        let blocked = received.try_recv().is_err();
        let snapshot = control.snapshot();
        control.release();
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        join.join().unwrap();
        assert!(blocked, "spawn gate returned before release");
        assert!(!snapshot.spawn_attempted);
        assert!(!snapshot.spawn_owned);
        assert!(!snapshot.accepted_committed);
    }

    #[test]
    fn early_release_is_latched_for_both_gates() {
        for mode in [FixtureMode::HoldInit, FixtureMode::HoldBeforeSpawn] {
            let control = FixtureControl::new(mode);
            control.release();
            let (sent, received) = mpsc::channel();
            let worker = control.clone();
            let join = thread::spawn(move || {
                worker.init_reached();
                worker.before_spawn();
                sent.send(()).unwrap();
            });
            received.recv_timeout(Duration::from_secs(5)).unwrap();
            join.join().unwrap();
            assert!(control.snapshot().init_reached);
            assert!(control.snapshot().before_spawn_reached);
        }
    }

    #[test]
    fn modes_do_not_hold_other_boundaries_or_inject_unrequested_panics() {
        let init = FixtureControl::new(FixtureMode::HoldInit);
        init.before_spawn();
        init.after_owned_spawn();
        let spawn = FixtureControl::new(FixtureMode::HoldBeforeSpawn);
        spawn.init_reached();
        spawn.after_owned_spawn();
        let observe = FixtureControl::new(FixtureMode::Observe);
        observe.init_reached();
        observe.before_spawn();
        observe.after_owned_spawn();
        assert!(init.snapshot().before_spawn_reached);
        assert!(spawn.snapshot().init_reached);
        assert!(!observe.snapshot().panic_injected);
    }

    #[test]
    fn owned_spawn_panic_is_static_and_observed_before_unwind() {
        let control = FixtureControl::new(FixtureMode::PanicAfterOwnedSpawn);
        // These marks model call order only; this test owns no native process.
        control.mark_spawn_attempted();
        control.mark_spawn_owned();
        let payload = std::panic::catch_unwind(|| control.after_owned_spawn()).unwrap_err();
        assert_eq!(
            payload.downcast_ref::<&'static str>(),
            Some(&"fixture panic after owned spawn")
        );
        let snapshot = control.snapshot();
        assert!(snapshot.panic_injected);
        assert!(snapshot.spawn_owned);
        assert!(snapshot.spawn_attempted);
        assert!(!snapshot.terminal_committed);
    }

    #[test]
    fn concurrent_marks_are_never_lost() {
        let control = FixtureControl::new(FixtureMode::Observe);
        thread::scope(|scope| {
            let first = &control;
            scope.spawn(move || {
                for _ in 0..1000 {
                    first.mark_acquire_committed();
                    first.mark_terminal_committed();
                    first.mark_spawn_owned();
                }
            });
            let second = &control;
            scope.spawn(move || {
                for _ in 0..1000 {
                    second.mark_accepted_committed();
                    second.mark_spawn_attempted();
                    second.mark_write_pending();
                }
            });
        });
        let snapshot = control.snapshot();
        assert!(
            snapshot.acquire_committed
                && snapshot.accepted_committed
                && snapshot.terminal_committed
        );
        assert!(snapshot.spawn_attempted && snapshot.spawn_owned && snapshot.write_pending);
    }

    #[test]
    fn control_ack_count_is_shared_atomic_and_saturating() {
        let control = FixtureControl::new(FixtureMode::Observe);
        thread::scope(|scope| {
            for _ in 0..4 {
                let worker = control.clone();
                scope.spawn(move || {
                    for _ in 0..1000 {
                        worker.acknowledge_control_write();
                    }
                });
            }
        });
        assert_eq!(control.snapshot().control_write_acks, 4000);
        control
            .state
            .control_write_acks
            .store(u64::MAX - 1, Ordering::Relaxed);
        control.acknowledge_control_write();
        control.acknowledge_control_write();
        assert_eq!(control.snapshot().control_write_acks, u64::MAX);
        assert!(!control.snapshot().write_pending);
    }
}
