//! Fair bounded polling and retained ownership for the local façade.
use super::{
    budget::{self, EncodeFailure, OutboundFrame},
    framing::{CancelWrite, FrameDecoder, SlotId, Slots},
    owned_error::OwnedError,
    query::{QueryCompletion, QueryDestination, QueryJob, QueryValue, QueryWork, QueryWorker},
    registry::{AdmissionDecision, Registry, RegistryUpdate},
    roots::Roots,
    shutdown::{ErrorBundle, RuntimeAction, Shutdown},
    types::*,
    wire::WireState,
    worker::{EngineWorker, WorkerCompletion},
};
use crate::engine::CancellationToken;
use std::{
    io,
    sync::Arc,
    time::{Duration, Instant},
};
use theseus_host::stdio::{AcquireFailure, ReadStep, Stdio, WriteStep};

pub(crate) const IDLE_WAIT: Duration = Duration::from_millis(10);
const QUANTUM: usize = 4096;

/// Only the transport methods may touch native I/O on the owner thread.
pub(crate) trait Transport {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<ReadStep>;
    fn write(&mut self, bytes: &[u8]) -> io::Result<WriteStep>;
    fn write_in_flight(&self) -> bool;
    fn stop(&mut self);
    fn poll_stop(&mut self) -> io::Result<bool>;
    fn settled(&self) -> bool;
}
pub(crate) enum NativeTransport {
    Acquired(Stdio),
    Failed(AcquireFailure),
}
impl Transport for NativeTransport {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<ReadStep> {
        match self {
            Self::Acquired(t) => t.try_read(bytes),
            Self::Failed(_) => Ok(ReadStep::Pending),
        }
    }
    fn write(&mut self, bytes: &[u8]) -> io::Result<WriteStep> {
        match self {
            Self::Acquired(t) => t.try_write(bytes),
            Self::Failed(_) => Ok(WriteStep::Pending),
        }
    }
    fn write_in_flight(&self) -> bool {
        matches!(self, Self::Acquired(t) if t.write_in_flight())
    }
    fn stop(&mut self) {
        match self {
            Self::Acquired(t) => t.request_stop(),
            Self::Failed(t) => t.request_stop(),
        }
    }
    fn poll_stop(&mut self) -> io::Result<bool> {
        match self {
            Self::Acquired(t) => t.poll_stop_or_restore(),
            Self::Failed(t) => t.poll_stop_or_restore(),
        }
    }
    fn settled(&self) -> bool {
        match self {
            Self::Acquired(t) => t.is_settled(),
            Self::Failed(t) => t.is_settled(),
        }
    }
}
/// The concrete worker performs only nonblocking mailbox reads and actual
/// finished-thread joins. This private seam also permits deterministic schedules.
trait EnginePoll {
    fn binding(&mut self) -> Option<SavedBinding>;
    fn joined(&mut self) -> Option<WorkerCompletion>;
}
impl EnginePoll for EngineWorker {
    fn binding(&mut self) -> Option<SavedBinding> {
        self.try_binding()
    }
    fn joined(&mut self) -> Option<WorkerCompletion> {
        self.try_join()
    }
}
struct EngineProgress {
    bindings: [Option<SavedBinding>; 4],
    completion: Option<WorkerCompletion>,
}
fn poll_engine(worker: &mut impl EnginePoll) -> EngineProgress {
    let mut bindings = std::array::from_fn(|_| None);
    for binding in &mut bindings[..2] {
        *binding = worker.binding();
        if binding.is_none() {
            break;
        }
    }
    let completion = worker.joined();
    if completion.is_some() {
        // The producer is actually joined, so publication cannot race this
        // final bounded drain. Keep its owner until both cells are collected.
        for binding in &mut bindings[2..] {
            *binding = worker.binding();
            if binding.is_none() {
                break;
            }
        }
    }
    EngineProgress {
        bindings,
        completion,
    }
}

