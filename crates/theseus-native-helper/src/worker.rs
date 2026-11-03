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
            if let Err(payload) = result {
                owner.fail(WorkerPrimary::Panic(payload));
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
struct Worker {
    ports: WorkerPorts,
    tokens: Vec<DataPermit>,
    pending: Option<WorkerEvent>,
    program: Option<v2::ProgramId>,
    invocation: Option<Invocation>,
    store: Option<LedgerStore>,
    lease: Option<(v2::IntentId, v2::LeaseId, v2::LeaseView)>,
    last_spawn: Option<theseus_host::process::SpawnFailure>,
    active: Option<Active>,
    response: Option<(ResponseTicket, v2::Reply, Option<RunKey>)>,
    failure: Option<OwnedWorkerFailure>,
    stopping: bool,
}
impl Worker {
    fn new(ports: WorkerPorts, tokens: [DataPermit; 4]) -> Self {
        Self {
            ports,
            tokens: Vec::from(tokens),
            pending: None,
            program: None,
            invocation: None,
            store: None,
            lease: None,
            last_spawn: None,
            active: None,
            response: None,
            failure: None,
            stopping: false,
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
                    retained_spawn: None,
                    cleanup_errors: std::array::from_fn(|_| None),
                });
            }
        }
        match self.failure.take() {
            Some(failure) => WorkerCompletion::Failed(failure),
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
                retained_spawn: None,
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
        self.respond(ticket, v2::Reply::Refused { code }, None);
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
            if self.failure.is_some() {
                break;
            }
            if self.stopping && self.active.as_ref().is_none_or(|a| a.committed) {
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
            WorkerCommand::Init(invocation) => self.initialize(invocation)?,
            WorkerCommand::Acquire { request, ticket } => self.acquire(request, ticket)?,
            WorkerCommand::Run {
                request,
                ticket,
                cancel,
                start,
            } => self.begin(request, ticket, cancel, start)?,
            WorkerCommand::Release { lease, ticket } => self.release(lease, ticket)?,
            WorkerCommand::Stop => self.stopping = true,
        }
        Ok(())
    }
    fn initialize(&mut self, inv: Invocation) -> Result<(), WorkerPrimary> {
        if self.invocation.is_some() {
            return Err(WorkerPrimary::Io(io::Error::other(
                "duplicate initialization",
            )));
        }
        #[cfg(feature = "test-support")]
        if let Some(control) = &inv.fixture {
            control.init_reached();
        }
        let ledger =
            HostDir::open(&inv.ledger_root, RootPolicy::LeafNoFollow).map_err(WorkerPrimary::Io)?;
        let workspace = HostDir::open(&inv.workspace_root, RootPolicy::LeafNoFollow)
            .map_err(WorkerPrimary::Io)?;
        let expected = theseus_native_helper_store::StoreBinding {
            binding: inv.expected.binding,
            manifest_digest: inv.expected.manifest_digest,
            workspace_marker: inv.expected.workspace_marker,
            os: inv.expected.os,
        };
        if expected.os != crate::invocation::host_os() {
            return Err(WorkerPrimary::Io(io::Error::other("wrong host OS")));
        }
        let store =
            LedgerStore::open(&ledger, &workspace, expected).map_err(WorkerPrimary::Store)?;
        let helper_principal =
            match theseus_host::principal::current_principal().map_err(WorkerPrimary::Io)? {
                theseus_host::principal::CurrentPrincipal::Posix {
                    effective_uid,
                    effective_gid,
                } => v2::Principal::Posix {
                    effective_uid,
                    effective_gid,
                },
                theseus_host::principal::CurrentPrincipal::Windows { token_user_sid } => {
                    v2::Principal::Windows {
                        token_user_sid: v2::SidText::new(&token_user_sid).map_err(|_| {
                            WorkerPrimary::Io(io::Error::other("invalid native principal"))
                        })?,
                    }
                }
            };
        let mut operations = Vec::new();
        for record in store.records() {
            match record.entry() {
                Entry::LeaseReserved { intent, lease } => {
                    self.lease = Some((
                        *intent,
                        *lease,
                        store
                            .restart_view()
                            .query_lease(*lease)
                            .map_err(|_| WorkerPrimary::Io(io::Error::other("invalid lease")))?,
                    ))
                }
                Entry::OperationAccepted {
                    lease,
                    operation,
                    request_digest,
                    operation_kind,
                } => operations.push(RecoveredOperation {
                    lease: *lease,
                    operation: *operation,
                    request_digest: *request_digest,
                    kind: *operation_kind,
                    view: store
                        .restart_view()
                        .query_operation(*lease, *operation)
                        .map_err(|_| WorkerPrimary::Io(io::Error::other("invalid operation")))?,
                }),
                _ => {}
            }
        }
        let description = v2::Reply::Description {
            binding: inv.expected.binding,
            manifest_digest: inv.expected.manifest_digest,
            workspace_marker: inv.expected.workspace_marker,
            os: crate::invocation::host_os(),
            helper_principal,
            programs: v2::ProgramList::new(inv.catalog.iter().map(|e| e.id.clone()).collect())
                .map_err(|_| WorkerPrimary::Io(io::Error::other("invalid catalog")))?,
        };
        self.store = Some(store);
        self.invocation = Some(inv);
        self.publish(WorkerEvent::CacheUpdate(CachePatch::Initialized {
            description,
            lease: self.lease.clone(),
            operations,
        }));
        Ok(())
    }
    fn acquire(
        &mut self,
        request: v2::Request,
        ticket: ResponseTicket,
    ) -> Result<(), WorkerPrimary> {
        let v2::Request::Acquire {
            intent,
            binding,
            manifest_digest,
        } = request
        else {
            self.refuse(ticket, v2::RefusalCode::InvalidRequest);
            return Ok(());
        };
        let Some(inv) = &self.invocation else {
            self.refuse(ticket, v2::RefusalCode::OwnershipPending);
            return Ok(());
        };
        if inv.expected.binding != binding {
            self.refuse(ticket, v2::RefusalCode::WrongBinding);
            return Ok(());
        }
        if inv.expected.manifest_digest != manifest_digest {
            self.refuse(ticket, v2::RefusalCode::WrongManifest);
            return Ok(());
        }
        if let Some((existing, lease, view)) = &self.lease {
            let reply = if *existing == intent {
                v2::Reply::Acquired {
                    intent,
                    binding,
                    manifest_digest,
                    state: v2::AcquireView::Lease {
                        lease: *lease,
                        state: view.clone(),
                    },
                }
            } else {
                v2::Reply::Refused {
                    code: v2::RefusalCode::Busy,
                }
            };
            self.respond(ticket, reply, None);
            return Ok(());
        }
        let mut bytes = [0; 16];
        theseus_host::random_bytes(&mut bytes).map_err(WorkerPrimary::Io)?;
        let lease = v2::LeaseId::from_bytes(bytes);
        let view = v2::LeaseView::Held { intent };
        let _committed = self.append(Entry::LeaseReserved { intent, lease })?;
        #[cfg(feature = "test-support")]
        if let Some(control) = self
            .invocation
            .as_ref()
            .and_then(|inv| inv.fixture.as_ref())
        {
            control.mark_acquire_committed();
        }
        self.lease = Some((intent, lease, view.clone()));
        self.response = Some((
            ticket,
            v2::Reply::Acquired {
                intent,
                binding,
                manifest_digest,
                state: v2::AcquireView::Lease {
                    lease,
                    state: view.clone(),
                },
            },
            None,
        ));
        self.publish(WorkerEvent::CacheUpdate(CachePatch::Lease {
            intent,
            lease,
            view,
        }));
        Ok(())
    }
    fn begin(
        &mut self,
        request: v2::Request,
        ticket: ResponseTicket,
        cancel: CancelToken,
        start: StartWatch,
    ) -> Result<(), WorkerPrimary> {
        if self.active.is_some() {
            self.refuse(ticket, v2::RefusalCode::Busy);
            return Ok(());
        }
        let v2::Request::Run {
            lease,
            operation,
            program,
        } = &request
        else {
            self.refuse(ticket, v2::RefusalCode::InvalidRequest);
            return Ok(());
        };
        let lease = *lease;
        let operation = *operation;
        if !self.lease.as_ref().is_some_and(|(_, l, _)| *l == lease) {
            self.refuse(ticket, v2::RefusalCode::WrongLease);
            return Ok(());
        }
        let key = cancel.key;
        if key.lease != lease || key.operation != operation || start.key() != key {
            self.refuse(ticket, v2::RefusalCode::InvalidRequest);
            return Ok(());
        }
        match self
            .store
            .as_ref()
            .unwrap()
            .restart_view()
            .replay_operation(&request)
        {
            Ok(v2::ledger::ReplayDecision::NotRecorded) => {}
            Ok(v2::ledger::ReplayDecision::Recorded(view)) => {
                let now = Instant::now();
                let out = OutputState::new(key);
                let boundary = out.boundary();
                self.active = Some(Active {
                    key,
                    cancel,
                    start,
                    _accepted: None,
                    accepted_at: now,
                    deadline: now,
                    attempted: false,
                    child: None,
                    spawn_failure: None,
                    out,
                    stdout_eof: true,
                    stderr_eof: true,
                    status: None,
                    reason: v2::RunReason::Natural,
                    outstanding: [None; 4],
                    boundary: Some(boundary),
                    committed: true,
                    outcome: None,
                    cleanup_rounds: 0,
                    terminal: true,
                    returned: 0,
                    abandoned: false,
                });
                self.response = Some((
                    ticket,
                    v2::Reply::Operation {
                        lease,
                        operation,
                        state: view.clone(),
                    },
                    None,
                ));
                self.publish(WorkerEvent::CacheUpdate(CachePatch::Terminal {
                    key,
                    view,
                    boundary,
                }));
                return Ok(());
            }
            Err(code) => {
                self.refuse(ticket, code);
                return Ok(());
            }
        }
        if !self
            .invocation
            .as_ref()
            .is_some_and(|i| i.catalog.iter().any(|c| &c.id == program))
        {
            self.refuse(ticket, v2::RefusalCode::Unsupported);
            return Ok(());
        }
        self.last_spawn = None;
        let digest = v2::ledger::request_digest(&request)
            .map_err(|_| WorkerPrimary::Io(io::Error::other("invalid request digest")))?;
        let committed = self.append(Entry::OperationAccepted {
            lease,
            operation,
            request_digest: digest,
            operation_kind: v2::OperationKind::Run,
        })?;
        #[cfg(feature = "test-support")]
        if let Some(control) = self
            .invocation
            .as_ref()
            .and_then(|inv| inv.fixture.as_ref())
        {
            control.mark_accepted_committed();
        }
        let accepted_at = Instant::now();
        let deadline = accepted_at + Duration::from_secs(30);
        self.active = Some(Active {
            key,
            cancel,
            start,
            _accepted: Some(committed),
            accepted_at,
            deadline,
            attempted: false,
            child: None,
            spawn_failure: None,
            out: OutputState::new(key),
            stdout_eof: false,
            stderr_eof: false,
            status: None,
            reason: v2::RunReason::Natural,
            outstanding: [None; 4],
            boundary: None,
            committed: false,
            outcome: None,
            cleanup_rounds: 0,
            terminal: false,
            returned: 0,
            abandoned: false,
        });
        #[cfg(feature = "test-support")]
        {
            self.active.as_mut().unwrap().out.fixture =
                self.invocation.as_ref().and_then(|inv| inv.fixture.clone());
        }
        // Catalog selection is retained by digest and immutable invocation; no peer argv.
        self.program = Some(program.clone());
        self.response = Some((
            ticket,
            v2::Reply::Operation {
                lease,
                operation,
                state: v2::OperationView::Accepted {
                    operation_kind: v2::OperationKind::Run,
                },
            },
            Some(key),
        ));
        self.publish(WorkerEvent::CacheUpdate(CachePatch::Phase {
            key,
            kind: v2::OperationKind::Run,
            phase: LivePhase::Accepted,
            accepted_at: Some(accepted_at),
            deadline: Some(deadline),
        }));
        Ok(())
    }
    fn release(&mut self, lease: v2::LeaseId, ticket: ResponseTicket) -> Result<(), WorkerPrimary> {
        if self.active.is_some() {
            self.refuse(ticket, v2::RefusalCode::Busy);
            return Ok(());
        }
        let Some((intent, actual, old)) = self.lease.clone() else {
            self.refuse(ticket, v2::RefusalCode::WrongLease);
            return Ok(());
        };
        if actual != lease {
            self.refuse(ticket, v2::RefusalCode::WrongLease);
            return Ok(());
        }
        if !matches!(old, v2::LeaseView::Held { .. }) {
            self.respond(ticket, v2::Reply::Lease { lease, state: old }, None);
            return Ok(());
        }
        self.append(Entry::LeaseClosed {
            lease,
            disposition: Disposition::AwaitingExternalReset,
        })?;
        let view = v2::LeaseView::AwaitingExternalReset { intent };
        self.lease = Some((intent, lease, view.clone()));
        self.response = Some((
            ticket,
            v2::Reply::Lease {
                lease,
                state: view.clone(),
            },
            None,
        ));
        self.publish(WorkerEvent::CacheUpdate(CachePatch::Lease {
            intent,
            lease,
            view,
        }));
        Ok(())
    }
    fn returns(&mut self) {
        for _ in 0..4 {
            match self.ports.returned.try_recv() {
                Ok(DataReturn::Reusable {
                    key,
                    ordinal,
                    disposition,
                    permit,
                }) => {
                    if let Some(a) = &mut self.active {
                        if key != a.key
                            || ordinal != a.returned + 1
                            || ordinal > a.out.ordinal
                            || a.outstanding.get(permit.id as usize) != Some(&Some(ordinal))
                        {
                            self.fail(WorkerPrimary::Io(io::Error::other("invalid token return")));
                            self.tokens.push(permit);
                            return;
                        }
                        a.outstanding[permit.id as usize] = None;
                        a.returned = ordinal;
                        a.abandoned |=
                            matches!(disposition, ReturnDisposition::AbandonedBeforeSubmit);
                    }
                    self.tokens.push(permit);
                }
                Ok(DataReturn::ProducerDrained { key, final_ordinal }) => {
                    if self.active.as_ref().is_some_and(|a| {
                        a.key == key
                            && a.terminal
                            && !a.abandoned
                            && a.boundary.is_some_and(|b| b.final_ordinal == final_ordinal)
                            && a.returned == final_ordinal
                    }) {
                        self.active = None;
                        self.program = None;
                    } else {
                        self.fail(WorkerPrimary::Io(io::Error::other(
                            "invalid producer drain",
                        )));
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.stopping = true;
                    break;
                }
            }
        }
    }
    fn advance(&mut self) -> Result<(), WorkerPrimary> {
        self.advance_at(Instant::now())
    }
    fn advance_at(&mut self, now: Instant) -> Result<(), WorkerPrimary> {
        let a = self.active.as_mut().unwrap();
        if a.terminal {
            // CachePatch::Terminal is the sole frozen boundary publication.
            // A second event could outlive retirement and hit the next RunKey.
            return Ok(());
        }
        if a.committed {
            if self.pending.is_none() {
                let key = a.key;
                let boundary = a.boundary.unwrap();
                let view = v2::OperationView::Finished {
                    outcome: a.outcome.take().unwrap(),
                };
                a.terminal = true;
                self.publish(WorkerEvent::CacheUpdate(CachePatch::Terminal {
                    key,
                    view,
                    boundary,
                }));
            }
            return Ok(());
        }
        let expired = now >= a.deadline;
        let cancelled = self.stopping
            || a.cancel.flag.load(Ordering::Acquire)
            || matches!(a.start.poll(), StartState::Aborted);
        if expired {
            a.reason = v2::RunReason::Deadline;
        } else if cancelled {
            a.reason = v2::RunReason::CancelRequested;
        }
        if !a.attempted {
            if expired || cancelled {
                let reason = if expired {
                    v2::NotStartedReason::Deadline
                } else {
                    v2::NotStartedReason::CancelRequested
                };
                return self.finish(v2::Outcome::NotStarted { reason });
            }
            if !matches!(a.start.poll(), StartState::Granted) {
                return Ok(());
            }
            #[cfg(feature = "test-support")]
            if let Some(control) = self
                .invocation
                .as_ref()
                .and_then(|inv| inv.fixture.as_ref())
            {
                control.before_spawn();
                // A fixture gate may hold across cancellation, stop or deadline.
                let expired = Instant::now() >= a.deadline;
                let cancelled = self.stopping
                    || self.ports.stop.load(Ordering::Acquire)
                    || a.cancel.flag.load(Ordering::Acquire)
                    || matches!(a.start.poll(), StartState::Aborted);
                if expired || cancelled {
                    let reason = if expired {
                        v2::NotStartedReason::Deadline
                    } else {
                        v2::NotStartedReason::CancelRequested
                    };
                    return self.finish(v2::Outcome::NotStarted { reason });
                }
            }
            a.attempted = true;
            let command = &self
                .invocation
                .as_ref()
                .unwrap()
                .catalog
                .iter()
                .find(|c| Some(&c.id) == self.program.as_ref())
                .unwrap()
                .command;
            #[cfg(feature = "test-support")]
            if let Some(control) = self
                .invocation
                .as_ref()
                .and_then(|inv| inv.fixture.as_ref())
            {
                control.mark_spawn_attempted();
            }
            match OwnedChild::spawn(command, InputMode::Null) {
                Ok(child) => {
                    a.child = Some(child);
                    #[cfg(feature = "test-support")]
                    if let Some(control) = self
                        .invocation
                        .as_ref()
                        .and_then(|inv| inv.fixture.as_ref())
                    {
                        control.mark_spawn_owned();
                        control.after_owned_spawn();
                    }
                }
                Err(failure) => a.spawn_failure = Some(failure),
            }
            if a.child.is_some() && self.pending.is_none() {
                let key = a.key;
                let deadline = a.deadline;
                let accepted_at = a.accepted_at;
                self.publish(WorkerEvent::CacheUpdate(CachePatch::Phase {
                    key,
                    kind: v2::OperationKind::Run,
                    phase: LivePhase::Running,
                    accepted_at: Some(accepted_at),
                    deadline: Some(deadline),
                }));
            }
        }
        let a = self.active.as_mut().unwrap();
        if let Some(failure) = &mut a.spawn_failure {
            if let HostCleanup::Pending(cleanup) = &mut failure.cleanup {
                if !cleanup.try_complete().map_err(WorkerPrimary::Io)? {
                    return Ok(());
                }
                failure.cleanup = HostCleanup::Complete;
            }
            let phase = match failure.launch {
                theseus_host::process::LaunchPhase::NeverCreated => v2::LaunchPhase::NeverCreated,
                theseus_host::process::LaunchPhase::CreatedSuspended => {
                    v2::LaunchPhase::CreatedSuspended
                }
                theseus_host::process::LaunchPhase::MayRun => v2::LaunchPhase::MayRun,
            };
            let cleanup = match phase {
                v2::LaunchPhase::NeverCreated => v2::SpawnCleanup::NoChildCreated,
                _ => {
                    if cfg!(windows) {
                        v2::SpawnCleanup::WindowsProcessExitedJobEmpty
                    } else {
                        v2::SpawnCleanup::PosixLeaderReapedGroupTerminationRequested
                    }
                }
            };
            return self.finish(v2::Outcome::LaunchFailed {
                phase,
                code: v2::FailureCode::NativeFailure,
                cleanup,
            });
        }
        let a = self.active.as_mut().unwrap();
        if let Some(child) = &mut a.child {
            read_stream(
                &self.ports,
                &mut self.tokens,
                &mut a.out,
                &mut a.outstanding,
                &mut child.stdout,
                &mut a.stdout_eof,
                v2::Channel::Stdout,
            )
            .map_err(WorkerPrimary::Io)?;
            read_stream(
                &self.ports,
                &mut self.tokens,
                &mut a.out,
                &mut a.outstanding,
                &mut child.stderr,
                &mut a.stderr_eof,
                v2::Channel::Stderr,
            )
            .map_err(WorkerPrimary::Io)?;
            // Observe natural exit with reserved original child authority before cleanup.
            if a.status.is_none()
                && (expired
                    || cancelled
                    || child.child.exited_reserved().map_err(WorkerPrimary::Io)?)
            {
                if self.pending.is_none() {
                    let event = WorkerEvent::CacheUpdate(CachePatch::Phase {
                        key: a.key,
                        kind: v2::OperationKind::Run,
                        phase: LivePhase::CleanupPending,
                        accepted_at: Some(a.accepted_at),
                        deadline: Some(a.deadline),
                    });
                    match self.ports.events.try_send(event) {
                        Ok(()) => {}
                        Err(TrySendError::Full(e)) => self.pending = Some(e),
                        Err(TrySendError::Disconnected(e)) => {
                            self.pending = Some(e);
                            self.stopping = true;
                        }
                    }
                }
                child.child.terminate_owned().map_err(WorkerPrimary::Io)?;
                a.status = child
                    .child
                    .try_reap_and_release()
                    .map_err(WorkerPrimary::Io)?;
            }
            if let Some(observed_status) = a.status {
                // A single-process fixture's output reaches EOF; for other descendants close
                // owned endpoints after bounded reads and honestly mark omitted output.
                if !(a.stdout_eof && a.stderr_eof) {
                    a.cleanup_rounds = a.cleanup_rounds.saturating_add(1);
                    if a.cleanup_rounds < 8 {
                        return Ok(());
                    }
                    a.out.truncated = true;
                }
                child
                    .stdout
                    .cancel_and_complete()
                    .map_err(WorkerPrimary::Io)?;
                child
                    .stderr
                    .cancel_and_complete()
                    .map_err(WorkerPrimary::Io)?;
                let native = status(observed_status).map_err(WorkerPrimary::Io)?;
                let outcome = v2::Outcome::RunCompleted {
                    reason: a.reason,
                    status: native,
                    cleanup: cleanup_fact(),
                    stdout_bytes: a.out.stdout,
                    stderr_bytes: a.out.stderr,
                    truncated: a.out.truncated,
                };
                a.child = None;
                return self.finish(outcome);
            }
        }
        Ok(())
    }
    fn finish(&mut self, outcome: v2::Outcome) -> Result<(), WorkerPrimary> {
        let a = self.active.as_mut().unwrap();
        // Producer boundary freezes before the physical terminal barrier. No client
        // acknowledgment or ProducerDrained participates in this persistence decision.
        let boundary = *a.boundary.get_or_insert_with(|| a.out.boundary());
        let key = a.key;
        if !a.committed && self.pending.is_none() {
            let accepted_at = a.accepted_at;
            let deadline = a.deadline;
            self.publish(WorkerEvent::CacheUpdate(CachePatch::Phase {
                key,
                kind: v2::OperationKind::Run,
                phase: LivePhase::TerminalCommitPending,
                accepted_at: Some(accepted_at),
                deadline: Some(deadline),
            }));
        }
        let a = self.active.as_mut().unwrap();
        if !a.committed {
            self.append(Entry::OperationFinished {
                lease: key.lease,
                operation: key.operation,
                outcome: outcome.clone(),
            })?;
            #[cfg(feature = "test-support")]
            if let Some(control) = self
                .invocation
                .as_ref()
                .and_then(|inv| inv.fixture.as_ref())
            {
                control.mark_terminal_committed();
            }
            self.active.as_mut().unwrap().committed = true;
            self.last_spawn = self.active.as_mut().unwrap().spawn_failure.take();
            self.active.as_mut().unwrap().outcome = Some(outcome);
        }
        if self.pending.is_none() {
            let a = self.active.as_mut().unwrap();
            let view = v2::OperationView::Finished {
                outcome: a.outcome.take().unwrap(),
            };
            a.terminal = true;
            self.publish(WorkerEvent::CacheUpdate(CachePatch::Terminal {
                key,
                view,
                boundary,
            }));
        }
        Ok(())
    }
    fn settle_native(&mut self) {
        // Retain every authority on errors and retry; completion never means detached.
        loop {
            let mut complete = true;
            if let Some(a) = &mut self.active {
                if let Some(child) = &mut a.child {
                    let stdout = child.stdout.cancel_and_complete();
                    let stderr = child.stderr.cancel_and_complete();
                    let reap = child.child.try_reap_and_release();
                    complete = stdout.is_ok() && stderr.is_ok() && matches!(reap, Ok(Some(_)));
                    if complete {
                        a.child = None;
                    } else {
                        self.cleanup_results([stdout.err(), stderr.err(), reap.err()]);
                    }
                }
                if let Some(a) = &mut self.active
                    && let Some(f) = &mut a.spawn_failure
                    && let HostCleanup::Pending(cleanup) = &mut f.cleanup
                {
                    match cleanup.try_complete() {
                        Ok(true) => f.cleanup = HostCleanup::Complete,
                        Ok(false) => complete = false,
                        Err(e) => {
                            complete = false;
                            self.cleanup_error(e);
                        }
                    }
                }
            }
            if complete {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
}
fn read_stream(
    ports: &WorkerPorts,
    tokens: &mut Vec<DataPermit>,
    out: &mut OutputState,
    outstanding: &mut [Option<u32>; 4],
    reader: &mut PipeReader,
    eof: &mut bool,
    channel: v2::Channel,
) -> io::Result<()> {
    if *eof {
        return Ok(());
    }
    let mut discard = [0; 4096];
    let n = match reader.try_read(&mut discard) {
        Ok(0) => {
            *eof = true;
            return Ok(());
        }
        Ok(n) => n,
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
        Err(e) => return Err(e),
    };
    let (offset, retain) = out.observe(channel, n, !tokens.is_empty())?;
    if retain == 0 {
        #[cfg(feature = "test-support")]
        if tokens.is_empty()
            && let Some(control) = &out.fixture
        {
            // Nonempty child bytes are actually discarded without a permit.
            control.mark_capture_saturated();
        }
        return Ok(());
    }
    let mut permit = tokens.pop().unwrap();
    permit.storage[..retain].copy_from_slice(&discard[..retain]);
    let chunk = DataChunk {
        key: out.key,
        ordinal: out.ordinal + 1,
        channel,
        offset,
        len: retain as u16,
        permit,
    };
    let id = chunk.permit.id as usize;
    match ports.data.try_send(chunk) {
        Ok(()) => {
            out.ordinal += 1;
            outstanding[id] = Some(out.ordinal);
        }
        Err(TrySendError::Full(chunk)) => {
            out.truncated = true;
            tokens.push(chunk.permit)
        }
        Err(TrySendError::Disconnected(chunk)) => {
            out.truncated = true;
            tokens.push(chunk.permit)
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stopped_empty_worker_joins_without_native_owners() {
        let (dispatcher, ports, _, tokens) = channels();
        dispatcher
            .stop
            .store(true, std::sync::atomic::Ordering::Release);
        let join = spawn(ports, tokens).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !join.is_finished() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(matches!(
            join.join().unwrap(),
            WorkerCompletion::Clean {
                native_settled: true,
                ..
            }
        ));
    }
}
#[cfg(test)]
mod native_tests {
    use super::*;
    use std::io::Write;
    use theseus_host::process::{ChildEnvironment, NativeCommand};
    use theseus_native_helper_store::{BootstrapRequest, StoreBinding, bootstrap};
    #[test]
    #[ignore]
    fn fixed_probe_child() {
        let Some(path) = std::env::var_os("THESEUS_FIXED_PROBE_RECEIPT") else {
            return;
        };
        let path = std::path::PathBuf::from(path);
        let nonce = std::env::var("THESEUS_FIXED_PROBE_NONCE").unwrap();
        std::fs::write(path.with_extension("started"), nonce.as_bytes()).unwrap();
        if std::env::var_os("THESEUS_FIXED_PROBE_PACED").is_some() {
            let deadline = Instant::now() + Duration::from_secs(10);
            // One bounded batch waits for an explicit parent acknowledgment.
            // A descheduled parent cannot let the child consume all four tokens
            // and drain the remainder before the recycling proof has run.
            for stage in 1..=6 {
                std::io::stderr().write_all(&[b'e'; 1024]).unwrap();
                std::io::stderr().flush().unwrap();
                let progress = format!("{nonce}:{stage}");
                std::fs::write(path.with_extension("progress"), progress.as_bytes()).unwrap();
                while std::fs::read(path.with_extension("ack")).ok().as_deref()
                    != Some(progress.as_bytes())
                {
                    assert!(
                        Instant::now() < deadline,
                        "bounded original fixture acknowledgment"
                    );
                    thread::yield_now();
                }
            }
        }
        std::io::stdout().write_all(&[b'o'; 32768]).unwrap();
        std::io::stderr().write_all(&[b'e'; 32768]).unwrap();
        std::fs::write(path, nonce.as_bytes()).unwrap();
    }

    fn event(ports: &DispatcherPorts) -> WorkerEvent {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(e) = ports.events.try_recv() {
                return e;
            }
            assert!(Instant::now() < deadline, "bounded worker event deadline");
            thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn physical_finish_does_not_wait_for_output_drain() {
        run_native(false);
    }
    #[test]
    fn readily_drained_native_streams_share_lifetime_budget() {
        run_native(true);
    }
    fn run_native(drain: bool) {
        let tmp = tempfile::tempdir().unwrap();
        let lp = tmp.path().join("ledger");
        let wp = tmp.path().join("workspace");
        let l = HostDir::create_private(&lp).unwrap();
        let w = HostDir::create_private(&wp).unwrap();
        let binding = v2::BindingId::from_bytes([1; 16]);
        let manifest_digest = v2::Digest::from_bytes([2; 32]);
        let receipt = bootstrap(
            &l,
            &w,
            BootstrapRequest {
                binding,
                manifest_digest,
                os: crate::invocation::host_os(),
            },
        )
        .unwrap();
        let nonce = tmp.path().join("completion");
        let mut nonce_bytes = [0; 16];
        theseus_host::random_bytes(&mut nonce_bytes).unwrap();
        let nonce_value: String = nonce_bytes.iter().map(|b| format!("{b:02x}")).collect();
        let mut command = NativeCommand::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "worker::native_tests::fixed_probe_child",
            "--ignored",
            "--nocapture",
        ]);
        command.environment = ChildEnvironment::Replace(vec![
            (
                "THESEUS_FIXED_PROBE_RECEIPT".into(),
                nonce.clone().into_os_string(),
            ),
            (
                "THESEUS_FIXED_PROBE_NONCE".into(),
                nonce_value.clone().into(),
            ),
        ]);
        if drain && let ChildEnvironment::Replace(entries) = &mut command.environment {
            entries.push(("THESEUS_FIXED_PROBE_PACED".into(), "1".into()));
        }
        let inv = Invocation::new(
            lp.clone(),
            wp,
            StoreBinding {
                binding,
                manifest_digest,
                workspace_marker: receipt.workspace_marker(),
                os: crate::invocation::host_os(),
            },
            vec![crate::CatalogEntry {
                id: v2::ProgramId::new("probe").unwrap(),
                command,
            }],
        )
        .unwrap();
        #[cfg(feature = "test-support")]
        let control =
            crate::test_support::FixtureControl::new(crate::test_support::FixtureMode::Observe);
        #[cfg(feature = "test-support")]
        let inv = {
            let mut inv = inv;
            inv.fixture = Some(control.clone());
            inv
        };
        let (ports, worker, [permit0, permit1], tokens) = channels();
        let join = spawn(worker, tokens).unwrap();
        ports
            .commands
            .try_send(WorkerCommand::Init(inv))
            .ok()
            .unwrap();
        assert!(matches!(
            event(&ports),
            WorkerEvent::CacheUpdate(CachePatch::Initialized { .. })
        ));
        #[cfg(feature = "test-support")]
        assert!(control.snapshot().init_reached);
        let intent = v2::IntentId::from_bytes([3; 16]);
        ports
            .commands
            .try_send(WorkerCommand::Acquire {
                request: v2::Request::Acquire {
                    intent,
                    binding,
                    manifest_digest,
                },
                ticket: ResponseTicket {
                    reply_to: 3,
                    permit: permit0,
                },
            })
            .ok()
            .unwrap();
        let lease = match event(&ports) {
            WorkerEvent::CacheUpdate(CachePatch::Lease { lease, .. }) => lease,
            _ => panic!("lease cache"),
        };
        let _acquire_permit = match event(&ports) {
            WorkerEvent::ResponseReady {
                ticket,
                reply: v2::Reply::Acquired { .. },
                ..
            } => ticket.permit,
            _ => panic!("acquire response"),
        };
        #[cfg(feature = "test-support")]
        assert!(control.snapshot().acquire_committed);
        let operation = v2::OperationId::from_bytes([4; 16]);
        let key = RunKey {
            lease,
            operation,
            generation: 1,
        };
        let (mut grant, start) = start_pair(key, 4);
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        ports
            .commands
            .try_send(WorkerCommand::Run {
                request: v2::Request::Run {
                    lease,
                    operation,
                    program: v2::ProgramId::new("probe").unwrap(),
                },
                ticket: ResponseTicket {
                    reply_to: 4,
                    permit: permit1,
                },
                cancel: CancelToken { key, flag },
                start,
            })
            .ok()
            .unwrap();
        assert!(matches!(
            event(&ports),
            WorkerEvent::CacheUpdate(CachePatch::Phase {
                phase: LivePhase::Accepted,
                ..
            })
        ));
        assert!(
            !nonce.with_extension("started").exists(),
            "Accepted durability alone grants no spawn authority"
        );
        assert!(matches!(event(&ports),WorkerEvent::ResponseReady{start_key:Some(k),..} if k==key));
        // Worker-only admission model: integrated dispatcher proves actual FIFO insertion.
        grant
            .grant(AcceptedAdmission {
                key,
                reply_to: 4,
                outbound_sequence: 3,
            })
            .unwrap();
        #[cfg(feature = "test-support")]
        assert!(control.snapshot().accepted_committed);
        let mut raw_emitted = 0usize;
        let mut returned = 0u32;
        let mut stderr_returned = 0u64;
        let mut acknowledged_stage = 0;
        let mut original_storage = [None; 4];
        let mut recycled = 0;
        let overall_deadline = Instant::now() + Duration::from_secs(10);
        let boundary = loop {
            if drain && let Ok(chunk) = ports.data.try_recv() {
                raw_emitted += usize::from(chunk.len);
                if chunk.channel == v2::Channel::Stderr {
                    stderr_returned += u64::from(chunk.len);
                }
                let original = &mut original_storage[chunk.permit.id as usize];
                let address = chunk.permit.storage.as_ptr();
                if let Some(previous) = original.replace(address) {
                    assert_eq!(previous, address, "same original storage returns");
                    recycled += 1;
                }
                assert_eq!(chunk.ordinal, returned + 1);
                returned = chunk.ordinal;
                ports
                    .returned
                    .try_send(DataReturn::Reusable {
                        key: chunk.key,
                        ordinal: chunk.ordinal,
                        disposition: ReturnDisposition::TransportAcknowledged,
                        permit: chunk.permit,
                    })
                    .ok()
                    .unwrap();
            }
            if drain && acknowledged_stage < 6 {
                let stage = acknowledged_stage + 1;
                let progress = format!("{nonce_value}:{stage}");
                if stderr_returned >= stage * 1024
                    && std::fs::read(nonce.with_extension("progress"))
                        .ok()
                        .as_deref()
                        == Some(progress.as_bytes())
                {
                    // The exact chunk permits have entered the original return
                    // mailbox before authorizing the next child publication.
                    std::fs::write(nonce.with_extension("ack"), progress.as_bytes()).unwrap();
                    acknowledged_stage = stage;
                }
            }
            let next = if drain {
                match ports.events.try_recv() {
                    Ok(e) => e,
                    Err(TryRecvError::Empty) => {
                        assert!(Instant::now() < overall_deadline);
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(_) => panic!("worker event disconnected"),
                }
            } else {
                event(&ports)
            };
            match next {
                WorkerEvent::CacheUpdate(CachePatch::Terminal {
                    view:
                        v2::OperationView::Finished {
                            outcome:
                                v2::Outcome::RunCompleted {
                                    reason: v2::RunReason::Natural,
                                    status,
                                    ..
                                },
                        },
                    boundary,
                    ..
                }) => {
                    assert!(matches!(
                        status,
                        v2::NativeStatus::PosixExit { code: 0 }
                            | v2::NativeStatus::WindowsExit { code: 0 }
                    ));
                    break boundary;
                }
                WorkerEvent::CacheUpdate(CachePatch::Phase { .. }) => {}
                _ => panic!("truthful durable completion"),
            }
        };
        #[cfg(feature = "test-support")]
        {
            let snapshot = control.snapshot();
            assert!(
                snapshot.before_spawn_reached && snapshot.spawn_attempted && snapshot.spawn_owned
            );
            assert!(snapshot.terminal_committed);
            assert!(!snapshot.panic_injected);
            if !drain {
                assert!(snapshot.capture_saturated);
            }
        }
        assert_eq!(std::fs::read(&nonce).unwrap(), nonce_value.as_bytes());
        assert!(boundary.truncated);
        assert!(boundary.stdout_bytes + boundary.stderr_bytes >= 65536);
        if drain {
            while returned < boundary.final_ordinal {
                assert!(Instant::now() < overall_deadline);
                if let Ok(chunk) = ports.data.try_recv() {
                    raw_emitted += usize::from(chunk.len);
                    returned = chunk.ordinal;
                    ports
                        .returned
                        .try_send(DataReturn::Reusable {
                            key: chunk.key,
                            ordinal: chunk.ordinal,
                            disposition: ReturnDisposition::TransportAcknowledged,
                            permit: chunk.permit,
                        })
                        .ok()
                        .unwrap();
                } else {
                    thread::sleep(Duration::from_millis(1));
                }
            }
            assert!(
                boundary.final_ordinal > 4,
                "original four tokens actually recycled"
            );
            assert_eq!(acknowledged_stage, 6);
            assert!(
                recycled > 0,
                "publication consumed returned original storage"
            );
            assert!(raw_emitted <= 16384);
            assert!(raw_emitted > 0);
        } else {
            assert!(boundary.final_ordinal <= 4);
        }
        // Undrained case has consumed no Output or returned tokens; neither case
        // sends ProducerDrained before checking the physical Finished barrier.
        let physical = std::fs::read(lp.join("ledger.bin"))
            .or_else(|_| std::fs::read(lp.join("ledger")))
            .unwrap();
        let mut decoder = v2::ledger::Decoder::new();
        let mut offset = 0;
        let mut finished = false;
        while offset < physical.len() {
            let p = decoder.feed(&physical[offset..]).unwrap();
            offset += p.consumed;
            if let Some(r) = p.record {
                finished |= matches!(r.entry(), Entry::OperationFinished { .. });
            }
        }
        decoder.finish().unwrap();
        assert!(
            finished,
            "physical terminal record before optional output acknowledgment"
        );
        ports.stop.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !join.is_finished() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert!(matches!(
            join.join().unwrap(),
            WorkerCompletion::Clean {
                native_settled: true,
                ..
            }
        ));
    }
}
#[cfg(test)]
mod private_barrier_tests {
    use super::*;
    use theseus_host::process::{ChildEnvironment, NativeCommand};
    use theseus_native_helper_store::{BootstrapRequest, StoreBinding, bootstrap};
    fn fixture() -> (
        tempfile::TempDir,
        Worker,
        DispatcherPorts,
        [ControlPermit; 2],
        Invocation,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let lp = tmp.path().join("l");
        let wp = tmp.path().join("w");
        let l = HostDir::create_private(&lp).unwrap();
        let w = HostDir::create_private(&wp).unwrap();
        let binding = v2::BindingId::from_bytes([1; 16]);
        let manifest_digest = v2::Digest::from_bytes([2; 32]);
        let receipt = bootstrap(
            &l,
            &w,
            BootstrapRequest {
                binding,
                manifest_digest,
                os: crate::invocation::host_os(),
            },
        )
        .unwrap();
        let mut command = NativeCommand::new(std::env::current_exe().unwrap());
        command.environment = ChildEnvironment::Replace(vec![]);
        let invocation = Invocation::new(
            lp,
            wp,
            StoreBinding {
                binding,
                manifest_digest,
                workspace_marker: receipt.workspace_marker(),
                os: crate::invocation::host_os(),
            },
            vec![crate::CatalogEntry {
                id: v2::ProgramId::new("probe").unwrap(),
                command,
            }],
        )
        .unwrap();
        let (ports, wp, controls, tokens) = channels();
        (tmp, Worker::new(wp, tokens), ports, controls, invocation)
    }
    fn dispatch_input(d: &mut crate::dispatcher::Dispatcher, sequence: u64, message: v2::Message) {
        let bytes = v2::encode(
            &v2::Envelope::new(v2::SessionId::from_bytes([7; 16]), sequence, message).unwrap(),
        )
        .unwrap();
        d.input_buffer().unwrap()[..bytes.len()].copy_from_slice(&bytes);
        d.read_bytes(bytes.len());
        d.input_quantum().unwrap();
    }
    fn dispatch_flush(d: &mut crate::dispatcher::Dispatcher) {
        while !d.frames.is_empty() || d.current.is_some() {
            if d.current.is_none() {
                d.current = d.frames.pop_front();
            }
            let f = d.current.as_mut().unwrap();
            let n = f.write_slice().len();
            if f.acknowledged(n).unwrap() {
                d.frame_done().unwrap();
            }
        }
    }
    fn request(d: &mut crate::dispatcher::Dispatcher, sequence: u64, request: v2::Request) {
        dispatch_input(d, sequence, v2::Message::Request { request });
    }
    #[derive(Debug)]
    struct Original(u32);
    impl std::fmt::Display for Original {
        fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            panic!("must retain typed source")
        }
    }
    impl std::error::Error for Original {}
    #[test]
    fn completion_retains_original_spawn_beside_later_primary() {
        for active in [false, true] {
            for store_error in [false, true] {
                let (_tmp, mut worker, ports, [p0, p1], inv) = fixture();
                let binding = inv.expected.binding;
                let manifest_digest = inv.expected.manifest_digest;
                worker.initialize(inv).unwrap_or_else(|_| panic!("init"));
                ports.events.try_recv().ok().unwrap();
                worker
                    .acquire(
                        v2::Request::Acquire {
                            intent: v2::IntentId::from_bytes([3; 16]),
                            binding,
                            manifest_digest,
                        },
                        ResponseTicket {
                            reply_to: 1,
                            permit: p0,
                        },
                    )
                    .unwrap_or_else(|_| panic!("acquire"));
                ports.events.try_recv().ok().unwrap();
                worker.response.take();
                let lease = worker.lease.as_ref().unwrap().1;
                let key = RunKey {
                    lease,
                    operation: v2::OperationId::from_bytes([4; 16]),
                    generation: 1,
                };
                let (_, start) = start_pair(key, 2);
                worker
                    .begin(
                        v2::Request::Run {
                            lease,
                            operation: key.operation,
                            program: v2::ProgramId::new("probe").unwrap(),
                        },
                        ResponseTicket {
                            reply_to: 2,
                            permit: p1,
                        },
                        CancelToken {
                            key,
                            flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                        },
                        start,
                    )
                    .unwrap_or_else(|_| panic!("begin"));
                let error = io::Error::other(Original(71));
                let original =
                    error.get_ref().unwrap().downcast_ref::<Original>().unwrap() as *const Original;
                let failure = theseus_host::process::SpawnFailure {
                    launch: theseus_host::process::LaunchPhase::MayRun,
                    cleanup: HostCleanup::Complete,
                    error,
                    cleanup_errors: vec![io::Error::other(Original(72))],
                };
                if active {
                    worker.active.as_mut().unwrap().spawn_failure = Some(failure);
                } else {
                    worker.last_spawn = Some(failure);
                }
                let primary = if store_error {
                    WorkerPrimary::Store(io::Error::other(Original(73)).into())
                } else {
                    WorkerPrimary::Io(io::Error::other(Original(74)))
                };
                worker.fail(primary);
                let join = thread::spawn(move || {
                    worker.settle_native();
                    worker.completion()
                });
                let deadline = Instant::now() + Duration::from_secs(3);
                while !join.is_finished() {
                    assert!(Instant::now() < deadline);
                    thread::yield_now();
                }
                let WorkerCompletion::Failed(f) = join.join().unwrap() else {
                    panic!("failure")
                };
                assert_eq!(f.classification(), if store_error { "store" } else { "io" });
                let retained = f
                    .spawn_failure()
                    .expect("original launch failure survives completion");
                assert!(matches!(
                    retained.launch,
                    theseus_host::process::LaunchPhase::MayRun
                ));
                assert!(matches!(retained.cleanup, HostCleanup::Complete));
                assert_eq!(
                    retained
                        .error
                        .get_ref()
                        .unwrap()
                        .downcast_ref::<Original>()
                        .unwrap() as *const Original,
                    original
                );
                assert_eq!(
                    retained
                        .error
                        .get_ref()
                        .unwrap()
                        .downcast_ref::<Original>()
                        .unwrap()
                        .0,
                    71
                );
                assert_eq!(
                    retained.cleanup_errors[0]
                        .get_ref()
                        .unwrap()
                        .downcast_ref::<Original>()
                        .unwrap()
                        .0,
                    72
                );
                assert_eq!(
                    format!("{f:?} {f}"),
                    "helper worker failure helper worker failure"
                );
            }
        }
    }
    #[test]
    fn simultaneous_cleanup_errors_survive_with_free_slots() {
        let (_tmp, mut worker, _ports, _controls, _inv) = fixture();
        worker.fail(WorkerPrimary::Io(io::Error::other(Original(70))));
        worker.cleanup_results([
            Some(io::Error::other(Original(71))),
            Some(io::Error::other(Original(72))),
            None,
        ]);
        worker.settle_native();
        let WorkerCompletion::Failed(f) = worker.completion() else {
            panic!("failure")
        };
        let values: Vec<_> = f
            .cleanup_errors
            .iter()
            .flatten()
            .map(|e| e.get_ref().unwrap().downcast_ref::<Original>().unwrap().0)
            .collect();
        assert_eq!(values, vec![71, 72]);
    }
    fn integrated_accepted() -> (
        tempfile::TempDir,
        Worker,
        crate::dispatcher::Dispatcher,
        v2::LeaseId,
        v2::OperationId,
    ) {
        let (tmp, mut worker, ports, controls, inv) = fixture();
        let binding = inv.expected.binding;
        let manifest_digest = inv.expected.manifest_digest;
        worker.initialize(inv).unwrap_or_else(|_| panic!("init"));
        let mut d = crate::dispatcher::Dispatcher::new(ports, controls);
        d.event_quantum().unwrap();
        dispatch_input(
            &mut d,
            0,
            v2::Message::Hello {
                role: v2::Role::Controller,
            },
        );
        dispatch_flush(&mut d);
        request(&mut d, 1, v2::Request::Describe);
        d.response_quantum().unwrap();
        dispatch_flush(&mut d);
        request(
            &mut d,
            2,
            v2::Request::Acquire {
                intent: v2::IntentId::from_bytes([3; 16]),
                binding,
                manifest_digest,
            },
        );
        worker
            .command(worker.ports.commands.try_recv().ok().unwrap())
            .unwrap_or_else(|_| panic!("acquire"));
        d.event_quantum().unwrap();
        let (t, r, k) = worker.response.take().unwrap();
        worker.respond(t, r, k);
        d.event_quantum().unwrap();
        d.response_quantum().unwrap();
        dispatch_flush(&mut d);
        let lease = worker.lease.as_ref().unwrap().1;
        let operation = v2::OperationId::from_bytes([4; 16]);
        request(
            &mut d,
            3,
            v2::Request::Run {
                lease,
                operation,
                program: v2::ProgramId::new("probe").unwrap(),
            },
        );
        worker
            .command(worker.ports.commands.try_recv().ok().unwrap())
            .unwrap_or_else(|_| panic!("run"));
        d.event_quantum().unwrap(); // Accepted cache, original ResponseReady deliberately retained.
        (tmp, worker, d, lease, operation)
    }
    #[test]
    fn cancelled_terminal_waits_for_original_accepted_fifo() {
        let (_tmp, mut worker, mut d, lease, operation) = integrated_accepted();
        #[cfg(feature = "test-support")]
        let control =
            crate::test_support::FixtureControl::new(crate::test_support::FixtureMode::Observe);
        #[cfg(feature = "test-support")]
        {
            d.fixture = Some(control.clone());
            worker.invocation.as_mut().unwrap().fixture = Some(control.clone());
        }
        request(
            &mut d,
            4,
            v2::Request::Cancel {
                lease: v2::LeaseId::from_bytes([9; 16]),
                operation,
            },
        );
        assert!(
            !worker
                .active
                .as_ref()
                .unwrap()
                .cancel
                .flag
                .load(Ordering::Acquire)
        );
        #[cfg(feature = "test-support")]
        assert!(!control.snapshot().cancel_requested);
        d.response_quantum().unwrap();
        dispatch_flush(&mut d);
        request(&mut d, 5, v2::Request::Cancel { lease, operation });
        #[cfg(feature = "test-support")]
        assert!(control.snapshot().cancel_requested);

        d.response_quantum().unwrap();
        dispatch_flush(&mut d);
        worker
            .advance()
            .unwrap_or_else(|_| panic!("cancel physical finish"));
        assert!(worker.active.as_ref().unwrap().committed);
        assert!(!worker.active.as_ref().unwrap().attempted);
        #[cfg(feature = "test-support")]
        {
            let snapshot = control.snapshot();
            assert!(snapshot.terminal_committed);
            assert!(!snapshot.spawn_attempted && !snapshot.spawn_owned);
            assert!(snapshot.control_write_acks > 0);
        }

        assert!(matches!(
            worker
                .store
                .as_ref()
                .unwrap()
                .records()
                .last()
                .unwrap()
                .entry(),
            Entry::OperationFinished {
                outcome: v2::Outcome::NotStarted { .. },
                ..
            }
        ));
        d.event_quantum().unwrap();
        d.event_quantum().unwrap(); // Terminal before original response.
        request(&mut d, 6, v2::Request::QueryOperation { lease, operation });
        assert!(
            !d.response_quantum().unwrap(),
            "terminal must await original Accepted admission"
        );
        let (t, r, k) = worker.response.take().unwrap();
        worker.respond(t, r, k);
        d.event_quantum().unwrap();
        assert!(d.response_quantum().unwrap()); // Accepted admitted first despite later arrival.
        assert_eq!(d.frames.len(), 1);
        assert!(d.response_quantum().unwrap()); // terminal guard transition now valid.
        dispatch_flush(&mut d);
        d.return_quantum().unwrap();
        request(
            &mut d,
            7,
            v2::Request::Run {
                lease,
                operation: v2::OperationId::from_bytes([5; 16]),
                program: v2::ProgramId::new("probe").unwrap(),
            },
        );
        // Delay the old producer's next advance until the dispatcher has installed
        // the new generation, before the worker consumes its drain acknowledgment.
        worker
            .advance()
            .unwrap_or_else(|_| panic!("terminal advance"));
        assert!(
            !d.event_quantum().unwrap(),
            "Terminal is the sole final boundary event"
        );
        worker.returns();
        assert!(worker.active.is_none());
        worker
            .command(worker.ports.commands.try_recv().ok().unwrap())
            .unwrap_or_else(|_| panic!("next run"));
        assert!(d.event_quantum().unwrap());
        assert!(!worker.active.as_ref().unwrap().attempted);
    }
    #[test]
    fn delayed_final_advance_cannot_cross_next_generation_after_data_ack() {
        // Deterministic native-facts model; the data permit, physical store,
        // dispatcher guard/FIFO, and return channels are the actual components.
        let (_tmp, mut worker, mut d, lease, operation) = integrated_accepted();
        let (t, r, k) = worker.response.take().unwrap();
        worker.respond(t, r, k);
        d.event_quantum().unwrap();
        d.response_quantum().unwrap();
        dispatch_flush(&mut d);
        let a = worker.active.as_mut().unwrap();
        let key = a.key;
        assert!(matches!(a.start.poll(), StartState::Granted));
        a.attempted = true;
        a.out.observe(v2::Channel::Stdout, 1, true).unwrap();
        a.out.ordinal = 1;
        let mut permit = worker.tokens.pop().unwrap();
        permit.storage[0] = b'x';
        a.outstanding[permit.id as usize] = Some(1);
        worker
            .ports
            .data
            .try_send(DataChunk {
                key,
                ordinal: 1,
                channel: v2::Channel::Stdout,
                offset: 0,
                len: 1,
                permit,
            })
            .ok()
            .unwrap();
        let status = if cfg!(windows) {
            v2::NativeStatus::WindowsExit { code: 0 }
        } else {
            v2::NativeStatus::PosixExit { code: 0 }
        };
        worker
            .finish(v2::Outcome::RunCompleted {
                reason: v2::RunReason::Natural,
                status,
                cleanup: cleanup_fact(),
                stdout_bytes: 1,
                stderr_bytes: 0,
                truncated: false,
            })
            .unwrap_or_else(|_| panic!("physical finish"));
        assert!(
            worker.active.as_ref().unwrap().committed,
            "terminal commits before data acknowledgment"
        );
        d.event_quantum().unwrap();
        d.event_quantum().unwrap();
        request(&mut d, 4, v2::Request::QueryOperation { lease, operation });
        assert!(!d.response_quantum().unwrap());
        d.data_quantum().unwrap();
        dispatch_flush(&mut d);
        assert!(d.response_quantum().unwrap());
        dispatch_flush(&mut d);
        assert!(d.return_quantum().unwrap()); // exact original data token
        assert!(d.return_quantum().unwrap()); // ProducerDrained, old barrier retired
        request(
            &mut d,
            5,
            v2::Request::Run {
                lease,
                operation: v2::OperationId::from_bytes([5; 16]),
                program: v2::ProgramId::new("probe").unwrap(),
            },
        );
        // This is precisely the late-old-event window: new dispatcher key,
        // old worker context, and a not-yet-consumed drain acknowledgment.
        worker
            .advance()
            .unwrap_or_else(|_| panic!("late old advance"));
        assert!(
            !d.event_quantum().unwrap(),
            "no duplicate final boundary crosses retirement"
        );
        worker.returns();
        assert!(worker.active.is_none());
        worker
            .command(worker.ports.commands.try_recv().ok().unwrap())
            .unwrap_or_else(|_| panic!("next run"));
        assert!(d.event_quantum().unwrap());
        assert_eq!(
            worker.active.as_ref().unwrap().key.generation,
            key.generation + 1
        );
        assert!(!worker.active.as_ref().unwrap().attempted);
    }
    #[test]
    fn closed_history_replays_before_effect_admission() {
        for quarantine in [false, true] {
            for choice in 0..3 {
                let (_tmp, mut worker, ports, [p0, p1], inv) = fixture();
                let binding = inv.expected.binding;
                let manifest_digest = inv.expected.manifest_digest;
                worker.initialize(inv).unwrap_or_else(|_| panic!("init"));
                ports.events.try_recv().ok().unwrap();
                worker
                    .acquire(
                        v2::Request::Acquire {
                            intent: v2::IntentId::from_bytes([3; 16]),
                            binding,
                            manifest_digest,
                        },
                        ResponseTicket {
                            reply_to: 1,
                            permit: p0,
                        },
                    )
                    .unwrap_or_else(|_| panic!("acquire"));
                ports.events.try_recv().ok().unwrap();
                worker.response.take();
                let lease = worker.lease.as_ref().unwrap().1;
                let operation = v2::OperationId::from_bytes([4; 16]);
                let mut req = v2::Request::Run {
                    lease,
                    operation,
                    program: v2::ProgramId::new("probe").unwrap(),
                };
                worker
                    .append(Entry::OperationAccepted {
                        lease,
                        operation,
                        request_digest: v2::request_digest(&req).unwrap(),
                        operation_kind: v2::OperationKind::Run,
                    })
                    .unwrap_or_else(|_| panic!("accept"));
                if !quarantine {
                    worker
                        .append(Entry::OperationFinished {
                            lease,
                            operation,
                            outcome: v2::Outcome::NotStarted {
                                reason: v2::NotStartedReason::CancelRequested,
                            },
                        })
                        .unwrap_or_else(|_| panic!("finish"));
                    worker
                        .append(Entry::LeaseClosed {
                            lease,
                            disposition: Disposition::AwaitingExternalReset,
                        })
                        .unwrap_or_else(|_| panic!("close"));
                }
                let inv = worker.invocation.take().unwrap();
                worker.store.take();
                worker.lease = None;
                worker.initialize(inv).unwrap_or_else(|_| panic!("reopen"));
                ports.events.try_recv().ok().unwrap();
                let expected = worker
                    .store
                    .as_ref()
                    .unwrap()
                    .restart_view()
                    .query_operation(lease, operation)
                    .unwrap();
                if choice == 1 {
                    req = v2::Request::Run {
                        lease,
                        operation,
                        program: v2::ProgramId::new("different").unwrap(),
                    };
                }
                if choice == 2 {
                    req = v2::Request::Run {
                        lease,
                        operation: v2::OperationId::from_bytes([5; 16]),
                        program: v2::ProgramId::new("probe").unwrap(),
                    };
                }
                let op = if choice == 2 {
                    v2::OperationId::from_bytes([5; 16])
                } else {
                    operation
                };
                let key = RunKey {
                    lease,
                    operation: op,
                    generation: 1,
                };
                let (_, start) = start_pair(key, 2);
                let before = worker.store.as_ref().unwrap().records().len();
                worker
                    .begin(
                        req,
                        ResponseTicket {
                            reply_to: 2,
                            permit: p1,
                        },
                        CancelToken {
                            key,
                            flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                        },
                        start,
                    )
                    .unwrap_or_else(|_| panic!("replay"));
                let actual = if let Some((_, reply, _)) = worker.response.take() {
                    reply
                } else {
                    let WorkerEvent::ResponseReady { reply, .. } =
                        ports.events.try_recv().ok().unwrap()
                    else {
                        panic!("response")
                    };
                    reply
                };
                let wanted = match choice {
                    0 => v2::Reply::Operation {
                        lease,
                        operation,
                        state: expected,
                    },
                    1 => v2::Reply::Refused {
                        code: v2::RefusalCode::ReplayConflict,
                    },
                    _ => v2::Reply::Refused {
                        code: if quarantine {
                            v2::RefusalCode::LeaseQuarantined
                        } else {
                            v2::RefusalCode::AwaitingExternalReset
                        },
                    },
                };
                assert_eq!(actual, wanted);
                assert_eq!(before, worker.store.as_ref().unwrap().records().len());
                assert!(
                    worker
                        .active
                        .as_ref()
                        .is_none_or(|a| !a.attempted && a.child.is_none())
                );
            }
        }
    }
    #[test]
    fn cancel_and_deadline_before_attempt_commit_through_saturated_events() {
        // Private grant/clock/event-saturation model; appends are the actual store barrier.
        for deadline in [false, true] {
            let (_tmp, mut worker, ports, [p0, p1], inv) = fixture();
            let binding = inv.expected.binding;
            let manifest_digest = inv.expected.manifest_digest;
            worker
                .initialize(inv)
                .unwrap_or_else(|_| panic!("fixture init"));
            let intent = v2::IntentId::from_bytes([3; 16]);
            worker
                .acquire(
                    v2::Request::Acquire {
                        intent,
                        binding,
                        manifest_digest,
                    },
                    ResponseTicket {
                        reply_to: 1,
                        permit: p0,
                    },
                )
                .unwrap_or_else(|_| panic!("fixture acquire"));
            let lease = worker.lease.as_ref().unwrap().1;
            let operation = v2::OperationId::from_bytes([4; 16]);
            let key = RunKey {
                lease,
                operation,
                generation: 1,
            };
            let (_grant, start) = start_pair(key, 2);
            // Acquisition reply is safely abandoned before any native submission in this model.
            let _acquire_response = worker.response.take().unwrap();
            worker
                .begin(
                    v2::Request::Run {
                        lease,
                        operation,
                        program: v2::ProgramId::new("probe").unwrap(),
                    },
                    ResponseTicket {
                        reply_to: 2,
                        permit: p1,
                    },
                    CancelToken {
                        key,
                        flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(!deadline)),
                    },
                    start,
                )
                .unwrap_or_else(|_| panic!("fixture accept"));
            assert!(worker.pending.is_some());
            let accepted = worker.active.as_ref().unwrap().accepted_at;
            let expires = worker.active.as_ref().unwrap().deadline;
            assert_eq!(expires.duration_since(accepted), Duration::from_secs(30));
            worker
                .advance_at(if deadline { expires } else { accepted })
                .unwrap_or_else(|_| panic!("terminal persistence"));
            let active = worker.active.as_ref().unwrap();
            assert!(!active.attempted);
            assert!(active.committed);
            assert!(active.child.is_none());
            assert_eq!(active.out.ordinal, 0);
            assert!(
                matches!(worker.store.as_ref().unwrap().records().last().unwrap().entry(),Entry::OperationFinished{outcome:v2::Outcome::NotStarted{reason},..} if *reason==if deadline{v2::NotStartedReason::Deadline}else{v2::NotStartedReason::CancelRequested})
            );
            assert!(ports.events.try_recv().is_ok());
            assert!(ports.events.try_recv().is_ok());
        }
    }
    #[test]
    fn full_ledger_and_marker_validation_precedes_description() {
        for corruption in [0, 1, 2] {
            let (_tmp, mut worker, ports, _controls, mut invocation) = fixture();
            match corruption {
                0 => {
                    let mut file = std::fs::OpenOptions::new()
                        .append(true)
                        .open(invocation.ledger_root.join("ledger.bin"))
                        .unwrap();
                    use std::io::Write;
                    file.write_all(&[0]).unwrap();
                }
                1 => {
                    std::fs::write(
                        invocation.workspace_root.join(".theseus-workspace-marker"),
                        b"wrong",
                    )
                    .unwrap();
                }
                _ => invocation.expected.manifest_digest = v2::Digest::from_bytes([9; 32]),
            }
            assert!(worker.initialize(invocation).is_err());
            assert!(ports.events.try_recv().is_err());
            assert!(worker.store.is_none());
        }
    }
}
