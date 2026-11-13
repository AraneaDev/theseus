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
mod conversation;
mod dispatch;
mod entry;
mod pending;
use conversation::Conversation;
#[cfg(test)]
use entry::entry_predecessor_matches;
use pending::{OriginalRefusal, RetainedRuntime};
pub(super) use pending::{
    Pending, finish_activation_check, validate_cancel_identity, with_cancel_flag,
};
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
    pub(super) fn remember(&mut self, error: io::Error) {
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
    pub(super) fn reply(&mut self, sequence: u64, reply: v2::Reply) -> io::Result<()> {
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
