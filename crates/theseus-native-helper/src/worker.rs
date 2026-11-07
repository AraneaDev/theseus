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
        let store = match inv.profile {
            v2::HelperProfile::Integration { .. } => {
                LedgerStore::open_profile(&ledger, &workspace, expected, inv.profile)
            }
            v2::HelperProfile::Fixture => LedgerStore::open(&ledger, &workspace, expected),
            v2::HelperProfile::Native { provider_epoch } => {
                LedgerStore::open_native(&ledger, &workspace, expected, provider_epoch)
            }
        }
        .map_err(WorkerPrimary::Store)?;
        self.native_authority_failure = store.restart_view().native_authority_failure();
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
        let description = match (inv.profile, description) {
            (
                v2::HelperProfile::Native { provider_epoch }
                | v2::HelperProfile::Integration { provider_epoch, .. },
                v2::Reply::Description {
                    binding,
                    manifest_digest,
                    workspace_marker,
                    os,
                    helper_principal,
                    programs,
                },
            ) => v2::Reply::NativeDescription {
                description: Box::new(v2::NativeDescription {
                    binding,
                    manifest_digest,
                    workspace_marker,
                    os,
                    helper_principal,
                    programs,
                    capabilities: v2::NativeCapabilities,
                    provider_epoch,
                    artifact_commitment: inv.artifact_commitment.ok_or_else(|| {
                        WorkerPrimary::Io(io::Error::other(
                            "native catalog artifact commitment missing",
                        ))
                    })?,
                    build: v2::Provenance::Unavailable {
                        reason: "build provenance unavailable".into(),
                    },
                    os_build: v2::Provenance::Unavailable {
                        reason: "OS build provenance unavailable".into(),
                    },
                }),
            },
            (_, description) => description,
        };
        let description = if let v2::HelperProfile::Integration {
            core_manifest_digest,
            integration_manifest_digest,
            ..
        } = inv.profile
        {
            let config = inv.integration.as_ref().ok_or_else(|| {
                WorkerPrimary::Io(io::Error::other("integration context missing"))
            })?;
            if config.manifest.desktop.is_some() {
                let declared = v2::broker::declared_desktop(store.records())
                    .map_err(|e| WorkerPrimary::Io(io::Error::other(e)))?;
                if declared
                    != config
                        .manifest
                        .desktop
                        .as_ref()
                        .zip(config.context.desktop_broker_header.as_ref())
                {
                    return Err(WorkerPrimary::Io(io::Error::other(
                        "desktop ceiling differs from original helper declaration",
                    )));
                }
            }
            let context_digest = config
                .context
                .digest()
                .map_err(|e| WorkerPrimary::Io(io::Error::other(e)))?;
            match store.records()[0].entry() {
                Entry::IntegrationHeader {
                    context_digest: stored,
                    ownership_id,
                    artifact_commitment,
                    ..
                } if *stored == context_digest
                    && ownership_id == &config.context.ownership_id
                    && Some(*artifact_commitment) == inv.artifact_commitment => {}
                _ => {
                    return Err(WorkerPrimary::Io(io::Error::other(
                        "integration runtime context differs from original header",
                    )));
                }
            }
            let v2::Reply::NativeDescription {
                description: native,
            } = description
            else {
                return Err(WorkerPrimary::Io(io::Error::other(
                    "integration native description missing",
                )));
            };
            if config.manifest.desktop.is_some() {
                let mut desktop =
                    crate::integration::desktop::DesktopRuntime::new(config, &inv.ledger_root)
                        .map_err(WorkerPrimary::Io)?;
                desktop
                    .retain_provider_qualification(&store, &config.context)
                    .map_err(WorkerPrimary::Io)?;
                self.desktop = Some(desktop);
            }
            if config.manifest.service_authority.is_some() {
                self.service = Some(crate::integration::service::ServiceRuntime::new(
                    config.as_ref().clone(),
                ));
            }
            v2::Reply::IntegrationDescription {
                description: Box::new(v2::IntegrationDescription {
                    manifest: Box::new(config.manifest.clone()),
                    namespace_receipt_digest: config
                        .context
                        .authority_receipt_digest()
                        .map_err(|e| WorkerPrimary::Io(io::Error::other(e)))?,
                    native: *native,
                    core_manifest_digest,
                    integration_manifest_digest,
                    context_digest,
                    ownership_id: config.context.ownership_id.clone(),
                    capabilities: v2::IntegrationCapabilities::for_manifest(&config.manifest)
                        .map_err(|e| WorkerPrimary::Io(io::Error::other(e)))?,
                }),
            }
        } else {
            description
        };
        self.store = Some(store);
        self.workspace = Some(workspace);
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
                    inspect_failure: None,
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
        if self.active.is_some() || self.file.is_some() || self.service_action.is_some() {
            self.refuse(ticket, v2::RefusalCode::Busy);
            return Ok(());
        }
        if self.service.as_ref().is_some_and(|s| s.uncertain) {
            self.refuse(ticket, v2::RefusalCode::OwnershipPending);
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
        if let Some(reason) = self.check_native_mapping(lease)? {
            self.respond(
                ticket,
                v2::Reply::PrefixAuthorityRevoked { lease, reason },
                None,
            );
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
                    request_digest: v2::request_digest(&request)
                        .map_err(|e| WorkerPrimary::Io(io::Error::other(e)))?,
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
            request_digest: digest,
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
    fn native_mapping(&mut self) -> Option<v2::PrefixAuthorityFailure> {
        if let Some(reason) = self.native_authority_failure {
            return Some(reason);
        }
        let inv = self.invocation.as_ref()?;
        if inv.profile == v2::HelperProfile::Fixture {
            return None;
        }
        let result =
            HostDir::open(&inv.workspace_root, RootPolicy::LeafNoFollow).and_then(|current| {
                self.workspace
                    .as_ref()
                    .unwrap()
                    .same_directory_object(&current)
            });
        match result {
            Ok(true) => None,
            Ok(false) => Some(v2::PrefixAuthorityFailure::Replaced),
            Err(error) => {
                // Generic root/identity errors do not prove completion of every
                // subordinate native query. Retain the original source and root
                // through conservative incomplete shutdown, never Clean.
                self.prefix_mapping_error = Some(error);
                self.stopping = true;
                Some(v2::PrefixAuthorityFailure::MappingUnproved)
            }
        }
    }
    fn check_native_mapping(
        &mut self,
        lease: v2::LeaseId,
    ) -> Result<Option<v2::PrefixAuthorityFailure>, WorkerPrimary> {
        let Some(reason) = self.native_mapping() else {
            return Ok(None);
        };
        if self.native_authority_failure.is_none() {
            // Revoke in memory before publication; uncertain persistence cannot grant effects.
            self.native_authority_failure = Some(reason);
            self.append(Entry::NativeAuthorityRevoked { lease, reason })?;
        }
        Ok(Some(reason))
    }
    fn held_refusal(&self, lease: v2::LeaseId) -> Option<v2::RefusalCode> {
        match &self.lease {
            Some((_, actual, _)) if *actual != lease => Some(v2::RefusalCode::WrongLease),
            Some((_, _, v2::LeaseView::Held { .. })) => None,
            Some((_, _, v2::LeaseView::Quarantined { .. })) => {
                Some(v2::RefusalCode::LeaseQuarantined)
            }
            Some(_) => Some(v2::RefusalCode::AwaitingExternalReset),
            None => Some(v2::RefusalCode::WrongLease),
        }
    }
    fn begin_file(
        &mut self,
        request: v2::Request,
        ticket: ResponseTicket,
        cancel: CancelToken,
        start: StartWatch,
    ) -> Result<(), WorkerPrimary> {
        if self.active.is_some() || self.file.is_some() || self.service_action.is_some() {
            self.refuse(ticket, v2::RefusalCode::Busy);
            return Ok(());
        }
        if self.service.as_ref().is_some_and(|s| s.uncertain) {
            self.refuse(ticket, v2::RefusalCode::OwnershipPending);
            return Ok(());
        }
        let v2::Request::CreateFile {
            lease, operation, ..
        } = &request
        else {
            self.refuse(ticket, v2::RefusalCode::InvalidRequest);
            return Ok(());
        };
        let (lease, operation) = (*lease, *operation);
        if !self.lease.as_ref().is_some_and(|(_, l, _)| *l == lease) {
            self.refuse(ticket, v2::RefusalCode::WrongLease);
            return Ok(());
        }
        if let Some(reason) = self.check_native_mapping(lease)? {
            self.respond(
                ticket,
                v2::Reply::PrefixAuthorityRevoked { lease, reason },
                None,
            );
            return Ok(());
        }
        let key = cancel.key;
        if key.lease != lease || key.operation != operation || start.key() != key {
            self.refuse(ticket, v2::RefusalCode::InvalidRequest);
            return Ok(());
        }
        let replay = self
            .store
            .as_ref()
            .unwrap()
            .restart_view()
            .replay_operation(&request);
        let now = Instant::now();
        match replay {
            Ok(v2::ledger::ReplayDecision::Recorded(view)) => {
                self.file = Some(PendingFileRequest {
                    request,
                    ticket: None,
                    owner: Box::new(crate::files::FileOwner::new()),
                    key: Some(key),
                    create: Some(FileCreate {
                        cancel,
                        start,
                        deadline: now,
                        terminal: Some(view.clone()),
                        published: true,
                    }),
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
                    boundary: file_boundary(key),
                }));
                return Ok(());
            }
            Err(code) => {
                self.refuse(ticket, code);
                return Ok(());
            }
            Ok(v2::ledger::ReplayDecision::NotRecorded) => {}
        }
        if let Some(code) = self.held_refusal(lease) {
            self.refuse(ticket, code);
            return Ok(());
        }
        let digest = v2::request_digest(&request)
            .map_err(|_| WorkerPrimary::Io(io::Error::other("invalid file request")))?;
        self.append(Entry::OperationAccepted {
            lease,
            operation,
            request_digest: digest,
            operation_kind: v2::OperationKind::CreateFile,
        })?;
        #[cfg(feature = "test-support")]
        if let Some(control) = self
            .invocation
            .as_ref()
            .and_then(|inv| inv.fixture.as_ref())
        {
            control.mark_accepted_committed();
        }
        let now = Instant::now();
        let deadline = now + Duration::from_secs(30);
        self.file = Some(PendingFileRequest {
            request,
            ticket: None,
            owner: Box::new(crate::files::FileOwner::new()),
            key: Some(key),
            create: Some(FileCreate {
                cancel,
                start,
                deadline,
                terminal: None,
                published: false,
            }),
        });
        self.response = Some((
            ticket,
            v2::Reply::Operation {
                lease,
                operation,
                state: v2::OperationView::Accepted {
                    operation_kind: v2::OperationKind::CreateFile,
                },
            },
            Some(key),
        ));
        self.publish(WorkerEvent::CacheUpdate(CachePatch::Phase {
            key,
            kind: v2::OperationKind::CreateFile,
            phase: LivePhase::Accepted,
            accepted_at: Some(now),
            deadline: Some(deadline),
        }));
        Ok(())
    }
    fn inspect_native_file(
        &mut self,
        request: v2::Request,
        ticket: ResponseTicket,
        lease: v2::LeaseId,
        path: v2::LeafName,
    ) -> Result<(), WorkerPrimary> {
        if !matches!(
            self.invocation.as_ref().map(|i| i.profile),
            Some(v2::HelperProfile::Native { .. } | v2::HelperProfile::Integration { .. })
        ) {
            self.refuse(ticket, v2::RefusalCode::Unsupported);
            return Ok(());
        }
        if let Some(reason) = self.check_native_mapping(lease)? {
            self.respond(
                ticket,
                v2::Reply::PrefixAuthorityRevoked { lease, reason },
                None,
            );
            return Ok(());
        }
        self.file = Some(PendingFileRequest {
            request,
            ticket: Some(ticket),
            owner: Box::new(crate::files::FileOwner::new()),
            key: None,
            create: None,
        });
        let result = {
            let pending = self.file.as_mut().unwrap();
            let stop = &self.ports.stop;
            crate::files::inspect_native(
                &mut pending.owner,
                self.workspace.as_ref().unwrap(),
                &path,
                Instant::now() + Duration::from_secs(30),
                || stop.load(Ordering::Acquire),
            )
        };
        let mapping = self.check_native_mapping(lease)?;
        let reply = if let Some(reason) = mapping {
            v2::Reply::PrefixAuthorityRevoked { lease, reason }
        } else {
            match result {
                Ok(crate::files::NativeObservation::Present { length, sha256 }) => {
                    v2::Reply::NativeFileInfo {
                        lease,
                        path,
                        length,
                        sha256,
                    }
                }
                Ok(crate::files::NativeObservation::Absent) => {
                    v2::Reply::FileAbsent { lease, path }
                }
                Err(_) => v2::Reply::Refused {
                    code: v2::RefusalCode::Unsupported,
                    inspect_failure: None,
                },
            }
        };
        let pending = self.file.as_mut().unwrap();
        let ticket = pending.ticket.take().unwrap();
        let sticky = pending.owner.sticky_open;
        self.respond(ticket, reply, None);
        if sticky {
            self.stopping = true;
        } else {
            self.file.take();
        }
        Ok(())
    }
    fn observe_file(
        &mut self,
        request: v2::Request,
        ticket: ResponseTicket,
    ) -> Result<(), WorkerPrimary> {
        let (lease, path) = match &request {
            v2::Request::ReadFile { lease, path, .. }
            | v2::Request::InspectFile { lease, path }
            | v2::Request::InspectNativeFile { lease, path } => (*lease, path.clone()),
            _ => {
                self.refuse(ticket, v2::RefusalCode::InvalidRequest);
                return Ok(());
            }
        };
        if let Some(code) = self.held_refusal(lease) {
            self.refuse(ticket, code);
            return Ok(());
        }
        if self.active.is_some() || self.file.is_some() || self.service_action.is_some() {
            self.refuse(ticket, v2::RefusalCode::Busy);
            return Ok(());
        }
        if self.service.as_ref().is_some_and(|s| s.uncertain) {
            self.refuse(ticket, v2::RefusalCode::OwnershipPending);
            return Ok(());
        }
        if matches!(request, v2::Request::InspectNativeFile { .. }) {
            return self.inspect_native_file(request, ticket, lease, path);
        }
        if let v2::Request::ReadFile { offset, length, .. } = &request
            && (*length == 0 || *length > 4096 || offset.checked_add(u64::from(*length)).is_none())
        {
            self.refuse(ticket, v2::RefusalCode::InvalidRequest);
            return Ok(());
        }
        self.file = Some(PendingFileRequest {
            request,
            ticket: Some(ticket),
            owner: Box::new(crate::files::FileOwner::new()),
            key: None,
            create: None,
        });
        let result = {
            let pending = self.file.as_mut().unwrap();
            crate::files::observe(&mut pending.owner, self.workspace.as_ref().unwrap(), &path)
        };
        let reply = match result {
            Ok(observed) => match &self.file.as_ref().unwrap().request {
                v2::Request::InspectFile { .. } => v2::Reply::FileInfo {
                    lease,
                    path,
                    length: observed.bytes.as_slice().len() as u16,
                    digest: observed.digest,
                },
                v2::Request::ReadFile { offset, length, .. } => {
                    let all = observed.bytes.as_slice();
                    let begin = if *offset >= all.len() as u64 {
                        all.len()
                    } else {
                        *offset as usize
                    };
                    let end = begin.saturating_add(usize::from(*length)).min(all.len());
                    v2::Reply::FileBytes {
                        lease,
                        path,
                        offset: *offset,
                        bytes: v2::FileBytes::new(&all[begin..end]).expect("bounded observation"),
                        eof: end == all.len(),
                    }
                }
                _ => unreachable!(),
            },
            Err(kind) => v2::Reply::Refused {
                code: if kind == crate::files::FileFailureKind::Changed {
                    v2::RefusalCode::Busy
                } else {
                    v2::RefusalCode::Unsupported
                },
                inspect_failure: None,
            },
        };
        let pending = self.file.as_mut().unwrap();
        let ticket = pending.ticket.take().unwrap();
        let sticky = pending.owner.sticky_open;
        self.respond(ticket, reply, None);
        if sticky {
            self.stopping = true;
        } else {
            // Synchronous observation completed; the request may retire its known
            // handle and source after handing back its reserved response.
            self.file.take();
        }
        Ok(())
    }
    fn advance_file(&mut self) -> Result<(), WorkerPrimary> {
        let Some(pending) = &mut self.file else {
            return Ok(());
        };
        let Some(create) = &mut pending.create else {
            return Ok(());
        };
        let key = pending.key.unwrap();
        if let Some(view) = &create.terminal {
            if !create.published && self.pending.is_none() {
                let view = view.clone();
                create.published = true;
                self.publish(WorkerEvent::CacheUpdate(CachePatch::Terminal {
                    key,
                    view,
                    boundary: file_boundary(key),
                }));
            }
            return Ok(());
        }
        let expired = Instant::now() >= create.deadline;
        let cancelled = self.stopping
            || self.ports.stop.load(Ordering::Acquire)
            || create.cancel.flag.load(Ordering::Acquire)
            || matches!(create.start.poll(), StartState::Aborted);
        if expired || cancelled {
            return self.finish_file(v2::Outcome::NotStarted {
                reason: if expired {
                    v2::NotStartedReason::Deadline
                } else {
                    v2::NotStartedReason::CancelRequested
                },
            });
        }
        if !matches!(create.start.poll(), StartState::Granted) {
            return Ok(());
        }
        #[cfg(feature = "test-support")]
        if let Some(control) = self
            .invocation
            .as_ref()
            .and_then(|inv| inv.fixture.as_ref())
        {
            control.before_file_effect();
            let expired = Instant::now() >= create.deadline;
            let cancelled = self.stopping
                || self.ports.stop.load(Ordering::Acquire)
                || create.cancel.flag.load(Ordering::Acquire)
                || matches!(create.start.poll(), StartState::Aborted);
            if expired || cancelled {
                return self.finish_file(v2::Outcome::NotStarted {
                    reason: if expired {
                        v2::NotStartedReason::Deadline
                    } else {
                        v2::NotStartedReason::CancelRequested
                    },
                });
            }
            pending.owner.fixture = Some(control.clone());
        }
        let v2::Request::CreateFile { path, bytes, .. } = &pending.request else {
            unreachable!()
        };
        let result = crate::files::create(
            &mut pending.owner,
            self.workspace.as_ref().unwrap(),
            path,
            bytes,
        );
        match result {
            Ok(created) => self.finish_file(v2::Outcome::FileCreated {
                length: created.length,
                digest: created.digest,
            }),
            Err(_) => self.quarantine_file(),
        }
    }
    fn finish_file(&mut self, outcome: v2::Outcome) -> Result<(), WorkerPrimary> {
        let key = self.file.as_ref().unwrap().key.unwrap();
        self.append(Entry::OperationFinished {
            lease: key.lease,
            operation: key.operation,
            outcome: outcome.clone(),
        })?;
        self.file
            .as_mut()
            .unwrap()
            .create
            .as_mut()
            .unwrap()
            .terminal = Some(v2::OperationView::Finished { outcome });
        #[cfg(feature = "test-support")]
        if let Some(control) = self
            .invocation
            .as_ref()
            .and_then(|inv| inv.fixture.as_ref())
        {
            control.after_file_finished();
        }
        self.advance_file()
    }
    fn quarantine_file(&mut self) -> Result<(), WorkerPrimary> {
        let key = self.file.as_ref().unwrap().key.unwrap();
        let reason = v2::UnknownReason::FileEffectUncertain;
        if let Err(error) = self.append(Entry::LeaseClosed {
            lease: key.lease,
            disposition: Disposition::Quarantined { reason },
        }) {
            if let WorkerPrimary::Store(error) = error {
                self.file.as_mut().unwrap().owner.ledger_error = Some(error);
            } else {
                self.fail(error);
            }
            self.stopping = true;
            self.transfer_file_failure();
            return Ok(());
        }
        #[cfg(feature = "test-support")]
        if let Some(control) = self
            .invocation
            .as_ref()
            .and_then(|inv| inv.fixture.as_ref())
        {
            control.mark_file_quarantine_committed();
        }
        let intent = self.lease.as_ref().unwrap().0;
        let view = v2::LeaseView::Quarantined { intent, reason };
        self.lease = Some((intent, key.lease, view.clone()));
        self.file
            .as_mut()
            .unwrap()
            .create
            .as_mut()
            .unwrap()
            .terminal = Some(v2::OperationView::Unknown { reason });
        self.publish(WorkerEvent::CacheUpdate(CachePatch::Lease {
            intent,
            lease: key.lease,
            view,
        }));
        if self.file.as_ref().unwrap().owner.sticky_open {
            self.stopping = true;
        }
        Ok(())
    }
    fn transfer_file_failure(&mut self) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        if self.failure.is_none() {
            let primary = file
                .owner
                .primary
                .take()
                .map(WorkerPrimary::File)
                .or_else(|| file.owner.panic.take().map(WorkerPrimary::Panic))
                .or_else(|| file.owner.ledger_error.take().map(WorkerPrimary::Store));
            if let Some(primary) = primary {
                self.failure = Some(OwnedWorkerFailure {
                    primary,
                    retained_file: None,
                    retained_spawn: None,
                    retained_service_errors: vec![],
                    cleanup_errors: std::array::from_fn(|_| None),
                });
            }
        }
    }
    fn retire_file(&mut self) {
        self.transfer_file_failure();
        if let Some(pending) = self.file.take()
            && let Some(failure) = &mut self.failure
        {
            failure.retained_file = Some(pending.owner);
        }
    }
    fn file_panic(&mut self, payload: Box<dyn std::any::Any + Send>) {
        if let Some(pending) = &mut self.file {
            pending.owner.panic = Some(payload);
            if pending.owner.opening {
                pending.owner.sticky_open = true;
            }
            let uncertain = pending.owner.attempted
                && pending
                    .create
                    .as_ref()
                    .is_some_and(|c| c.terminal.is_none());
            if uncertain {
                let _ = self.quarantine_file();
            }
            self.transfer_file_failure();
            self.stopping = true;
        } else {
            self.fail(WorkerPrimary::Panic(payload));
        }
    }
    fn release(&mut self, lease: v2::LeaseId, ticket: ResponseTicket) -> Result<(), WorkerPrimary> {
        if self.desktop.is_some() {
            let desktop_view =
                v2::broker::BrokerLedgerView::replay(self.store.as_ref().unwrap().records())
                    .map_err(|e| WorkerPrimary::Io(io::Error::other(e)))?;
            if desktop_view.binding_original().is_some() && !desktop_view.helper_released() {
                return self.begin_desktop(v2::Request::Release { lease }, ticket);
            }
        }
        if self.active.is_some() || self.file.is_some() || self.service_action.is_some() {
            self.refuse(ticket, v2::RefusalCode::Busy);
            return Ok(());
        }
        if self.service.as_ref().is_some_and(|s| s.uncertain) {
            self.refuse(ticket, v2::RefusalCode::OwnershipPending);
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
        if let Some(service) = &mut self.service
            && let Err(error) = service.cleanup_readiness()
        {
            service.errors.push(error);
            service.uncertain = true;
            self.refuse(ticket, v2::RefusalCode::OwnershipPending);
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
                    if self
                        .service_action
                        .as_ref()
                        .is_some_and(|p| p.key == key && final_ordinal == 0 && p.published)
                    {
                        if let Some(service) = &mut self.service
                            && let Err(e) = service.retire()
                        {
                            service.errors.push(e);
                            service.uncertain = true;
                        }
                        self.service_action = None;
                        continue;
                    }
                    if self.file.as_ref().is_some_and(|p| {
                        p.key == Some(key)
                            && final_ordinal == 0
                            && p.create.as_ref().is_some_and(|c| c.published)
                    }) {
                        self.retire_file();
                        continue;
                    }
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
            if let Some(desktop) = &mut self.desktop {
                let program = self.program.as_ref().ok_or_else(|| {
                    WorkerPrimary::Io(io::Error::other("original Run program missing"))
                })?;
                match desktop.prepare_original_run(
                    self.store.as_mut().unwrap(),
                    a._accepted.as_ref().ok_or_else(|| {
                        WorkerPrimary::Io(io::Error::other(
                            "original Run acceptance capability missing",
                        ))
                    })?,
                    program,
                    a.deadline,
                ) {
                    Ok(true) => {}
                    Ok(false) => return Ok(()),
                    Err(error) => {
                        desktop.fail(error);
                        return self.finish(v2::Outcome::NotStarted {
                            reason: v2::NotStartedReason::CancelRequested,
                        });
                    }
                }
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
            // Only a durably completed original Run may authorize a SUT transition.
            if let (Some(service), Some(program), Some(active)) =
                (&mut self.service, &self.program, &self.active)
            {
                service.note_sut(
                    program,
                    active.key.lease,
                    active.key.operation,
                    active.request_digest,
                    &outcome,
                );
            }
            if let (Some(desktop), Some(program), Some(active), Some(store)) = (
                &mut self.desktop,
                &self.program,
                &self.active,
                &mut self.store,
            ) {
                desktop
                    .note_sut(
                        store,
                        program,
                        active.key.lease,
                        active.key.operation,
                        active.request_digest,
                        &outcome,
                    )
                    .map_err(WorkerPrimary::Io)?;
            }
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
        if let Some(desktop) = &mut self.desktop {
            while !desktop.poll_settle() {
                thread::sleep(Duration::from_millis(1));
            }
        }
        if self.service.as_ref().is_some_and(|s| s.unsettled())
            || self.prefix_mapping_error.is_some()
            || self
                .file
                .as_ref()
                .is_some_and(|f| f.owner.sticky_open || f.owner.opening)
        {
            while !self.flush() {
                thread::sleep(Duration::from_millis(1));
            }
            self.publish(WorkerEvent::CacheUpdate(CachePatch::Stopped {
                store_poisoned: self.poisoned(),
                native_settled: false,
            }));
            while !self.flush() {
                thread::sleep(Duration::from_millis(1));
            }
            // HostDir may retain lower-level pending native authority. Keep the
            // whole worker alive until the existing process shutdown budget fires.
            loop {
                std::hint::black_box(&self.file);
                thread::park();
            }
        }

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
/// Bounded nonblocking reads per stream per worker tick. One read per tick
/// capped child output at one 4 KiB read per sleep; small native pipes (macOS
/// 16 KiB) then throttled the child far below the observed deadline budget.
const READS_PER_TICK: usize = 16;
fn read_stream(
    ports: &WorkerPorts,
    tokens: &mut Vec<DataPermit>,
    out: &mut OutputState,
    outstanding: &mut [Option<u32>; 4],
    reader: &mut PipeReader,
    eof: &mut bool,
    channel: v2::Channel,
) -> io::Result<()> {
    for read in 0..READS_PER_TICK {
        // The first read keeps the original per-tick behavior. Later reads
        // never discard a still-complete transcript for want of a permit:
        // pipe backpressure waits for permits returned before the next tick.
        if read > 0 && tokens.is_empty() && !out.discard_preserves_completeness() {
            break;
        }
        if !read_stream_once(ports, tokens, out, outstanding, reader, eof, channel)? {
            break;
        }
    }
    Ok(())
}
/// Returns whether child bytes were read.
fn read_stream_once(
    ports: &WorkerPorts,
    tokens: &mut Vec<DataPermit>,
    out: &mut OutputState,
    outstanding: &mut [Option<u32>; 4],
    reader: &mut PipeReader,
    eof: &mut bool,
    channel: v2::Channel,
) -> io::Result<bool> {
    if *eof {
        return Ok(false);
    }
    let mut discard = [0; 4096];
    let n = match reader.try_read(&mut discard) {
        Ok(0) => {
            *eof = true;
            return Ok(false);
        }
        Ok(n) => n,
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(false),
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
        return Ok(true);
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
    Ok(true)
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
        // Read through the live worker's own locked store handle: Windows
        // byte-range locks refuse reads through any other handle, and reading
        // after the owner drops would no longer prove Finished precedes drain.
        let (physical_tx, physical_rx) = std::sync::mpsc::sync_channel(1);
        ports
            .commands
            .try_send(WorkerCommand::ReadPhysicalLedger(physical_tx))
            .ok()
            .unwrap();
        let physical = physical_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
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
    fn accepted_file(
        cancelled: bool,
    ) -> (
        tempfile::TempDir,
        Worker,
        DispatcherPorts,
        StartGrant,
        ResponseTicket,
    ) {
        let (tmp, mut worker, ports, [p0, p1], inv) = fixture();
        let request = v2::Request::Acquire {
            intent: v2::IntentId::from_bytes([3; 16]),
            binding: inv.expected.binding,
            manifest_digest: inv.expected.manifest_digest,
        };
        worker
            .initialize(inv)
            .unwrap_or_else(|_| panic!("initialize"));
        ports.events.try_recv().unwrap();
        worker
            .acquire(
                request,
                ResponseTicket {
                    reply_to: 1,
                    permit: p0,
                },
            )
            .unwrap_or_else(|_| panic!("acquire"));
        ports.events.try_recv().unwrap();
        let (spare, _, _) = worker.response.take().unwrap();
        let key = RunKey {
            lease: worker.lease.as_ref().unwrap().1,
            operation: v2::OperationId::from_bytes([4; 16]),
            generation: 1,
        };
        let (grant, start) = start_pair(key, 2);
        worker
            .begin_file(
                v2::Request::CreateFile {
                    lease: key.lease,
                    operation: key.operation,
                    path: v2::LeafName::new("leaf").unwrap(),
                    bytes: v2::FileBytes::new(b"actual").unwrap(),
                },
                ResponseTicket {
                    reply_to: 2,
                    permit: p1,
                },
                CancelToken {
                    key,
                    flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(cancelled)),
                },
                start,
            )
            .unwrap_or_else(|_| panic!("file admission"));
        ports.events.try_recv().unwrap();
        (tmp, worker, ports, grant, spare)
    }
    #[test]
    fn native_prefix_rename_replacement_removal_permanently_blocks_all_catalog_effects() {
        for replace in [false, true] {
            let (tmp, mut worker, ports, [p0, p1], mut inv) = fixture();
            let epoch = v2::Digest::from_bytes([0x44; 32]);
            inv.profile = v2::HelperProfile::Native {
                provider_epoch: epoch,
            };
            // Synthetic artifact authority for this prefix-only worker fixture.
            inv.artifact_commitment = Some(v2::Digest::from_bytes([0x66; 32]));
            let header = Record::new(
                0,
                Entry::NativeHeader {
                    binding: inv.expected.binding,
                    manifest_digest: inv.expected.manifest_digest,
                    workspace_marker: inv.expected.workspace_marker,
                    os: inv.expected.os,
                    capabilities: v2::NativeCapabilities,
                    provider_epoch: epoch,
                },
            )
            .unwrap();
            std::fs::write(
                inv.ledger_root.join("ledger.bin"),
                v2::ledger::encode(&header).unwrap(),
            )
            .unwrap();
            let original = inv.workspace_root.clone();
            let moved = tmp.path().join("moved");
            let acquire = v2::Request::Acquire {
                intent: v2::IntentId::from_bytes([3; 16]),
                binding: inv.expected.binding,
                manifest_digest: inv.expected.manifest_digest,
            };
            worker
                .initialize(inv)
                .unwrap_or_else(|_| panic!("native initialize"));
            ports.events.try_recv().unwrap();
            worker
                .acquire(
                    acquire,
                    ResponseTicket {
                        reply_to: 1,
                        permit: p0,
                    },
                )
                .unwrap_or_else(|_| panic!("native acquire"));
            ports.events.try_recv().unwrap();
            worker.response.take();
            let lease = worker.lease.as_ref().unwrap().1;
            std::fs::rename(&original, &moved).unwrap();
            if replace {
                std::fs::create_dir(&original).unwrap();
                std::fs::write(original.join("reader"), b"replacement").unwrap();
            }
            worker
                .observe_file(
                    v2::Request::InspectNativeFile {
                        lease,
                        path: v2::LeafName::new("reader").unwrap(),
                    },
                    ResponseTicket {
                        reply_to: 2,
                        permit: p1,
                    },
                )
                .unwrap_or_else(|_| panic!("native inspect"));
            let WorkerEvent::ResponseReady {
                ticket,
                reply: v2::Reply::PrefixAuthorityRevoked { reason, .. },
                ..
            } = ports.events.try_recv().unwrap()
            else {
                panic!("must revoke whole native authority")
            };
            assert_eq!(worker.native_authority_failure, Some(reason));
            assert_eq!(worker.prefix_mapping_error.is_some(), !replace);
            assert_eq!(worker.stopping, !replace);
            assert_eq!(
                worker
                    .store
                    .as_ref()
                    .unwrap()
                    .restart_view()
                    .native_authority_failure(),
                Some(reason)
            );
            // Restore the original mapping; revocation must remain sticky.
            if replace {
                std::fs::remove_file(original.join("reader")).unwrap();
                std::fs::remove_dir(&original).unwrap();
            }
            std::fs::rename(&moved, &original).unwrap();
            let mut ticket = ticket;
            for (n, id) in ["check_target", "uninstall"].iter().enumerate() {
                let key = RunKey {
                    lease,
                    operation: v2::OperationId::from_bytes([n as u8 + 8; 16]),
                    generation: n as u64 + 1,
                };
                let (_grant, start) = start_pair(key, 3 + n as u64);
                worker
                    .begin(
                        v2::Request::Run {
                            lease,
                            operation: key.operation,
                            program: v2::ProgramId::new(id).unwrap(),
                        },
                        ticket,
                        CancelToken {
                            key,
                            flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                        },
                        start,
                    )
                    .unwrap_or_else(|_| panic!("revoked run refusal"));
                let WorkerEvent::ResponseReady {
                    ticket: returned,
                    reply: v2::Reply::PrefixAuthorityRevoked { .. },
                    ..
                } = ports.events.try_recv().unwrap()
                else {
                    panic!("zero later catalog Run")
                };
                ticket = returned;
            }
            let key = RunKey {
                lease,
                operation: v2::OperationId::from_bytes([10; 16]),
                generation: 3,
            };
            let (_grant, start) = start_pair(key, 5);
            worker
                .begin_file(
                    v2::Request::CreateFile {
                        lease,
                        operation: key.operation,
                        path: v2::LeafName::new("app").unwrap(),
                        bytes: v2::FileBytes::new(b"forbidden").unwrap(),
                    },
                    ticket,
                    CancelToken {
                        key,
                        flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    },
                    start,
                )
                .unwrap_or_else(|_| panic!("revoked create refusal"));
            assert!(matches!(
                ports.events.try_recv().unwrap(),
                WorkerEvent::ResponseReady {
                    reply: v2::Reply::PrefixAuthorityRevoked { .. },
                    ..
                }
            ));
            assert!(worker.active.is_none() && worker.file.is_none());
            assert!(!original.join("app").exists());
            assert!(
                !worker
                    .store
                    .as_ref()
                    .unwrap()
                    .records()
                    .iter()
                    .any(|r| matches!(r.entry(), Entry::OperationAccepted { .. }))
            );
        }
    }
    #[test]
    fn file_acceptance_grant_actual_finish_and_zero_boundary_retirement() {
        let (_tmp, mut worker, ports, mut grant, ticket) = accepted_file(false);
        let path = worker
            .invocation
            .as_ref()
            .unwrap()
            .workspace_root
            .join("leaf");
        assert!(!path.exists());
        assert!(matches!(
            worker
                .store
                .as_ref()
                .unwrap()
                .records()
                .last()
                .unwrap()
                .entry(),
            Entry::OperationAccepted {
                operation_kind: v2::OperationKind::CreateFile,
                ..
            }
        ));
        worker.advance_file().unwrap_or_else(|_| panic!("waiting"));
        assert!(
            !path.exists(),
            "physical create must wait original Accepted admission"
        );
        let key = worker.file.as_ref().unwrap().key.unwrap();
        grant
            .grant(AcceptedAdmission {
                key,
                reply_to: 2,
                outbound_sequence: 3,
            })
            .unwrap();
        worker.advance_file().unwrap_or_else(|_| panic!("create"));
        assert_eq!(std::fs::read(&path).unwrap(), b"actual");
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
                outcome: v2::Outcome::FileCreated { length: 6, .. },
                ..
            }
        ));
        let WorkerEvent::CacheUpdate(CachePatch::Terminal { boundary, view, .. }) =
            ports.events.try_recv().unwrap()
        else {
            panic!("terminal")
        };
        assert_eq!(boundary, file_boundary(key));
        assert!(matches!(
            view,
            v2::OperationView::Finished {
                outcome: v2::Outcome::FileCreated { .. }
            }
        ));
        assert!(
            worker.active.is_none(),
            "file never owns process output state"
        );
        ports
            .returned
            .try_send(DataReturn::ProducerDrained {
                key,
                final_ordinal: 0,
            })
            .unwrap_or_else(|_| panic!("drain"));
        worker.returns();
        assert!(worker.file.is_none());
        worker
            .observe_file(
                v2::Request::ReadFile {
                    lease: key.lease,
                    path: v2::LeafName::new("leaf").unwrap(),
                    offset: u64::MAX - 4096,
                    length: 4096,
                },
                ticket,
            )
            .unwrap_or_else(|_| panic!("read EOF"));
        let WorkerEvent::ResponseReady {
            reply: v2::Reply::FileBytes { bytes, eof, .. },
            ..
        } = ports.events.try_recv().unwrap()
        else {
            panic!("read")
        };
        assert!(bytes.as_slice().is_empty() && eof);
    }
    #[test]
    fn file_cancel_before_attempt_commits_not_started_without_native_open() {
        let (_tmp, mut worker, ports, _grant, _ticket) = accepted_file(true);
        worker.advance_file().unwrap_or_else(|_| panic!("cancel"));
        let pending = worker.file.as_ref().unwrap();
        assert!(!pending.owner.attempted && pending.owner.handle.is_none());
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
                outcome: v2::Outcome::NotStarted {
                    reason: v2::NotStartedReason::CancelRequested
                },
                ..
            }
        ));
        assert!(matches!(
            ports.events.try_recv().unwrap(),
            WorkerEvent::CacheUpdate(CachePatch::Terminal {
                boundary: FinalBoundary {
                    final_ordinal: 0,
                    ..
                },
                ..
            })
        ));
    }
    #[test]
    fn file_uncertainty_is_exact_synced_close_and_never_finished() {
        let (_tmp, mut worker, ports, mut grant, _ticket) = accepted_file(false);
        let path = worker
            .invocation
            .as_ref()
            .unwrap()
            .workspace_root
            .join("leaf");
        std::fs::write(&path, b"untouched").unwrap();
        let key = worker.file.as_ref().unwrap().key.unwrap();
        grant
            .grant(AcceptedAdmission {
                key,
                reply_to: 2,
                outbound_sequence: 3,
            })
            .unwrap();
        worker
            .advance_file()
            .unwrap_or_else(|_| panic!("uncertain"));
        assert_eq!(std::fs::read(path).unwrap(), b"untouched");
        assert!(matches!(
            worker
                .store
                .as_ref()
                .unwrap()
                .records()
                .last()
                .unwrap()
                .entry(),
            Entry::LeaseClosed {
                disposition: Disposition::Quarantined {
                    reason: v2::UnknownReason::FileEffectUncertain
                },
                ..
            }
        ));
        assert!(
            !worker
                .store
                .as_ref()
                .unwrap()
                .records()
                .iter()
                .any(|r| matches!(r.entry(), Entry::OperationFinished { .. }))
        );
        assert!(worker.stopping && worker.file.as_ref().unwrap().owner.sticky_open);
        ports.events.try_recv().unwrap(); // exact lease close is already committed
        worker.advance_file().unwrap_or_else(|_| panic!("publish"));
        assert!(matches!(
            ports.events.try_recv().unwrap(),
            WorkerEvent::CacheUpdate(CachePatch::Terminal {
                view: v2::OperationView::Unknown {
                    reason: v2::UnknownReason::FileEffectUncertain
                },
                ..
            })
        ));
        worker.retire_file();
        let failure = worker.failure.as_ref().unwrap();
        assert!(matches!(
            failure.primary,
            WorkerPrimary::File(crate::files::FileFailure::OpenUnclassified(_))
        ));
        assert!(failure.retained_file.as_ref().unwrap().sticky_open);
    }
    #[test]
    fn observation_requires_exact_held_lease_and_busy_never_opens() {
        for state in 0..5 {
            let (_tmp, mut worker, ports, [p0, p1], inv) = fixture();
            let binding = inv.expected.binding;
            let manifest_digest = inv.expected.manifest_digest;
            worker.initialize(inv).unwrap_or_else(|_| panic!("init"));
            ports.events.try_recv().unwrap();
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
            ports.events.try_recv().unwrap();
            worker.response.take();
            let (intent, actual, _) = worker.lease.clone().unwrap();
            let lease = if state == 0 {
                v2::LeaseId::from_bytes([9; 16])
            } else {
                actual
            };
            let expected = match state {
                0 | 1 => v2::RefusalCode::WrongLease,
                2 => v2::RefusalCode::AwaitingExternalReset,
                3 => v2::RefusalCode::LeaseQuarantined,
                _ => v2::RefusalCode::Busy,
            };
            match state {
                1 => worker.lease = None,
                2 => {
                    worker.lease = Some((
                        intent,
                        actual,
                        v2::LeaseView::AwaitingExternalReset { intent },
                    ))
                }
                3 => {
                    worker.lease = Some((
                        intent,
                        actual,
                        v2::LeaseView::Quarantined {
                            intent,
                            reason: v2::UnknownReason::FileEffectUncertain,
                        },
                    ))
                }
                4 => {
                    worker.file = Some(PendingFileRequest {
                        request: v2::Request::Describe,
                        ticket: None,
                        owner: Box::new(crate::files::FileOwner::new()),
                        key: None,
                        create: None,
                    })
                }
                _ => {}
            }
            worker
                .observe_file(
                    v2::Request::InspectFile {
                        lease,
                        path: v2::LeafName::new("absent").unwrap(),
                    },
                    ResponseTicket {
                        reply_to: 2,
                        permit: p1,
                    },
                )
                .unwrap_or_else(|_| panic!("authorization"));
            assert!(
                matches!(ports.events.try_recv().unwrap(),WorkerEvent::ResponseReady { reply:v2::Reply::Refused { code, .. },.. } if code==expected)
            );
            assert!(
                !worker.stopping,
                "absent open would latch sticky uncertainty"
            );
            assert!(worker.file.as_ref().is_none_or(|p| !p.owner.attempted));
        }
    }
    #[test]
    fn file_observation_open_uncertainty_stops_but_known_oversize_retires() {
        for missing in [false, true] {
            let (_tmp, mut worker, ports, _grant, ticket) = accepted_file(true);
            worker
                .advance_file()
                .unwrap_or_else(|_| panic!("cancel create"));
            ports.events.try_recv().unwrap();
            let key = worker.file.as_ref().unwrap().key.unwrap();
            ports
                .returned
                .try_send(DataReturn::ProducerDrained {
                    key,
                    final_ordinal: 0,
                })
                .unwrap_or_else(|_| panic!("drain"));
            worker.returns();
            worker.response.take();
            let lease = worker.lease.as_ref().unwrap().1;
            if !missing {
                std::fs::write(
                    worker
                        .invocation
                        .as_ref()
                        .unwrap()
                        .workspace_root
                        .join("leaf"),
                    vec![1u8; 4097],
                )
                .unwrap();
            }
            let records = worker.store.as_ref().unwrap().records().len();
            worker
                .observe_file(
                    v2::Request::InspectFile {
                        lease,
                        path: v2::LeafName::new("leaf").unwrap(),
                    },
                    ticket,
                )
                .unwrap_or_else(|_| panic!("observe"));
            assert!(matches!(
                ports.events.try_recv().unwrap(),
                WorkerEvent::ResponseReady {
                    reply: v2::Reply::Refused {
                        code: v2::RefusalCode::Unsupported,
                        ..
                    },
                    ..
                }
            ));
            assert_eq!(worker.store.as_ref().unwrap().records().len(), records);
            assert_eq!(worker.stopping, missing);
            if missing {
                let pending = worker.file.as_ref().unwrap();
                assert!(pending.owner.sticky_open && pending.owner.handle.is_none());
                worker.retire_file();
                assert!(matches!(
                    worker.failure.as_ref().unwrap().primary,
                    WorkerPrimary::File(crate::files::FileFailure::OpenUnclassified(_))
                ));
            } else {
                assert!(worker.file.is_none() && worker.failure.is_none());
            }
        }
    }
    #[test]
    fn file_panic_preserves_exact_payload_primary_and_committed_finish() {
        for finished in [false, true] {
            let (_tmp, mut worker, ports, mut grant, _ticket) = accepted_file(false);
            let key = worker.file.as_ref().unwrap().key.unwrap();
            if finished {
                grant
                    .grant(AcceptedAdmission {
                        key,
                        reply_to: 2,
                        outbound_sequence: 3,
                    })
                    .unwrap();
                worker.advance_file().unwrap_or_else(|_| panic!("create"));
                ports.events.try_recv().unwrap();
            } else {
                worker.file.as_mut().unwrap().owner.attempted = true;
                worker.file.as_mut().unwrap().owner.opening = true;
            }
            let payload = Box::new([91u8; 32]);
            let address = payload.as_ptr();
            worker.file_panic(payload);
            let last = worker
                .store
                .as_ref()
                .unwrap()
                .records()
                .last()
                .unwrap()
                .entry();
            if finished {
                assert!(matches!(
                    last,
                    Entry::OperationFinished {
                        outcome: v2::Outcome::FileCreated { .. },
                        ..
                    }
                ));
            } else {
                assert!(matches!(
                    last,
                    Entry::LeaseClosed {
                        disposition: Disposition::Quarantined {
                            reason: v2::UnknownReason::FileEffectUncertain
                        },
                        ..
                    }
                ));
            }
            worker.retire_file();
            assert_eq!(
                worker
                    .failure
                    .as_ref()
                    .unwrap()
                    .panic_payload()
                    .unwrap()
                    .downcast_ref::<[u8; 32]>()
                    .unwrap()
                    .as_ptr(),
                address
            );
            assert_eq!(
                worker
                    .failure
                    .as_ref()
                    .unwrap()
                    .retained_file
                    .as_ref()
                    .unwrap()
                    .sticky_open,
                !finished
            );
        }
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
                // Manager owners pin the worker to its creation thread. This
                // case has only explicitly settled spawn ownership to retire.
                worker.settle_native();
                let WorkerCompletion::Failed(f) = worker.completion() else {
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
    fn integrated_admitted(
        file: bool,
    ) -> (
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
            if file {
                v2::Request::CreateFile {
                    lease,
                    operation,
                    path: v2::LeafName::new("leaf").unwrap(),
                    bytes: v2::FileBytes::new(b"bytes").unwrap(),
                }
            } else {
                v2::Request::Run {
                    lease,
                    operation,
                    program: v2::ProgramId::new("probe").unwrap(),
                }
            },
        );
        worker
            .command(worker.ports.commands.try_recv().ok().unwrap())
            .unwrap_or_else(|_| panic!("run"));
        d.event_quantum().unwrap(); // Accepted cache, original ResponseReady deliberately retained.
        (tmp, worker, d, lease, operation)
    }
    fn integrated_accepted() -> (
        tempfile::TempDir,
        Worker,
        crate::dispatcher::Dispatcher,
        v2::LeaseId,
        v2::OperationId,
    ) {
        integrated_admitted(false)
    }
    #[test]
    fn file_terminal_waits_for_original_accepted_and_matching_zero_drain() {
        let (_tmp, mut worker, mut d, lease, operation) = integrated_admitted(true);
        request(&mut d, 4, v2::Request::Cancel { lease, operation });
        d.response_quantum().unwrap();
        dispatch_flush(&mut d);
        worker
            .advance_file()
            .unwrap_or_else(|_| panic!("cancel physical"));
        d.event_quantum().unwrap();
        request(&mut d, 5, v2::Request::QueryOperation { lease, operation });
        assert!(
            !d.response_quantum().unwrap(),
            "terminal cannot precede original Accepted"
        );
        let (ticket, reply, key) = worker.response.take().unwrap();
        worker.respond(ticket, reply, key);
        d.event_quantum().unwrap();
        assert!(d.response_quantum().unwrap()); // original Accepted
        assert!(d.response_quantum().unwrap()); // terminal
        dispatch_flush(&mut d);
        d.return_quantum().unwrap();
        worker.returns();
        assert!(worker.file.is_none() && worker.active.is_none());
        request(
            &mut d,
            6,
            v2::Request::Run {
                lease,
                operation: v2::OperationId::from_bytes([8; 16]),
                program: v2::ProgramId::new("probe").unwrap(),
            },
        );
        worker
            .command(worker.ports.commands.try_recv().unwrap())
            .unwrap_or_else(|_| panic!("run after file"));
        assert!(worker.active.is_some() && worker.file.is_none());
        assert_eq!(worker.active.as_ref().unwrap().key.generation, 2);
    }
    #[cfg(feature = "test-support")]
    #[test]
    fn file_fixture_native_write_error_retains_both_handles_and_exact_quarantine() {
        use crate::test_support::{FixtureControl, FixtureMode};
        let (_tmp, mut worker, ports, mut grant, _ticket) = accepted_file(false);
        let control = FixtureControl::new(FixtureMode::FailFileWrite);
        worker.invocation.as_mut().unwrap().fixture = Some(control.clone());
        let key = worker.file.as_ref().unwrap().key.unwrap();
        grant
            .grant(AcceptedAdmission {
                key,
                reply_to: 2,
                outbound_sequence: 3,
            })
            .unwrap();
        worker
            .advance_file()
            .unwrap_or_else(|_| panic!("native failure"));
        let owner = &worker.file.as_ref().unwrap().owner;
        let Some(crate::files::FileFailure::IoAfterOpen {
            stage: crate::files::FileStage::Write,
            error,
        }) = &owner.primary
        else {
            panic!("actual write source")
        };
        assert!(
            error.raw_os_error().is_some(),
            "write_all on actual read-only native file supplies OS source"
        );
        assert!(owner.handle.is_some() && owner.fixture_reopened.is_some());
        assert!(!owner.sticky_open && !owner.opening);
        assert_eq!(
            std::fs::read(
                worker
                    .invocation
                    .as_ref()
                    .unwrap()
                    .workspace_root
                    .join("leaf")
            )
            .unwrap(),
            b""
        );
        let snapshot = control.snapshot();
        assert!(
            snapshot.file_before_effect
                && snapshot.file_effect_attempted
                && snapshot.file_write_failed
                && snapshot.file_quarantine_committed
        );
        assert!(!snapshot.file_finished_committed);
        assert!(matches!(
            worker
                .store
                .as_ref()
                .unwrap()
                .records()
                .last()
                .unwrap()
                .entry(),
            Entry::LeaseClosed {
                disposition: Disposition::Quarantined {
                    reason: v2::UnknownReason::FileEffectUncertain
                },
                ..
            }
        ));
        assert!(
            !worker
                .store
                .as_ref()
                .unwrap()
                .records()
                .iter()
                .any(|r| matches!(r.entry(), Entry::OperationFinished { .. }))
        );
        ports.events.try_recv().unwrap();
        worker
            .advance_file()
            .unwrap_or_else(|_| panic!("publish unknown"));
        assert!(matches!(
            ports.events.try_recv().unwrap(),
            WorkerEvent::CacheUpdate(CachePatch::Terminal {
                view: v2::OperationView::Unknown {
                    reason: v2::UnknownReason::FileEffectUncertain
                },
                ..
            })
        ));
        worker.retire_file();
        let retained = worker
            .failure
            .as_ref()
            .unwrap()
            .retained_file
            .as_ref()
            .unwrap();
        assert!(retained.handle.is_some() && retained.fixture_reopened.is_some());
    }
    #[test]
    fn file_close_error_preserves_original_file_source_and_secondary_store_error() {
        let (_tmp, mut worker, _ports, _grant, _ticket) = accepted_file(false);
        let key = worker.file.as_ref().unwrap().key.unwrap();
        worker.file.as_mut().unwrap().owner.primary =
            Some(crate::files::FileFailure::IoAfterOpen {
                stage: crate::files::FileStage::Write,
                error: io::Error::other(Original(73)),
            });
        // An actual ledger rejection provides a typed secondary StoreError; this
        // is an ownership model, not a claim of an injected native sync failure.
        worker
            .append(Entry::LeaseClosed {
                lease: key.lease,
                disposition: Disposition::Quarantined {
                    reason: v2::UnknownReason::HelperRestartedWithUnfinishedOperation,
                },
            })
            .unwrap_or_else(|_| panic!("close"));
        let records = worker.store.as_ref().unwrap().records().len();
        worker
            .quarantine_file()
            .unwrap_or_else(|_| panic!("retained failure"));
        assert_eq!(worker.store.as_ref().unwrap().records().len(), records);
        assert!(worker.file.as_ref().unwrap().owner.ledger_error.is_some());
        worker.retire_file();
        let failure = worker.failure.as_ref().unwrap();
        let WorkerPrimary::File(crate::files::FileFailure::IoAfterOpen { error, .. }) =
            &failure.primary
        else {
            panic!("original file error")
        };
        assert_eq!(
            error
                .get_ref()
                .unwrap()
                .downcast_ref::<Original>()
                .unwrap()
                .0,
            73
        );
        assert!(
            failure
                .retained_file
                .as_ref()
                .unwrap()
                .ledger_error
                .is_some()
        );
        assert!(worker.stopping);
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
                        inspect_failure: None,
                    },
                    _ => v2::Reply::Refused {
                        code: if quarantine {
                            v2::RefusalCode::LeaseQuarantined
                        } else {
                            v2::RefusalCode::AwaitingExternalReset
                        },
                        inspect_failure: None,
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

impl Worker {
    fn inspect_service(
        &mut self,
        request: v2::Request,
        ticket: ResponseTicket,
    ) -> Result<(), WorkerPrimary> {
        let v2::Request::InspectService { lease, slot } = request else {
            self.refuse(ticket, v2::RefusalCode::InvalidRequest);
            return Ok(());
        };
        if self.active.is_some() || self.file.is_some() || self.service_action.is_some() {
            self.refuse(ticket, v2::RefusalCode::Busy);
            return Ok(());
        }
        if self.service.as_ref().is_some_and(|s| s.uncertain) {
            self.refuse(ticket, v2::RefusalCode::OwnershipPending);
            return Ok(());
        }
        if let Some(code) = self.held_refusal(lease) {
            self.refuse(ticket, code);
            return Ok(());
        }
        let Some(service) = &mut self.service else {
            self.refuse(ticket, v2::RefusalCode::Unsupported);
            return Ok(());
        };
        match service.inspect(
            lease,
            &slot,
            Instant::now() + Duration::from_secs(30),
            &self.ports.stop,
        ) {
            Ok(observation) => self.respond(
                ticket,
                v2::Reply::ServiceObservation {
                    lease,
                    observation: Box::new(observation),
                },
                None,
            ),
            Err(error) => {
                // Same OwnershipPending refusal; only the closed step name of
                // the failed inspection travels, never the retained error text.
                let failure = crate::integration::service::inspect_failure(&error);
                service.errors.push(error);
                self.respond(
                    ticket,
                    v2::Reply::Refused {
                        code: v2::RefusalCode::OwnershipPending,
                        inspect_failure: Some(failure),
                    },
                    None,
                );
            }
        }
        Ok(())
    }
    fn begin_service(
        &mut self,
        request: v2::Request,
        ticket: ResponseTicket,
        cancel: CancelToken,
        start: StartWatch,
    ) -> Result<(), WorkerPrimary> {
        if self.service.is_none() {
            self.refuse(ticket, v2::RefusalCode::Unsupported);
            return Ok(());
        }
        if self.active.is_some() || self.file.is_some() || self.service_action.is_some() {
            self.refuse(ticket, v2::RefusalCode::Busy);
            return Ok(());
        }
        if self.service.as_ref().is_some_and(|s| s.uncertain) {
            self.refuse(ticket, v2::RefusalCode::OwnershipPending);
            return Ok(());
        }
        let v2::Request::ServiceAction {
            lease, operation, ..
        } = &request
        else {
            self.refuse(ticket, v2::RefusalCode::InvalidRequest);
            return Ok(());
        };
        let (lease, operation) = (*lease, *operation);
        let key = cancel.key;
        if key.lease != lease || key.operation != operation || start.key() != key {
            self.refuse(ticket, v2::RefusalCode::InvalidRequest);
            return Ok(());
        }
        if let Some(code) = self.held_refusal(lease) {
            self.refuse(ticket, code);
            return Ok(());
        }
        let replay = self
            .store
            .as_ref()
            .unwrap()
            .restart_view()
            .replay_operation(&request);
        match replay {
            Ok(v2::ledger::ReplayDecision::Recorded(view)) => {
                self.respond(
                    ticket,
                    v2::Reply::Operation {
                        lease,
                        operation,
                        state: view,
                    },
                    None,
                );
                return Ok(());
            }
            Err(code) => {
                self.refuse(ticket, code);
                return Ok(());
            }
            Ok(v2::ledger::ReplayDecision::NotRecorded) => (),
        }
        let digest =
            v2::request_digest(&request).map_err(|e| WorkerPrimary::Io(io::Error::other(e)))?;
        self.append(Entry::OperationAccepted {
            lease,
            operation,
            request_digest: digest,
            operation_kind: v2::OperationKind::ServiceAction,
        })?;
        let now = Instant::now();
        let deadline = now + Duration::from_secs(30);
        self.service_action = Some(PendingService {
            request,
            key,
            cancel,
            start,
            deadline,
            attempted: false,
            terminal: None,
            published: false,
        });
        self.response = Some((
            ticket,
            v2::Reply::Operation {
                lease,
                operation,
                state: v2::OperationView::Accepted {
                    operation_kind: v2::OperationKind::ServiceAction,
                },
            },
            Some(key),
        ));
        self.publish(WorkerEvent::CacheUpdate(CachePatch::Phase {
            key,
            kind: v2::OperationKind::ServiceAction,
            phase: LivePhase::Accepted,
            accepted_at: Some(now),
            deadline: Some(deadline),
        }));
        Ok(())
    }
    fn advance_service(&mut self) -> Result<(), WorkerPrimary> {
        let Some(pending) = &mut self.service_action else {
            return Ok(());
        };
        let key = pending.key;
        if let Some(view) = &pending.terminal {
            if !pending.published && self.pending.is_none() {
                let view = view.clone();
                pending.published = true;
                self.publish(WorkerEvent::CacheUpdate(CachePatch::Terminal {
                    key,
                    view,
                    boundary: file_boundary(key),
                }));
            }
            return Ok(());
        }
        let stopped = self.stopping
            || self.ports.stop.load(Ordering::Acquire)
            || pending.cancel.flag.load(Ordering::Acquire)
            || matches!(pending.start.poll(), StartState::Aborted);
        let expired = Instant::now() >= pending.deadline;
        if !pending.attempted && (stopped || expired) {
            return self.finish_service(v2::Outcome::NotStarted {
                reason: if expired {
                    v2::NotStartedReason::Deadline
                } else {
                    v2::NotStartedReason::CancelRequested
                },
            });
        }
        if !matches!(pending.start.poll(), StartState::Granted) {
            return Ok(());
        }
        if self.stopping || self.ports.stop.load(Ordering::Acquire) {
            pending.cancel.flag.store(true, Ordering::Release);
        }
        let service = self.service.as_mut().unwrap();
        let result = if pending.attempted {
            service.poll(pending.deadline, &pending.cancel.flag)
        } else {
            pending.attempted = true;
            service
                .note_action(&pending.request)
                .map_err(WorkerPrimary::Io)?;
            let v2::Request::ServiceAction {
                slot,
                prior_binding_digest,
                action,
                ..
            } = &pending.request
            else {
                unreachable!()
            };
            service.action(
                slot,
                *prior_binding_digest,
                *action,
                pending.deadline,
                &pending.cancel.flag,
            )
        };
        match result {
            Ok(Some(outcome)) => {
                self.finish_service(v2::Outcome::ServiceActionCompleted { outcome })
            }
            Ok(None) => Ok(()),
            Err(error) => {
                service.errors.push(error);
                service.uncertain = true;
                self.finish_service(v2::Outcome::ServiceActionCompleted {
                    outcome: v2::ServiceOutcome::Unknown {
                        reason: "native service action retains unresolved authority".into(),
                    },
                })
            }
        }
    }
    fn finish_service(&mut self, outcome: v2::Outcome) -> Result<(), WorkerPrimary> {
        let key = self.service_action.as_ref().unwrap().key;
        self.append(Entry::OperationFinished {
            lease: key.lease,
            operation: key.operation,
            outcome: outcome.clone(),
        })?;
        self.service_action.as_mut().unwrap().terminal =
            Some(v2::OperationView::Finished { outcome });
        self.advance_service()
    }
}

impl Worker {
    fn probe_readiness(
        &mut self,
        request: v2::Request,
        ticket: ResponseTicket,
    ) -> Result<(), WorkerPrimary> {
        let v2::Request::ProbeReadiness {
            lease, deadline_ms, ..
        } = &request
        else {
            self.refuse(ticket, v2::RefusalCode::InvalidRequest);
            return Ok(());
        };
        let lease = *lease;
        if self.active.is_some() || self.file.is_some() || self.service_action.is_some() {
            self.refuse(ticket, v2::RefusalCode::Busy);
            return Ok(());
        }
        if self.service.as_ref().is_some_and(|s| s.uncertain) {
            self.refuse(ticket, v2::RefusalCode::OwnershipPending);
            return Ok(());
        }
        if let Some(code) = self.held_refusal(lease) {
            self.refuse(ticket, code);
            return Ok(());
        }
        let Some(service) = &mut self.service else {
            self.refuse(ticket, v2::RefusalCode::Unsupported);
            return Ok(());
        };
        match service.probe(
            &request,
            Instant::now() + Duration::from_millis(u64::from(*deadline_ms)),
            &self.ports.stop,
        ) {
            Ok(observation) => self.respond(
                ticket,
                v2::Reply::ReadinessObservation {
                    lease,
                    observation: Box::new(observation),
                },
                None,
            ),
            Err(error) => {
                service.errors.push(error);
                self.refuse(ticket, v2::RefusalCode::OwnershipPending);
            }
        }
        Ok(())
    }
}

impl Worker {
    fn begin_desktop(
        &mut self,
        request: v2::Request,
        ticket: ResponseTicket,
    ) -> Result<(), WorkerPrimary> {
        if self.active.is_some()
            || self.file.is_some()
            || self.service_action.is_some()
            || self.desktop_tickets.len() >= 2
        {
            self.refuse(ticket, v2::RefusalCode::Busy);
            return Ok(());
        }
        let lease = match &request {
            v2::Request::BrokerBind { original } | v2::Request::QueryBrokerBind { original } => {
                v2::LeaseId::parse(&original.helper_lease)
            }
            v2::Request::DesktopAction { request } => v2::LeaseId::parse(&request.helper_lease),
            v2::Request::InspectDesktop { lease, .. } | v2::Request::Release { lease } => {
                Ok(*lease)
            }
            v2::Request::QueryDesktop { identity }
            | v2::Request::CancelDesktop { identity }
            | v2::Request::ProbeDesktopReadiness { identity, .. } => {
                v2::LeaseId::parse(&identity.helper_lease)
            }
            _ => {
                self.refuse(ticket, v2::RefusalCode::InvalidRequest);
                return Ok(());
            }
        }
        .map_err(|e| WorkerPrimary::Io(io::Error::other(e)))?;
        let original_control = matches!(
            request,
            v2::Request::QueryBrokerBind { .. }
                | v2::Request::QueryDesktop { .. }
                | v2::Request::CancelDesktop { .. }
        );
        if original_control {
            if !self
                .lease
                .as_ref()
                .is_some_and(|(_, actual, _)| *actual == lease)
            {
                self.refuse(ticket, v2::RefusalCode::WrongLease);
                return Ok(());
            }
        } else if let Some(code) = self.held_refusal(lease) {
            self.refuse(ticket, code);
            return Ok(());
        }
        let Some(desktop) = &mut self.desktop else {
            self.refuse(ticket, v2::RefusalCode::Unsupported);
            return Ok(());
        };
        match desktop.start(self.store.as_mut().unwrap(), lease, request.clone()) {
            Ok(Some(reply)) => self.respond(ticket, reply, None),
            Ok(None) => self.desktop_tickets.push_back((request, ticket)),
            Err(error) => {
                desktop.fail(error);
                self.refuse(ticket, v2::RefusalCode::OwnershipPending);
            }
        }
        Ok(())
    }
    fn advance_desktop(&mut self) -> Result<(), WorkerPrimary> {
        if self.stopping {
            if let Some(desktop) = &mut self.desktop {
                desktop.request_stop();
            }
            self.desktop_tickets.clear();
            return Ok(());
        }
        let result = self
            .desktop
            .as_mut()
            .unwrap()
            .poll(self.store.as_mut().unwrap());
        match result {
            Ok(Some((original, reply))) => {
                let position = self
                    .desktop_tickets
                    .iter()
                    .position(|(request, _)| request == &original)
                    .ok_or_else(|| {
                        WorkerPrimary::Io(io::Error::other(
                            "original desktop response ticket missing",
                        ))
                    })?;
                let (_, ticket) = self.desktop_tickets.remove(position).unwrap();
                if let v2::Reply::DesktopState { identity, state } = &reply {
                    self.publish(WorkerEvent::CacheUpdate(CachePatch::DesktopState {
                        identity: *identity.clone(),
                        state: *state.clone(),
                    }));
                }
                if let (
                    v2::Request::Release { lease },
                    v2::Reply::Lease {
                        state: v2::LeaseView::AwaitingExternalReset { .. },
                        ..
                    },
                ) = (&original, &reply)
                {
                    self.release(*lease, ticket)?;
                } else {
                    self.response = Some((ticket, reply, None));
                }
            }
            Ok(None) => {}
            Err(error) => {
                self.desktop.as_mut().unwrap().fail(error);
                if let Some((_, ticket)) = self.desktop_tickets.pop_front() {
                    self.refuse(ticket, v2::RefusalCode::OwnershipPending);
                }
            }
        }
        Ok(())
    }
}
