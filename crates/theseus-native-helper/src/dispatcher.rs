//! Single dispatcher owner: protocol, cached queries, permits and FIFO admission.
use crate::{
    RuntimeError,
    cache::{Cache, refused},
    contract::*,
    frames::{Frame, FrameOwner},
    pump::BoundaryTracker,
};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{TryRecvError, TrySendError},
    },
};
use theseus_native_protocol::v2;
struct PendingResponse {
    ticket: ResponseTicket,
    reply: v2::Reply,
    start_key: Option<RunKey>,
}
struct Active {
    key: RunKey,
    request_sequence: u64,
    grant: StartGrant,
    cancel: Arc<AtomicBool>,
    accepted: bool,
    requires_accepted: bool,
    terminal_admitted: bool,
}
pub(crate) struct Dispatcher {
    #[cfg(feature = "test-support")]
    pub(crate) fixture: Option<crate::test_support::FixtureControl>,
    pub(crate) ports: DispatcherPorts,
    pub(crate) cache: Cache,
    decoder: v2::Decoder,
    guard: Option<v2::SessionGuard>,
    session: Option<v2::SessionId>,
    next_sequence: u64,
    generation: u64,
    controls: Vec<ControlPermit>,
    responses: VecDeque<PendingResponse>,
    pub(crate) frames: VecDeque<Frame>,
    pub(crate) current: Option<Frame>,
    returned: Option<DataReturn>,
    barrier: Option<BoundaryTracker>,
    active: Option<Active>,
    input: Box<[u8; 4096]>,
    input_start: usize,
    input_end: usize,
    mutation_pending: bool,
    pub(crate) stopping: bool,
    pub(crate) communication_incomplete: bool,
}
impl Dispatcher {
    pub(crate) fn new(ports: DispatcherPorts, controls: [ControlPermit; 2]) -> Self {
        Self {
            #[cfg(feature = "test-support")]
            fixture: None,
            ports,
            cache: Cache::new(),
            decoder: v2::Decoder::new(),
            guard: None,
            session: None,
            next_sequence: 0,
            generation: 0,
            controls: Vec::from(controls),
            responses: VecDeque::with_capacity(2),
            frames: VecDeque::with_capacity(6),
            current: None,
            returned: None,
            barrier: None,
            active: None,
            input: Box::new([0; 4096]),
            input_start: 0,
            input_end: 0,
            mutation_pending: false,
            stopping: false,
            communication_incomplete: false,
        }
    }
    pub(crate) fn stop(&mut self) {
        if !self.stopping {
            let _ = self.ports.commands.try_send(WorkerCommand::Stop);
        }
        self.stopping = true;
        self.ports.stop.store(true, Ordering::Release);
        if let Some(active) = &mut self.active {
            active.cancel.store(true, Ordering::Release);
            active.grant.abort();
        }
    }
    pub(crate) fn deadline(&mut self) {
        if self.cache.live.as_ref().is_some_and(|l| {
            l.terminal.is_none() && l.deadline.is_some_and(|d| std::time::Instant::now() >= d)
        }) {
            self.stop()
        }
    }
    pub(crate) fn input_buffer(&mut self) -> Option<&mut [u8]> {
        (self.input_start == self.input_end && !self.stopping).then_some(&mut self.input[..])
    }
    pub(crate) fn read_bytes(&mut self, n: usize) {
        self.input_start = 0;
        self.input_end = n;
    }
    pub(crate) fn eof(&mut self) -> Result<(), RuntimeError> {
        self.communication_incomplete = true;
        self.stop();
        self.decoder.finish().map_err(RuntimeError::Protocol)
    }
    pub(crate) fn input_quantum(&mut self) -> Result<bool, RuntimeError> {
        if self.stopping || self.input_start == self.input_end {
            return Ok(false);
        }
        let progress = self
            .decoder
            .feed(&self.input[self.input_start..self.input_end])
            .map_err(RuntimeError::Protocol)?;
        self.input_start += progress.consumed;
        let Some(frame) = progress.frame else {
            return Ok(progress.consumed != 0);
        };
        if self.guard.is_none() {
            if !matches!(
                frame.message(),
                v2::Message::Hello {
                    role: v2::Role::Controller
                }
            ) {
                return Err(RuntimeError::InvalidState);
            }
            self.session = Some(frame.session().clone());
            self.guard = Some(v2::SessionGuard::new(
                frame.session().clone(),
                v2::Role::Helper,
            ));
        }
        self.guard
            .as_mut()
            .ok_or(RuntimeError::InvalidState)?
            .accept_inbound(&frame)
            .map_err(RuntimeError::Protocol)?;
        match frame.message() {
            v2::Message::Hello { .. } => {
                let permit = self.controls.pop().ok_or(RuntimeError::Capacity)?;
                self.admit(
                    v2::Message::Hello {
                        role: v2::Role::Helper,
                    },
                    FrameOwner::Control(permit),
                )?;
            }
            v2::Message::Request { request } => self.request(frame.sequence(), request.clone())?,
            v2::Message::Closed { .. } => {
                self.stop();
            }
            _ => return Err(RuntimeError::InvalidState),
        }
        Ok(true)
    }
    fn response(
        &mut self,
        ticket: ResponseTicket,
        reply: v2::Reply,
        start_key: Option<RunKey>,
    ) -> Result<(), RuntimeError> {
        if self.responses.len() == 2 {
            return Err(RuntimeError::Capacity);
        }
        self.responses.push_back(PendingResponse {
            ticket,
            reply,
            start_key,
        });
        Ok(())
    }
    fn request(&mut self, sequence: u64, request: v2::Request) -> Result<(), RuntimeError> {
        let Some(permit) = self.controls.pop() else {
            self.stop();
            return Err(RuntimeError::Capacity);
        };
        let ticket = ResponseTicket {
            reply_to: sequence,
            permit,
        };
        if let v2::Request::Cancel { lease, operation } = &request
            && let Some(active) = self
                .active
                .as_ref()
                .filter(|a| a.key.lease == *lease && a.key.operation == *operation)
        {
            active.cancel.store(true, Ordering::Release);
            #[cfg(feature = "test-support")]
            if let Some(control) = &self.fixture {
                control.mark_cancel_requested();
            }
        }
        // QueryAcquire during a reservation barrier cannot claim NotFound.
        if self.mutation_pending && matches!(request, v2::Request::QueryAcquire { .. }) {
            return self.response(ticket, refused(v2::RefusalCode::OwnershipPending), None);
        }
        if let Some(reply) = self.cache.query(&request) {
            return self.response(ticket, reply, None);
        }
        if self.cache.poisoned {
            return self.response(ticket, refused(v2::RefusalCode::LedgerUnavailable), None);
        }
        if self.mutation_pending
            || self.barrier.as_ref().is_some_and(|b| !b.retired)
            || self.active.as_ref().is_some_and(|a| !a.terminal_admitted)
        {
            return self.response(ticket, refused(v2::RefusalCode::Busy), None);
        }
        if let Some(code) = self.cache.replay_refusal(&request) {
            return self.response(ticket, refused(code), None);
        }
        let command = match &request {
            v2::Request::Acquire { .. } => WorkerCommand::Acquire { request, ticket },
            v2::Request::Release { lease } => WorkerCommand::Release {
                lease: *lease,
                ticket,
            },
            v2::Request::Run {
                lease, operation, ..
            } => {
                if self.cache.live.as_ref().is_some_and(|l| {
                    l.key.lease == *lease && l.key.operation == *operation && l.terminal.is_some()
                }) {
                    return self.response(ticket, refused(v2::RefusalCode::InvalidRequest), None);
                }
                self.generation = self
                    .generation
                    .checked_add(1)
                    .ok_or(RuntimeError::Capacity)?;
                let key = RunKey {
                    lease: *lease,
                    operation: *operation,
                    generation: self.generation,
                };
                let flag = Arc::new(AtomicBool::new(false));
                let (grant, start) = start_pair(key, sequence);
                self.cache.begin(key)?;
                self.barrier = Some(BoundaryTracker::new(key));
                self.active = Some(Active {
                    key,
                    request_sequence: sequence,
                    grant,
                    cancel: flag.clone(),
                    accepted: false,
                    requires_accepted: false,
                    terminal_admitted: false,
                });
                WorkerCommand::Run {
                    request,
                    ticket,
                    cancel: CancelToken { key, flag },
                    start,
                }
            }
            _ => return self.response(ticket, refused(v2::RefusalCode::InvalidRequest), None),
        };
        match self.ports.commands.try_send(command) {
            Ok(()) => {
                self.mutation_pending = true;
                Ok(())
            }
            Err(error) => {
                let command = match error {
                    TrySendError::Full(command) => command,
                    TrySendError::Disconnected(command) => {
                        self.stop();
                        command
                    }
                };
                let ticket = match command {
                    WorkerCommand::Acquire { ticket, .. }
                    | WorkerCommand::Release { ticket, .. } => ticket,
                    WorkerCommand::Run { ticket, .. } => {
                        self.active = None;
                        self.barrier = None;
                        self.cache.live = None;
                        ticket
                    }
                    _ => return Err(RuntimeError::InvalidState),
                };
                self.response(ticket, refused(v2::RefusalCode::Busy), None)
            }
        }
    }
    fn admit(&mut self, message: v2::Message, owner: FrameOwner) -> Result<u64, RuntimeError> {
        if self.frames.len() + usize::from(self.current.is_some()) >= 6 {
            return Err(RuntimeError::Capacity);
        }
        let sequence = self.next_sequence;
        let envelope = v2::Envelope::new(
            self.session.clone().ok_or(RuntimeError::InvalidState)?,
            sequence,
            message,
        )
        .map_err(RuntimeError::Protocol)?;
        #[cfg(not(feature = "test-support"))]
        let frame = Frame::encode(&envelope, owner).map_err(RuntimeError::Protocol)?;
        #[cfg(feature = "test-support")]
        let frame = Frame::encode_with_fixture(&envelope, owner, self.fixture.as_ref())
            .map_err(RuntimeError::Protocol)?;
        self.guard
            .as_mut()
            .ok_or(RuntimeError::InvalidState)?
            .accept_outbound(&envelope)
            .map_err(RuntimeError::Protocol)?;
        self.frames.push_back(frame);
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(RuntimeError::Capacity)?;
        Ok(sequence)
    }
    pub(crate) fn event_quantum(&mut self) -> Result<bool, RuntimeError> {
        let event = match self.ports.events.try_recv() {
            Ok(e) => e,
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => return Ok(false),
        };
        match event {
            WorkerEvent::CacheUpdate(patch) => {
                if let CachePatch::Phase {
                    key,
                    phase: LivePhase::Accepted,
                    ..
                } = &patch
                {
                    let active = self
                        .active
                        .as_mut()
                        .filter(|a| a.key == *key)
                        .ok_or(RuntimeError::InvalidState)?;
                    active.requires_accepted = true;
                }
                if let CachePatch::Terminal { boundary, .. } = &patch {
                    self.barrier
                        .as_mut()
                        .ok_or(RuntimeError::InvalidState)?
                        .freeze(*boundary)?;
                }
                self.cache.apply(patch)?;
            }
            WorkerEvent::ResponseReady {
                ticket,
                reply,
                start_key,
            } => {
                if matches!(reply, v2::Reply::Refused { .. })
                    && self
                        .active
                        .as_ref()
                        .is_some_and(|a| a.request_sequence == ticket.reply_to && !a.accepted)
                {
                    self.active = None;
                    self.barrier = None;
                    self.cache.live = None;
                }
                self.mutation_pending = false;
                self.response(ticket, reply, start_key)?;
            }
        }
        Ok(true)
    }
    fn terminal_ready(&self, reply: &v2::Reply) -> bool {
        if let v2::Reply::Operation {
            lease,
            operation,
            state: v2::OperationView::Finished { .. } | v2::OperationView::Unknown { .. },
        } = reply
            && let Some(b) = self
                .barrier
                .as_ref()
                .filter(|b| b.key.lease == *lease && b.key.operation == *operation)
        {
            // A new Accepted generation may finish physically before its retained
            // original response arrives. Do not let a cached terminal resurrect
            // that response in SessionGuard. Durable replay has no new grant.
            return b.wire_ready()
                && self
                    .active
                    .as_ref()
                    .is_some_and(|a| a.key == b.key && (!a.requires_accepted || a.accepted));
        }
        true
    }
    pub(crate) fn response_quantum(&mut self) -> Result<bool, RuntimeError> {
        if self.stopping {
            return Ok(false);
        }
        let Some(index) = self
            .responses
            .iter()
            .position(|r| self.terminal_ready(&r.reply))
        else {
            return Ok(false);
        };
        let pending = self
            .responses
            .remove(index)
            .ok_or(RuntimeError::InvalidState)?;
        let accepted = matches!(
            &pending.reply,
            v2::Reply::Operation {
                state: v2::OperationView::Accepted {
                    operation_kind: v2::OperationKind::Run
                },
                ..
            }
        );
        if let Some(key) = pending.start_key {
            let active = self
                .active
                .as_ref()
                .filter(|a| a.key == key && a.request_sequence == pending.ticket.reply_to)
                .ok_or(RuntimeError::InvalidGrant)?;
            if !accepted || active.accepted {
                return Err(RuntimeError::InvalidGrant);
            }
            if !matches!(&pending.reply,v2::Reply::Operation{lease,operation,..} if *lease==key.lease&&*operation==key.operation)
            {
                return Err(RuntimeError::InvalidGrant);
            }
        } else if accepted
            && self
                .active
                .as_ref()
                .is_some_and(|a| a.request_sequence == pending.ticket.reply_to && !a.accepted)
        {
            return Err(RuntimeError::InvalidGrant);
        }
        let reply_to = pending.ticket.reply_to;
        let terminal_key = match &pending.reply {
            v2::Reply::Operation {
                lease,
                operation,
                state: v2::OperationView::Finished { .. } | v2::OperationView::Unknown { .. },
            } => Some((*lease, *operation)),
            _ => None,
        };
        let seq = self.admit(
            v2::Message::Response {
                reply_to,
                result: pending.reply,
            },
            FrameOwner::Control(pending.ticket.permit),
        )?;
        if let Some((lease, operation)) = terminal_key {
            self.cache.note_wire_terminal(lease, operation)?;
        }
        if let Some((lease, operation)) = terminal_key
            && let Some(active) = self
                .active
                .as_mut()
                .filter(|a| a.key.lease == lease && a.key.operation == operation)
        {
            active.terminal_admitted = true;
        }
        if let Some(key) = pending.start_key {
            let active = self.active.as_mut().ok_or(RuntimeError::InvalidGrant)?;
            active.grant.grant(AcceptedAdmission {
                key,
                reply_to,
                outbound_sequence: seq,
            })?;
            active.accepted = true;
        }
        Ok(true)
    }
    pub(crate) fn data_quantum(&mut self) -> Result<bool, RuntimeError> {
        if self.stopping || self.returned.is_some() {
            return Ok(false);
        }
        let chunk = match self.ports.data.try_recv() {
            Ok(c) => c,
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => return Ok(false),
        };
        if !self
            .active
            .as_ref()
            .is_some_and(|a| a.key == chunk.key && a.accepted)
            || chunk.len == 0
            || chunk.len > 4096
        {
            return Err(RuntimeError::InvalidState);
        }
        let message = v2::Message::Output {
            lease: chunk.key.lease,
            operation: chunk.key.operation,
            channel: chunk.channel,
            offset: chunk.offset,
            bytes: v2::FileBytes::new(&chunk.permit.storage[..usize::from(chunk.len)])
                .map_err(RuntimeError::Protocol)?,
        };
        self.admit(message, FrameOwner::Data(chunk))?;
        Ok(true)
    }
    pub(crate) fn can_select_frame(&self) -> bool {
        self.returned.is_none()
    }
    pub(crate) fn frame_done(&mut self) -> Result<(), RuntimeError> {
        let frame = self.current.take().ok_or(RuntimeError::InvalidState)?;
        match frame.owner {
            FrameOwner::Control(permit) => {
                if permit.id() > 1 || self.controls.iter().any(|p| p.id() == permit.id()) {
                    return Err(RuntimeError::InvalidState);
                }
                self.controls.push(permit);
                #[cfg(feature = "test-support")]
                if let Some(control) = &self.fixture {
                    control.acknowledge_control_write();
                }
            }
            FrameOwner::Data(chunk) => {
                if self.returned.is_some() {
                    return Err(RuntimeError::InvalidState);
                }
                self.barrier
                    .as_mut()
                    .ok_or(RuntimeError::InvalidState)?
                    .acknowledge(chunk.key, chunk.ordinal)?;
                self.returned = Some(DataReturn::Reusable {
                    key: chunk.key,
                    ordinal: chunk.ordinal,
                    disposition: ReturnDisposition::TransportAcknowledged,
                    permit: chunk.permit,
                });
            }
        }
        Ok(())
    }
    pub(crate) fn stopping_quantum(&mut self) -> bool {
        if let Some(item) = self.returned.take() {
            match self.ports.returned.try_send(item) {
                Ok(()) => return true,
                Err(TrySendError::Full(item) | TrySendError::Disconnected(item)) => {
                    self.returned = Some(item);
                    return false;
                }
            }
        }
        if let Some(frame) = self.frames.pop_front() {
            match frame.owner {
                FrameOwner::Control(permit) => self.controls.push(permit),
                FrameOwner::Data(chunk) => {
                    self.returned = Some(DataReturn::Reusable {
                        key: chunk.key,
                        ordinal: chunk.ordinal,
                        disposition: ReturnDisposition::AbandonedBeforeSubmit,
                        permit: chunk.permit,
                    })
                }
            }
            self.communication_incomplete = true;
            return true;
        }
        false
    }
    pub(crate) fn return_quantum(&mut self) -> Result<bool, RuntimeError> {
        if self.stopping {
            return Ok(false);
        }
        if let Some(item) = self.returned.take() {
            return match self.ports.returned.try_send(item) {
                Ok(()) => Ok(true),
                Err(TrySendError::Full(item)) => {
                    self.returned = Some(item);
                    Ok(false)
                }
                Err(TrySendError::Disconnected(item)) => {
                    self.returned = Some(item);
                    self.stop();
                    Err(RuntimeError::Worker)
                }
            };
        }
        if let Some((key, final_ordinal)) =
            self.barrier.as_ref().and_then(BoundaryTracker::drain_ack)
        {
            match self
                .ports
                .returned
                .try_send(DataReturn::ProducerDrained { key, final_ordinal })
            {
                Ok(()) => {
                    self.barrier
                        .as_mut()
                        .ok_or(RuntimeError::InvalidState)?
                        .retired = true;
                    return Ok(true);
                }
                Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => {
                    self.stop();
                    return Err(RuntimeError::Worker);
                }
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn incoming(d: &mut Dispatcher, sequence: u64, message: v2::Message) {
        let bytes = v2::encode(
            &v2::Envelope::new(v2::SessionId::from_bytes([7; 16]), sequence, message).unwrap(),
        )
        .unwrap();
        assert!(bytes.len() <= 4096);
        d.input[..bytes.len()].copy_from_slice(&bytes);
        d.read_bytes(bytes.len());
        d.input_quantum().unwrap();
    }
    fn flush(d: &mut Dispatcher) {
        if d.current.is_none() {
            d.current = d.frames.pop_front()
        }
        while let Some(frame) = &mut d.current {
            let n = frame.write_slice().len();
            if frame.acknowledged(n).unwrap() {
                d.frame_done().unwrap();
                break;
            }
        }
    }
    fn ready() -> (Dispatcher, WorkerPorts, [DataPermit; 4]) {
        let (ports, worker, controls, data) = channels();
        let mut d = Dispatcher::new(ports, controls);
        incoming(
            &mut d,
            0,
            v2::Message::Hello {
                role: v2::Role::Controller,
            },
        );
        flush(&mut d);
        worker
            .events
            .try_send(WorkerEvent::CacheUpdate(CachePatch::Initialized {
                description: v2::Reply::Description {
                    binding: v2::BindingId::from_bytes([1; 16]),
                    manifest_digest: v2::Digest::from_bytes([2; 32]),
                    workspace_marker: v2::WorkspaceMarker::from_bytes([3; 16]),
                    os: v2::Os::Linux,
                    helper_principal: v2::Principal::Posix {
                        effective_uid: 1,
                        effective_gid: 1,
                    },
                    programs: v2::ProgramList::new(vec![v2::ProgramId::new("probe").unwrap()])
                        .unwrap(),
                },
                lease: Some((
                    v2::IntentId::from_bytes([4; 16]),
                    v2::LeaseId::from_bytes([5; 16]),
                    v2::LeaseView::Held {
                        intent: v2::IntentId::from_bytes([4; 16]),
                    },
                )),
                operations: vec![],
            }))
            .ok()
            .unwrap();
        d.event_quantum().unwrap();
        incoming(
            &mut d,
            1,
            v2::Message::Request {
                request: v2::Request::Describe,
            },
        );
        d.response_quantum().unwrap();
        flush(&mut d);
        (d, worker, data)
    }
    fn run(d: &mut Dispatcher, w: &WorkerPorts) -> (RunKey, StartWatch) {
        incoming(
            d,
            2,
            v2::Message::Request {
                request: v2::Request::Run {
                    lease: v2::LeaseId::from_bytes([5; 16]),
                    operation: v2::OperationId::from_bytes([6; 16]),
                    program: v2::ProgramId::new("probe").unwrap(),
                },
            },
        );
        let WorkerCommand::Run {
            ticket,
            cancel,
            start,
            ..
        } = w.commands.try_recv().ok().unwrap()
        else {
            panic!("expected run")
        };
        let key = cancel.key;
        let now = std::time::Instant::now();
        w.events
            .try_send(WorkerEvent::CacheUpdate(CachePatch::Phase {
                key,
                kind: v2::OperationKind::Run,
                phase: LivePhase::Accepted,
                accepted_at: Some(now),
                deadline: Some(now + std::time::Duration::from_secs(30)),
            }))
            .ok()
            .unwrap();
        d.event_quantum().unwrap();
        w.events
            .try_send(WorkerEvent::ResponseReady {
                ticket,
                reply: v2::Reply::Operation {
                    lease: key.lease,
                    operation: key.operation,
                    state: v2::OperationView::Accepted {
                        operation_kind: v2::OperationKind::Run,
                    },
                },
                start_key: Some(key),
            })
            .ok()
            .unwrap();
        d.event_quantum().unwrap();
        (key, start)
    }
    #[test]
    fn fresh_closed_history_terminal_replay_has_no_new_grant_requirement() {
        let (mut d, w, _) = ready();
        let lease = v2::LeaseId::from_bytes([5; 16]);
        let operation = v2::OperationId::from_bytes([6; 16]);
        d.cache.lease.as_mut().unwrap().2 = v2::LeaseView::AwaitingExternalReset {
            intent: v2::IntentId::from_bytes([4; 16]),
        };
        incoming(
            &mut d,
            2,
            v2::Message::Request {
                request: v2::Request::Run {
                    lease,
                    operation,
                    program: v2::ProgramId::new("probe").unwrap(),
                },
            },
        );
        let WorkerCommand::Run {
            ticket,
            cancel,
            start,
            ..
        } = w.commands.try_recv().ok().unwrap()
        else {
            panic!("run")
        };
        let key = cancel.key;
        let view = v2::OperationView::Finished {
            outcome: v2::Outcome::NotStarted {
                reason: v2::NotStartedReason::CancelRequested,
            },
        };
        w.events
            .try_send(WorkerEvent::CacheUpdate(CachePatch::Terminal {
                key,
                view: view.clone(),
                boundary: FinalBoundary {
                    key,
                    final_ordinal: 0,
                    stdout_bytes: 0,
                    stderr_bytes: 0,
                    truncated: false,
                },
            }))
            .ok()
            .unwrap();
        d.event_quantum().unwrap();
        w.events
            .try_send(WorkerEvent::ResponseReady {
                ticket,
                reply: v2::Reply::Operation {
                    lease,
                    operation,
                    state: view,
                },
                start_key: None,
            })
            .ok()
            .unwrap();
        d.event_quantum().unwrap();
        assert!(d.response_quantum().unwrap());
        assert!(matches!(start.poll(), StartState::Waiting));
        flush(&mut d);
        d.return_quantum().unwrap();
        incoming(
            &mut d,
            3,
            v2::Message::Request {
                request: v2::Request::Run {
                    lease,
                    operation,
                    program: v2::ProgramId::new("probe").unwrap(),
                },
            },
        );
        assert!(
            w.commands.try_recv().is_err(),
            "same-session terminal mutation cannot reach worker"
        );
        assert!(d.response_quantum().unwrap());
    }
    #[test]
    fn grant_follows_accepted_fifo_admission_and_never_requires_stdout_drain() {
        let (mut d, w, _) = ready();
        let (_, start) = run(&mut d, &w);
        assert!(matches!(start.poll(), StartState::Waiting));
        assert!(d.frames.is_empty());
        d.response_quantum().unwrap();
        assert_eq!(d.frames.len(), 1);
        assert!(matches!(start.poll(), StartState::Granted));
        assert!(d.current.is_none()); // no transport submission was needed for the admission proof
    }
    #[test]
    fn durable_terminal_before_last_raw_chunk_still_waits_exact_ack() {
        let (mut d, w, [mut token, _, _, _]) = ready();
        let (key, start) = run(&mut d, &w);
        d.response_quantum().unwrap();
        assert!(matches!(start.poll(), StartState::Granted));
        flush(&mut d);
        let boundary = FinalBoundary {
            key,
            final_ordinal: 1,
            stdout_bytes: 1,
            stderr_bytes: 0,
            truncated: false,
        };
        let view = v2::OperationView::Finished {
            outcome: v2::Outcome::RunCompleted {
                reason: v2::RunReason::Natural,
                status: v2::NativeStatus::PosixExit { code: 0 },
                cleanup: v2::Cleanup::PosixLeaderReapedGroupTerminationRequested,
                stdout_bytes: 1,
                stderr_bytes: 0,
                truncated: false,
            },
        };
        w.events
            .try_send(WorkerEvent::CacheUpdate(CachePatch::Terminal {
                key,
                view: view.clone(),
                boundary,
            }))
            .ok()
            .unwrap();
        d.event_quantum().unwrap();
        assert_eq!(
            d.cache.operation(key.lease, key.operation),
            v2::Reply::Operation {
                lease: key.lease,
                operation: key.operation,
                state: view
            }
        );
        incoming(
            &mut d,
            3,
            v2::Message::Request {
                request: v2::Request::QueryOperation {
                    lease: key.lease,
                    operation: key.operation,
                },
            },
        );
        assert!(!d.response_quantum().unwrap());
        assert!(!d.return_quantum().unwrap());
        token.storage[0] = b'x';
        w.data
            .try_send(DataChunk {
                key,
                ordinal: 1,
                channel: v2::Channel::Stdout,
                offset: 0,
                len: 1,
                permit: token,
            })
            .ok()
            .unwrap();
        d.data_quantum().unwrap();
        assert!(!d.response_quantum().unwrap());
        flush(&mut d);
        assert!(d.response_quantum().unwrap()); // only now may Finished enter FIFO
        d.return_quantum().unwrap();
        assert!(matches!(
            w.returned.try_recv(),
            Ok(DataReturn::Reusable { ordinal: 1, .. })
        ));
        d.return_quantum().unwrap();
        assert!(matches!(
            w.returned.try_recv(),
            Ok(DataReturn::ProducerDrained {
                final_ordinal: 1,
                ..
            })
        ));
    }
    #[test]
    fn cancel_remains_cached_with_all_data_tokens_and_a_pending_frame() {
        let (mut d, w, tokens) = ready();
        let (key, start) = run(&mut d, &w);
        d.response_quantum().unwrap();
        assert!(matches!(start.poll(), StartState::Granted));
        d.current = d.frames.pop_front();
        let first = d.current.as_mut().unwrap().write_slice();
        let pointer = first.as_ptr();
        let bytes = first.to_vec();
        for (i, mut permit) in tokens.into_iter().enumerate() {
            permit.storage[0] = b'x';
            w.data
                .try_send(DataChunk {
                    key,
                    ordinal: i as u32 + 1,
                    channel: v2::Channel::Stdout,
                    offset: i as u64,
                    len: 1,
                    permit,
                })
                .ok()
                .unwrap();
            d.data_quantum().unwrap();
        }
        incoming(
            &mut d,
            3,
            v2::Message::Request {
                request: v2::Request::Cancel {
                    lease: key.lease,
                    operation: key.operation,
                },
            },
        );
        assert!(d.active.as_ref().unwrap().cancel.load(Ordering::Acquire));
        d.response_quantum().unwrap();
        assert_eq!(d.frames.len(), 5);
        let retry = d.current.as_mut().unwrap().write_slice();
        assert_eq!(retry.as_ptr(), pointer);
        assert_eq!(retry, bytes);
        assert!(!d.stopping);
    }

    #[test]
    fn exhausted_control_capacity_stops_without_response_backlog() {
        let (mut d, _, _) = ready();
        incoming(
            &mut d,
            2,
            v2::Message::Request {
                request: v2::Request::QueryLease {
                    lease: v2::LeaseId::from_bytes([5; 16]),
                },
            },
        );
        incoming(
            &mut d,
            3,
            v2::Message::Request {
                request: v2::Request::QueryLease {
                    lease: v2::LeaseId::from_bytes([5; 16]),
                },
            },
        );
        assert_eq!(d.responses.len(), 2);
        assert!(d.controls.is_empty());
        let bytes = v2::encode(
            &v2::Envelope::new(
                v2::SessionId::from_bytes([7; 16]),
                4,
                v2::Message::Request {
                    request: v2::Request::QueryLease {
                        lease: v2::LeaseId::from_bytes([5; 16]),
                    },
                },
            )
            .unwrap(),
        )
        .unwrap();
        d.input[..bytes.len()].copy_from_slice(&bytes);
        d.read_bytes(bytes.len());
        assert!(matches!(d.input_quantum(), Err(RuntimeError::Capacity)));
        assert!(d.stopping);
        assert_eq!(d.responses.len(), 2);
    }
}
