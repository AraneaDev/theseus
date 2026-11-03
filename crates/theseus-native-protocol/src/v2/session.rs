use super::*;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;

#[derive(Default)]
struct Direction {
    next: u64,
    hello: bool,
}

struct RunStream {
    lease: LeaseId,
    operation: OperationId,
    ends: [Option<u64>; 2],
    observed: usize,
    gap: bool,
    logs: u8,
}
impl RunStream {
    fn matches(&self, lease: LeaseId, operation: OperationId) -> bool {
        self.lease == lease && self.operation == operation
    }
    fn evidence(&self) -> bool {
        self.observed != 0 || self.gap || self.logs & 1 != 0
    }
    fn finish(&self, outcome: &Outcome) -> Result<(), Error> {
        match outcome {
            Outcome::RunCompleted {
                stdout_bytes,
                stderr_bytes,
                truncated,
                ..
            } => {
                if self.ends[0].is_some_and(|end| end > *stdout_bytes)
                    || self.ends[1].is_some_and(|end| end > *stderr_bytes)
                    || ((self.gap || self.logs & 1 != 0) && !truncated)
                {
                    return Err(Error::UnexpectedMessage);
                }
            }
            Outcome::LaunchFailed { .. } | Outcome::NotStarted { .. } if !self.evidence() => {}
            _ => return Err(Error::UnexpectedMessage),
        }
        Ok(())
    }
}

