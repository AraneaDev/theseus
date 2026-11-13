//! One serialized owner of the ledger and every native process authority.
use crate::{
    Invocation,
    contract::*,
    operation::{OutputState, cleanup_fact, status},
};
use std::{
    io,
    sync::{
        atomic::Ordering,
        mpsc::{TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use theseus_host::{
    fs::{HostDir, RootPolicy},
    process::{AcquiredChild, HostCleanup, InputMode, OwnedChild, PipeReader},
};
use theseus_native_helper_store::{CommittedRecord, LedgerStore};
use theseus_native_protocol::v2::{
    self,
    ledger::{Disposition, Entry, Record},
};
mod desktop;
mod file;
mod lifecycle;
#[cfg(test)]
mod native_tests;
#[cfg(test)]
mod private_barrier_tests;
mod readiness;
mod run;
mod service;
#[cfg(test)]
mod tests;
pub(crate) fn spawn(
    ports: WorkerPorts,
    tokens: [DataPermit; 4],
) -> io::Result<JoinHandle<WorkerCompletion>> {
    thread::Builder::new()
        .name("native-helper-worker".into())
        .spawn(move || {
            // Native owners and panic storage live outside the unwind closure.
            let mut owner = Box::new(Worker::new(ports, tokens));
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| owner.run()));
            if let Err(payload) = result
                && let Err(recovery_payload) =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        owner.file_panic(payload)
                    }))
            {
                loop {
                    std::hint::black_box((&owner, &recovery_payload));
                    thread::park();
                }
            }
            if let Err(cleanup_payload) =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| owner.settle_native()))
            {
                // A cleanup panic proves neither native settlement nor safe Drop.
                // Retain both original primary and this payload with the whole
                // worker owner. The standalone shutdown budget may terminate the
                // process, but the library never drops or detaches this authority.
                loop {
                    std::hint::black_box((&owner, &cleanup_payload));
                    thread::park();
                }
            }
            owner.completion()
        })
}
struct Active {
    key: RunKey,
    request_digest: v2::Digest,
    cancel: CancelToken,
    start: StartWatch,
    _accepted: Option<CommittedRecord>,
    accepted_at: Instant,
    deadline: Instant,
    attempted: bool,
    child: Option<AcquiredChild>,
    spawn_failure: Option<theseus_host::process::SpawnFailure>,
    out: OutputState,
    stdout_eof: bool,
    stderr_eof: bool,
    status: Option<std::process::ExitStatus>,
    reason: v2::RunReason,
    outstanding: [Option<u32>; 4],
    boundary: Option<FinalBoundary>,
    committed: bool,
    outcome: Option<v2::Outcome>,
    cleanup_rounds: u8,
    terminal: bool,
    returned: u32,
    abandoned: bool,
}
struct PendingFileRequest {
    request: v2::Request,
    ticket: Option<ResponseTicket>,
    owner: Box<crate::files::FileOwner>,
    key: Option<RunKey>,
    create: Option<FileCreate>,
}
struct FileCreate {
    cancel: CancelToken,
    start: StartWatch,
    deadline: Instant,
    terminal: Option<v2::OperationView>,
    published: bool,
}
fn file_boundary(key: RunKey) -> FinalBoundary {
    FinalBoundary {
        key,
        final_ordinal: 0,
        stdout_bytes: 0,
        stderr_bytes: 0,
        truncated: false,
    }
}
struct PendingService {
    request: v2::Request,
    key: RunKey,
    cancel: CancelToken,
    start: StartWatch,
    deadline: Instant,
    attempted: bool,
    terminal: Option<v2::OperationView>,
    published: bool,
}
struct Worker {
    desktop: Option<crate::integration::desktop::DesktopRuntime>,
    desktop_tickets: std::collections::VecDeque<(v2::Request, ResponseTicket)>,
    ports: WorkerPorts,
    tokens: Vec<DataPermit>,
    pending: Option<WorkerEvent>,
    program: Option<v2::ProgramId>,
    invocation: Option<Invocation>,
    store: Option<LedgerStore>,
    workspace: Option<HostDir>,
    file: Option<PendingFileRequest>,
    service: Option<crate::integration::service::ServiceRuntime>,
    service_action: Option<PendingService>,
    lease: Option<(v2::IntentId, v2::LeaseId, v2::LeaseView)>,
    last_spawn: Option<theseus_host::process::SpawnFailure>,
    active: Option<Active>,
    response: Option<(ResponseTicket, v2::Reply, Option<RunKey>)>,
    failure: Option<OwnedWorkerFailure>,
    stopping: bool,
    native_authority_failure: Option<v2::PrefixAuthorityFailure>,
    prefix_mapping_error: Option<io::Error>,
}
impl Worker {
    fn new(ports: WorkerPorts, tokens: [DataPermit; 4]) -> Self {
        Self {
            desktop: None,
            desktop_tickets: std::collections::VecDeque::with_capacity(2),
            ports,
            tokens: Vec::from(tokens),
            pending: None,
            program: None,
            invocation: None,
            store: None,
            workspace: None,
            file: None,
            service: None,
            service_action: None,
            lease: None,
            last_spawn: None,
            active: None,
            response: None,
            failure: None,
            stopping: false,
            native_authority_failure: None,
            prefix_mapping_error: None,
        }
    }
    fn completion(&mut self) -> WorkerCompletion {
        // Settlement has finished, but a later primary must not drop the
        // original launch facts and typed sources still owned by this worker.
        let spawn = self
            .active
            .as_mut()
            .and_then(|a| a.spawn_failure.take())
            .or_else(|| self.last_spawn.take());
        if let Some(spawn) = spawn {
            if let Some(failure) = &mut self.failure {
                failure.retained_spawn = Some(Box::new(spawn));
            } else {
                self.failure = Some(OwnedWorkerFailure {
                    primary: WorkerPrimary::Spawn(spawn),
                    retained_file: None,
                    retained_spawn: None,
                    retained_service_errors: vec![],
                    cleanup_errors: std::array::from_fn(|_| None),
                });
            }
        }
        let desktop_errors = self
            .desktop
            .as_mut()
            .map(|d| std::mem::take(&mut d.errors))
            .unwrap_or_default();
        if !desktop_errors.is_empty() {
            if self.failure.is_none() {
                self.fail(WorkerPrimary::Io(io::Error::other(
                    "original desktop broker transport failed",
                )));
            }
            self.failure
                .as_mut()
                .unwrap()
                .retained_service_errors
                .extend(desktop_errors);
        }
        let service_errors = self
            .service
            .as_mut()
            .map(|s| std::mem::take(&mut s.errors))
            .unwrap_or_default();
        if !service_errors.is_empty() {
            if self.failure.is_none() {
                self.fail(WorkerPrimary::Io(io::Error::other(
                    "native service ownership/inspection failed",
                )));
            }
            self.failure
                .as_mut()
                .unwrap()
                .retained_service_errors
                .extend(service_errors);
        }
        self.retire_file();
        match self.failure.take() {
            Some(failure) => WorkerCompletion::Failed(Box::new(failure)),
            None => WorkerCompletion::Clean {
                store_poisoned: self.poisoned(),
                native_settled: true,
            },
        }
    }
    fn cleanup_results(&mut self, errors: [Option<io::Error>; 3]) {
        for error in errors.into_iter().flatten() {
            self.cleanup_error(error);
        }
    }
    fn poisoned(&self) -> bool {
        self.store
            .as_ref()
            .is_some_and(LedgerStore::is_write_poisoned)
    }
    fn fail(&mut self, primary: WorkerPrimary) {
        if self.failure.is_none() {
            self.failure = Some(OwnedWorkerFailure {
                primary,
                retained_file: None,
                retained_spawn: None,
                retained_service_errors: vec![],
                cleanup_errors: std::array::from_fn(|_| None),
            });
        }
        self.stopping = true;
    }
    fn cleanup_error(&mut self, error: io::Error) {
        if let Some(f) = &mut self.failure {
            if let Some(slot) = f.cleanup_errors.iter_mut().find(|s| s.is_none()) {
                *slot = Some(error);
            }
        } else {
            self.fail(WorkerPrimary::Io(error));
        }
    }
    fn publish(&mut self, event: WorkerEvent) {
        assert!(self.pending.is_none());
        match self.ports.events.try_send(event) {
            Ok(()) => {}
            Err(TrySendError::Full(event)) => self.pending = Some(event),
            Err(TrySendError::Disconnected(event)) => {
                self.pending = Some(event);
                self.stopping = true;
            }
        }
    }
    fn flush(&mut self) -> bool {
        if let Some(event) = self.pending.take() {
            self.publish(event);
        }
        self.pending.is_none()
    }
    fn append(&mut self, entry: Entry) -> Result<CommittedRecord, WorkerPrimary> {
        let store = self
            .store
            .as_mut()
            .ok_or_else(|| WorkerPrimary::Io(io::Error::other("store unavailable")))?;
        let record = Record::new(store.records().len() as u16, entry)
            .map_err(|e| WorkerPrimary::Store(e.into()))?;
        store.append(record).map_err(WorkerPrimary::Store)
    }
    fn respond(&mut self, ticket: ResponseTicket, reply: v2::Reply, start_key: Option<RunKey>) {
        self.publish(WorkerEvent::ResponseReady {
            ticket,
            reply,
            start_key,
        });
    }
    fn refuse(&mut self, ticket: ResponseTicket, code: v2::RefusalCode) {
        self.respond(
            ticket,
            v2::Reply::Refused {
                code,
                inspect_failure: None,
            },
            None,
        );
    }
    fn run(&mut self) {
        loop {
            self.stopping |= self.ports.stop.load(Ordering::Acquire);
            self.returns();
            let available = self.flush();
            if self.active.is_some()
                && let Err(primary) = self.advance()
            {
                self.fail(primary);
            }
            if self.file.as_ref().is_some_and(|p| p.create.is_some())
                && let Err(primary) = self.advance_file()
            {
                self.fail(primary);
            }
            if self.service_action.is_some()
                && let Err(primary) = self.advance_service()
            {
                self.fail(primary);
            }
            if self.pending.is_none()
                && self.response.is_none()
                && self.desktop.as_ref().is_some_and(|d| d.is_pending())
                && let Err(error) = self.advance_desktop()
            {
                self.fail(error);
            }
            if self.desktop.as_ref().is_some_and(|d| d.unavailable)
                && self.pending.is_none()
                && self.response.is_none()
                && let Some((_, ticket)) = self.desktop_tickets.pop_front()
            {
                self.refuse(ticket, v2::RefusalCode::OwnershipPending);
            }
            if self.failure.is_some() {
                break;
            }
            if self.stopping
                && self.active.as_ref().is_none_or(|a| a.committed)
                && self.file.as_ref().is_none_or(|f| {
                    f.create
                        .as_ref()
                        .is_none_or(|c| c.terminal.is_some() && c.published)
                })
                && self
                    .service_action
                    .as_ref()
                    .is_none_or(|s| s.terminal.is_some() && s.published)
                && self.pending.is_none()
                && self.response.is_none()
            {
                break;
            }
            if self.pending.is_none()
                && let Some((ticket, reply, key)) = self.response.take()
            {
                self.respond(ticket, reply, key);
            }
            if available && self.pending.is_none() && self.response.is_none() && !self.stopping {
                match self.ports.commands.try_recv() {
                    Ok(command) => {
                        if let Err(e) = self.command(command) {
                            self.fail(e)
                        }
                    }
                    Err(TryRecvError::Disconnected) => self.stopping = true,
                    Err(TryRecvError::Empty) => {}
                }
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
    fn command(&mut self, command: WorkerCommand) -> Result<(), WorkerPrimary> {
        match command {
            WorkerCommand::Desktop { request, ticket } => self.begin_desktop(request, ticket)?,
            WorkerCommand::ProbeReadiness { request, ticket } => {
                self.probe_readiness(request, ticket)?
            }
            WorkerCommand::ServiceAction {
                request,
                ticket,
                cancel,
                start,
            } => self.begin_service(request, ticket, cancel, start)?,
            WorkerCommand::InspectService { request, ticket } => {
                self.inspect_service(request, ticket)?
            }
            WorkerCommand::Init(invocation) => self.initialize(invocation)?,
            WorkerCommand::Acquire { request, ticket } => self.acquire(request, ticket)?,
            WorkerCommand::Run {
                request,
                ticket,
                cancel,
                start,
            } => self.begin(request, ticket, cancel, start)?,
            WorkerCommand::CreateFile {
                request,
                ticket,
                cancel,
                start,
            } => self.begin_file(request, ticket, cancel, start)?,
            WorkerCommand::ReadFile { request, ticket }
            | WorkerCommand::InspectFile { request, ticket } => {
                self.observe_file(request, ticket)?
            }
            WorkerCommand::Release { lease, ticket } => self.release(lease, ticket)?,
            WorkerCommand::Stop => self.stopping = true,
            #[cfg(test)]
            WorkerCommand::ReadPhysicalLedger(reply) => {
                let physical = match self.store.as_mut() {
                    Some(store) => store.read_physical().map_err(|e| format!("{e:?}")),
                    None => Err("no open store".into()),
                };
                let _ = reply.send(physical);
            }
        }
        Ok(())
    }
}
