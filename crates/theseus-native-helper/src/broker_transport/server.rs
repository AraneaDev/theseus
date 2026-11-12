use super::{
    BrokerServerConfig,
    connection::{endpoint, principal},
    launch::{self, DefinitionRoots, RetainedDefinition},
};
use crate::{
    broker::BrokerRuntime,
    contract::ControlPermit,
    frames::{Frame, FrameOwner},
};
use sha2::{Digest as _, Sha256};
use std::{
    collections::VecDeque,
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use theseus_host::{
    desktop::transport::{
        AuthenticatedStream, FrameIo, LocalListener, MeasuredIdentity, PeerCeiling,
        measure_local_identity,
    },
    fs::{HostDir, RootPolicy},
    services::readiness as host,
    stdio::{ReadStep, WriteStep},
};
use theseus_native_helper_store::LedgerStore;
use theseus_native_protocol::v2::{self, broker::*};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerPoll {
    Progress,
    Pending,
    Settled,
}
pub struct ServerStartFailure {
    error: io::Error,
    owner: Option<Box<BrokerServer>>,
}
impl ServerStartFailure {
    pub fn error(&self) -> &io::Error {
        &self.error
    }
    pub fn into_parts(self) -> (io::Error, Option<Box<BrokerServer>>) {
        (self.error, self.owner)
    }
}
impl std::fmt::Debug for ServerStartFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerStartFailure")
            .field("error", &self.error)
            .field("retained_owner", &self.owner.is_some())
            .finish()
    }
}
struct Conversation {
    io: AuthenticatedStream,
    decoder: v2::Decoder,
    guard: Option<v2::SessionGuard>,
    session: Option<v2::SessionId>,
    next: u64,
    input: Vec<u8>,
    offset: usize,
    frames: VecDeque<Frame>,
    permits: Vec<ControlPermit>,
    stopping: bool,
}
impl Conversation {
    fn new(io: AuthenticatedStream) -> Self {
        let (_, _, permits, _) = crate::contract::channels();
        Self {
            io,
            decoder: v2::Decoder::new(),
            guard: None,
            session: None,
            next: 0,
            input: Vec::with_capacity(4096),
            offset: 0,
            frames: VecDeque::with_capacity(2),
            permits: Vec::from(permits),
            stopping: false,
        }
    }
    fn queue(&mut self, message: v2::Message) -> io::Result<()> {
        let permit = self.permits.pop().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "bounded broker control frames occupied",
            )
        })?;
        let envelope = v2::Envelope::new(
            self.session
                .clone()
                .ok_or_else(|| io::Error::other("broker hello missing"))?,
            self.next,
            message,
        )
        .map_err(io::Error::other)?;
        self.guard
            .as_mut()
            .ok_or_else(|| io::Error::other("conversation guard missing"))?
            .accept_outbound(&envelope)
            .map_err(io::Error::other)?;
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| io::Error::other("broker sequence exhausted"))?;
        self.frames.push_back(
            Frame::encode(&envelope, FrameOwner::Control(permit)).map_err(io::Error::other)?,
        );
        Ok(())
    }
    fn flush(&mut self) -> io::Result<bool> {
        let Some(frame) = self.frames.front_mut() else {
            return Ok(false);
        };
        match self.io.try_write(frame.write_slice())? {
            WriteStep::Pending => Ok(false),
            WriteStep::Bytes(n) => {
                if frame.acknowledged(n).map_err(io::Error::other)? {
                    let frame = self.frames.pop_front().expect("original frame");
                    if let FrameOwner::Control(permit) = frame.owner {
                        self.permits.push(permit);
                    } else {
                        return Err(io::Error::other("broker control permit changed"));
                    }
                }
                Ok(true)
            }
        }
    }
    fn request(&mut self) -> io::Result<Option<(u64, v2::Request)>> {
        if self.permits.is_empty() {
            return Ok(None);
        }
        if self.input.len() == self.offset {
            self.input.clear();
            self.offset = 0;
            let mut bytes = [0; 4096];
            match self.io.try_read(&mut bytes)? {
                ReadStep::Pending => return Ok(None),
                ReadStep::Eof => {
                    self.decoder.finish().map_err(io::Error::other)?;
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "original broker conversation ended",
                    ));
                }
                ReadStep::Bytes(n) => self.input.extend_from_slice(&bytes[..n]),
            }
        }
        let progress = self
            .decoder
            .feed(&self.input[self.offset..])
            .map_err(io::Error::other)?;
        self.offset += progress.consumed;
        let Some(frame) = progress.frame else {
            return Ok(None);
        };
        if self.guard.is_none() {
            self.session = Some(frame.session().clone());
            self.guard = Some(v2::SessionGuard::new(
                frame.session().clone(),
                v2::Role::Helper,
            ));
        }
        self.guard
            .as_mut()
            .expect("original guard")
            .accept_inbound(&frame)
            .map_err(io::Error::other)?;
        match frame.message() {
            v2::Message::Hello {
                role: v2::Role::Controller,
            } => {
                self.queue(v2::Message::Hello {
                    role: v2::Role::Helper,
                })?;
                Ok(None)
            }
            v2::Message::Request { request } => Ok(Some((frame.sequence(), request.clone()))),
            _ => Err(io::Error::other("unexpected broker inbound message")),
        }
    }
    fn request_stop(&mut self) {
        self.stopping = true;
        self.io.request_stop();
    }
    fn poll_settle(&mut self) -> io::Result<bool> {
        self.io.poll_settle()
    }
}
impl Drop for Conversation {
    fn drop(&mut self) {
        self.request_stop();
        if !matches!(self.poll_settle(), Ok(true)) {
            std::mem::forget(std::mem::take(&mut self.frames));
            std::mem::forget(std::mem::take(&mut self.input));
        }
    }
}
pub(super) enum Pending {
    Activation {
        reply_to: u64,
        identity: Box<BrokerOperationIdentity>,
        deadline: Instant,
    },
    Ready {
        reply_to: u64,
        identity: Box<BrokerOperationIdentity>,
        nonce: v2::BindingId,
        deadline: Instant,
    },
    Close {
        reply_to: u64,
        request: Box<DesktopRequest>,
        deadline: Instant,
    },
}
impl Pending {
    pub(super) fn semantic_deadline(&self) -> Instant {
        match self {
            Self::Ready { deadline, .. }
            | Self::Close { deadline, .. }
            | Self::Activation { deadline, .. } => *deadline,
        }
    }
}
struct RetainedRuntime(Option<BrokerRuntime>);
impl std::ops::Deref for RetainedRuntime {
    type Target = BrokerRuntime;
    fn deref(&self) -> &BrokerRuntime {
        self.0.as_ref().expect("retained original runtime")
    }
}
impl std::ops::DerefMut for RetainedRuntime {
    fn deref_mut(&mut self) -> &mut BrokerRuntime {
        self.0.as_mut().expect("retained original runtime")
    }
}
pub struct BrokerServer {
    config: BrokerServerConfig,
    runtime: RetainedRuntime,
    listener: Option<Box<LocalListener>>,
    connection: Option<Conversation>,
    measurement: MeasuredIdentity,
    pending: Option<Pending>,
    replies: VecDeque<(u64, v2::Reply)>,
    cancel: Arc<AtomicBool>,
    stopping: bool,
    errors: Vec<io::Error>,
    truncated: bool,
    last_entry_observation: Option<DesktopObservation>,
    observed_sut: Option<DesktopSutRunWitness>,
    prepared_entries: Vec<EntryPreparation>,
    definition: Option<RetainedDefinition>,
    definition_used: bool,
    retired_definitions: Vec<RetainedDefinition>,
    observed_generation: Option<DesktopActivationTransition>,
    unconsumed_launch: Option<crate::broker::PreparedActivation>,
    #[cfg(windows)]
    apartment: Option<theseus_host::desktop::windows::ComApartment>,
    roots: DefinitionRoots,
    generation: Option<String>,
    definition_revoked: bool,
    ready_channel: Option<host::LocalChallenge>,
    close_channel: Option<host::LocalChallenge>,
    #[cfg(target_os = "linux")]
    scope: Option<theseus_host::desktop::scope::DedicatedBrokerScope>,
    #[cfg(target_os = "linux")]
    native_factory: Option<super::native_linux_factory::LinuxNativeFactory>,
}
impl BrokerServer {
    pub fn start(
        config: BrokerServerConfig,
        #[cfg(target_os = "linux")] scope: theseus_host::desktop::scope::DedicatedBrokerScope,
    ) -> Result<Self, ServerStartFailure> {
        Self::start_inner(
            config,
            #[cfg(target_os = "linux")]
            scope,
            #[cfg(target_os = "linux")]
            None,
        )
    }
    #[cfg(target_os = "linux")]
    pub fn start_qualified_linux(
        input: super::native_linux_factory::QualifiedLinuxBrokerConfig,
        scope: theseus_host::desktop::scope::DedicatedBrokerScope,
        initialize: super::native_linux_factory::LinuxFactoryInitializer,
        finalize: super::native_linux::OriginalFinalizer,
    ) -> Result<Self, ServerStartFailure> {
        Self::start_inner(
            input.config,
            scope,
            Some((input.store, input.proof, initialize, finalize)),
        )
    }
    fn start_inner(
        config: BrokerServerConfig,
        #[cfg(target_os = "linux")] scope: theseus_host::desktop::scope::DedicatedBrokerScope,
        #[cfg(target_os = "linux")] qualified: Option<(
            LedgerStore,
            theseus_native_helper_store::VerifiedLinuxGioBootstrap,
            super::native_linux_factory::LinuxFactoryInitializer,
            super::native_linux::OriginalFinalizer,
        )>,
    ) -> Result<Self, ServerStartFailure> {
        let prepared = (|| {
            #[cfg(target_os = "linux")]
            if let Some((_, proof, _, _)) = &qualified {
                config.validate_qualified(proof)?;
            } else {
                config.validate()?;
            }
            #[cfg(not(target_os = "linux"))]
            config.validate()?;
            let measurement = measure_local_identity()?;
            let _session = launch::binding(&config, &measurement)?;
            super::owned_entry::validate_session(
                config.manifest.desktop_owned_entry.as_ref(),
                &_session,
                &config.header.os,
            )?;
            crate::broker::owned_lifetime::validate_backend(
                config.manifest.desktop_owned_lifetime,
                &_session,
                &config.header.os,
            )?;
            let ledger = HostDir::open(
                std::path::Path::new(&config.ledger_root),
                RootPolicy::AllComponentsNoFollow,
            )?;
            let workspace = HostDir::open(
                std::path::Path::new(&config.workspace_root),
                RootPolicy::AllComponentsNoFollow,
            )?;
            #[cfg(target_os = "linux")]
            let (store, native_factory) = match qualified {
                Some((store, proof, initialize, finalize)) => {
                    let expected_session = _session.binding().clone();
                    let original = initialize(&proof, &config, _session)?;
                    let factory = super::native_linux_factory::LinuxNativeFactory::new(
                        proof,
                        original,
                        expected_session,
                        finalize,
                    )?;
                    (store, Some(factory))
                }
                None => (
                    LedgerStore::open_broker(&ledger, &workspace, &config.header)
                        .map_err(io::Error::other)?,
                    None,
                ),
            };
            #[cfg(not(target_os = "linux"))]
            let store = LedgerStore::open_broker(&ledger, &workspace, &config.header)
                .map_err(io::Error::other)?;
            let runtime = BrokerRuntime::open(store)?;
            let roots = DefinitionRoots::open(&config)?;
            #[cfg(windows)]
            let apartment = if config.manifest.desktop_native.is_some()
                || config.manifest.desktop_owned_entry.is_some()
            {
                Some(theseus_host::desktop::windows::ComApartment::initialize_original_thread()?)
            } else {
                None
            };
            #[cfg(windows)]
            return Ok::<_, io::Error>((measurement, runtime, roots, apartment));
            #[cfg(target_os = "linux")]
            return Ok::<_, io::Error>((measurement, runtime, roots, native_factory));
            #[cfg(all(not(windows), not(target_os = "linux")))]
            Ok::<_, io::Error>((measurement, runtime, roots))
        })();
        #[cfg(windows)]
        let (measurement, runtime, roots, apartment) =
            prepared.map_err(|error| ServerStartFailure { error, owner: None })?;
        #[cfg(target_os = "linux")]
        let (measurement, runtime, roots, native_factory) =
            prepared.map_err(|error| ServerStartFailure { error, owner: None })?;
        #[cfg(all(not(windows), not(target_os = "linux")))]
        let (measurement, runtime, roots) =
            prepared.map_err(|error| ServerStartFailure { error, owner: None })?;
        let mut server = Self {
            config,
            runtime: RetainedRuntime(Some(runtime)),
            listener: None,
            connection: None,
            measurement,
            pending: None,
            replies: VecDeque::with_capacity(2),
            cancel: Arc::new(AtomicBool::new(false)),
            stopping: false,
            errors: Vec::new(),
            truncated: false,
            last_entry_observation: None,
            observed_sut: None,
            prepared_entries: Vec::new(),
            definition: None,
            definition_used: false,
            retired_definitions: Vec::new(),
            observed_generation: None,
            unconsumed_launch: None,
            #[cfg(windows)]
            apartment,
            roots,
            generation: None,
            definition_revoked: false,
            ready_channel: None,
            close_channel: None,
            #[cfg(target_os = "linux")]
            scope: Some(scope),
            #[cfg(target_os = "linux")]
            native_factory,
        };
        let channel_result = (|| {
            let desktop = server
                .config
                .manifest
                .desktop
                .as_ref()
                .ok_or_else(|| io::Error::other("desktop missing"))?;
            server.ready_channel = Some(launch::bind_channel(
                &server.config,
                &desktop.ready_endpoint,
            )?);
            server.close_channel = Some(launch::bind_channel(
                &server.config,
                &desktop.close_endpoint,
            )?);
            Ok::<_, io::Error>(())
        })();
        if let Err(error) = channel_result {
            return Err(ServerStartFailure {
                error,
                owner: Some(Box::new(server)),
            });
        }
        let local = PeerCeiling {
            principal: principal(&server.config.header.broker_principal),
            session_id: server.measurement.session_id,
        };
        let peer = PeerCeiling {
            principal: principal(&server.config.header.helper_principal),
            session_id: None,
        };
        let endpoint = endpoint(&server.config.header.broker_endpoint)
            .map_err(|error| ServerStartFailure { error, owner: None })?;
        match LocalListener::bind(endpoint, local, peer) {
            Ok(listener) => server.listener = Some(Box::new(listener)),
            Err(failure) => {
                let (error, owner) = failure.into_parts();
                server.listener = owner;
                return Err(ServerStartFailure {
                    error,
                    owner: Some(Box::new(server)),
                });
            }
        }
        Ok(server)
    }
    pub fn header(&self) -> &IntegrationBrokerHeader {
        &self.config.header
    }
    pub fn retained_errors(&self) -> &[io::Error] {
        &self.errors
    }
    pub fn errors_truncated(&self) -> bool {
        self.truncated
    }
    fn remember(&mut self, error: io::Error) {
        #[cfg(feature = "test-support")]
        {
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr().lock(), "source broker failure: {error}");
        }
        if self.errors.len() < 16 {
            self.errors.push(error)
        } else {
            self.truncated = true;
        }
    }
    fn revalidate_session(&mut self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        if let Some(factory) = &self.native_factory
            && let Err(error) = factory.revalidate()
        {
            let _ = self.runtime.observe_session(None);
            return Err(error);
        }
        let measured = measure_local_identity()?;
        if measured != self.measurement {
            let _ = self.runtime.observe_session(None);
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "original broker process/session changed",
            ));
        }
        Ok(())
    }
    fn quarantine_entry(&mut self) -> io::Result<()> {
        self.definition_revoked = true;
        self.runtime.commit(BrokerEvent::EntryProofQuarantined {})?;
        Ok(())
    }
    fn retire_entry_proof(&mut self) -> io::Result<()> {
        if self.definition.is_none() {
            return Ok(());
        }
        if self.retired_definitions.len() >= v2::MAX_OPERATIONS {
            self.definition_revoked = true;
            self.runtime.commit(BrokerEvent::EntryProofQuarantined {})?;
            return Err(io::Error::other("entry tombstone capacity"));
        }
        let result = self
            .roots
            .revalidate()
            .and_then(|()| self.definition.as_ref().unwrap().revalidate())
            .and_then(|()| {
                self.definition.as_mut().unwrap().retire_owned_entry(
                    #[cfg(windows)]
                    &mut self.apartment,
                )
            });
        if let Err(error) = result {
            self.definition_revoked = true;
            self.runtime.commit(BrokerEvent::EntryProofQuarantined {})?;
            let operations = BrokerLedgerView::replay(self.runtime.store().records())
                .map_err(io::Error::other)?
                .requests()
                .filter(|r| matches!(r.action, DesktopAction::Activate { .. }))
                .last()
                .map(|r| r.helper_operation.clone());
            if let Some(operation) = operations {
                self.runtime
                    .commit(BrokerEvent::Quarantined { operation })?;
            }
            self.definition_revoked = true;
            return Err(error);
        }
        self.retired_definitions
            .push(self.definition.take().unwrap());
        self.definition_used = false;
        Ok(())
    }
    fn observe_owned_sut(&mut self, sut: Option<&DesktopSutRunWitness>) -> io::Result<()> {
        if self.config.manifest.desktop_owned_entry.is_none() {
            return Err(io::Error::other("owned entry inspection not declared"));
        }
        let view =
            BrokerLedgerView::replay(self.runtime.store().records()).map_err(io::Error::other)?;
        if view.entry_blocked()
            || view.entry_preparations().len() != self.prepared_entries.len()
            || (view.requests().next().is_some() && self.prepared_entries.is_empty())
        {
            return Err(io::Error::other(
                "entry preparation pending/quarantined/original continuity lost",
            ));
        }
        if sut == self.observed_sut.as_ref() {
            return Ok(());
        }
        let sut = sut.ok_or_else(|| io::Error::other("original SUT witness disappeared"))?;
        sut.validate().map_err(io::Error::other)?;
        let prepared = self
            .prepared_entries
            .last()
            .ok_or_else(|| io::Error::other("original SUT preparation continuity missing"))?;
        if sut.lease != prepared.helper_lease
            || sut.operation != prepared.operation
            || sut.request_digest != prepared.request_digest
            || sut.program != prepared.program
        {
            return Err(io::Error::other(
                "completed SUT differs from original prepared Run",
            ));
        }
        if self.definition.is_some() || self.unconsumed_launch.is_some() {
            return Err(io::Error::other("prior original proof remains retained"));
        }
        self.generation = None;
        self.observed_sut = Some(sut.clone());
        Ok(())
    }
    fn prepare_owned_entry(&mut self, preparation: &EntryPreparation) -> io::Result<()> {
        preparation.validate().map_err(io::Error::other)?;
        if self.config.manifest.desktop_owned_entry.is_none()
            || self.definition_revoked
            || self.unconsumed_launch.is_some()
            || self.pending.is_some()
        {
            return Err(io::Error::other("entry retirement not available"));
        }
        let view =
            BrokerLedgerView::replay(self.runtime.store().records()).map_err(io::Error::other)?;
        if view.entry_quarantined() {
            return Err(io::Error::other("entry proof quarantined"));
        }
        if let Some((old, settled)) = view
            .entry_preparations()
            .iter()
            .find(|(p, _)| p.operation == preparation.operation)
        {
            if old == preparation && *settled && self.prepared_entries.contains(preparation) {
                return Ok(());
            }
            return Err(io::Error::other(
                "original entry preparation is pending/lost/changed",
            ));
        }
        let lease = v2::LeaseId::parse(&preparation.broker_lease).map_err(io::Error::other)?;
        if !matches!(
            self.runtime.store().restart_view().query_lease(lease),
            Ok(v2::LeaseView::Held { .. })
        ) {
            return Err(io::Error::other("entry preparation wrong broker lease"));
        }
        let observation = self
            .last_entry_observation
            .as_ref()
            .ok_or_else(|| io::Error::other("original entry inspection missing"))?;
        if !entry_predecessor_matches(
            preparation,
            observation,
            self.observed_sut.as_ref(),
            self.prepared_entries.last(),
            &self
                .config
                .header
                .digest()
                .map_err(io::Error::other)?
                .to_hex(),
        ) {
            return Err(io::Error::other(
                "original entry preparation chronology changed",
            ));
        }
        if let Some(original) = view
            .requests()
            .filter(|r| matches!(r.action, DesktopAction::Activate { .. }))
            .last()
        {
            let identity = original.identity().map_err(io::Error::other)?;
            if !view
                .query(original)
                .map_err(io::Error::other)?
                .original_retired()
                || self.runtime.retirement_continuity()?.as_ref() != Some(&identity)
            {
                return Err(io::Error::other("original activated owner not retired"));
            }
        }
        let mapping = (|| {
            self.roots.revalidate()?;
            if let Some(definition) = &self.definition {
                definition.revalidate()?;
            } else if observation.entry_present {
                // Only the latest inspected original, never obsolete historical images.
                self.retired_definitions
                    .last()
                    .ok_or_else(|| io::Error::other("original entry tombstone missing"))?
                    .revalidate_original_files()?;
            } else {
                let selected = super::generation::inspection(
                    &self.config,
                    &view,
                    preparation.generation.as_ref(),
                    self.runtime.retirement_continuity()?.as_ref(),
                )?;
                let current = launch::inspect(&self.config, &self.roots, &selected)?;
                if current.entry_present || current.executable_digest.is_some() {
                    return Err(io::Error::other("original vacant entry mapping changed"));
                }
            }
            Ok(())
        })();
        if let Err(error) = mapping {
            self.quarantine_entry()?;
            return Err(error);
        }
        self.runtime.commit(BrokerEvent::EntryPreparationAccepted {
            preparation: preparation.clone(),
        })?;
        self.retire_entry_proof()?;
        self.runtime.commit(BrokerEvent::EntryPreparationSettled {
            operation: preparation.operation.clone(),
        })?;
        self.prepared_entries.push(preparation.clone());
        Ok(())
    }
    fn reply(&mut self, sequence: u64, reply: v2::Reply) -> io::Result<()> {
        if self.config.manifest.desktop_owned_entry.is_some() {
            if let v2::Reply::DesktopState { identity, state } = &reply
                && state.original_retired()
            {
                let view = BrokerLedgerView::replay(self.runtime.store().records())
                    .map_err(io::Error::other)?;
                if view.entry_quarantined() || self.definition_revoked {
                    return Err(io::Error::other("original entry proof quarantine"));
                }
                let latest = view
                    .requests()
                    .filter(|r| matches!(r.action, DesktopAction::Activate { .. }))
                    .last();
                let current = latest.is_some_and(|r| {
                    r.helper_operation == identity.helper_operation
                        || r.close_request()
                            .is_ok_and(|c| c.helper_operation == identity.helper_operation)
                });
                if current {
                    self.retire_entry_proof()?;
                }
            }
            if let v2::Reply::DesktopObservation { observation, .. } = &reply {
                self.last_entry_observation = Some(*observation.clone());
            }
        }
        if self.replies.len() >= 2 {
            return Err(io::Error::other("bounded broker reply queue full"));
        }
        self.replies.push_back((sequence, reply));
        Ok(())
    }
    pub fn poll(&mut self) -> io::Result<ServerPoll> {
        self.pump_native_events()?;
        if self.stopping {
            return self.poll_settle().map(|done| {
                if done {
                    ServerPoll::Settled
                } else {
                    ServerPoll::Pending
                }
            });
        }
        if self.connection.as_ref().is_some_and(|c| c.stopping) {
            let result = self
                .connection
                .as_mut()
                .expect("original connection")
                .poll_settle();
            match result {
                Ok(true) => {
                    self.connection = None;
                    self.pending = None;
                    self.replies.clear();
                }
                Ok(false) => return Ok(ServerPoll::Pending),
                Err(error) => {
                    self.remember(error);
                    return Ok(ServerPoll::Pending);
                }
            }
        }
        if self.connection.is_none() {
            match self
                .listener
                .as_mut()
                .ok_or_else(|| io::Error::other("broker listener missing"))?
                .try_accept()
            {
                Ok(Some(io)) => self.connection = Some(Conversation::new(io)),
                Ok(None) => return Ok(ServerPoll::Pending),
                Err(error) => {
                    self.remember(error);
                    return Ok(ServerPoll::Pending);
                }
            }
        }
        let progress = self.connection.as_mut().expect("live conversation").flush();
        if let Err(error) = progress {
            self.remember(error);
            self.connection
                .as_mut()
                .expect("original conversation")
                .request_stop();
            return Ok(ServerPoll::Progress);
        }
        if !self.replies.is_empty()
            && !self
                .connection
                .as_ref()
                .expect("live conversation")
                .permits
                .is_empty()
        {
            let (sequence, reply) = self.replies.pop_front().expect("original reply");
            if let Err(error) =
                self.connection
                    .as_mut()
                    .expect("live conversation")
                    .queue(v2::Message::Response {
                        reply_to: sequence,
                        result: reply,
                    })
            {
                self.remember(error);
                self.connection
                    .as_mut()
                    .expect("original conversation")
                    .request_stop();
                return Ok(ServerPoll::Progress);
            }
        }
        let request = self
            .connection
            .as_mut()
            .expect("live conversation")
            .request();
        match request {
            Ok(Some((sequence, request))) => {
                if let Err(error) = self.dispatch(sequence, request) {
                    self.remember(error);
                    self.reply(
                        sequence,
                        v2::Reply::Refused {
                            code: v2::RefusalCode::InvalidRequest,
                            inspect_failure: None,
                        },
                    )?;
                }
            }
            Ok(None) => {}
            Err(error) => {
                self.remember(error);
                self.connection
                    .as_mut()
                    .expect("original conversation")
                    .request_stop();
                return Ok(ServerPoll::Progress);
            }
        }
        self.advance_pending()?;
        Ok(ServerPoll::Progress)
    }
    fn dispatch(&mut self, sequence: u64, request: v2::Request) -> io::Result<()> {
        if self.pending.is_some()
            && !matches!(
                request,
                v2::Request::CancelDesktop { .. } | v2::Request::QueryDesktop { .. }
            )
        {
            return self.reply(
                sequence,
                v2::Reply::Refused {
                    code: v2::RefusalCode::Busy,
                    inspect_failure: None,
                },
            );
        }
        let request = match request {
            v2::Request::InspectOwnedEntry {
                lease,
                slot,
                generation,
                sut,
            } => {
                self.observe_owned_sut(sut.as_deref())?;
                v2::Request::InspectDesktop {
                    lease,
                    slot,
                    generation,
                }
            }
            v2::Request::InspectDesktop { .. }
                if self.config.manifest.desktop_owned_entry.is_some() =>
            {
                return Err(io::Error::other(
                    "owned entry requires original helper SUT inspection",
                ));
            }
            other => other,
        };
        match request {
            v2::Request::PrepareOwnedEntry { preparation } => {
                self.prepare_owned_entry(&preparation)?;
                self.reply(sequence, v2::Reply::OwnedEntryPrepared { preparation })
            }
            v2::Request::Describe => self.reply(
                sequence,
                v2::Reply::BrokerDescription {
                    header: Box::new(self.config.header.clone()),
                },
            ),
            v2::Request::Acquire {
                intent,
                binding,
                manifest_digest,
            } => {
                let state = self.runtime.acquire(intent, binding, manifest_digest)?;
                self.reply(
                    sequence,
                    v2::Reply::Acquired {
                        intent,
                        binding,
                        manifest_digest,
                        state,
                    },
                )
            }
            v2::Request::QueryAcquire {
                intent,
                binding,
                manifest_digest,
            } => {
                let state = self
                    .runtime
                    .query_acquire(intent, binding, manifest_digest)?;
                self.reply(
                    sequence,
                    v2::Reply::Acquired {
                        intent,
                        binding,
                        manifest_digest,
                        state,
                    },
                )
            }
            v2::Request::QueryLease { lease } => {
                let state = self
                    .runtime
                    .store()
                    .restart_view()
                    .query_lease(lease)
                    .map_err(|code| io::Error::other(OriginalRefusal(code)))?;
                self.reply(sequence, v2::Reply::Lease { lease, state })
            }
            v2::Request::Release { lease } => {
                let state = self.runtime.release_original(lease)?;
                self.definition = None;
                self.reply(sequence, v2::Reply::Lease { lease, state })
            }
            v2::Request::InspectDesktop {
                lease,
                slot,
                generation,
            } => {
                self.runtime
                    .store()
                    .restart_view()
                    .query_lease(lease)
                    .map_err(|code| io::Error::other(OriginalRefusal(code)))?;
                if self
                    .config
                    .manifest
                    .desktop
                    .as_ref()
                    .is_none_or(|d| d.slot != slot)
                {
                    return Err(io::Error::other("wrong desktop slot"));
                }
                let view = BrokerLedgerView::replay(self.runtime.store().records())
                    .map_err(io::Error::other)?;
                let continuity = self.runtime.retirement_continuity()?;
                let selected = super::generation::inspection(
                    &self.config,
                    &view,
                    generation.as_ref(),
                    continuity.as_ref(),
                )?;
                if generation.is_some() && self.observed_generation.as_ref() != generation.as_ref()
                {
                    self.roots.revalidate()?;
                    if self.unconsumed_launch.is_some() {
                        return Err(io::Error::other(
                            "original prepared generation remains retained",
                        ));
                    }
                    if self.definition_used
                        && self.definition.is_some()
                        && self.retired_definitions.len() >= v2::MAX_OPERATIONS
                    {
                        return Err(io::Error::other("retired definition capacity"));
                    }
                    if let Some(old) = self.definition.take()
                        && self.definition_used
                    {
                        self.retired_definitions.push(old);
                    }
                    // Unused metadata admission has no activation owner; dropping
                    // it settles original COM/parser state before fresh admission.
                    self.definition_used = false;
                    self.generation = None;
                    self.observed_generation = generation.clone();
                }
                let observed = match launch::inspect(&self.config, &self.roots, &selected) {
                    Ok(observed) => observed,
                    Err(error) => {
                        self.definition_revoked = true;
                        let _ = self.runtime.observe_session(None);
                        return Err(error);
                    }
                };
                let d = &selected.desktop;
                let expected = observed.entry_digest.as_ref() == Some(&d.entry_digest)
                    && observed.executable_digest.as_ref() == Some(&d.executable_digest);
                let mut valid = false;
                if observed.entry_present {
                    if !expected {
                        self.definition_revoked = true;
                    }
                    if !self.definition_revoked {
                        if self.definition.is_none()
                            && self.config.manifest.desktop_owned_entry.is_some()
                            && self.generation.is_some()
                        {
                            return Err(io::Error::other(
                                "retired entry proof cannot be re-admitted without original SUT transition",
                            ));
                        }
                        if self.definition.is_none() {
                            #[cfg(windows)]
                            if (selected.native.is_some() || selected.owned_entry.is_some())
                                && self.apartment.is_none()
                            {
                                self.apartment=Some(theseus_host::desktop::windows::ComApartment::initialize_original_thread()?);
                            }
                            match RetainedDefinition::admit(
                                &self.config,
                                &self.roots,
                                &selected,
                                #[cfg(windows)]
                                &mut self.apartment,
                            ) {
                                Ok(definition) => {
                                    let base = definition.binding_digest(
                                        &self.config,
                                        &self.roots,
                                        lease,
                                    )?;
                                    self.generation = Some(super::generation::inspection_binding(
                                        &base,
                                        &selected,
                                        generation.as_ref(),
                                    )?);
                                    self.definition = Some(definition);
                                }
                                Err(error) => {
                                    self.remember(error);
                                    self.definition_revoked = true;
                                }
                            }
                        }
                        if let Some(definition) = &self.definition {
                            match definition.revalidate() {
                                Ok(()) => valid = true,
                                Err(error) => {
                                    self.remember(error);
                                    self.definition_revoked = true;
                                    let _ = self.runtime.observe_session(None);
                                }
                            }
                        }
                    }
                } else if self.definition.is_some() {
                    self.definition_revoked = true;
                }
                let binding_digest = match &self.generation {
                    Some(value) => value.clone(),
                    None => super::generation::inspection_binding(
                        &self.roots.vacant_binding(&self.config, lease)?,
                        &selected,
                        generation.as_ref(),
                    )?,
                };
                let header = self
                    .config
                    .header
                    .digest()
                    .map_err(io::Error::other)?
                    .to_hex();
                self.reply(
                    sequence,
                    v2::Reply::DesktopObservation {
                        lease,
                        observation: Box::new(DesktopObservation {
                            generation: generation.clone(),
                            slot,
                            entry_present: observed.entry_present,
                            entry_valid: valid,
                            entry_digest: observed.entry_digest,
                            executable_digest: observed.executable_digest,
                            session_header_digest: header,
                            binding_digest,
                            complete: true,
                        }),
                    },
                )
            }

            v2::Request::DesktopAction { request } => match request.action {
                DesktopAction::Activate { .. } => {
                    self.revalidate_session()?;
                    let view = BrokerLedgerView::replay(self.runtime.store().records())
                        .map_err(io::Error::other)?;
                    let mut launch = None;
                    let continuity = self.runtime.retirement_continuity()?;
                    let selected = if view.original(&request.helper_operation).is_none() {
                        Some(super::generation::activation(
                            &self.config,
                            &view,
                            &request,
                            continuity.as_ref(),
                        )?)
                    } else {
                        None
                    };

                    if view.original(&request.helper_operation).is_none() {
                        if request.transition() != self.observed_generation.as_ref()
                            && (!self.config.manifest.desktop_definitions.is_empty()
                                || request.transition().is_some())
                        {
                            return Err(io::Error::other(
                                "activation changed original inspected SUT generation",
                            ));
                        }
                        if self.definition_revoked
                            || self.generation.as_deref()
                                != Some(request.prior_binding_digest.as_str())
                        {
                            return Err(io::Error::new(
                                io::ErrorKind::PermissionDenied,
                                "original observed desktop generation unavailable/replaced",
                            ));
                        }
                        let validation = self.roots.revalidate().and_then(|()| {
                            self.definition
                                .as_ref()
                                .ok_or_else(|| {
                                    io::Error::other("original installed definition not retained")
                                })?
                                .revalidate()
                        });
                        if let Err(error) = validation {
                            self.definition_revoked = true;
                            if let Err(error) = self.runtime.observe_session(None) {
                                self.remember(error);
                            }
                            return Err(error);
                        }
                        let selected = selected
                            .as_ref()
                            .ok_or_else(|| io::Error::other("selected tuple missing"))?;
                        crate::broker::owned_lifetime::validate_definition(
                            selected.owned_lifetime,
                            request.owned_lifetime_definition(),
                        )?;
                        let action_digest = request.digest().map_err(io::Error::other)?;
                        if self.unconsumed_launch.is_none() {
                            #[cfg(target_os = "linux")]
                            if let Some(handoff) = self.runtime.prepare_generation(&request)? {
                                match handoff {
                                    crate::broker::LinuxGenerationHandoff::OwnedScope(scope) => {
                                        if self.scope.is_some() {
                                            return Err(io::Error::other(
                                                "original scope handoff collision",
                                            ));
                                        }
                                        self.scope = Some(scope);
                                    }
                                    crate::broker::LinuxGenerationHandoff::NativeBroker(
                                        original,
                                    ) => self
                                        .native_factory
                                        .as_mut()
                                        .ok_or_else(|| {
                                            io::Error::other("original qualified factory missing")
                                        })?
                                        .restore(*original)?,
                                }
                            }
                            #[cfg(not(target_os = "linux"))]
                            self.runtime.prepare_generation(&request)?;
                            let d = &selected.desktop;
                            if self.ready_channel.is_none() && self.close_channel.is_none() {
                                self.ready_channel =
                                    Some(launch::bind_channel(&self.config, &d.ready_endpoint)?);
                                self.close_channel =
                                    Some(launch::bind_channel(&self.config, &d.close_endpoint)?);
                            }
                            let session = launch::binding(&self.config, &self.measurement)?;
                            launch = Some(launch::prepare(
                                &self.config,
                                &request,
                                session,
                                &mut self.ready_channel,
                                &mut self.close_channel,
                                #[cfg(target_os = "linux")]
                                &mut self.scope,
                                selected,
                                #[cfg(target_os = "linux")]
                                &mut self.native_factory,
                                self.definition.as_mut().ok_or_else(|| {
                                    io::Error::other("original definition missing")
                                })?,
                            )?);
                            self.definition_used = true;
                        } else if self.unconsumed_launch.as_ref().is_none_or(|old| {
                            old.binding().ready.action_digest != *action_digest.as_bytes()
                        }) {
                            return Err(io::Error::other(
                                "different original preparation already retained",
                            ));
                        }
                    }
                    if launch.is_some() {
                        if self.unconsumed_launch.is_some() {
                            return Err(io::Error::other(
                                "original unconsumed launch remains retained",
                            ));
                        }
                        self.unconsumed_launch = launch;
                    }
                    let deadline = Instant::now() + Duration::from_secs(30);
                    let state = self.runtime.activate_prepared(
                        (*request).clone(),
                        &mut self.unconsumed_launch,
                        deadline,
                        &self.cancel,
                    )?;
                    let validation = self.roots.revalidate().and_then(|()| {
                        self.definition
                            .as_ref()
                            .ok_or_else(|| {
                                io::Error::other("original definition missing after activation")
                            })?
                            .revalidate()
                    });
                    let identity = request.identity().map_err(io::Error::other)?;
                    let state = finish_activation_check(state, validation, |error| {
                        self.definition_revoked = true;
                        self.remember(error);
                        if let Err(error) = self.runtime.observe_session(None) {
                            self.remember(error);
                        }
                        self.runtime.query(&identity)
                    })?;
                    if matches!(
                        request.action,
                        DesktopAction::Activate {
                            mode: DesktopMode::NativeActivation,
                            ..
                        }
                    ) && !state.quarantined
                        && !matches!(state.effect, BrokerEffect::Receipt { .. })
                    {
                        self.pending = Some(Pending::Activation {
                            reply_to: sequence,
                            identity: Box::new(request.identity().map_err(io::Error::other)?),
                            deadline,
                        });
                        Ok(())
                    } else {
                        self.reply(
                            sequence,
                            v2::Reply::DesktopState {
                                identity: Box::new(request.identity().map_err(io::Error::other)?),
                                state: Box::new(state),
                            },
                        )
                    }
                }
                DesktopAction::RequestClose { .. } => {
                    self.pending = Some(Pending::Close {
                        reply_to: sequence,
                        request,
                        deadline: Instant::now() + Duration::from_secs(30),
                    });
                    Ok(())
                }
            },
            v2::Request::QueryDesktop { identity } => {
                self.runtime.query(&identity)?;
                let view = BrokerLedgerView::replay(self.runtime.store().records())
                    .map_err(io::Error::other)?;
                let original = view.original(&identity.helper_operation).cloned();
                if let Some(request) =
                    original.filter(|r| matches!(r.action, DesktopAction::RequestClose { .. }))
                    && self.pending.is_none()
                {
                    self.pending = Some(Pending::Close {
                        reply_to: sequence,
                        request: Box::new(request),
                        deadline: Instant::now() + Duration::from_secs(30),
                    });
                    return Ok(());
                }
                let state = self.runtime.poll_original(&identity)?;
                self.reply(
                    sequence,
                    v2::Reply::DesktopState {
                        identity,
                        state: Box::new(state),
                    },
                )
            }
            v2::Request::CancelDesktop { identity } => {
                self.runtime.query(&identity)?;
                let view = BrokerLedgerView::replay(self.runtime.store().records())
                    .map_err(io::Error::other)?;
                let original = view
                    .original(&identity.helper_operation)
                    .ok_or_else(|| io::Error::other("original cancellation identity missing"))?;
                validate_cancel_identity(original, self.pending.as_ref(), &identity)?;
                let cancel = Arc::clone(&self.cancel);
                let state = with_cancel_flag(&cancel, || {
                    self.advance_pending()?;
                    self.runtime.cancel(&identity)
                })?;
                self.reply(
                    sequence,
                    v2::Reply::DesktopState {
                        identity,
                        state: Box::new(state),
                    },
                )
            }
            v2::Request::ProbeDesktopReadiness {
                identity,
                nonce,
                deadline_ms,
            } => {
                if deadline_ms == 0 || deadline_ms > 30_000 {
                    return Err(io::Error::other("readiness deadline outside bound"));
                }
                self.pending = Some(Pending::Ready {
                    reply_to: sequence,
                    identity,
                    nonce,
                    deadline: Instant::now() + Duration::from_millis(deadline_ms as u64),
                });
                Ok(())
            }
            _ => self.reply(
                sequence,
                v2::Reply::Refused {
                    code: v2::RefusalCode::Unsupported,
                    inspect_failure: None,
                },
            ),
        }
    }
    fn advance_pending(&mut self) -> io::Result<()> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        let semantic_deadline = pending.semantic_deadline();
        let result = (|| match pending {
            Pending::Activation {
                reply_to,
                identity,
                deadline,
            } => {
                let Some(state) =
                    self.runtime
                        .poll_activation(&identity, semantic_deadline, &self.cancel)?
                else {
                    self.pending = Some(Pending::Activation {
                        reply_to,
                        identity,
                        deadline,
                    });
                    return Ok(());
                };
                self.reply(
                    reply_to,
                    v2::Reply::DesktopState {
                        identity,
                        state: Box::new(state),
                    },
                )
            }
            Pending::Ready {
                reply_to,
                identity,
                nonce,
                deadline,
            } => {
                let observed = self.runtime.poll_ready_with_nonce(
                    &identity,
                    nonce,
                    semantic_deadline,
                    &self.cancel,
                )?;
                let Some(observed) = observed else {
                    self.pending = Some(Pending::Ready {
                        reply_to,
                        identity,
                        nonce,
                        deadline,
                    });
                    return Ok(());
                };
                let reply =
                    readiness_reply(&self.config, &self.runtime, &identity, nonce, observed)?;
                self.reply(reply_to, reply)
            }
            Pending::Close {
                reply_to,
                request,
                deadline,
            } => {
                let state = self.runtime.poll_request_close(
                    (*request).clone(),
                    semantic_deadline,
                    &self.cancel,
                )?;
                let Some(state) = state else {
                    self.pending = Some(Pending::Close {
                        reply_to,
                        request,
                        deadline,
                    });
                    return Ok(());
                };
                self.reply(
                    reply_to,
                    v2::Reply::DesktopState {
                        identity: Box::new(request.identity().map_err(io::Error::other)?),
                        state: Box::new(state),
                    },
                )
            }
        })();
        if let Err(error) = result {
            self.remember(error);
            return Err(io::Error::other(
                "original pending broker action failed; owners retained",
            ));
        }
        Ok(())
    }
    fn pump_native_events(&mut self) -> io::Result<()> {
        self.runtime.pump_native_events()?;
        #[cfg(target_os = "macos")]
        if self.config.manifest.desktop_native.is_some() && !self.runtime.has_retained_owner() {
            theseus_host::desktop::macos::pump_once()?;
        }
        #[cfg(windows)]
        {
            if let Some(apartment) = &self.apartment {
                apartment.pump_once()?;
            }
            if let Some(definition) = &self.definition {
                definition.pump()?;
            }
        }
        Ok(())
    }
    pub fn request_stop(&mut self) {
        self.stopping = true;
        self.cancel.store(true, Ordering::Release);
        if let Some(connection) = &mut self.connection {
            connection.request_stop();
        }
        if let Some(listener) = &mut self.listener {
            listener.request_stop();
        }
    }
    pub fn poll_settle(&mut self) -> io::Result<bool> {
        self.pump_native_events()?;
        if !self.stopping {
            return Ok(false);
        }
        let view =
            BrokerLedgerView::replay(self.runtime.store().records()).map_err(io::Error::other)?;
        let originals = view
            .requests()
            .filter(|r| matches!(r.action, DesktopAction::Activate { .. }))
            .map(|r| r.identity().map_err(io::Error::other))
            .collect::<io::Result<Vec<_>>>()?;
        let mut done = true;
        for identity in originals {
            match self.runtime.cancel(&identity) {
                Ok(state) => {
                    // A declared owned lifetime (Mac original primary) never
                    // sets legacy `cleanup_complete` (protocol invariant); its
                    // retirement is the explicit owned-axis predicate.
                    done &= if state.native_lifetime.is_some() || state.owned_lifetime.is_some() {
                        state.original_retired()
                    } else {
                        state.cleanup_complete
                    }
                }
                Err(error) => {
                    self.remember(error);
                    done = false;
                }
            }
        }
        if let Some(connection) = &mut self.connection {
            match connection.poll_settle() {
                Ok(true) => self.connection = None,
                Ok(false) => done = false,
                Err(error) => {
                    self.remember(error);
                    done = false;
                }
            }
        }
        if let Some(listener) = &mut self.listener {
            match listener.poll_settle() {
                Ok(true) => self.listener = None,
                Ok(false) => done = false,
                Err(error) => {
                    self.remember(error);
                    done = false;
                }
            }
        }
        if let Some(launch) = &mut self.unconsumed_launch {
            match launch.cleanup() {
                Ok(true) => self.unconsumed_launch = None,
                Ok(false) => done = false,
                Err(error) => {
                    self.remember(error);
                    done = false;
                }
            }
        }
        for ready in [true, false] {
            let channel = if ready {
                &mut self.ready_channel
            } else {
                &mut self.close_channel
            };
            if let Some(owner) = channel {
                match owner.cleanup() {
                    Ok(()) => *channel = None,
                    Err(error) => {
                        self.remember(error);
                        done = false;
                    }
                }
            }
        }
        if done {
            self.definition = None;
            self.pending = None;
            self.replies.clear();
        }
        Ok(done)
    }
}
impl Drop for BrokerServer {
    fn drop(&mut self) {
        self.request_stop();
        if !matches!(self.poll_settle(), Ok(true)) {
            std::mem::forget(self.runtime.0.take());
            std::mem::forget(self.connection.take());
            std::mem::forget(self.listener.take());
            std::mem::forget(self.definition.take());
            std::mem::forget(std::mem::take(&mut self.retired_definitions));
            std::mem::forget(self.unconsumed_launch.take());
            #[cfg(target_os = "linux")]
            std::mem::forget(self.native_factory.take());
            std::mem::forget(self.ready_channel.take());
            std::mem::forget(self.close_channel.take());
            std::mem::forget(std::mem::take(&mut self.errors));
        }
    }
}
fn readiness_reply(
    config: &BrokerServerConfig,
    runtime: &BrokerRuntime,
    identity: &BrokerOperationIdentity,
    nonce: v2::BindingId,
    observed: host::ReadinessObservation,
) -> io::Result<v2::Reply> {
    let view = BrokerLedgerView::replay(runtime.store().records()).map_err(io::Error::other)?;
    let request = view
        .original(&identity.helper_operation)
        .ok_or_else(|| io::Error::other("original readiness activation missing"))?;
    let key = request
        .transition()
        .map_or(v2::DesktopDefinitionKey::CONFIGURED_TARGET, |t| {
            t.definition
        });
    let expectation = config
        .manifest
        .desktop_definition(&key)
        .map_err(io::Error::other)?
        .readiness;
    let action = request.digest().map_err(io::Error::other)?;
    let challenge = host::ReadinessChallenge {
        protocol: host::ReadyProtocol::TheseusFixtureReadyV1,
        lease: request.broker_lease.clone(),
        slot: request.slot.as_str().into(),
        resource_binding_digest: launch::digest(&request.prior_binding_digest)?,
        start_or_activation_operation: request.helper_operation.clone(),
        action_digest: *action.as_bytes(),
        nonce: *nonce.as_bytes(),
        expectation_digest: *expectation.digest().map_err(io::Error::other)?.as_bytes(),
    };
    let expectation_digest = expectation.digest().map_err(io::Error::other)?.to_hex();
    let response = observed.response.as_ref();
    let wire = v2::ReadinessObservation {
        peer_sid: observed.peer_sid.as_ref().map(|sid| sid.as_str().into()),
        expectation,
        slot: request.slot.clone(),
        operation: request.helper_operation.clone(),
        action_digest: action.to_hex(),
        resource_binding_digest: request.prior_binding_digest.clone(),
        challenge_digest: format!("{:x}", Sha256::digest(host::encode_challenge(&challenge)?)),
        expectation_digest,
        state: match observed.state {
            host::ReadinessState::Ready => v2::ReadinessState::Ready,
            host::ReadinessState::Failed => v2::ReadinessState::Failed,
            host::ReadinessState::Incomplete => v2::ReadinessState::Incomplete,
        },
        authenticated_instance: observed.authenticated_instance,
        instance_binding_digest: v2::Digest::from_bytes(observed.instance_binding_digest).to_hex(),
        peer_uid: observed.peer.map(|p| p.uid),
        peer_gid: observed.peer.map(|p| p.gid),
        artifact_sha256: response.map(|r| v2::Digest::from_bytes(r.artifact_sha256).to_hex()),
        version_literal: response.map(|r| r.version_literal.clone()),
        data_sha256: response.map(|r| v2::Digest::from_bytes(r.data_sha256).to_hex()),
        configuration_digest: response
            .map(|r| v2::Digest::from_bytes(r.configuration_digest).to_hex()),
        initialized: response.map(|r| r.initialized),
        response_digest: response
            .map(|r| host::encode_response(r).map(|bytes| format!("{:x}", Sha256::digest(bytes))))
            .transpose()?,
        failure: observed.failure.map(|failure| format!("{failure:?}")),
    };
    wire.validate_desktop(identity).map_err(io::Error::other)?;
    wire.validate_desktop_peer(&config.header.broker_principal)
        .map_err(io::Error::other)?;
    Ok(v2::Reply::DesktopReadiness {
        identity: Box::new(identity.clone()),
        observation: Box::new(wire),
    })
}

