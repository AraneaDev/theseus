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
mod tests {
    use super::*;
    use crate::transport::{Settlement, TransportOwner};
    fn offline(next: Option<u64>) -> Client {
        let mut owner = TransportOwner::empty();
        owner.pump = Some(crate::pump::tests::ready());
        Client {
            expected: crate::pump::tests::expected(),
            settlement: Settlement::new(owner),
            recovery: crate::recovery::Recovery::default(),
            query_only: false,
            next_id: next,
            error: None,
            lost: false,
        }
    }
    #[test]
    fn maximum_public_id_is_issued_once_then_refused_before_queueing() {
        let mut client = offline(Some(u64::MAX));
        let lease = v2::LeaseId::from_bytes([4; 16]);
        assert_eq!(
            client.submit(Action::QueryLease(lease)).unwrap(),
            RequestId(u64::MAX)
        );
        // Release the ordinary slot by installing a new wire epoch. This cannot
        // replenish the independent public counter.
        client.settlement.owner.as_mut().unwrap().pump = Some(crate::pump::tests::ready());
        assert_eq!(
            client.submit(Action::QueryLease(lease)),
            Err(ClientError::Exhausted)
        );
        assert!(
            client
                .settlement
                .owner
                .as_ref()
                .unwrap()
                .pump
                .as_ref()
                .unwrap()
                .frames
                .is_empty()
        );
    }
    #[test]
    fn failed_reconnect_does_not_reset_public_namespace() {
        let mut client = offline(Some(17));
        let lease = v2::LeaseId::from_bytes([4; 16]);
        let old = client.submit(Action::QueryLease(lease)).unwrap();
        client.request_stop();
        assert!(client.poll_settle().unwrap());
        let mut command = NativeCommand::new("relative-program");
        command.environment = theseus_host::process::ChildEnvironment::Replace(vec![]);
        assert_eq!(client.reconnect(command), Err(ClientError::InvalidCommand));
        client.settlement = Settlement::new({
            let mut owner = TransportOwner::empty();
            owner.pump = Some(crate::pump::tests::ready());
            owner
        });
        let new = client.submit(Action::QueryLease(lease)).unwrap();
        assert_eq!(old, RequestId(17));
        assert_eq!(new, RequestId(18));
        let bytes = &client
            .settlement
            .owner
            .as_ref()
            .unwrap()
            .pump
            .as_ref()
            .unwrap()
            .frames[0]
            .bytes;
        let frame = v2::Decoder::new().feed(bytes).unwrap().frame.unwrap();
        assert_eq!(frame.sequence(), 2);
    }
}

