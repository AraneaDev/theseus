use crate::{ClientError, Failure};
use theseus_host::process::{AcquiredChild, InputMode, NativeCommand, OwnedChild, SpawnFailure};
type NativeSpawn = fn(&NativeCommand, InputMode) -> Result<AcquiredChild, SpawnFailure>;
use theseus_native_helper_store::{PreparedAcquire, StoreBinding};
use theseus_native_protocol::v2;

pub use crate::recovery::{ClientMode, RecoveredOperation, RecoveryImage};
pub struct ExpectedHelper {
    pub profile: v2::HelperProfile,
    pub binding: StoreBinding,
    pub programs: v2::ProgramList,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestId(pub(crate) u64);
pub enum Action<'a> {
    BrokerBind {
        original: v2::broker::BrokerBind,
    },
    QueryBrokerBind {
        original: v2::broker::BrokerBind,
    },
    DesktopAction {
        request: v2::broker::DesktopRequest,
    },
    InspectDesktop {
        lease: v2::LeaseId,
        slot: v2::ResourceSlot,
    },
    QueryDesktop {
        identity: v2::broker::BrokerOperationIdentity,
    },
    CancelDesktop {
        identity: v2::broker::BrokerOperationIdentity,
    },
    ProbeDesktopReadiness {
        identity: v2::broker::BrokerOperationIdentity,
        nonce: v2::BindingId,
        deadline_ms: u32,
    },
    ProbeReadiness {
        deadline_ms: u32,
        lease: v2::LeaseId,
        slot: v2::ResourceSlot,
        operation: v2::OperationId,
        action_digest: v2::Digest,
        prior_binding_digest: v2::Digest,
        nonce: v2::BindingId,
        definition_role: String,
        artifact_role: String,
    },
    ServiceAction {
        lease: v2::LeaseId,
        operation: v2::OperationId,
        slot: v2::ResourceSlot,
        prior_binding_digest: v2::Digest,
        action: v2::ServiceAction,
    },
    InspectService {
        lease: v2::LeaseId,
        slot: v2::ResourceSlot,
    },
    Acquire(&'a PreparedAcquire),
    QueryOriginalAcquire(&'a theseus_native_helper_store::VerifiedOriginalIntent),
    RecoverAcquire(&'a PreparedAcquire),
    Run {
        lease: v2::LeaseId,
        operation: v2::OperationId,
        program: v2::ProgramId,
    },
    /// Explicit opt-in for a known file-capable helper. Persist the complete
    /// typed request and canonical digest before submit for process-crash recovery.
    CreateFile {
        lease: v2::LeaseId,
        operation: v2::OperationId,
        path: v2::LeafName,
        bytes: v2::FileBytes,
    },
    ReadFile {
        lease: v2::LeaseId,
        path: v2::LeafName,
        offset: u64,
        length: u16,
    },
    InspectNativeFile {
        lease: v2::LeaseId,
        path: v2::LeafName,
    },
    InspectFile {
        lease: v2::LeaseId,
        path: v2::LeafName,
    },
    Query {
        lease: v2::LeaseId,
        operation: v2::OperationId,
    },
    Cancel {
        lease: v2::LeaseId,
        operation: v2::OperationId,
    },
    QueryLease(v2::LeaseId),
    Release(v2::LeaseId),
}
#[derive(Debug)]
pub enum Event {
    Ready,
    Reply {
        request: RequestId,
        reply: v2::Reply,
    },
    Output {
        lease: v2::LeaseId,
        operation: v2::OperationId,
        channel: v2::Channel,
        offset: u64,
        bytes: v2::FileBytes,
    },
    Log {
        lease: v2::LeaseId,
        operation: v2::OperationId,
        code: v2::LogCode,
    },
    PeerClosed(v2::CloseCode),
    ConnectionLost,
}
#[derive(Debug)]
pub enum Poll {
    Progress,
    Pending,
    Event(Event),
    TransportSettled,
}
pub struct Client {
    expected: ExpectedHelper,
    settlement: crate::transport::Settlement,
    recovery: crate::recovery::Recovery,
    query_only: bool,
    next_id: Option<u64>,
    error: Option<ClientError>,
    lost: bool,
}
pub struct StartFailure {
    client: Box<Client>,
    error: ClientError,
}
impl Client {
    /// Catalog artifact facts observed during the validated native handshake.
    /// Unavailable before readiness, after transfer to settlement, or for fixtures.
    pub fn integration_description(&self) -> Option<&v2::IntegrationDescription> {
        self.settlement
            .owner
            .as_ref()?
            .pump
            .as_ref()?
            .integration_description
            .as_ref()
    }
    pub fn artifact_commitment(&self) -> Option<v2::Digest> {
        self.settlement
            .owner
            .as_ref()?
            .pump
            .as_ref()?
            .artifact_commitment
    }
    pub fn start(command: NativeCommand, expected: ExpectedHelper) -> Result<Self, StartFailure> {
        Self::start_with(command, expected, ClientMode::Fresh)
    }
    pub fn start_with(
        command: NativeCommand,
        expected: ExpectedHelper,
        mode: ClientMode,
    ) -> Result<Self, StartFailure> {
        let (recovery, query_only, mode_error) =
            match crate::recovery::Recovery::from_mode(mode, &expected) {
                Ok((r, q)) => (r, q, None),
                Err(e) => (crate::recovery::Recovery::default(), true, Some(e)),
            };
        let mut client = Self {
            expected,
            settlement: crate::transport::Settlement::new(crate::transport::TransportOwner::empty()),
            recovery,
            query_only,
            next_id: Some(0),
            error: None,
            lost: false,
        };
        if let Some(error) = mode_error {
            client.settlement.stopped = true;
            return Err(StartFailure {
                client: Box::new(client),
                error,
            });
        }
        if let Err(error) = client.attempt(command, OwnedChild::spawn) {
            return Err(StartFailure {
                client: Box::new(client),
                error,
            });
        }
        Ok(client)
    }
    fn attempt(&mut self, command: NativeCommand, spawn: NativeSpawn) -> Result<(), ClientError> {
        use crate::{
            error::Source,
            pump::Pump,
            transport::{Settlement, TransportOwner},
        };
        use theseus_host::process::ChildEnvironment;
        self.error = None;
        self.lost = false;
        self.settlement = Settlement::new(TransportOwner::empty());
        // Bounds include the explicit native command supplied by the caller.
        let valid = std::path::Path::new(&command.program).is_absolute()
            && command.cwd.as_ref().is_none_or(|p| p.is_absolute())
            && command.args.len() <= 64
            && matches!(&command.environment, ChildEnvironment::Replace(entries) if entries.len() <= 64 && entries.iter().all(|(k,v)| k.len() <= 4096 && v.len() <= 4096))
            && command.program.len() <= 4096
            && command.args.iter().all(|a| a.len() <= 4096);
        if !valid {
            self.settlement.stopped = true;
            self.error = Some(ClientError::InvalidCommand);
            return Err(ClientError::InvalidCommand);
        }
        let mut random = [0; 16];
        if let Err(error) = theseus_host::random_bytes(&mut random) {
            self.settlement
                .owner
                .as_mut()
                .expect("attempt owner")
                .remember(Source::Io(error));
            self.settlement.stopped = true;
            self.error = Some(ClientError::Native);
            return Err(ClientError::Native);
        }
        let pump = Pump::new(v2::SessionId::from_bytes(random)).map_err(|error| {
            self.settlement
                .owner
                .as_mut()
                .expect("attempt owner")
                .remember(Source::Protocol(error));
            ClientError::Protocol
        })?;
        self.settlement.owner.as_mut().expect("attempt owner").pump = Some(pump);
        match spawn(&command, InputMode::Pipe) {
            Ok(native) => {
                self.settlement
                    .owner
                    .as_mut()
                    .expect("attempt owner")
                    .native = Some(native);
                Ok(())
            }
            Err(error) => {
                self.settlement
                    .owner
                    .as_mut()
                    .expect("attempt owner")
                    .remember(Source::Spawn(error));
                self.error = Some(ClientError::Spawn);
                let _ = self.settlement.request_stop();
                Err(ClientError::Spawn)
            }
        }
    }
    pub fn export_recovery(&self) -> Result<RecoveryImage, ClientError> {
        self.recovery.export(&self.expected)
    }
    pub fn submit(&mut self, action: Action<'_>) -> Result<RequestId, ClientError> {
        if self.query_only
            && !matches!(
                &action,
                Action::QueryOriginalAcquire(_)
                    | Action::Query { .. }
                    | Action::Cancel { .. }
                    | Action::QueryBrokerBind { .. }
                    | Action::QueryDesktop { .. }
                    | Action::CancelDesktop { .. }
                    | Action::QueryLease(_)
                    | Action::Release(_)
            )
        {
            return Err(ClientError::RecoveryRequired);
        }
        if self.settlement.stopped {
            return Err(ClientError::Stopping);
        }
        let owner = self
            .settlement
            .owner
            .as_mut()
            .ok_or(ClientError::Stopping)?;
        let pump = owner.pump.as_mut().ok_or(ClientError::NotReady)?;
        if !pump.ready {
            return Err(ClientError::NotReady);
        }
        let cancel = matches!(action, Action::Cancel { .. } | Action::CancelDesktop { .. });
        if !pump.available(cancel) {
            return Err(ClientError::Busy);
        }
        let next = self.next_id.ok_or(ClientError::Exhausted)?;
        self.next_id = next.checked_add(1);
        let request = self.recovery.request(action, &self.expected)?;
        let id = RequestId(next);
        if let Err(error) = pump.submit(id, request) {
            owner.remember(crate::error::Source::Protocol(error));
            self.error = Some(ClientError::Protocol);
            let _ = self.settlement.request_stop();
            return Err(ClientError::Protocol);
        }
        Ok(id)
    }
    pub fn poll(&mut self) -> Result<Poll, ClientError> {
        if self.settlement.stopped {
            if self.settlement.poll()? {
                return Ok(Poll::TransportSettled);
            }
            return Ok(Poll::Pending);
        }
        match self.pump_once() {
            Ok(poll) => Ok(poll),
            Err(error) => {
                self.error = Some(error);
                let _ = self.settlement.request_stop();
                Err(error)
            }
        }
    }
    fn pump_once(&mut self) -> Result<Poll, ClientError> {
        use crate::{error::Source, transport::transient};
        let owner = self
            .settlement
            .owner
            .as_mut()
            .ok_or(ClientError::Stopping)?;
        let native = owner.native.as_mut().ok_or(ClientError::NotReady)?;
        let pump = owner.pump.as_mut().ok_or(ClientError::NotReady)?;
        if pump.timeout() {
            return Err(ClientError::PreflightTimeout);
        }
        let mut progressed = false;
        let mut io_error = None;
        if let Some(frame) = pump.frames.front_mut() {
            match frame.advance(|bytes| {
                native
                    .stdin
                    .as_mut()
                    .ok_or_else(|| std::io::Error::other("missing owned stdin"))?
                    .try_write(bytes)
            }) {
                Ok(done) => {
                    progressed = true;
                    if done {
                        pump.frames.pop_front();
                    }
                }
                Err(error) if transient(&error) => {}
                Err(error) => io_error = Some(error),
            }
        }
        // Independent stderr drainage runs on every active poll, even while a
        // write is pending or stdout contains an undelivered frame.
        let mut scratch = [0; 4096];
        match native.stderr.try_read(&mut scratch) {
            Ok(n) => {
                progressed |= n != 0;
                // Delay returning the bound classification until any original
                // native error from this fair quantum has been retained.
                crate::transport::capture_stderr(
                    &mut owner.stderr,
                    &mut owner.stderr_overflow,
                    &scratch[..n],
                );
            }
            Err(error) if transient(&error) => {}
            Err(error) => {
                if io_error.is_none() {
                    io_error = Some(error);
                }
            }
        }
        if let Some(error) = io_error {
            owner.remember(Source::Io(error));
            return Err(ClientError::Native);
        }
        if owner.stderr_overflow {
            return Err(ClientError::StderrOverflow);
        }
        let mut eof = false;
        if pump.residual.is_empty() {
            match native.stdout.try_read(&mut scratch) {
                Ok(0) => eof = true,
                Ok(n) => {
                    pump.residual.extend_from_slice(&scratch[..n]);
                    progressed = true;
                }
                Err(error) if transient(&error) => {}
                Err(error) => {
                    owner.remember(Source::Io(error));
                    return Err(ClientError::Native);
                }
            }
        }
        let has_residual = !pump.residual.is_empty();
        if has_residual {
            if let Some(event) = self.consume_residual()? {
                return Ok(Poll::Event(event));
            }
            progressed = true;
        }
        if eof {
            return self.connection_lost();
        }
        Ok(if progressed {
            Poll::Progress
        } else {
            Poll::Pending
        })
    }
    // Shared consumer transition after a bounded stdout read. Model tests can
    // supply a bounded residual at this boundary without inventing host faults.
    fn consume_residual(&mut self) -> Result<Option<Event>, ClientError> {
        let owner = self
            .settlement
            .owner
            .as_mut()
            .ok_or(ClientError::Stopping)?;
        let pump = owner.pump.as_mut().ok_or(ClientError::NotReady)?;
        let residual = std::mem::take(&mut pump.residual);
        let (consumed, event) = match pump.decode(&residual, &self.expected) {
            Ok(progress) => progress,
            Err(error) => {
                if let Some(source) = pump.protocol_failure.take() {
                    owner.remember(crate::error::Source::Protocol(source));
                }
                return Err(error);
            }
        };
        pump.residual.extend_from_slice(&residual[consumed..]);
        if let Some(event) = &event {
            match event {
                Event::Reply { reply, .. } => self.recovery.observe(reply)?,
                Event::Output {
                    lease,
                    operation,
                    channel,
                    offset,
                    bytes,
                } => self.recovery.output(
                    *lease,
                    *operation,
                    *channel,
                    *offset,
                    bytes.as_slice().len(),
                )?,
                Event::Log {
                    lease,
                    operation,
                    code,
                } => self.recovery.log(*lease, *operation, *code)?,
                _ => {}
            }
            if matches!(event, Event::PeerClosed(_)) {
                self.lost = true;
                self.settlement.request_stop()?;
            }
        }
        Ok(event)
    }
    fn connection_lost(&mut self) -> Result<Poll, ClientError> {
        let owner = self
            .settlement
            .owner
            .as_mut()
            .ok_or(ClientError::Stopping)?;
        if let Err(error) = owner.pump.as_mut().ok_or(ClientError::NotReady)?.eof() {
            owner.remember(crate::error::Source::Protocol(error));
            return Err(ClientError::Protocol);
        }
        self.lost = true;
        self.settlement.request_stop()?;
        Ok(Poll::Event(Event::ConnectionLost))
    }
    pub fn request_stop(&mut self) {
        let _ = self.settlement.request_stop();
    }
    pub fn poll_settle(&mut self) -> Result<bool, ClientError> {
        self.settlement.poll()
    }
    pub fn reconnect(&mut self, command: NativeCommand) -> Result<(), ClientError> {
        self.reconnect_with(command, OwnedChild::spawn)
    }
    fn reconnect_with(
        &mut self,
        command: NativeCommand,
        spawn: NativeSpawn,
    ) -> Result<(), ClientError> {
        if !self.is_settled() {
            return Err(ClientError::NotSettled);
        }
        if self.recovery.has_desktop() {
            self.query_only = true;
        }
        self.attempt(command, spawn)
    }
    pub fn is_settled(&self) -> bool {
        self.settlement.is_settled()
    }
    pub fn failure(&self) -> Option<&Failure> {
        self.settlement.failure()
    }
}
impl StartFailure {
    pub fn error(&self) -> &ClientError {
        &self.error
    }
    pub fn failure(&self) -> Option<&Failure> {
        self.client.failure()
    }
    pub fn request_stop(&mut self) {
        self.client.request_stop();
    }
    pub fn poll_settle(&mut self) -> Result<bool, ClientError> {
        self.client.poll_settle()
    }
    pub fn is_settled(&self) -> bool {
        self.client.is_settled()
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod reconnect_tests;

#[cfg(test)]
mod wire_review_tests;