/// Pure duplex conversation validation. Readiness supplies no authentication,
/// execution authority, persistence proof, or native cleanup proof.
pub struct SessionGuard {
    session: SessionId,
    local: Role,
    directions: [Direction; 2],
    closed: bool,
    description: Option<Reply>,
    pending: BTreeMap<u64, Request>,
    stream: Option<RunStream>,
    // Every observed non-Missing ID keeps its slot through refresh/settlement.
    // In particular, an active operation reserves space for its terminal view.
    known_operations: Vec<(LeaseId, OperationId)>,
    terminals: Vec<(LeaseId, OperationId, OperationView)>,
}
impl SessionGuard {
    pub fn new(session: SessionId, local: Role) -> Self {
        Self {
            session,
            local,
            directions: [Direction::default(), Direction::default()],
            closed: false,
            description: None,
            pending: BTreeMap::new(),
            stream: None,
            known_operations: Vec::new(),
            terminals: Vec::new(),
        }
    }
    pub fn accept_inbound(&mut self, v: &Envelope) -> Result<(), Error> {
        self.accept(v, 0)
    }
    pub fn accept_outbound(&mut self, v: &Envelope) -> Result<(), Error> {
        self.accept(v, 1)
    }
    pub fn is_established(&self) -> bool {
        !self.closed && self.directions.iter().all(|direction| direction.hello)
    }
    pub fn is_ready(&self) -> bool {
        self.is_established() && self.description.is_some()
    }
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    fn accept(&mut self, v: &Envelope, direction: usize) -> Result<(), Error> {
        if self.closed {
            return Err(Error::Closed);
        }
        let result = self.admit(v, direction);
        if result.is_err() || matches!(v.message(), Message::Closed { .. }) {
            self.closed = true;
            self.pending.clear();
        }
        result
    }
    fn admit(&mut self, v: &Envelope, direction: usize) -> Result<(), Error> {
        if v.session() != &self.session {
            return Err(Error::SessionMismatch);
        }
        if v.sequence() != self.directions[direction].next
            || (v.sequence() == u64::MAX
                && !matches!(
                    v.message(),
                    Message::Closed {
                        code: CloseCode::SequenceExhausted
                    }
                ))
        {
            return Err(Error::UnexpectedSequence);
        }
        let sender = if direction == 1 {
            self.local
        } else {
            opposite(self.local)
        };
        if !self.directions[direction].hello {
            match v.message() {
                Message::Hello { role } if *role == sender => {
                    self.directions[direction].hello = true;
                    self.directions[direction].next = 1;
                    return Ok(());
                }
                _ => return Err(Error::UnexpectedMessage),
            }
        }
        if !self.is_established() {
            return Err(Error::UnexpectedMessage);
        }
        match v.message() {
            Message::Hello { .. } => return Err(Error::UnexpectedMessage),
            Message::Closed { .. } => return Ok(()),
            Message::Request { .. } if sender != Role::Controller => {
                return Err(Error::UnexpectedMessage);
            }
            Message::Response { .. } | Message::Output { .. } | Message::Log { .. }
                if sender != Role::Helper =>
            {
                return Err(Error::UnexpectedMessage);
            }
            _ => {}
        }
        if self.description.is_none() {
            match v.message() {
                Message::Request {
                    request: Request::Describe,
                } if self.pending.is_empty() => {}
                Message::Response { .. } => {}
                _ => return Err(Error::UnexpectedMessage),
            }
        }
        match v.message() {
            Message::Request { request } => {
                if self.pending.len() == MAX_INFLIGHT_REQUESTS {
                    return Err(Error::TooManyRequests);
                }
                self.pending.insert(v.sequence(), request.clone());
            }
            Message::Response { reply_to, result } => {
                let request = self
                    .pending
                    .remove(reply_to)
                    .ok_or(Error::UnexpectedReply)?;
                self.reply(&request, result)?;
            }
            Message::Output {
                lease,
                operation,
                channel,
                offset,
                bytes,
            } => {
                let stream = self
                    .stream
                    .as_mut()
                    .filter(|s| s.matches(*lease, *operation))
                    .ok_or(Error::UnexpectedMessage)?;
                let channel = match channel {
                    Channel::Stdout => 0,
                    Channel::Stderr => 1,
                };
                if let Some(end) = stream.ends[channel] {
                    if *offset < end {
                        return Err(Error::UnexpectedMessage);
                    }
                    if *offset > end {
                        stream.gap = true;
                    }
                }
                let end = offset
                    .checked_add(bytes.as_slice().len() as u64)
                    .ok_or(Error::UnexpectedMessage)?;
                let observed = stream
                    .observed
                    .checked_add(bytes.as_slice().len())
                    .ok_or(Error::LimitExceeded)?;
                if observed > MAX_STREAM_BYTES {
                    return Err(Error::LimitExceeded);
                }
                stream.observed = observed;
                stream.ends[channel] = Some(end);
            }
            Message::Log {
                lease,
                operation,
                code,
            } => {
                let stream = self
                    .stream
                    .as_mut()
                    .filter(|s| s.matches(*lease, *operation))
                    .ok_or(Error::UnexpectedMessage)?;
                let bit = match code {
                    LogCode::OutputTruncated => 1,
                    LogCode::CancelRequested => 2,
                    LogCode::DeadlineExpired => 4,
                    LogCode::CleanupPending => 8,
                };
                if stream.logs & bit != 0 {
                    return Err(Error::UnexpectedMessage);
                }
                stream.logs |= bit;
            }
            _ => unreachable!("hello and close handled above"),
        }
        self.directions[direction].next = v
            .sequence()
            .checked_add(1)
            .ok_or(Error::UnexpectedSequence)?;
        Ok(())
    }
    fn reply(&mut self, request: &Request, result: &Reply) -> Result<(), Error> {
        if matches!(result, Reply::Refused { .. }) {
            return Ok(());
        }
        match (request, result) {
            (Request::Describe, Reply::Description { .. }) => {
                if self.description.as_ref().is_some_and(|old| old != result) {
                    return Err(Error::UnexpectedMessage);
                }
                self.description = Some(result.clone());
            }
            (
                Request::Acquire {
                    intent,
                    binding,
                    manifest_digest,
                }
                | Request::QueryAcquire {
                    intent,
                    binding,
                    manifest_digest,
                },
                Reply::Acquired {
                    intent: ri,
                    binding: rb,
                    manifest_digest: rd,
                    state,
                },
            ) => {
                if intent != ri
                    || binding != rb
                    || manifest_digest != rd
                    || (matches!(request, Request::Acquire { .. })
                        && matches!(state, AcquireView::NotFound))
                {
                    return Err(Error::UnexpectedReply);
                }
            }
            (
                Request::Release { lease } | Request::QueryLease { lease },
                Reply::Lease { lease: rl, .. },
            ) if lease == rl => {}
            (
                Request::ReadFile {
                    lease,
                    path,
                    offset,
                    length,
                },
                Reply::FileBytes {
                    lease: rl,
                    path: rp,
                    offset: ro,
                    bytes,
                    ..
                },
            ) if lease == rl
                && path == rp
                && offset == ro
                && bytes.as_slice().len() <= usize::from(*length) => {}
            (
                Request::InspectFile { lease, path },
                Reply::FileInfo {
                    lease: rl,
                    path: rp,
                    ..
                },
            ) if lease == rl && path == rp => {}
            (
                Request::CreateFile {
                    lease, operation, ..
                }
                | Request::Run {
                    lease, operation, ..
                }
                | Request::QueryOperation { lease, operation }
                | Request::Cancel { lease, operation },
                Reply::Operation {
                    lease: rl,
                    operation: ro,
                    state,
                },
            ) if lease == rl && operation == ro => {
                self.operation(request, *lease, *operation, state)?;
            }
            _ => return Err(Error::UnexpectedReply),
        }
        Ok(())
    }
    fn operation(
        &mut self,
        request: &Request,
        lease: LeaseId,
        operation: OperationId,
        state: &OperationView,
    ) -> Result<(), Error> {
        if let Some((_, _, previous)) = self
            .terminals
            .iter()
            .find(|(l, o, _)| *l == lease && *o == operation)
        {
            if !matches!(
                request,
                Request::QueryOperation { .. } | Request::Cancel { .. }
            ) {
                return Err(Error::UnexpectedReply);
            }
            return if previous == state {
                Ok(())
            } else {
                Err(Error::UnexpectedMessage)
            };
        }
        let new_operation = !matches!(state, OperationView::Missing)
            && !self.known_operations.contains(&(lease, operation));
        if new_operation && self.known_operations.len() == MAX_OPERATIONS {
            return Err(Error::LimitExceeded);
        }
        let expected = match request {
            Request::Run { .. } => Some(OperationKind::Run),
            Request::CreateFile { .. } => Some(OperationKind::CreateFile),
            _ => None,
        };
        match state {
            OperationView::Missing if expected.is_some() => return Err(Error::UnexpectedReply),
            OperationView::Accepted { operation_kind }
                if expected.is_some_and(|e| e != *operation_kind) =>
            {
                return Err(Error::UnexpectedReply);
            }
            OperationView::Running if expected == Some(OperationKind::CreateFile) => {
                return Err(Error::UnexpectedReply);
            }
            OperationView::Finished { outcome } => {
                if !outcome_matches(outcome, expected) {
                    return Err(Error::UnexpectedReply);
                }
                let os = match self.description.as_ref() {
                    Some(Reply::Description { os, .. }) => *os,
                    _ => return Err(Error::UnexpectedMessage),
                };
                super::types::validate_outcome_for_os(outcome, os)?;
                if let (
                    Request::CreateFile { bytes, .. },
                    Outcome::FileCreated { length, digest },
                ) = (request, outcome)
                {
                    let actual = Digest::from_bytes(Sha256::digest(bytes.as_slice()).into());
                    if usize::from(*length) != bytes.as_slice().len() || *digest != actual {
                        return Err(Error::UnexpectedReply);
                    }
                }
            }
            _ => {}
        }
        let matching_stream = self
            .stream
            .as_ref()
            .is_some_and(|s| s.matches(lease, operation));
        if self.stream.is_some()
            && !matching_stream
            && matches!(
                state,
                OperationView::Accepted { .. } | OperationView::Running
            )
        {
            return Err(Error::UnexpectedMessage);
        }
        if matching_stream {
            match state {
                OperationView::Missing
                | OperationView::Accepted {
                    operation_kind: OperationKind::CreateFile,
                } => return Err(Error::UnexpectedMessage),
                OperationView::Finished { outcome } => self
                    .stream
                    .as_ref()
                    .ok_or(Error::UnexpectedMessage)?
                    .finish(outcome)?,
                _ => {}
            }
        }
        if new_operation {
            self.known_operations.push((lease, operation));
        }
        match state {
            OperationView::Running
            | OperationView::Accepted {
                operation_kind: OperationKind::Run,
            } => {
                if !matching_stream {
                    if self.stream.is_some() {
                        return Err(Error::UnexpectedMessage);
                    }
                    let fresh = matches!(request, Request::Run { .. });
                    self.stream = Some(RunStream {
                        lease,
                        operation,
                        ends: if fresh { [Some(0); 2] } else { [None; 2] },
                        observed: 0,
                        gap: false,
                        logs: 0,
                    });
                }
            }
            OperationView::Finished { .. } | OperationView::Unknown { .. } => {
                if matching_stream {
                    self.stream = None;
                }
                self.terminals.push((lease, operation, state.clone()));
            }
            _ => {}
        }
        Ok(())
    }
}
fn opposite(role: Role) -> Role {
    match role {
        Role::Controller => Role::Helper,
        Role::Helper => Role::Controller,
    }
}
fn outcome_matches(outcome: &Outcome, kind: Option<OperationKind>) -> bool {
    match outcome {
        Outcome::FileCreated { .. } | Outcome::FileCreateFailed { .. } => {
            kind != Some(OperationKind::Run)
        }
        Outcome::RunCompleted { .. } | Outcome::LaunchFailed { .. } => {
            kind != Some(OperationKind::CreateFile)
        }
        Outcome::NotStarted { .. } => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn last_ordinary_sequence_advances_to_max_without_wrapping() {
        for direction in 0..2 {
            let local = if direction == 1 {
                Role::Controller
            } else {
                Role::Helper
            };
            let mut guard = SessionGuard::new(SessionId::from_bytes([0; 16]), local);
            guard.directions = [
                Direction {
                    next: u64::MAX - 1,
                    hello: true,
                },
                Direction {
                    next: u64::MAX - 1,
                    hello: true,
                },
            ];
            // A successful preflight is represented only inside this private test.
            guard.description = Some(Reply::Description {
                binding: BindingId::from_bytes([0; 16]),
                manifest_digest: Digest::from_bytes([0; 32]),
                workspace_marker: WorkspaceMarker::from_bytes([0; 16]),
                os: Os::Linux,
                helper_principal: Principal::Posix {
                    effective_uid: 0,
                    effective_gid: 0,
                },
                programs: ProgramList::new(vec![ProgramId::new("probe").unwrap()]).unwrap(),
            });
            let request = Envelope::new(
                SessionId::from_bytes([0; 16]),
                u64::MAX - 1,
                Message::Request {
                    request: Request::Describe,
                },
            )
            .unwrap();
            guard.accept(&request, direction).unwrap();
            assert_eq!(guard.directions[direction].next, u64::MAX);
            let close = Envelope::new(
                SessionId::from_bytes([0; 16]),
                u64::MAX,
                Message::Closed {
                    code: CloseCode::SequenceExhausted,
                },
            )
            .unwrap();
            guard.accept(&close, direction).unwrap();
            assert_eq!(guard.directions[direction].next, u64::MAX);
            assert!(guard.is_closed());
            assert!(guard.pending.is_empty());
        }
    }
    #[test]
    fn sequence_max_is_reserved_for_terminal_exhaustion_in_both_directions() {
        for local in [Role::Controller, Role::Helper] {
            for direction in 0..2 {
                for code in [
                    Some(CloseCode::SequenceExhausted),
                    Some(CloseCode::Normal),
                    None,
                ] {
                    let session = SessionId::from_bytes([0; 16]);
                    let mut guard = SessionGuard::new(session.clone(), local);
                    guard.directions = [
                        Direction {
                            next: u64::MAX,
                            hello: true,
                        },
                        Direction {
                            next: u64::MAX,
                            hello: true,
                        },
                    ];
                    let message = code.map_or(Message::Hello { role: local }, |code| {
                        Message::Closed { code }
                    });
                    let envelope = Envelope::new(session.clone(), u64::MAX, message).unwrap();
                    assert_eq!(
                        guard.accept(&envelope, direction),
                        if code == Some(CloseCode::SequenceExhausted) {
                            Ok(())
                        } else {
                            Err(Error::UnexpectedSequence)
                        }
                    );
                    assert!(guard.is_closed());
                    let wrong = Envelope::new(
                        SessionId::from_bytes([1; 16]),
                        0,
                        Message::Hello { role: local },
                    )
                    .unwrap();
                    assert_eq!(guard.accept(&wrong, direction), Err(Error::Closed));
                }
            }
        }
    }
}