#[cfg(test)]
mod reconnect_tests {
    use super::*;
    use crate::{
        recovery::Recovery,
        transport::{Settlement, TransportOwner},
    };
    fn partial_failure(_: &NativeCommand, _: InputMode) -> Result<AcquiredChild, SpawnFailure> {
        Err(SpawnFailure {
            launch: theseus_host::process::LaunchPhase::NeverCreated,
            cleanup: theseus_host::process::HostCleanup::Pending(
                theseus_host::process::OwnedSpawnCleanup::from_endpoints(vec![], vec![]),
            ),
            error: std::io::Error::other("modeled retained spawn source"),
            cleanup_errors: vec![],
        })
    }
    #[test]
    fn failed_reconnect_retains_partial_attempt_and_blocks_next_until_actual_join() {
        let mut client = Client {
            expected: crate::pump::tests::expected(),
            settlement: Settlement::new(TransportOwner::empty()),
            recovery: Recovery::default(),
            query_only: false,
            next_id: Some(73),
            error: None,
            lost: false,
        };
        client.request_stop();
        assert!(client.is_settled());
        let mut command = NativeCommand::new(if cfg!(windows) {
            "C:\\trusted-modeled-fixture.exe"
        } else {
            "/trusted-modeled-fixture"
        });
        command.environment = theseus_host::process::ChildEnvironment::Replace(vec![]);
        assert_eq!(
            client.reconnect_with(command.clone(), partial_failure),
            Err(ClientError::Spawn)
        );
        assert!(!client.is_settled());
        assert_eq!(
            client.reconnect_with(command, partial_failure),
            Err(ClientError::NotSettled)
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !client.poll_settle().unwrap() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(
            client
                .failure()
                .unwrap()
                .source()
                .unwrap()
                .is::<SpawnFailure>()
        );
        assert_eq!(client.next_id, Some(73));
    }
    #[test]
    fn readiness_is_required_before_mutation_admission() {
        let mut owner = TransportOwner::empty();
        owner.pump = Some(crate::pump::Pump::new(v2::SessionId::from_bytes([7; 16])).unwrap());
        let mut client = Client {
            expected: crate::pump::tests::expected(),
            settlement: Settlement::new(owner),
            recovery: Recovery::default(),
            query_only: false,
            next_id: Some(0),
            error: None,
            lost: false,
        };
        assert_eq!(
            client.submit(Action::Run {
                lease: v2::LeaseId::from_bytes([1; 16]),
                operation: v2::OperationId::from_bytes([2; 16]),
                program: v2::ProgramId::new("probe").unwrap()
            }),
            Err(ClientError::NotReady)
        );
        let frames = &client
            .settlement
            .owner
            .as_ref()
            .unwrap()
            .pump
            .as_ref()
            .unwrap()
            .frames;
        assert_eq!(frames.len(), 1);
        let hello = v2::Decoder::new()
            .feed(&frames[0].bytes)
            .unwrap()
            .frame
            .unwrap();
        assert!(matches!(hello.message(), v2::Message::Hello { .. }));
        assert_eq!(client.next_id, Some(0));
    }
}

#[cfg(test)]
mod wire_review_tests {
    // Consumer transport model: encoded frames cross the same bounded
    // Frame::advance / Client::consume_residual boundaries as native I/O. The
    // owned child is our test executable --list, only to exercise actual
    // Client start/reconnect/stop/join ownership. It is NOT a protocol peer or
    // helper fault fixture, and no bytes are attributed to its native pipes.
    use super::*;
    use std::time::{Duration, Instant};
    use theseus_native_helper_store::{IntentRecord, IntentStore};
    fn command() -> NativeCommand {
        let mut c = NativeCommand::new(std::env::current_exe().unwrap());
        c.arg("--list");
        c.environment = theseus_host::process::ChildEnvironment::Replace(vec![]);
        c
    }
    fn client() -> Client {
        match Client::start(command(), crate::pump::tests::expected()) {
            Ok(c) => c,
            Err(_) => panic!("trusted model owner spawn failed"),
        }
    }
    fn sent(c: &mut Client) -> Vec<v2::Envelope> {
        let pump = c.settlement.owner.as_mut().unwrap().pump.as_mut().unwrap();
        let mut bytes = Vec::new();
        while let Some(frame) = pump.frames.front_mut() {
            let done = frame
                .advance(|chunk| {
                    let n = chunk.len().min(7);
                    assert!(bytes.len() + n <= 2 * v2::MAX_FRAME_BYTES);
                    bytes.extend_from_slice(&chunk[..n]);
                    Ok(n)
                })
                .unwrap();
            if done {
                pump.frames.pop_front();
            }
        }
        let mut decoder = v2::Decoder::new();
        let mut frames = Vec::new();
        let mut offset = 0;
        while offset < bytes.len() {
            let progress = decoder.feed(&bytes[offset..]).unwrap();
            offset += progress.consumed;
            if let Some(frame) = progress.frame {
                frames.push(frame);
            }
        }
        decoder.finish().unwrap();
        frames
    }
    fn feed(
        c: &mut Client,
        session: &v2::SessionId,
        sequence: u64,
        message: v2::Message,
    ) -> Result<Option<Event>, ClientError> {
        let bytes =
            v2::encode(&v2::Envelope::new(session.clone(), sequence, message).unwrap()).unwrap();
        assert!(bytes.len() <= 4096);
        c.settlement
            .owner
            .as_mut()
            .unwrap()
            .pump
            .as_mut()
            .unwrap()
            .residual
            .extend_from_slice(&bytes);
        c.consume_residual()
    }
    fn ready(c: &mut Client) -> v2::SessionId {
        let hello = sent(c);
        assert_eq!(hello.len(), 1);
        assert_eq!(hello[0].sequence(), 0);
        assert!(matches!(
            hello[0].message(),
            v2::Message::Hello {
                role: v2::Role::Controller
            }
        ));
        let session = hello[0].session().clone();
        assert!(
            feed(
                c,
                &session,
                0,
                v2::Message::Hello {
                    role: v2::Role::Helper
                }
            )
            .unwrap()
            .is_none()
        );
        let describe = sent(c);
        assert_eq!(describe.len(), 1);
        assert_eq!(describe[0].sequence(), 1);
        assert!(matches!(
            describe[0].message(),
            v2::Message::Request {
                request: v2::Request::Describe
            }
        ));
        assert!(matches!(
            feed(
                c,
                &session,
                1,
                v2::Message::Response {
                    reply_to: 1,
                    result: crate::pump::tests::description()
                }
            )
            .unwrap(),
            Some(Event::Ready)
        ));
        for _ in 0..3 {
            assert!(
                sent(c).is_empty(),
                "automatic post-preflight action transmitted"
            );
        }
        session
    }
    fn settle(c: &mut Client) {
        c.request_stop();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !c.poll_settle().unwrap() {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }
    fn reconnect(c: &mut Client, old: &v2::SessionId) -> v2::SessionId {
        assert!(matches!(
            c.connection_lost().unwrap(),
            Poll::Event(Event::ConnectionLost)
        ));
        settle(c);
        c.reconnect(command()).unwrap();
        let session = ready(c);
        assert_ne!(&session, old);
        session
    }
    fn prepared() -> (
        tempfile::TempDir,
        theseus_native_helper_store::PreparedAcquire,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let mut store = IntentStore::new(
            theseus_host::fs::HostDir::create_private(&temp.path().join("intent")).unwrap(),
        );
        let p = store
            .prepare(IntentRecord {
                intent: v2::IntentId::from_bytes([8; 16]),
                binding: v2::BindingId::from_bytes([1; 16]),
                manifest_digest: v2::Digest::from_bytes([2; 32]),
                workspace_marker: v2::WorkspaceMarker::from_bytes([3; 16]),
            })
            .unwrap();
        (temp, p)
    }
    fn reply(state: v2::AcquireView) -> v2::Reply {
        v2::Reply::Acquired {
            intent: v2::IntentId::from_bytes([8; 16]),
            binding: v2::BindingId::from_bytes([1; 16]),
            manifest_digest: v2::Digest::from_bytes([2; 32]),
            state,
        }
    }
    #[test]
    fn r3_transmitted_acquire_reply_absent_then_fresh_epoch_only_explicit_same_intent_query() {
        let (_temp, p) = prepared();
        let mut c = client();
        let old = ready(&mut c);
        let acquire = c.submit(Action::Acquire(&p)).unwrap();
        let first = sent(&mut c);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].sequence(), 2);
        assert_eq!(
            first[0].message(),
            &v2::Message::Request {
                request: p.request()
            }
        );
        // Model the remote committed reservation, but intentionally deliver NO
        // Acquired frame before EOF. No client state can observe this result.
        let lease = v2::LeaseId::from_bytes([5; 16]);
        let committed = reply(v2::AcquireView::Lease {
            lease,
            state: v2::LeaseView::Held {
                intent: p.record().intent,
            },
        });
        let session = reconnect(&mut c, &old);
        let recovery = c.submit(Action::RecoverAcquire(&p)).unwrap();
        assert_ne!(acquire, recovery);
        let second = sent(&mut c);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].sequence(), 2);
        assert_eq!(
            second[0].message(),
            &v2::Message::Request {
                request: v2::Request::QueryAcquire {
                    intent: p.record().intent,
                    binding: p.record().binding,
                    manifest_digest: p.record().manifest_digest
                }
            }
        );
        assert!(
            matches!(feed(&mut c,&session,2,v2::Message::Response { reply_to:2,result:committed }).unwrap(),Some(Event::Reply { request, .. }) if request==recovery)
        );
        assert!(sent(&mut c).is_empty());
        settle(&mut c);
    }
    #[test]
    fn r3_transmitted_lost_run_has_no_automatic_action_then_only_explicit_original_query() {
        let mut c = client();
        let old = ready(&mut c);
        let lease = v2::LeaseId::from_bytes([5; 16]);
        let operation = v2::OperationId::from_bytes([9; 16]);
        c.submit(Action::Run {
            lease,
            operation,
            program: v2::ProgramId::new("probe").unwrap(),
        })
        .unwrap();
        let first = sent(&mut c);
        assert_eq!(first.len(), 1);
        assert!(
            matches!(first[0].message(),v2::Message::Request { request:v2::Request::Run { lease:l,operation:o,.. } } if *l==lease && *o==operation)
        );
        let session = reconnect(&mut c, &old);
        c.submit(Action::Query { lease, operation }).unwrap();
        let recovered = sent(&mut c);
        assert_eq!(recovered.len(), 1);
        assert_eq!(
            recovered[0].message(),
            &v2::Message::Request {
                request: v2::Request::QueryOperation { lease, operation }
            }
        );
        feed(
            &mut c,
            &session,
            2,
            v2::Message::Response {
                reply_to: 2,
                result: v2::Reply::Operation {
                    lease,
                    operation,
                    state: v2::OperationView::Running,
                },
            },
        )
        .unwrap();
        assert!(sent(&mut c).is_empty());
        settle(&mut c);
    }
    #[test]
    fn r2_client_epochs_keep_observed_lease_and_allow_only_same_identity_progression() {
        let (_temp, p) = prepared();
        let lease = v2::LeaseId::from_bytes([5; 16]);
        for contradiction in [
            v2::AcquireView::NotFound,
            v2::AcquireView::Lease {
                lease: v2::LeaseId::from_bytes([6; 16]),
                state: v2::LeaseView::Held {
                    intent: p.record().intent,
                },
            },
        ] {
            let mut c = client();
            let old = ready(&mut c);
            c.submit(Action::Acquire(&p)).unwrap();
            sent(&mut c);
            feed(
                &mut c,
                &old,
                2,
                v2::Message::Response {
                    reply_to: 2,
                    result: reply(v2::AcquireView::Lease {
                        lease,
                        state: v2::LeaseView::Held {
                            intent: p.record().intent,
                        },
                    }),
                },
            )
            .unwrap();
            let session = reconnect(&mut c, &old);
            c.submit(Action::RecoverAcquire(&p)).unwrap();
            sent(&mut c);
            assert!(matches!(
                feed(
                    &mut c,
                    &session,
                    2,
                    v2::Message::Response {
                        reply_to: 2,
                        result: reply(contradiction)
                    }
                ),
                Err(ClientError::Protocol)
            ));
            settle(&mut c);
        }
        let mut c = client();
        let old = ready(&mut c);
        c.submit(Action::RecoverAcquire(&p)).unwrap();
        sent(&mut c);
        feed(
            &mut c,
            &old,
            2,
            v2::Message::Response {
                reply_to: 2,
                result: reply(v2::AcquireView::NotFound),
            },
        )
        .unwrap();
        let second = reconnect(&mut c, &old);
        c.submit(Action::RecoverAcquire(&p)).unwrap();
        sent(&mut c);
        feed(
            &mut c,
            &second,
            2,
            v2::Message::Response {
                reply_to: 2,
                result: reply(v2::AcquireView::Lease {
                    lease,
                    state: v2::LeaseView::Held {
                        intent: p.record().intent,
                    },
                }),
            },
        )
        .unwrap();
        let third = reconnect(&mut c, &second);
        c.submit(Action::RecoverAcquire(&p)).unwrap();
        sent(&mut c);
        feed(
            &mut c,
            &third,
            2,
            v2::Message::Response {
                reply_to: 2,
                result: reply(v2::AcquireView::Lease {
                    lease,
                    state: v2::LeaseView::AwaitingExternalReset {
                        intent: p.record().intent,
                    },
                }),
            },
        )
        .unwrap();
        settle(&mut c);
    }
    #[test]
    fn r1_client_epochs_reject_contradictory_terminal_without_emitting_reply() {
        let lease = v2::LeaseId::from_bytes([5; 16]);
        let operation = v2::OperationId::from_bytes([9; 16]);
        let completed = |truncated| v2::Outcome::RunCompleted {
            reason: v2::RunReason::Natural,
            status: v2::NativeStatus::PosixExit { code: 0 },
            cleanup: v2::Cleanup::PosixLeaderReapedGroupTerminationRequested,
            stdout_bytes: 10,
            stderr_bytes: 0,
            truncated,
        };
        for evidence in 0..4 {
            let mut c = client();
            let old = ready(&mut c);
            c.submit(Action::Run {
                lease,
                operation,
                program: v2::ProgramId::new("probe").unwrap(),
            })
            .unwrap();
            sent(&mut c);
            feed(
                &mut c,
                &old,
                2,
                v2::Message::Response {
                    reply_to: 2,
                    result: v2::Reply::Operation {
                        lease,
                        operation,
                        state: v2::OperationView::Running,
                    },
                },
            )
            .unwrap();
            if evidence == 3 {
                feed(
                    &mut c,
                    &old,
                    3,
                    v2::Message::Log {
                        lease,
                        operation,
                        code: v2::LogCode::OutputTruncated,
                    },
                )
                .unwrap();
            } else {
                feed(
                    &mut c,
                    &old,
                    3,
                    v2::Message::Output {
                        lease,
                        operation,
                        channel: v2::Channel::Stdout,
                        offset: 0,
                        bytes: v2::FileBytes::new(&[1]).unwrap(),
                    },
                )
                .unwrap();
                if evidence == 2 {
                    feed(
                        &mut c,
                        &old,
                        4,
                        v2::Message::Output {
                            lease,
                            operation,
                            channel: v2::Channel::Stdout,
                            offset: 3,
                            bytes: v2::FileBytes::new(&[2]).unwrap(),
                        },
                    )
                    .unwrap();
                }
            }
            let session = reconnect(&mut c, &old);
            c.submit(Action::Query { lease, operation }).unwrap();
            sent(&mut c);
            let bad = match evidence {
                0 => v2::Outcome::NotStarted {
                    reason: v2::NotStartedReason::Deadline,
                },
                1 => v2::Outcome::LaunchFailed {
                    phase: v2::LaunchPhase::NeverCreated,
                    code: v2::FailureCode::Io,
                    cleanup: v2::SpawnCleanup::NoChildCreated,
                },
                _ => completed(false),
            };
            assert!(matches!(
                feed(
                    &mut c,
                    &session,
                    2,
                    v2::Message::Response {
                        reply_to: 2,
                        result: v2::Reply::Operation {
                            lease,
                            operation,
                            state: v2::OperationView::Finished { outcome: bad }
                        }
                    }
                ),
                Err(ClientError::Protocol)
            ));
            assert_eq!(
                c.submit(Action::Run {
                    lease,
                    operation: v2::OperationId::from_bytes([10; 16]),
                    program: v2::ProgramId::new("probe").unwrap()
                }),
                Err(ClientError::RecoveryRequired)
            );
            let next = reconnect(&mut c, &session);
            c.submit(Action::Query { lease, operation }).unwrap();
            sent(&mut c);
            assert!(matches!(
                feed(
                    &mut c,
                    &next,
                    2,
                    v2::Message::Response {
                        reply_to: 2,
                        result: v2::Reply::Operation {
                            lease,
                            operation,
                            state: v2::OperationView::Finished {
                                outcome: completed(evidence >= 2)
                            }
                        }
                    }
                )
                .unwrap(),
                Some(Event::Reply { .. })
            ));
            settle(&mut c);
        }
    }
    #[test]
    fn create_reconnect_preserves_identity_checks_receipt_and_never_automatically_reexecutes() {
        use sha2::{Digest as _, Sha256};
        for bad in 0..4 {
            let mut c = client();
            let old = ready(&mut c);
            let lease = v2::LeaseId::from_bytes([1; 16]);
            let operation = v2::OperationId::from_bytes([2; 16]);
            c.submit(Action::CreateFile {
                lease,
                operation,
                path: v2::LeafName::new("note").unwrap(),
                bytes: v2::FileBytes::new(b"hello").unwrap(),
            })
            .unwrap();
            let frames = sent(&mut c);
            assert!(matches!(
                frames[0].message(),
                v2::Message::Request {
                    request: v2::Request::CreateFile { .. }
                }
            ));
            let session = reconnect(&mut c, &old);
            assert!(sent(&mut c).is_empty());
            let query = c.submit(Action::Query { lease, operation }).unwrap();
            let frames = sent(&mut c);
            assert!(matches!(
                frames[0].message(),
                v2::Message::Request {
                    request: v2::Request::QueryOperation { .. }
                }
            ));
            let digest = v2::Digest::from_bytes(Sha256::digest(b"hello").into());
            let state = match bad {
                0 => v2::OperationView::Finished {
                    outcome: v2::Outcome::FileCreated { length: 4, digest },
                },
                1 => v2::OperationView::Finished {
                    outcome: v2::Outcome::FileCreated {
                        length: 5,
                        digest: v2::Digest::from_bytes([0; 32]),
                    },
                },
                2 => v2::OperationView::Accepted {
                    operation_kind: v2::OperationKind::Run,
                },
                _ => v2::OperationView::Finished {
                    outcome: v2::Outcome::LaunchFailed {
                        phase: v2::LaunchPhase::NeverCreated,
                        code: v2::FailureCode::Io,
                        cleanup: v2::SpawnCleanup::NoChildCreated,
                    },
                },
            };
            assert!(matches!(
                feed(
                    &mut c,
                    &session,
                    2,
                    v2::Message::Response {
                        reply_to: 2,
                        result: v2::Reply::Operation {
                            lease,
                            operation,
                            state
                        }
                    }
                ),
                Err(ClientError::Protocol)
            ));
            assert_eq!(
                c.submit(Action::CreateFile {
                    lease,
                    operation: v2::OperationId::from_bytes([3; 16]),
                    path: v2::LeafName::new("other").unwrap(),
                    bytes: v2::FileBytes::new(&[]).unwrap()
                }),
                Err(ClientError::RecoveryRequired)
            );
            let next = reconnect(&mut c, &session);
            let next_query = c.submit(Action::Query { lease, operation }).unwrap();
            assert_ne!(query, next_query);
            sent(&mut c);
            assert!(
                matches!(feed(&mut c,&next,2,v2::Message::Response {reply_to:2,result:v2::Reply::Operation {lease,operation,state:v2::OperationView::Finished {outcome:v2::Outcome::FileCreated {length:5,digest}}}}).unwrap(),Some(Event::Reply {request,..}) if request == next_query)
            );
            settle(&mut c);
        }
    }
}
