use crate::{ClientError, Failure, error::Source, pump::Pump};
use std::{
    io,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::mpsc::{TrySendError, sync_channel},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use theseus_host::process::{AcquiredChild, HostCleanup};

/// The entire native owner, including the pump's pending write storage, is
/// transferred as one Box. No native owner is captured by a thread builder.
pub(crate) struct TransportOwner {
    pub(crate) native: Option<AcquiredChild>,
    pub(crate) pump: Option<Pump>,
    pub(crate) failure: Option<Failure>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) stderr_overflow: bool,
}
impl TransportOwner {
    pub(crate) fn empty() -> Self {
        Self {
            native: None,
            pump: None,
            failure: None,
            stderr: Vec::with_capacity(8192),
            stderr_overflow: false,
        }
    }
    pub(crate) fn remember(&mut self, source: Source) {
        match self.failure.as_mut() {
            None => self.failure = Some(Failure::new(source)),
            Some(failure) if failure.secondary.is_none() => failure.secondary = Some(source),
            Some(_) => {}
        }
    }
    pub(crate) fn authority_complete(&self) -> bool {
        self.native.is_none()
            && !matches!(self.failure.as_ref().and_then(|f|f.source.as_ref()), Some(Source::Spawn(f)) if matches!(f.cleanup, HostCleanup::Pending(_)))
    }
    fn settle(&mut self) {
        if let Some(native) = self.native.as_mut() {
            if let Some(input) = native.stdin.as_mut()
                && let Err(error) = input.close()
            {
                self.remember(Source::Io(error));
            }
            let deadline = Instant::now() + Duration::from_secs(8);
            let mut scratch = [0; 4096];
            let mut eof = [false; 2];
            loop {
                let native = self.native.as_mut().expect("retained native owner");
                let mut errors = [None, None, None];
                for (slot, reader) in [&mut native.stdout, &mut native.stderr]
                    .into_iter()
                    .enumerate()
                {
                    if !eof[slot] {
                        match reader.try_read(&mut scratch) {
                            Ok(0) => eof[slot] = true,
                            Ok(_) => {}
                            Err(error) if transient(&error) => {}
                            Err(error) => {
                                errors[slot] = Some(error);
                            }
                        }
                    }
                }
                let exited = match native.child.exited_reserved() {
                    Ok(v) => v,
                    Err(e) => {
                        errors[2] = Some(e);
                        false
                    }
                };
                for error in errors.into_iter().flatten() {
                    self.remember(Source::Io(error));
                }
                if (exited && eof.iter().all(|v| *v)) || Instant::now() >= deadline {
                    break;
                }
                thread::sleep(Duration::from_millis(1));
            }
            loop {
                let native = self.native.as_mut().expect("retained native owner");
                let mut errors = [None, None, None, None];
                let input = match native.stdin.as_mut() {
                    Some(input) => input.close().map_err(|e| {
                        errors[3] = Some(e);
                    }),
                    None => Ok(()),
                };
                let stdout = native.stdout.cancel_and_complete().map_err(|e| {
                    errors[0] = Some(e);
                });
                let stderr = native.stderr.cancel_and_complete().map_err(|e| {
                    errors[1] = Some(e);
                });
                let child = native.child.try_reap_and_release().map_err(|e| {
                    errors[2] = Some(e);
                });
                let complete = input.is_ok()
                    && stdout.is_ok()
                    && stderr.is_ok()
                    && matches!(child, Ok(Some(_)));
                for error in errors.into_iter().flatten() {
                    self.remember(Source::Io(error));
                }
                if complete {
                    self.native = None;
                    break;
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
        loop {
            let result = match self.failure.as_mut().and_then(|f| f.source.as_mut()) {
                Some(Source::Spawn(failure)) => match &mut failure.cleanup {
                    HostCleanup::Pending(cleanup) => cleanup.try_complete(),
                    HostCleanup::Complete => Ok(true),
                },
                _ => Ok(true),
            };
            match result {
                Ok(true) => {
                    if let Some(Source::Spawn(f)) =
                        self.failure.as_mut().and_then(|f| f.source.as_mut())
                    {
                        f.cleanup = HostCleanup::Complete;
                    }
                    break;
                }
                Ok(false) => thread::sleep(Duration::from_millis(1)),
                Err(error) => {
                    self.remember(Source::Io(error));
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }
}
pub(crate) fn capture_stderr(capture: &mut Vec<u8>, overflow: &mut bool, bytes: &[u8]) {
    let retain = bytes.len().min(8192 - capture.len());
    capture.extend_from_slice(&bytes[..retain]);
    *overflow |= retain != bytes.len();
}
pub(crate) fn transient(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

pub(crate) struct Settlement {
    pub(crate) owner: Option<Box<TransportOwner>>,
    worker: Option<JoinHandle<Option<Box<TransportOwner>>>>,
    pub(crate) stopped: bool,
    unproved: bool,
}
impl Settlement {
    pub(crate) fn new(owner: TransportOwner) -> Self {
        Self {
            owner: Some(Box::new(owner)),
            worker: None,
            stopped: false,
            unproved: false,
        }
    }
    pub(crate) fn failure(&self) -> Option<&Failure> {
        self.owner.as_ref().and_then(|o| o.failure.as_ref())
    }
    pub(crate) fn is_settled(&self) -> bool {
        !self.unproved
            && self.stopped
            && self.worker.is_none()
            && self.owner.as_ref().is_none_or(|o| o.authority_complete())
    }
    pub(crate) fn request_stop(&mut self) -> Result<(), ClientError> {
        self.stopped = true;
        if self.unproved {
            return Err(ClientError::WorkerPanic);
        }
        if self.worker.is_some() || self.owner.as_ref().is_none_or(|o| o.authority_complete()) {
            return Ok(());
        }
        self.handoff_with(
            |receiver| {
                thread::Builder::new()
                    .name("theseus-client-settle".into())
                    .spawn(move || stop_worker(receiver, TransportOwner::settle))
            },
            |sender, owner| sender.try_send(owner),
        )
    }
    fn handoff_with(
        &mut self,
        spawn: impl FnOnce(
            std::sync::mpsc::Receiver<Box<TransportOwner>>,
        ) -> io::Result<JoinHandle<Option<Box<TransportOwner>>>>,
        assign: impl FnOnce(
            &std::sync::mpsc::SyncSender<Box<TransportOwner>>,
            Box<TransportOwner>,
        ) -> Result<(), TrySendError<Box<TransportOwner>>>,
    ) -> Result<(), ClientError> {
        if self.worker.is_some() {
            return Err(ClientError::NotSettled);
        }
        let (sender, receiver) = sync_channel::<Box<TransportOwner>>(1);
        let worker = match spawn(receiver) {
            Ok(worker) => worker,
            Err(error) => {
                self.owner
                    .as_mut()
                    .expect("local owner")
                    .remember(Source::Io(error));
                return Err(ClientError::WorkerStart);
            }
        };
        self.worker = Some(worker);
        let owner = self
            .owner
            .take()
            .expect("owner transfers only after successful spawn");
        match assign(&sender, owner) {
            Ok(()) => {}
            Err(TrySendError::Full(owner) | TrySendError::Disconnected(owner)) => {
                self.owner = Some(owner);
                drop(sender);
                return Err(ClientError::WorkerStart);
            }
        }
        Ok(())
    }
    pub(crate) fn poll(&mut self) -> Result<bool, ClientError> {
        if self.unproved {
            return Err(ClientError::WorkerPanic);
        }
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.is_finished())
        {
            match self.worker.take().expect("finished worker").join() {
                Ok(Some(owner)) => {
                    let panicked = owner.failure.as_ref().is_some_and(|f| f.panic.is_some());
                    self.owner = Some(owner);
                    if panicked {
                        return Err(ClientError::WorkerPanic);
                    }
                }
                Ok(None) => {}
                Err(payload) => {
                    self.unproved = true;
                    // The closure catches all assigned-owner work. If panic occurs
                    // outside that boundary, preserve it and refuse settlement.
                    let owner = self
                        .owner
                        .get_or_insert_with(|| Box::new(TransportOwner::empty()));
                    let failure = owner.failure.get_or_insert(Failure {
                        source: None,
                        panic: None,
                        secondary: None,
                    });
                    failure.panic = Some(payload);
                    return Err(ClientError::WorkerPanic);
                }
            }
        }
        if self.worker.is_none() && !self.is_settled() && self.stopped {
            self.request_stop()?;
        }
        Ok(self.is_settled())
    }
}
impl Drop for Settlement {
    fn drop(&mut self) {
        if self.is_settled() {
            return;
        }
        let _ = self.request_stop();
        if self.is_settled() {
            return;
        }
        // No detach-as-success: retain the whole bounded state, including handle,
        // pending frame storage, original errors and native owners, as incomplete.
        let retained = Self {
            owner: self.owner.take(),
            worker: self.worker.take(),
            stopped: self.stopped,
            unproved: self.unproved,
        };
        std::mem::forget(Box::new(retained));
    }
}

fn stop_worker(
    receiver: std::sync::mpsc::Receiver<Box<TransportOwner>>,
    mut cleanup: impl FnMut(&mut TransportOwner),
) -> Option<Box<TransportOwner>> {
    let Ok(mut owner) = receiver.recv() else {
        return None;
    };
    // Both native authority and pending frame buffers live outside unwind scope.
    let result = catch_unwind(AssertUnwindSafe(|| cleanup(&mut owner)));
    if let Err(payload) = result {
        let failure = owner.failure.get_or_insert(Failure {
            source: None,
            panic: None,
            secondary: None,
        });
        if failure.panic.is_none() {
            failure.panic = Some(payload);
        }
        // A panic is an incomplete attempt. Return the whole owner for borrowed
        // inspection and an explicit later stop attempt after this actual join.
    }
    Some(owner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    #[derive(Debug)]
    struct Witness(Arc<AtomicUsize>);
    impl std::fmt::Display for Witness {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("witness")
        }
    }
    impl std::error::Error for Witness {}
    impl Drop for Witness {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn owner() -> (Settlement, Arc<AtomicUsize>, usize) {
        let count = Arc::new(AtomicUsize::new(0));
        let mut owner = TransportOwner::empty();
        owner.remember(Source::Io(io::Error::other(Witness(count.clone()))));
        owner.pump = Some(crate::pump::tests::ready());
        let settlement = Settlement::new(owner);
        let addr = std::ptr::from_ref(settlement.owner.as_ref().unwrap().as_ref()) as usize;
        (settlement, count, addr)
    }
    fn spawn(
        receiver: std::sync::mpsc::Receiver<Box<TransportOwner>>,
    ) -> io::Result<JoinHandle<Option<Box<TransportOwner>>>> {
        thread::Builder::new().spawn(move || stop_worker(receiver, |_| {}))
    }
    fn join(settlement: &mut Settlement) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while settlement.worker.as_ref().is_some_and(|h| !h.is_finished()) {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        settlement.stopped = true;
        let result = settlement.poll();
        assert!(result.is_ok() || result == Err(ClientError::WorkerPanic));
        assert!(settlement.worker.is_none());
    }
    #[test]
    fn spawn_failure_keeps_same_box_and_original_source_without_drop() {
        let (mut settlement, count, addr) = owner();
        assert_eq!(
            settlement.handoff_with(
                |_| Err(io::Error::other("modeled builder failure")),
                |s, o| s.try_send(o)
            ),
            Err(ClientError::WorkerStart)
        );
        assert_eq!(
            std::ptr::from_ref(settlement.owner.as_ref().unwrap().as_ref()) as usize,
            addr
        );
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert!(
            settlement
                .failure()
                .unwrap()
                .source()
                .unwrap()
                .downcast_ref::<io::Error>()
                .unwrap()
                .get_ref()
                .unwrap()
                .is::<Witness>()
        );
        settlement.stopped = true;
        drop(settlement);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn full_and_disconnected_return_same_box_and_join_empty_worker_before_retry() {
        for full in [true, false] {
            let (mut settlement, count, addr) = owner();
            let result = settlement.handoff_with(spawn, |_, o| {
                Err(if full {
                    TrySendError::Full(o)
                } else {
                    TrySendError::Disconnected(o)
                })
            });
            assert_eq!(result, Err(ClientError::WorkerStart));
            assert_eq!(count.load(Ordering::SeqCst), 0);
            assert_eq!(
                std::ptr::from_ref(settlement.owner.as_ref().unwrap().as_ref()) as usize,
                addr
            );
            assert_eq!(
                settlement.handoff_with(spawn, |s, o| s.try_send(o)),
                Err(ClientError::NotSettled)
            );
            assert!(!settlement.is_settled());
            join(&mut settlement);
            assert!(settlement.is_settled());
            drop(settlement);
            assert_eq!(count.load(Ordering::SeqCst), 1);
        }
    }
    #[test]
    fn worker_panic_preserves_original_payload_source_and_box_through_join() {
        let (mut settlement, count, addr) = owner();
        let panic_count = Arc::new(AtomicUsize::new(0));
        let p = panic_count.clone();
        settlement
            .handoff_with(
                |receiver| {
                    thread::Builder::new().spawn(move || {
                        stop_worker(receiver, |_| std::panic::panic_any(Witness(p.clone())))
                    })
                },
                |s, o| s.try_send(o),
            )
            .unwrap();
        assert!(settlement.owner.is_none());
        assert!(settlement.failure().is_none());
        assert!(!settlement.is_settled());
        join(&mut settlement);
        assert_eq!(
            std::ptr::from_ref(settlement.owner.as_ref().unwrap().as_ref()) as usize,
            addr
        );
        assert!(
            settlement
                .failure()
                .unwrap()
                .panic_payload()
                .unwrap()
                .is::<Witness>()
        );
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert_eq!(panic_count.load(Ordering::SeqCst), 0);
        drop(settlement);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(panic_count.load(Ordering::SeqCst), 1);
    }
}

#[cfg(test)]
mod panic_regression {
    use super::*;
    #[test]
    fn panicked_worker_reports_failure_and_keeps_unresolved_cleanup_after_join() {
        use theseus_host::process::{LaunchPhase, OwnedSpawnCleanup, SpawnFailure};
        let mut owner = TransportOwner::empty();
        owner.remember(Source::Spawn(SpawnFailure {
            launch: LaunchPhase::NeverCreated,
            cleanup: HostCleanup::Pending(OwnedSpawnCleanup::from_endpoints(vec![], vec![])),
            error: io::Error::other("modeled original"),
            cleanup_errors: vec![],
        }));
        let mut settlement = Settlement::new(owner);
        settlement.stopped = true;
        settlement
            .handoff_with(
                |receiver| {
                    thread::Builder::new()
                        .spawn(move || stop_worker(receiver, |_| std::panic::panic_any(17_u32)))
                },
                |s, o| s.try_send(o),
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !settlement.worker.as_ref().unwrap().is_finished() {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert_eq!(settlement.poll(), Err(ClientError::WorkerPanic));
        assert!(settlement.worker.is_none());
        assert!(!settlement.is_settled());
        assert_eq!(
            settlement
                .failure()
                .unwrap()
                .panic_payload()
                .unwrap()
                .downcast_ref::<u32>(),
            Some(&17)
        );
        assert!(
            settlement
                .failure()
                .unwrap()
                .source()
                .unwrap()
                .is::<SpawnFailure>()
        );
        // Explicit retry uses the retained original cleanup guard, then joins.
        settlement.request_stop().unwrap();
        while !settlement.is_settled() {
            let _ = settlement.poll();
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
    }
}

#[cfg(test)]
mod join_regression {
    use super::*;
    #[test]
    fn unexpected_outer_thread_panic_never_becomes_a_settlement_receipt() {
        let mut settlement = Settlement::new(TransportOwner::empty());
        settlement.stopped = true;
        settlement.worker = Some(thread::spawn(|| std::panic::panic_any(99_u32)));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !settlement.worker.as_ref().unwrap().is_finished() {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert_eq!(settlement.poll(), Err(ClientError::WorkerPanic));
        assert!(!settlement.is_settled());
        assert_eq!(
            settlement
                .failure()
                .unwrap()
                .panic_payload()
                .unwrap()
                .downcast_ref::<u32>(),
            Some(&99)
        );
        // Explicitly retain the incomplete owner, as public Drop does.
        std::mem::forget(settlement);
    }
}

#[cfg(test)]
mod stderr_tests {
    use super::*;
    #[test]
    fn stderr_retains_only_eight_kib_and_one_overflow_fact() {
        let mut owner = TransportOwner::empty();
        capture_stderr(&mut owner.stderr, &mut owner.stderr_overflow, &[11; 4096]);
        capture_stderr(&mut owner.stderr, &mut owner.stderr_overflow, &[22; 4096]);
        assert!(!owner.stderr_overflow);
        capture_stderr(&mut owner.stderr, &mut owner.stderr_overflow, &[33; 4096]);
        assert!(owner.stderr_overflow);
        assert_eq!(owner.stderr.len(), 8192);
        assert_eq!(owner.stderr[..4096], [11; 4096]);
        assert_eq!(owner.stderr[4096..], [22; 4096]);
        capture_stderr(&mut owner.stderr, &mut owner.stderr_overflow, &[44; 4096]);
        assert_eq!(owner.stderr.len(), 8192);
        assert!(owner.stderr_overflow);
    }
}