struct RequestSlot {
    error: Option<OwnedError>,
    slot: SlotId,
    reply: Option<ReplyContext>,
    handle: Option<Handle>,
    admission: bool,
    initialize: bool,
    close: bool,
}
struct ActiveQuery {
    generation: u64,
    destination: QueryDestination,
    token: CancellationToken,
    deadline: Instant,
    live: bool,
}
struct ActiveRun {
    handle: Handle,
    token: CancellationToken,
    deadline: Instant,
}
/// A force action is a command to the binary, never permission to drop this owner.
pub(crate) struct ServerRuntime<T: Transport = NativeTransport> {
    transport: T,
    roots: Option<Arc<Roots>>,
    instance: Instance,
    max_run_seconds: u64,
    stop: CancellationToken,
    wire: WireState,
    decoder: FrameDecoder,
    ingress: [u8; QUANTUM],
    ingress_start: usize,
    ingress_end: usize,
    slots: Slots,
    requests: [Option<RequestSlot>; 4],
    output: Option<SlotId>,
    output_progress: Option<Instant>,
    close_after: Option<SlotId>,
    query: Option<QueryWorker>,
    active_query: Option<ActiveQuery>,
    generation: u64,
    engine: Option<EngineWorker>,
    active_run: Option<ActiveRun>,
    registry: Registry,
    shutdown: Shutdown,
    owners: ErrorBundle,
    worker_failed: bool,
}
impl ServerRuntime<NativeTransport> {
    pub(crate) fn new(
        transport: Result<Stdio, AcquireFailure>,
        roots: Result<Arc<Roots>, QueryError>,
        instance: Instance,
        max_run_seconds: u64,
        stop: CancellationToken,
        now: Instant,
    ) -> Self {
        let failed = transport.is_err();
        let transport = match transport {
            Ok(t) => NativeTransport::Acquired(t),
            Err(e) => NativeTransport::Failed(e),
        };
        let mut runtime = Self::base(transport, None, instance, max_run_seconds, stop);
        match roots {
            Ok(roots) => {
                if !failed {
                    match QueryWorker::spawn(roots.clone(), max_run_seconds) {
                        Ok(q) => runtime.query = Some(q),
                        Err(e) => runtime.owners.coordinator(e.source),
                    }
                }
                runtime.roots = Some(roots);
            }
            Err(e) => runtime.owners.coordinator(e.source),
        }
        if failed || runtime.owners.has_coordinator() || !(1..=86400).contains(&max_run_seconds) {
            runtime.begin_shutdown(now, true);
        }
        runtime
    }
}
impl<T: Transport> ServerRuntime<T> {
    fn base(
        transport: T,
        roots: Option<Arc<Roots>>,
        instance: Instance,
        max_run_seconds: u64,
        stop: CancellationToken,
    ) -> Self {
        Self {
            transport,
            roots,
            registry: Registry::new(instance.clone()),
            instance,
            max_run_seconds,
            stop,
            wire: WireState::default(),
            decoder: FrameDecoder::default(),
            ingress: [0; QUANTUM],
            ingress_start: 0,
            ingress_end: 0,
            slots: Slots::default(),
            requests: std::array::from_fn(|_| None),
            output: None,
            output_progress: None,
            close_after: None,
            query: None,
            active_query: None,
            generation: 0,
            engine: None,
            active_run: None,
            shutdown: Shutdown::default(),
            owners: ErrorBundle::default(),
            worker_failed: false,
        }
    }
    pub(crate) fn poll(&mut self, now: Instant) -> RuntimeAction {
        self.control(now);
        if !self.shutdown.active() {
            self.input_turn(now);
            self.control(now);
        }
        if !self.shutdown.active() {
            self.output_turn(now);
            self.control(now);
        }
        if self.shutdown.active() {
            self.shutdown_turn(now)
        } else {
            RuntimeAction::Continue
        }
    }
    fn begin_shutdown(&mut self, now: Instant, fatal: bool) {
        if !self.shutdown.begin(now, fatal) {
            return;
        }
        self.decoder.eof();
        self.ingress_start = 0;
        self.ingress_end = 0;
        if let Some(q) = &mut self.active_query {
            q.token.cancel();
            q.live = false;
        }
        if let Some(q) = &mut self.query {
            q.request_stop();
        }
        if let Some(run) = &self.active_run {
            run.token.cancel();
            let _ = self
                .registry
                .cancel(&run.handle, CancellationReason::ServerShutdown);
        }
        // Admissions may outlive their start-response slots.
        if let Some(ActiveQuery {
            destination: QueryDestination::Admission(handle),
            ..
        }) = &self.active_query
        {
            let _ = self
                .registry
                .cancel(handle, CancellationReason::ServerShutdown);
        }
        self.transport.stop();
        self.owners
            .extend_registry(self.registry.take_error_owners_for_shutdown());
    }
    fn fatal(&mut self, now: Instant, message: &'static str) {
        self.owners.coordinator(anyhow::anyhow!(message));
        self.begin_shutdown(now, true);
    }
    fn control(&mut self, now: Instant) {
        if self.stop.is_cancelled() {
            self.begin_shutdown(now, false);
        }
        if self.owners.sequence_exhausted() {
            self.begin_shutdown(now, true);
        }
        // Expire before collecting: late success can never revive an expired admission.
        if self
            .active_query
            .as_ref()
            .is_some_and(|q| q.live && now >= q.deadline)
        {
            let q = self.active_query.as_mut().expect("active query");
            q.live = false;
            q.token.cancel();
            let destination = q.destination.clone();
            let generation = q.generation;
            match destination {
                QueryDestination::Admission(handle) => self.apply(
                    &handle,
                    RegistryUpdate::ValidationExpired {
                        generation,
                        code: ErrorCode::QueryTimeout,
                    },
                ),
                QueryDestination::Request(slot) => {
                    self.reply_failure(slot, ErrorCode::QueryTimeout, now)
                }
            }
        }
        if let Some(run) = &self.active_run
            && now >= run.deadline
        {
            run.token.cancel();
            let _ = self
                .registry
                .cancel(&run.handle, CancellationReason::RuntimeLimit);
        }
        if let Some(completion) = self.query.as_mut().and_then(QueryWorker::try_complete) {
            self.complete_query(completion, now);
        }
        if let Some(joined) = self.query.as_mut().and_then(QueryWorker::try_join) {
            self.query = None;
            if let Err(owner) = joined {
                self.owners.coordinator_owner(owner);
                self.begin_shutdown(now, true);
            } else if !self.shutdown.active() {
                self.fatal(now, "query worker stopped unexpectedly");
            }
        }
        if let Some(progress) = self.engine.as_mut().map(poll_engine) {
            self.apply_engine_progress(progress, now);
        }
        self.registry
            .set_cleanup_blocked(self.owners.iter().any(|e| e.view().cleanup_blocked));
        if !self.shutdown.active()
            && self
                .output_progress
                .is_some_and(|last| now.saturating_duration_since(last) >= Duration::from_secs(5))
        {
            self.fatal(now, "output stalled");
        }
    }
    fn apply_engine_progress(&mut self, progress: EngineProgress, now: Instant) {
        for binding in progress.bindings.into_iter().flatten() {
            let handle = binding.handle.clone();
            self.apply(&handle, RegistryUpdate::Bound(binding));
        }
        if let Some(completion) = progress.completion {
            self.engine = None;
            self.active_run = None;
            match completion {
                WorkerCompletion::Engine(completion) => {
                    self.worker_failed |= completion.result.is_err();
                    let handle = completion.handle.clone();
                    self.apply(&handle, RegistryUpdate::Joined(completion));
                    if self.shutdown.active() {
                        self.owners
                            .extend_registry(self.registry.take_error_owners_for_shutdown());
                    }
                }
                WorkerCompletion::Finalizer { owners } => {
                    if owners.iter().any(OwnedError::has_worker_panic) {
                        self.shutdown.begin(now, true);
                    }
                    self.owners.restore(owners);
                }
            }
        }
    }
    fn input_turn(&mut self, now: Instant) {
        if self.close_after.is_some() {
            return;
        }
        let mut operations = 0;
        loop {
            if self.shutdown.active() || self.close_after.is_some() {
                break;
            }
            // Process the retained native quantum before issuing another read.
            if let Some(frame) = self.decoder.take_frame() {
                let event = self.wire.decode(&frame);
                self.event(event, now);
                self.control(now);
                continue;
            }
            if self.ingress_start < self.ingress_end {
                match self
                    .decoder
                    .push(&self.ingress[self.ingress_start..self.ingress_end])
                {
                    Ok(n) => self.ingress_start += n,
                    Err(_) => {
                        self.fatal(now, "oversized or invalid input frame");
                        break;
                    }
                }
                continue;
            }
            if operations == 4 {
                break;
            }
            operations += 1;
            self.ingress_start = 0;
            self.ingress_end = 0;
            match self.transport.read(&mut self.ingress) {
                Ok(ReadStep::Bytes(n)) if n > 0 && n <= QUANTUM => self.ingress_end = n,
                Ok(ReadStep::Bytes(0) | ReadStep::Pending) => break,
                Ok(ReadStep::Eof) => {
                    self.begin_shutdown(now, false);
                    break;
                }
                Ok(_) => {
                    self.fatal(now, "invalid native read progress");
                    break;
                }
                Err(e) => {
                    self.owners.coordinator(e.into());
                    self.begin_shutdown(now, true);
                    break;
                }
            }
        }
    }
    fn output_turn(&mut self, now: Instant) {
        for _ in 0..4 {
            if self.output.is_none() {
                self.output = self
                    .requests
                    .iter()
                    .flatten()
                    .filter(|r| self.slots.frame(r.slot).is_ok_and(|f| f.is_some()))
                    .min_by_key(|r| r.slot.generation)
                    .map(|r| r.slot);
            }
            let Some(slot) = self.output else {
                self.output_progress = None;
                break;
            };
            self.output_progress.get_or_insert(now);
            let frame = self
                .slots
                .frame(slot)
                .expect("selected slot")
                .expect("selected frame");
            let bytes = &frame.remaining()[..frame.remaining().len().min(QUANTUM)];
            let offered = bytes.len();
            match self.transport.write(bytes) {
                Ok(WriteStep::Pending | WriteStep::Bytes(0)) => break,
                Ok(WriteStep::Bytes(n)) if n <= offered => {
                    self.output_progress = Some(now);
                    match self.slots.advance(slot, n) {
                        Ok(true) => {
                            let request = self.requests[usize::from(slot.index)]
                                .take()
                                .expect("request retained until newline");
                            self.output = None;
                            if request.close {
                                self.begin_shutdown(now, true);
                                break;
                            }
                        }
                        Ok(false) => (),
                        Err(_) => {
                            self.fatal(now, "invalid output progress");
                            break;
                        }
                    }
                }
                Ok(_) => {
                    self.fatal(now, "invalid native write progress");
                    break;
                }
                Err(e) => {
                    self.owners.coordinator(e.into());
                    self.begin_shutdown(now, true);
                    break;
                }
            }
        }
        // Do not keep a stall clock after the final queued frame completed.
        if !self
            .requests
            .iter()
            .flatten()
            .any(|r| self.slots.frame(r.slot).is_ok_and(|f| f.is_some()))
        {
            self.output_progress = None;
        }
    }
    fn event(&mut self, event: WireEvent, now: Instant) {
        match event {
            WireEvent::Ignore | WireEvent::Notification(Notification::Initialized) => (),
            WireEvent::Notification(Notification::Cancel(id)) => self.cancel_rpc(&id, now),
            WireEvent::ProtocolError(error) => {
                let slot = match &error.id {
                    ReplyId::Id(id) => self.slots.reserve(id.clone()),
                    _ => self.slots.reserve_anonymous(),
                };
                let Ok(slot) = slot else {
                    self.fatal(now, "protocol error exceeds request slots");
                    return;
                };
                let reply = match &error.id {
                    ReplyId::Id(id) => Some(ReplyContext {
                        era: self.wire.selected.unwrap_or(Era::Legacy),
                        id: id.clone(),
                        instance: self.instance.clone(),
                    }),
                    _ => None,
                };
                self.requests[usize::from(slot.index)] = Some(RequestSlot {
                    error: None,
                    slot,
                    reply,
                    handle: None,
                    admission: false,
                    initialize: false,
                    close: error.close_after_send,
                });
                if error.close_after_send {
                    self.close_after = Some(slot);
                }
                let encoded = super::diagnostics::guard(|| budget::encode_protocol_error(&error));
                self.encoded(slot, encoded, now);
            }
            WireEvent::Request(request) => {
                let Ok(slot) = self.slots.reserve(request.id.clone()) else {
                    self.fatal(now, "duplicate or excess outstanding request");
                    return;
                };
                self.requests[usize::from(slot.index)] = Some(RequestSlot {
                    error: None,
                    slot,
                    reply: Some(ReplyContext {
                        era: request.era,
                        id: request.id.clone(),
                        instance: self.instance.clone(),
                    }),
                    handle: None,
                    admission: false,
                    initialize: matches!(request.method, Method::Initialize),
                    close: false,
                });
                match request.method {
                    Method::ToolCall(call) => self.tool(slot, call, now),
                    Method::RejectedTool(_) => {
                        self.reply_failure(slot, ErrorCode::InvalidArguments, now)
                    }
                    method => {
                        let encoded = super::diagnostics::guard(|| {
                            budget::encode_protocol_result(request.era, &request.id, &method)
                        });
                        self.encoded(slot, encoded, now);
                    }
                }
            }
        }
    }
    fn cancel_rpc(&mut self, id: &RpcId, now: Instant) {
        let Some(request) = self
            .requests
            .iter()
            .flatten()
            .find(|r| r.reply.as_ref().is_some_and(|reply| &reply.id == id))
        else {
            return;
        };
        if request.initialize {
            return;
        }
        let slot = request.slot;
        let handle = request.handle.clone().filter(|_| request.admission);
        if let Some(handle) = &handle {
            let _ = self
                .registry
                .cancel(handle, CancellationReason::RequestCancelled);
        }
        if let Some(q) = &mut self.active_query {
            let matches = q.destination == QueryDestination::Request(slot)
                || matches!(&q.destination, QueryDestination::Admission(h) if handle.as_ref() == Some(h));
            if matches {
                q.live = false;
                q.token.cancel();
            }
        }
        if self.output == Some(slot) && self.transport.write_in_flight() {
            self.fatal(now, "cancelled write has uncertain physical progress");
            return;
        }
        match self.slots.cancel(id) {
            CancelWrite::Removed(slot) => {
                self.requests[usize::from(slot.index)] = None;
                if self.output == Some(slot) {
                    self.output = None;
                }
                if !self
                    .requests
                    .iter()
                    .flatten()
                    .any(|r| self.slots.frame(r.slot).is_ok_and(|f| f.is_some()))
                {
                    self.output_progress = None;
                }
            }
            CancelWrite::ClosePartial(_) => self.fatal(now, "cancelled partially written response"),
            CancelWrite::Absent => (),
        }
    }
    fn encoded(
        &mut self,
        slot: SlotId,
        encoded: Result<Result<OutboundFrame, EncodeFailure>, anyhow::Error>,
        now: Instant,
    ) {
        match encoded {
            Ok(frame) => self.stage(slot, frame, now),
            Err(error) => {
                self.owners.coordinator(error);
                self.begin_shutdown(now, true);
            }
        }
    }
    fn stage(&mut self, slot: SlotId, result: Result<OutboundFrame, EncodeFailure>, now: Instant) {
        let frame = match result {
            Ok(f) | Err(EncodeFailure::Capacity(f)) => f,
            Err(EncodeFailure::Fatal) => {
                self.fatal(now, "response encoder failed");
                return;
            }
        };
        if self.slots.stage(slot, frame).is_err() {
            self.fatal(now, "response slot invariant failed");
        } else {
            self.output_progress.get_or_insert(now);
        }
    }
    fn reply(&mut self, slot: SlotId, result: ToolResult, now: Instant) {
        if self.shutdown.active() {
            return;
        }
        let Some(request) = self.requests[usize::from(slot.index)]
            .as_ref()
            .filter(|r| r.slot == slot)
        else {
            return;
        };
        let reply = request.reply.as_ref().expect("tool request has id");
        let encoded = super::diagnostics::guard(|| {
            budget::encode(
                reply.era,
                &reply.id,
                &self.instance,
                request.handle.as_ref(),
                &result,
            )
        });
        self.encoded(slot, encoded, now);
    }
    fn failure(&self, code: ErrorCode, handle: Option<Handle>) -> Failure {
        Failure::new(
            self.instance.clone(),
            code,
            TextPreview::new("Request could not complete", false),
            handle,
        )
    }
    fn reply_failure(&mut self, slot: SlotId, code: ErrorCode, now: Instant) {
        let handle = self.requests[usize::from(slot.index)]
            .as_ref()
            .and_then(|r| r.handle.clone());
        self.reply(slot, Err(self.failure(code, handle)), now);
    }
    fn query_available(&self) -> bool {
        self.query.as_ref().is_some_and(|q| !q.is_busy()) && self.active_query.is_none()
    }
    fn submit(
        &mut self,
        destination: QueryDestination,
        work: QueryWork,
        now: Instant,
    ) -> Option<u64> {
        if !self.query_available() {
            return None;
        }
        let Some(generation) = self.generation.checked_add(1) else {
            self.fatal(now, "query generation exhausted");
            return None;
        };
        self.generation = generation;
        let token = CancellationToken::default();
        let job = QueryJob {
            generation,
            destination: destination.clone(),
            cancel: token.clone(),
            work,
        };
        if self
            .query
            .as_mut()
            .expect("available query")
            .try_submit(job)
            .is_err()
        {
            self.fatal(now, "query submission invariant failed");
            return None;
        }
        self.active_query = Some(ActiveQuery {
            generation,
            destination,
            token,
            deadline: now + Duration::from_secs(30),
            live: true,
        });
        Some(generation)
    }
    fn tool(&mut self, slot: SlotId, call: ToolCall, now: Instant) {
        let reply = self.requests[usize::from(slot.index)]
            .as_ref()
            .expect("reserved request")
            .reply
            .clone()
            .expect("tool request has reply context");
        match call {
            ToolCall::RecipeInspect(args) => {
                if self
                    .submit(
                        QueryDestination::Request(slot),
                        QueryWork::Inspect { args, reply },
                        now,
                    )
                    .is_none()
                {
                    self.reply_failure(slot, ErrorCode::QueryBusy, now);
                }
            }
            ToolCall::RunStart(args) => {
                self.requests[usize::from(slot.index)]
                    .as_mut()
                    .expect("request")
                    .admission = true;
                match self.registry.admit(args, self.query_available()) {
                    Err(failure) => self.reply(slot, Err(failure), now),
                    Ok(AdmissionDecision::Replay(data)) => {
                        self.requests[usize::from(slot.index)]
                            .as_mut()
                            .expect("reserved request")
                            .handle = Some(data.handle.clone());
                        self.reply(slot, Ok(ToolData::RunStart(data)), now);
                    }
                    Ok(AdmissionDecision::Reserved { handle, validation }) => {
                        self.requests[usize::from(slot.index)]
                            .as_mut()
                            .expect("reserved request")
                            .handle = Some(handle.clone());
                        if let Some(generation) = self.submit(
                            QueryDestination::Admission(handle.clone()),
                            QueryWork::ValidateStart(validation),
                            now,
                        ) {
                            self.apply(&handle, RegistryUpdate::ValidationStarted { generation });
                            self.reply(
                                slot,
                                Ok(ToolData::RunStart(StartData {
                                    handle,
                                    state: RunState::Validating,
                                    replayed: false,
                                    cancellation_requested: false,
                                    lifetime: Lifetime::ServerProcess,
                                    poll_after_ms: 500,
                                })),
                                now,
                            );
                        } else {
                            let _ = self
                                .registry
                                .cancel(&handle, CancellationReason::ServerShutdown);
                            self.fatal(now, "reserved admission could not submit validation");
                        }
                    }
                }
            }
            ToolCall::RunList(args) => {
                let result = self.registry.list(&args, &reply).map(ToolData::RunList);
                self.reply(slot, result, now);
            }
            ToolCall::RunCancel(args) => {
                let result = if args.server_instance != self.instance {
                    Err(self.failure(ErrorCode::StaleInstance, Some(args.handle.clone())))
                } else {
                    self.registry
                        .cancel(&args.handle, CancellationReason::Client)
                        .map(ToolData::RunCancel)
                };
                if result.is_ok()
                    && let Some(q) = &mut self.active_query
                    && q.destination == QueryDestination::Admission(args.handle.clone())
                {
                    q.token.cancel();
                    q.live = false;
                }
                self.reply(slot, result, now);
            }
            ToolCall::RunStatus(args) => {
                self.requests[usize::from(slot.index)]
                    .as_mut()
                    .expect("request")
                    .handle = Some(args.handle.clone());
                if args.server_instance != self.instance {
                    self.reply_failure(slot, ErrorCode::StaleInstance, now);
                    return;
                }
                let run = match self.registry.cached(&args.handle) {
                    Ok(v) => v.clone(),
                    Err(f) => {
                        self.reply(slot, Err(f), now);
                        return;
                    }
                };
                let binding = self.registry.binding(&args.handle).ok().flatten().cloned();
                let refresh = if !args.refresh {
                    Refresh::Cached
                } else if binding.is_none() {
                    Refresh::Unavailable
                } else if !self.query_available() {
                    Refresh::Busy
                } else {
                    self.submit(
                        QueryDestination::Request(slot),
                        QueryWork::RefreshStatus {
                            binding: binding.expect("checked binding"),
                            sampled_hint: run.saved_hint_sequence.get(),
                        },
                        now,
                    );
                    return;
                };
                self.reply(
                    slot,
                    Ok(ToolData::RunStatus(StatusData {
                        run,
                        refresh,
                        poll_after_ms: 500,
                    })),
                    now,
                );
            }
            ToolCall::RunEvidence(args) => {
                self.requests[usize::from(slot.index)]
                    .as_mut()
                    .expect("request")
                    .handle = Some(args.handle.clone());
                if args.server_instance != self.instance {
                    self.reply_failure(slot, ErrorCode::StaleInstance, now);
                    return;
                }
                let binding = match self.registry.binding(&args.handle) {
                    Ok(Some(b)) => b.clone(),
                    Ok(None) => {
                        self.reply_failure(slot, ErrorCode::EvidenceInvalid, now);
                        return;
                    }
                    Err(f) => {
                        self.reply(slot, Err(f), now);
                        return;
                    }
                };
                let sampled_hint = self
                    .registry
                    .cached(&args.handle)
                    .expect("binding exists")
                    .saved_hint_sequence
                    .get();
                if self
                    .submit(
                        QueryDestination::Request(slot),
                        QueryWork::Evidence {
                            binding,
                            args,
                            sampled_hint,
                            reply,
                        },
                        now,
                    )
                    .is_none()
                {
                    self.reply_failure(slot, ErrorCode::QueryBusy, now);
                }
            }
        }
    }
    fn complete_query(&mut self, completion: QueryCompletion, now: Instant) {
        let panic = completion.worker_panicked
            || completion
                .result
                .as_ref()
                .err()
                .is_some_and(OwnedError::has_worker_panic);
        let active = self.active_query.take();
        let live = active.as_ref().is_some_and(|q| {
            q.live
                && q.generation == completion.generation
                && q.destination == completion.destination
        }) && !self.shutdown.active()
            && !panic;
        if !live {
            // The completed query has left active_query already. Preserve its
            // admission cancellation boundary before entering shutdown.
            if (panic || self.shutdown.active())
                && let Some(active) = &active
            {
                active.token.cancel();
                if let QueryDestination::Admission(handle) = &active.destination {
                    let _ = self
                        .registry
                        .cancel(handle, CancellationReason::ServerShutdown);
                }
            }
            match completion.result {
                Err(owner) if panic => {
                    self.owners.coordinator_owner(owner);
                    self.begin_shutdown(now, true);
                }
                Err(owner) => {
                    if owner.view().cleanup_blocked {
                        self.owners.push(owner);
                    }
                }
                Ok(_) if panic => self.fatal(now, "query worker panicked without an error owner"),
                Ok(_) => (),
            }
            return;
        }
        match completion.destination {
            QueryDestination::Admission(handle) => {
                let update = match completion.result {
                    Ok(QueryValue::Validated(recipe)) => RegistryUpdate::Validation {
                        generation: completion.generation,
                        result: Ok(recipe),
                    },
                    Ok(QueryValue::ValidatedNative(recipe)) => RegistryUpdate::NativeValidation {
                        generation: completion.generation,
                        result: Ok(recipe),
                    },
                    Err(owner) => RegistryUpdate::Validation {
                        generation: completion.generation,
                        result: Err(owner),
                    },
                    Ok(_) => {
                        self.fatal(now, "invalid admission completion");
                        return;
                    }
                };
                self.apply(&handle, update);
                if let Some(run) = self.registry.take_native_dispatch(&handle) {
                    if self.engine.is_some() {
                        self.fatal(now, "engine slot already occupied");
                        return;
                    }
                    let Some(roots) = self.roots.as_ref() else {
                        self.fatal(now, "missing opened roots");
                        return;
                    };
                    let token = run.token.clone();
                    let spawned = match roots.native_target.clone() {
                        Some(launch) => EngineWorker::spawn_native(
                            run.handle.clone(),
                            run.recipe,
                            run.options,
                            run.token,
                            roots.output.clone(),
                            launch,
                        ),
                        None => Err(QueryError::new(
                            ErrorCode::InvalidArguments,
                            anyhow::anyhow!("native server target unavailable"),
                        )),
                    };
                    match spawned {
                        Ok(worker) => {
                            self.engine = Some(worker);
                            self.active_run = Some(ActiveRun {
                                handle: handle.clone(),
                                token,
                                deadline: now + Duration::from_secs(self.max_run_seconds),
                            });
                            self.apply(&handle, RegistryUpdate::Dispatched);
                        }
                        Err(error) => {
                            self.worker_failed = true;
                            self.apply(
                                &handle,
                                RegistryUpdate::SpawnFailed(OwnedError::deferred(error)),
                            );
                        }
                    }
                } else if let Some(run) = self.registry.take_dispatch(&handle) {
                    if self.engine.is_some() {
                        self.fatal(now, "engine slot already occupied");
                        return;
                    }
                    let Some(roots) = self.roots.as_ref() else {
                        self.fatal(now, "missing opened roots");
                        return;
                    };
                    let token = run.token.clone();
                    match EngineWorker::spawn(
                        run.handle.clone(),
                        run.recipe,
                        run.options,
                        run.token,
                        roots.output.clone(),
                    ) {
                        Ok(worker) => {
                            self.engine = Some(worker);
                            self.active_run = Some(ActiveRun {
                                handle: handle.clone(),
                                token,
                                deadline: now + Duration::from_secs(self.max_run_seconds),
                            });
                            self.apply(&handle, RegistryUpdate::Dispatched);
                        }
                        Err(error) => {
                            // Thread creation failed: no producer/native operation exists.
                            self.worker_failed = true;
                            self.apply(
                                &handle,
                                RegistryUpdate::SpawnFailed(OwnedError::deferred(error)),
                            );
                        }
                    }
                }
            }
            QueryDestination::Request(slot) => {
                let result = match completion.result {
                    Err(owner) => {
                        let request = self.requests[usize::from(slot.index)]
                            .as_ref()
                            .filter(|r| r.slot == slot);
                        let failure = Failure::new(
                            self.instance.clone(),
                            owner.view().code,
                            owner.view().message.clone(),
                            request.and_then(|r| r.handle.clone()),
                        );
                        if owner.view().cleanup_blocked {
                            self.owners.push(owner);
                        } else if let Some(request) = self.requests[usize::from(slot.index)]
                            .as_mut()
                            .filter(|r| r.slot == slot)
                        {
                            request.error = Some(owner);
                        }
                        Err(failure)
                    }
                    Ok(QueryValue::Inspect(data)) => Ok(ToolData::RecipeInspect(data)),
                    Ok(QueryValue::Evidence(data)) => Ok(ToolData::RunEvidence(data)),
                    Ok(QueryValue::Refreshed(projection)) => {
                        let Some(handle) = self.requests[usize::from(slot.index)]
                            .as_ref()
                            .filter(|r| r.slot == slot)
                            .and_then(|r| r.handle.clone())
                        else {
                            return;
                        };
                        self.apply(&handle, RegistryUpdate::Refreshed(projection));
                        self.registry.cached(&handle).map(|run| {
                            ToolData::RunStatus(StatusData {
                                run: run.clone(),
                                refresh: Refresh::Loaded,
                                poll_after_ms: 500,
                            })
                        })
                    }
                    Ok(QueryValue::Validated(_) | QueryValue::ValidatedNative(_)) => {
                        self.fatal(now, "invalid request completion");
                        return;
                    }
                };
                self.reply(slot, result, now);
            }
        }
    }
    fn apply(&mut self, handle: &Handle, update: RegistryUpdate) {
        if let Err(update) = self.registry.apply(handle, update) {
            match update {
                RegistryUpdate::Validation {
                    result: Err(owner), ..
                }
                | RegistryUpdate::NativeValidation {
                    result: Err(owner), ..
                }
                | RegistryUpdate::SpawnFailed(owner) => self.owners.push(owner),
                RegistryUpdate::Joined(completion) => {
                    if let Err(owner) = completion.result {
                        self.owners.push(owner);
                    }
                }
                _ => (),
            }
        }
    }
    fn shutdown_turn(&mut self, now: Instant) -> RuntimeAction {
        // Error and settlement are independent facts: even a settled panic is fatal.
        if !self.transport.settled()
            && let Err(error) = self.transport.poll_stop()
        {
            self.owners.restoration(error);
            self.shutdown.begin(now, true);
        }
        self.owners
            .extend_registry(self.registry.take_error_owners_for_shutdown());
        // Finalization cannot race live engine or query ownership. The latter can
        // publish additional original owners, so collect its actual join first.
        if self.shutdown.action(now, false) != RuntimeAction::ForceProcessExit(2)
            && self.engine.is_none()
            && self.query.is_none()
            && self.owners.iter().any(|e| e.view().cleanup_blocked)
        {
            let owners = self.owners.take_for_finalizer();
            match EngineWorker::spawn_finalizer(owners) {
                Ok(worker) => self.engine = Some(worker),
                Err(error) => {
                    self.owners.restore(error.owners);
                    self.owners.coordinator(error.source.source);
                    self.shutdown.begin(now, true);
                }
            }
        }
        let discharged = self.query.is_none()
            && self.engine.is_none()
            && self.transport.settled()
            && !self.owners.unresolved();
        if self.worker_failed {
            self.shutdown.begin(now, true);
        }
        self.shutdown.action(now, discharged)
    }
}
#[cfg(test)]
#[path = "coordinator_tests.rs"]
mod tests;

#[cfg(feature = "mcp-test-support")]
#[path = "fixture.rs"]
pub(crate) mod fixture;