#[derive(Debug)]
struct OriginalRefusal(v2::RefusalCode);
impl std::fmt::Display for OriginalRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "original broker refusal: {:?}", self.0)
    }
}
impl std::error::Error for OriginalRefusal {}

/// A failed post-effect validation must return the new quarantined observation,
/// never the successful pre-validation snapshot.
pub(super) fn finish_activation_check(
    state: BrokerState,
    validation: io::Result<()>,
    quarantine: impl FnOnce(io::Error) -> io::Result<BrokerState>,
) -> io::Result<BrokerState> {
    match validation {
        Ok(()) => Ok(state),
        Err(error) => quarantine(error),
    }
}
pub(super) fn validate_cancel_identity(
    original: &DesktopRequest,
    pending: Option<&Pending>,
    identity: &BrokerOperationIdentity,
) -> io::Result<()> {
    if original.identity().map_err(io::Error::other)? != *identity
        || !matches!(original.action, DesktopAction::Activate { .. })
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "cancel does not name the original activation owner",
        ));
    }
    let related = match pending {
        None => true,
        Some(
            Pending::Ready {
                identity: ready, ..
            }
            | Pending::Activation {
                identity: ready, ..
            },
        ) => ready.as_ref() == identity,
        Some(Pending::Close { request, .. }) => {
            original.close_request().map_err(io::Error::other)? == **request
        }
    };
    if !related {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "cancel is unrelated to the original pending action",
        ));
    }
    Ok(())
}
pub(super) fn with_cancel_flag<T>(
    flag: &Arc<AtomicBool>,
    operation: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    struct Reset(Arc<AtomicBool>);
    impl Drop for Reset {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    let _reset = Reset(Arc::clone(flag));
    flag.store(true, Ordering::Release);
    operation()
}

fn entry_predecessor_matches(
    p: &EntryPreparation,
    observation: &DesktopObservation,
    observed_sut: Option<&DesktopSutRunWitness>,
    previous: Option<&EntryPreparation>,
    header_digest: &str,
) -> bool {
    if !observation.complete
        || p.slot != observation.slot
        || p.header_digest != header_digest
        || p.binding_digest != observation.binding_digest
        || p.generation != observation.generation
        || p.prior_sut.as_ref() != observed_sut
        || observation.entry_present && !observation.entry_valid
        || previous.is_some_and(|old| old.prior_sut == p.prior_sut)
    {
        return false;
    }
    match p.program.as_str() {
        "install_base" => {
            !observation.entry_present
                && observed_sut.is_none_or(|sut| sut.program.as_str() == "uninstall")
        }
        "install_target" | "upgrade" | "reinstall" => {
            if observation.entry_present {
                observed_sut.is_some_and(|sut| sut.program.as_str() == "configure")
            } else {
                observed_sut.is_none_or(|sut| sut.program.as_str() == "uninstall")
            }
        }
        "configure" | "uninstall" => {
            observation.entry_present
                && observed_sut.is_some_and(|sut| sut.program.as_str() != "uninstall")
        }
        _ => false,
    }
}
#[cfg(test)]
mod entry_retirement_tests {
    use super::*;
    fn original(program: &str, operation: u8) -> DesktopSutRunWitness {
        let program = v2::ProgramId::new(program).unwrap();
        let lease = v2::LeaseId::from_bytes([1; 16]);
        let operation = v2::OperationId::from_bytes([operation; 16]);
        DesktopSutRunWitness {
            lease: lease.to_hex(),
            operation: operation.to_hex(),
            request_digest: v2::request_digest(&v2::Request::Run {
                lease,
                operation,
                program: program.clone(),
            })
            .unwrap()
            .to_hex(),
            program,
        }
    }
    #[test]
    fn entry_retirement_known_unused_proof_requires_exact_successful_predecessor_and_next_step() {
        let sut = original("install_target", 2);
        let run = original("configure", 3);
        let p = EntryPreparation {
            helper_lease: run.lease,
            broker_lease: "04".repeat(16),
            operation: run.operation,
            request_digest: run.request_digest,
            program: run.program,
            slot: v2::ResourceSlot::new("desktop").unwrap(),
            header_digest: "05".repeat(32),
            binding_digest: "06".repeat(32),
            generation: None,
            prior_sut: Some(sut.clone()),
        };
        let observation = DesktopObservation {
            slot: p.slot.clone(),
            entry_present: true,
            entry_valid: true,
            entry_digest: Some("07".repeat(32)),
            executable_digest: Some("08".repeat(32)),
            session_header_digest: p.header_digest.clone(),
            binding_digest: p.binding_digest.clone(),
            complete: true,
            generation: None,
        };
        assert!(entry_predecessor_matches(
            &p,
            &observation,
            Some(&sut),
            None,
            &p.header_digest
        ));
        assert!(!entry_predecessor_matches(
            &p,
            &observation,
            None,
            None,
            &p.header_digest
        ));
        assert!(!entry_predecessor_matches(
            &p,
            &observation,
            Some(&original("install_target", 9)),
            None,
            &p.header_digest
        ));
        assert!(!entry_predecessor_matches(
            &p,
            &observation,
            Some(&sut),
            Some(&p),
            &p.header_digest
        ));
        let mut changed = observation.clone();
        changed.binding_digest = "09".repeat(32);
        assert!(!entry_predecessor_matches(
            &p,
            &changed,
            Some(&sut),
            None,
            &p.header_digest
        ));
        let mut changed = p.clone();
        changed.program = v2::ProgramId::new("install_base").unwrap();
        assert!(!entry_predecessor_matches(
            &changed,
            &observation,
            Some(&sut),
            None,
            &p.header_digest
        ));
        let mut vacant = observation;
        vacant.entry_present = false;
        vacant.entry_valid = false;
        let mut first = p;
        first.program = v2::ProgramId::new("install_target").unwrap();
        first.prior_sut = None;
        assert!(entry_predecessor_matches(
            &first,
            &vacant,
            None,
            None,
            &first.header_digest
        ));
    }
}
