//! Public nonblocking runtime. The entire unsettled owner survives errors/drop.
use crate::{
    Invocation, RuntimeError,
    contract::{self, WorkerCommand, WorkerCompletion},
    dispatcher::Dispatcher,
};
use std::{fmt, sync::mpsc::TrySendError, thread::JoinHandle};
use theseus_host::stdio::{ReadStep, Stdio, WriteStep};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Poll {
    Progress,
    Pending,
    Settled,
}
pub struct Runtime {
    owner: Option<Box<RuntimeOwner>>,
}
pub struct StartFailure {
    error: RuntimeError,
    owner: Option<Box<RuntimeOwner>>,
}
struct RuntimeOwner {
    stdio: Stdio,
    dispatcher: Dispatcher,
    worker: Option<JoinHandle<WorkerCompletion>>,
    completion: Option<WorkerCompletion>,
    settled: bool,
    failure_reported: bool,
    native_completion_proven: bool,
}
impl Runtime {
    #[cfg(feature = "test-support")]
    pub fn start_with_fixture(
        mut invocation: Invocation,
        stdio: Stdio,
        control: crate::test_support::FixtureControl,
    ) -> Result<Self, StartFailure> {
        invocation.fixture = Some(control);
        Self::start(invocation, stdio)
    }
    pub fn start(invocation: Invocation, stdio: Stdio) -> Result<Self, StartFailure> {
        let (ports, worker_ports, controls, data) = contract::channels();
        let mut owner = Box::new(RuntimeOwner {
            stdio,
            dispatcher: Dispatcher::new(ports, controls),
            worker: None,
            completion: None,
            settled: false,
            failure_reported: false,
            native_completion_proven: true,
        });
        #[cfg(feature = "test-support")]
        {
            owner.dispatcher.fixture = invocation.fixture.clone();
        }
        match crate::worker::spawn(worker_ports, data) {
            Ok(worker) => {
                owner.worker = Some(worker);
                owner.native_completion_proven = false;
            }
            Err(error) => {
                owner.stop();
                return Err(StartFailure {
                    error: RuntimeError::Transport(error),
                    owner: Some(owner),
                });
            }
        }
        if let Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) = owner
            .dispatcher
            .ports
            .commands
            .try_send(WorkerCommand::Init(invocation))
        {
            owner.stop();
            return Err(StartFailure {
                error: RuntimeError::Worker,
                owner: Some(owner),
            });
        }
        Ok(Self { owner: Some(owner) })
    }
    pub fn poll(&mut self) -> Result<Poll, RuntimeError> {
        self.owner
            .as_mut()
            .ok_or(RuntimeError::InvalidState)?
            .poll()
    }
    pub fn request_stop(&mut self) {
        if let Some(owner) = &mut self.owner {
            owner.stop()
        }
    }
    pub fn poll_settle(&mut self) -> Result<bool, RuntimeError> {
        self.request_stop();
        self.poll().map(|p| p == Poll::Settled)
    }
    pub fn worker_failure(&self) -> Option<&crate::WorkerFailure> {
        match self.owner.as_ref()?.completion.as_ref()? {
            WorkerCompletion::Failed(failure) => Some(failure),
            _ => None,
        }
    }
    pub fn is_stopping(&self) -> bool {
        self.owner.as_ref().is_some_and(|o| o.dispatcher.stopping)
    }
    pub fn is_settled(&self) -> bool {
        self.owner.as_ref().is_some_and(|o| o.settled)
    }
}
impl StartFailure {
    pub fn error(&self) -> &RuntimeError {
        &self.error
    }
    pub fn request_stop(&mut self) {
        if let Some(owner) = &mut self.owner {
            owner.stop()
        }
    }
    pub fn poll_settle(&mut self) -> Result<bool, RuntimeError> {
        self.request_stop();
        self.owner
            .as_mut()
            .ok_or(RuntimeError::InvalidState)?
            .poll()
            .map(|p| p == Poll::Settled)
    }
    pub fn is_settled(&self) -> bool {
        self.owner.as_ref().is_some_and(|o| o.settled)
    }
}
impl RuntimeOwner {
    fn stop(&mut self) {
        self.dispatcher.stop();
        self.stdio.request_stop();
    }
    fn poll(&mut self) -> Result<Poll, RuntimeError> {
        if self.settled {
            return Ok(Poll::Settled);
        }
        let result = self.round();
        if result.is_err() {
            self.dispatcher.communication_incomplete = true;
            self.stop()
        }
        result
    }
    fn round(&mut self) -> Result<Poll, RuntimeError> {
        let mut progress = false;
        self.dispatcher.deadline();
        if self.dispatcher.stopping {
            self.stdio.request_stop();
        } else {
            if let Some(buffer) = self.dispatcher.input_buffer() {
                match self
                    .stdio
                    .try_read(buffer)
                    .map_err(RuntimeError::Transport)?
                {
                    ReadStep::Bytes(n) => {
                        self.dispatcher.read_bytes(n);
                        progress = true
                    }
                    ReadStep::Pending => {}
                    ReadStep::Eof => {
                        self.dispatcher.eof()?;
                        progress = true
                    }
                }
            }
            progress |= self.dispatcher.input_quantum()?;
        }
        // Each source receives one bounded quantum even with unread stdout.
        progress |= self.dispatcher.event_quantum()?;
        progress |= self.dispatcher.response_quantum()?;
        progress |= self.dispatcher.data_quantum()?;
        progress |= self.dispatcher.return_quantum()?;
        if !self.dispatcher.stopping {
            // A pending token return stalls new frame selection, preserving the
            // one retained return slot even when control traffic is ready.
            if self.dispatcher.current.is_none() && self.dispatcher.can_select_frame() {
                self.dispatcher.current = self.dispatcher.frames.pop_front();
            }
            if let Some(frame) = self.dispatcher.current.as_mut() {
                match self
                    .stdio
                    .try_write(frame.write_slice())
                    .map_err(RuntimeError::Transport)?
                {
                    WriteStep::Pending =>
                    {
                        #[cfg(feature = "test-support")]
                        if let Some(control) = &self.dispatcher.fixture {
                            control.mark_write_pending();
                        }
                    }
                    WriteStep::Bytes(n) => {
                        progress = true;
                        if frame.acknowledged(n)? {
                            self.dispatcher.frame_done()?
                        }
                    }
                }
            }
        }
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            let handle = self.worker.take().ok_or(RuntimeError::InvalidState)?;
            self.completion = Some(match handle.join() {
                Ok(completion) => {
                    self.native_completion_proven = match &completion {
                        WorkerCompletion::Clean { native_settled, .. } => *native_settled,
                        WorkerCompletion::Failed(_) => true,
                    };
                    completion
                }
                Err(payload) => WorkerCompletion::Failed(crate::contract::OwnedWorkerFailure {
                    primary: crate::contract::WorkerPrimary::Panic(payload),
                    retained_file: None,
                    retained_spawn: None,
                    cleanup_errors: std::array::from_fn(|_| None),
                }),
            });
            if let Some(WorkerCompletion::Clean {
                store_poisoned,
                native_settled,
            }) = &self.completion
            {
                self.dispatcher
                    .cache
                    .apply(crate::contract::CachePatch::Stopped {
                        store_poisoned: *store_poisoned,
                        native_settled: *native_settled,
                    })?;
            }
            progress = true;
            self.stop();
        }
        if !self.failure_reported
            && self.completion.as_ref().is_some_and(|c| {
                matches!(
                    c,
                    WorkerCompletion::Failed(_)
                        | WorkerCompletion::Clean {
                            native_settled: false,
                            ..
                        }
                )
            })
        {
            self.failure_reported = true;
            return Err(RuntimeError::Worker);
        }
        if self.dispatcher.stopping {
            progress |= self.dispatcher.stopping_quantum();
            if self
                .stdio
                .poll_stop_or_restore()
                .map_err(RuntimeError::Transport)?
                && self.worker.is_none()
                && self.native_completion_proven
            {
                // Worker completion is only produced after actual native cleanup.
                self.settled = true;
                return Ok(Poll::Settled);
            }
        }
        Ok(if progress {
            Poll::Progress
        } else {
            Poll::Pending
        })
    }
}
fn drop_owner(owner: &mut Option<Box<RuntimeOwner>>) {
    if let Some(mut owned) = owner.take() {
        owned.stop();
        if !owned.settled {
            let _ = Box::leak(owned);
        }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        drop_owner(&mut self.owner)
    }
}
impl Drop for StartFailure {
    fn drop(&mut self) {
        drop_owner(&mut self.owner)
    }
}
impl fmt::Debug for Runtime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("helper runtime owner")
    }
}
impl fmt::Debug for StartFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("helper startup failure")
    }
}
impl fmt::Display for StartFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("helper startup failure")
    }
}
impl std::error::Error for StartFailure {}
