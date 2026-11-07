//! Broker mechanisms called by the existing serialized worker/transport.
//! Persistence is exclusively LedgerStore/IntentStore; native owners are held
//! here and cannot be reconstructed from any serialized receipt or PID.
use std::{io, sync::atomic::AtomicBool, time::Instant};
use theseus_host::{
    desktop::{
        owned::{DescendantScope, ExecutableBinding, OwnedExecutable},
        session::{BoundSession, SessionBinding},
    },
    process::NativeCommand,
    services::readiness::LocalChallenge,
};
use theseus_native_helper_store::{
    IntentRecord, IntentStore, LedgerStore, PreparedAcquire, StoreError, VerifiedOriginalIntent,
};
use theseus_native_protocol::v2::{
    self,
    broker::*,
    ledger::{Entry, Record},
};

#[path = "broker_activation.rs"]
mod activation;
#[cfg(target_os = "linux")]
pub use crate::broker_transport::native_linux::{
    LinuxReapPermit, OriginalFinalizer, Prepared as PreparedLinuxActivation,
};
pub use activation::{PreparedActivation, RetainedActivation};
#[path = "broker_owned.rs"]
pub(crate) mod owned_lifetime;
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
fn store_error(error: StoreError) -> io::Error {
    io::Error::other(error)
}
fn append(store: &mut LedgerStore, event: BrokerEvent) -> io::Result<()> {
    let index =
        u16::try_from(store.records().len()).map_err(|_| invalid("broker ledger capacity"))?;
    let record = Record::new(index, Entry::Broker { event }).map_err(io::Error::other)?;
    store.append(record).map_err(store_error)?;
    Ok(())
}
fn projection(store: &LedgerStore) -> io::Result<BrokerLedgerView> {
    BrokerLedgerView::replay(store.records()).map_err(io::Error::other)
}
fn intent(original: &BrokerBind) -> io::Result<IntentRecord> {
    original.validate().map_err(io::Error::other)?;
    let header = &original.expected_header;
    Ok(IntentRecord {
        intent: v2::IntentId::parse(&original.broker_intent).map_err(io::Error::other)?,
        binding: v2::BindingId::parse(&header.binding).map_err(io::Error::other)?,
        manifest_digest: v2::Digest::parse(&header.manifest_digest).map_err(io::Error::other)?,
        workspace_marker: v2::WorkspaceMarker::parse(&header.workspace_marker)
            .map_err(io::Error::other)?,
    })
}
/// Returned exactly once after the durable helper BindAccepted and original
/// intent publication barriers. This owner is not Clone.
pub struct PreparedBrokerAcquire {
    original: BrokerBind,
    prepared: PreparedAcquire,
    sent: bool,
}
impl PreparedBrokerAcquire {
    pub fn take_request(&mut self) -> io::Result<v2::Request> {
        if self.sent {
            return Err(invalid("original broker Acquire already dispatched"));
        }
        self.sent = true;
        Ok(self.prepared.request())
    }
    pub fn original(&self) -> &BrokerBind {
        &self.original
    }
}
pub enum BindDispatch {
    AcquireOnce(PreparedBrokerAcquire),
    QueryOriginal {
        original: BrokerBind,
        verified: VerifiedOriginalIntent,
    },
    Cached(BrokerBindingReceipt),
}
/// Helpers call this only after controller publication of this exact BrokerBind.
/// A restarted or repeated bind verifies the original file read-only and never
/// calls prepare/Acquire again, including after a missing/uncertain publication.
pub fn begin_bind(
    store: &mut LedgerStore,
    intents: &mut IntentStore,
    original: BrokerBind,
) -> io::Result<BindDispatch> {
    original.validate().map_err(io::Error::other)?;
    let view = projection(store)?;
    if let Some(stored) = view.binding_original() {
        if stored != &original {
            return Err(invalid("changed original broker binding"));
        }
        if let Some(receipt) = view.binding(&original).map_err(io::Error::other)? {
            return Ok(BindDispatch::Cached(receipt.clone()));
        }
        let verified = intents
            .verify_original(&intent(&original)?)
            .map_err(io::Error::other)?;
        return Ok(BindDispatch::QueryOriginal { original, verified });
    }
    append(
        store,
        BrokerEvent::BindAccepted {
            request_digest: original.digest().map_err(io::Error::other)?.to_hex(),
            original: original.clone(),
        },
    )?;
    let prepared = intents
        .prepare(intent(&original)?)
        .map_err(io::Error::other)?;
    append(
        store,
        BrokerEvent::BindIntentPublished {
            operation: original.helper_operation.clone(),
            scope: publication_scope(prepared.commit_scope()),
        },
    )?;
    Ok(BindDispatch::AcquireOnce(PreparedBrokerAcquire {
        original,
        prepared,
        sent: false,
    }))
}
pub fn query_original_acquire(verified: &VerifiedOriginalIntent) -> v2::Request {
    let r = verified.record();
    v2::Request::QueryAcquire {
        intent: r.intent,
        binding: r.binding,
        manifest_digest: r.manifest_digest,
    }
}
fn publication_scope(scope: theseus_host::fs::CommitScope) -> IntentPublicationScope {
    match scope {
        theseus_host::fs::CommitScope::FileAndDirectorySynced => {
            IntentPublicationScope::FileAndDirectorySynced
        }
        theseus_host::fs::CommitScope::FileFlushedAtomicReplace => {
            IntentPublicationScope::FileFlushedAtomicReplace
        }
    }
}
/// Commit the exact acquired original lease in the helper ledger BEFORE the
/// caller publishes it to the controller and permits any desktop action.
pub fn finish_bind(
    store: &mut LedgerStore,
    acquire: PreparedBrokerAcquire,
    lease: v2::LeaseId,
) -> io::Result<BrokerBindingReceipt> {
    if !acquire.sent {
        return Err(invalid("broker Acquire was not dispatched"));
    }
    let receipt = BrokerBindingReceipt {
        request_digest: acquire
            .original
            .digest()
            .map_err(io::Error::other)?
            .to_hex(),
        original: acquire.original,
        publication_scope: publication_scope(acquire.prepared.commit_scope()),
        broker_lease: lease.to_hex(),
    };
    if let Some(existing) = projection(store)?
        .binding(&receipt.original)
        .map_err(io::Error::other)?
    {
        if existing == &receipt {
            return Ok(existing.clone());
        }
        return Err(invalid("changed original acquired broker lease"));
    }
    append(
        store,
        BrokerEvent::BindingCommitted {
            receipt: receipt.clone(),
        },
    )?;
    Ok(receipt)
}
/// QueryAcquire recovery needs the ORIGINAL observed publication scope, supplied
/// from the controller/helper journal; verifying file bytes grants no barrier.
pub fn finish_original_bind(
    store: &mut LedgerStore,
    original: &BrokerBind,
    verified: &VerifiedOriginalIntent,
    known_publication_scope: IntentPublicationScope,
    lease: v2::LeaseId,
) -> io::Result<BrokerBindingReceipt> {
    if verified.record() != &intent(original)? {
        return Err(invalid("wrong original broker intent"));
    }
    if projection(store)?.binding_scope(original) != Some(known_publication_scope) {
        return Err(invalid("original intent publication scope unavailable"));
    }
    let receipt = BrokerBindingReceipt {
        original: original.clone(),
        request_digest: original.digest().map_err(io::Error::other)?.to_hex(),
        publication_scope: known_publication_scope,
        broker_lease: lease.to_hex(),
    };
    if let Some(existing) = projection(store)?
        .binding(original)
        .map_err(io::Error::other)?
    {
        if existing == &receipt {
            return Ok(existing.clone());
        }
        return Err(invalid("changed original recovered broker lease"));
    }
    append(
        store,
        BrokerEvent::BindingCommitted {
            receipt: receipt.clone(),
        },
    )?;
    Ok(receipt)
}
pub fn verify_action_mapping(
    store: &LedgerStore,
    original: &BrokerBind,
    request: &DesktopRequest,
) -> io::Result<BrokerOperationIdentity> {
    let view = projection(store)?;
    let receipt = view
        .binding(original)
        .map_err(io::Error::other)?
        .ok_or_else(|| invalid("broker lease mapping not committed"))?;
    if request.helper_lease != original.helper_lease
        || request.slot != original.slot
        || request.broker_lease != receipt.broker_lease
        || request.binding_digest
            != original
                .expected_header
                .digest()
                .map_err(io::Error::other)?
                .to_hex()
    {
        return Err(invalid("desktop action changed original broker mapping"));
    }
    request.identity().map_err(io::Error::other)
}

/// Commit this exact original helper action before any broker transmission.
/// A duplicate is query-only; it never authorizes a second native dispatch.
pub fn prepare_helper_action(
    store: &mut LedgerStore,
    original: &BrokerBind,
    request: &DesktopRequest,
) -> io::Result<bool> {
    let identity = verify_action_mapping(store, original, request)?;
    let view = projection(store)?;
    if view
        .helper_request(&identity)
        .map_err(io::Error::other)?
        .is_some()
    {
        return Ok(false);
    }
    append(
        store,
        BrokerEvent::HelperActionAccepted {
            request: request.clone(),
            request_digest: request.digest().map_err(io::Error::other)?.to_hex(),
        },
    )?;
    Ok(true)
}
pub fn record_helper_state(
    store: &mut LedgerStore,
    identity: &BrokerOperationIdentity,
    state: &BrokerState,
) -> io::Result<()> {
    let lease = v2::LeaseId::parse(&identity.helper_lease).map_err(io::Error::other)?;
    if !matches!(
        store.restart_view().query_lease(lease),
        Ok(v2::LeaseView::Held { .. })
    ) {
        // A closed original helper ledger remains immutable. Exact original
        // broker query/Cancel observations are journaled by the controller;
        // they cannot reopen this helper lease or create action authority.
        projection(store)?
            .helper_request(identity)
            .map_err(io::Error::other)?
            .ok_or_else(|| invalid("original helper action missing"))?;
        state.validate().map_err(io::Error::other)?;
        return Ok(());
    }
    if projection(store)?
        .helper_state(identity)
        .map_err(io::Error::other)?
        == Some(state)
    {
        return Ok(());
    }
    append(
        store,
        BrokerEvent::HelperStateObserved {
            identity: identity.clone(),
            state: state.clone(),
        },
    )
}
pub fn record_helper_release(
    store: &mut LedgerStore,
    original: &BrokerBind,
    broker_lease: &str,
) -> io::Result<()> {
    if projection(store)?.helper_released() {
        return Ok(());
    }
    append(
        store,
        BrokerEvent::HelperBrokerReleased {
            helper_lease: original.helper_lease.clone(),
            broker_lease: broker_lease.into(),
        },
    )
}
pub struct OwnedLaunch {
    pub command: NativeCommand,
    pub session: BoundSession,
    pub binding: ExecutableBinding,
    pub ready_channel: LocalChallenge,
    pub close_channel: LocalChallenge,
    #[cfg(target_os = "linux")]
    pub scope: theseus_host::desktop::scope::DedicatedBrokerScope,
}
#[cfg(target_os = "linux")]
pub(crate) enum LinuxGenerationHandoff {
    OwnedScope(theseus_host::desktop::scope::DedicatedBrokerScope),
    NativeBroker(Box<theseus_host::desktop::linux::DedicatedGioBroker>),
}
struct LiveOwner {
    operation: String,
    owner: RetainedActivation,
    activation_deadline: Option<Instant>,
    close_deadline: Option<Instant>,
    ready_deadline: Option<([u8; 16], Instant)>,
    initial_readiness: Option<theseus_host::services::readiness::ReadinessObservation>,
    handoff_started: bool,
}
/// An owning mechanism for one admitted broker ledger. It runs on the existing
/// serialized worker; it introduces no executor, pump, background task or PID map.
pub struct BrokerRuntime {
    store: LedgerStore,
    header: IntegrationBrokerHeader,
    owner: Option<LiveOwner>,
    retired: Vec<LiveOwner>,
    session_revoked: bool,
    poisoned: bool,
    errors: Vec<io::Error>,
    errors_truncated: bool,
    #[cfg(feature = "test-support")]
    source_interruption: Option<SourceInterruption>,
}
#[cfg(feature = "test-support")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceInterruption {
    AfterAcceptance,
    AfterSpawn,
    AfterReceipt,
}
impl BrokerRuntime {
    /// Opening a ledger never reattaches native resources. Every accepted owner
    /// that is not durably settled is immediately, durably quarantined.
    pub fn open(store: LedgerStore) -> io::Result<Self> {
        let header = match store.records().first().map(Record::entry) {
            Some(Entry::IntegrationBrokerHeader { header }) => header.clone(),
            _ => return Err(invalid("not a broker ledger")),
        };
        let mut runtime = Self {
            store,
            header,
            owner: None,
            retired: Vec::new(),
            session_revoked: false,
            poisoned: false,
            errors: Vec::new(),
            errors_truncated: false,
            #[cfg(feature = "test-support")]
            source_interruption: None,
        };
        let view = projection(&runtime.store)?;
        let lost: Vec<_> = view
            .requests()
            .filter(|r| {
                !view.is_quarantined(&r.helper_operation)
                    && view.query(r).is_ok_and(|state| {
                        !state.original_retired() || state.effect == BrokerEffect::Unknown
                    })
            })
            .map(|r| r.helper_operation.clone())
            .collect();
        for operation in lost {
            runtime.commit(BrokerEvent::Quarantined { operation })?;
        }
        Ok(runtime)
    }
    pub fn header(&self) -> &IntegrationBrokerHeader {
        &self.header
    }
    #[cfg(feature = "test-support")]
    pub fn source_interrupt_once(&mut self, point: SourceInterruption) {
        self.source_interruption = Some(point);
    }
    #[cfg(feature = "test-support")]
    fn source_boundary(&mut self, point: SourceInterruption) -> io::Result<()> {
        if self.source_interruption == Some(point) {
            self.source_interruption = None;
            return Err(io::Error::other("source-owned dropped activation reply"));
        }
        Ok(())
    }
    pub fn store(&self) -> &LedgerStore {
        &self.store
    }
    pub fn retained_errors(&self) -> &[io::Error] {
        &self.errors
    }
    pub fn errors_truncated(&self) -> bool {
        self.errors_truncated
    }
    fn retain_error(&mut self, error: io::Error) {
        if self.errors.len() < 16 {
            self.errors.push(error);
        } else {
            self.errors_truncated = true;
        }
    }
    fn native_result<T>(&mut self, operation: &str, result: io::Result<T>) -> io::Result<T> {
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                self.retain_error(error);
                if !projection(&self.store)?.is_quarantined(operation) {
                    self.commit(BrokerEvent::Quarantined {
                        operation: operation.into(),
                    })?;
                }
                Err(io::Error::other(
                    "original broker native I/O error retained and quarantined",
                ))
            }
        }
    }
    pub fn native_errors(&self) -> Option<&[io::Error]> {
        self.owner.as_ref().map(|live| live.owner.retained_errors())
    }
    pub(crate) fn commit(&mut self, event: BrokerEvent) -> io::Result<()> {
        if self.poisoned {
            return Err(invalid("broker commit uncertainty"));
        }
        self.poisoned = true;
        append(&mut self.store, event)?;
        self.poisoned = false;
        Ok(())
    }
    pub fn acquire(
        &mut self,
        intent: v2::IntentId,
        binding: v2::BindingId,
        manifest: v2::Digest,
    ) -> io::Result<v2::AcquireView> {
        if self.poisoned {
            return Err(invalid("broker commit uncertainty"));
        }
        let cached = self.query_acquire(intent, binding, manifest)?;
        if !matches!(cached, v2::AcquireView::NotFound) {
            return Ok(cached);
        }
        if self
            .store
            .records()
            .iter()
            .any(|r| matches!(r.entry(), Entry::LeaseReserved { .. }))
        {
            return Err(invalid("broker already bound to original intent"));
        }
        let mut bytes = [0; 16];
        theseus_host::random_bytes(&mut bytes)?;
        let lease = v2::LeaseId::from_bytes(bytes);
        let index = u16::try_from(self.store.records().len())
            .map_err(|_| invalid("broker ledger capacity"))?;
        let record =
            Record::new(index, Entry::LeaseReserved { intent, lease }).map_err(io::Error::other)?;
        self.poisoned = true;
        self.store.append(record).map_err(store_error)?;
        self.poisoned = false;
        self.query_acquire(intent, binding, manifest)
    }
    pub fn query_acquire(
        &self,
        intent: v2::IntentId,
        binding: v2::BindingId,
        manifest: v2::Digest,
    ) -> io::Result<v2::AcquireView> {
        let mut state = self
            .store
            .restart_view()
            .query_acquire(intent, binding, manifest)
            .map_err(|_| invalid("broker binding mismatch"))?;
        if !self.poisoned
            && !self.session_revoked
            && self.errors.is_empty()
            && self
                .owner
                .as_ref()
                .is_some_and(|o| !o.owner.observation().session_revoked)
            && self.owner.as_ref().is_some_and(|o| {
                projection(&self.store).is_ok_and(|view| !view.is_quarantined(&o.operation))
            })
            && let v2::AcquireView::Lease { state, .. } = &mut state
            && matches!(state, v2::LeaseView::Quarantined { .. })
        {
            *state = v2::LeaseView::Held { intent };
        }
        Ok(state)
    }
    /// Close only the original fully settled ledger lease. Unknown effects,
    /// native I/O uncertainty and retained channels/scopes block this barrier.
    /// Provider reset remains a separate external disposition.
    pub fn release_original(&mut self, lease: v2::LeaseId) -> io::Result<v2::LeaseView> {
        let current = self
            .store
            .restart_view()
            .query_lease(lease)
            .map_err(|_| invalid("wrong original broker lease"))?;
        if matches!(current, v2::LeaseView::AwaitingExternalReset { .. }) {
            return Ok(current);
        }
        if self.poisoned
            || !self.errors.is_empty()
            || self.session_revoked
            || self.owner.iter().chain(self.retired.iter()).any(|live| {
                !live.owner.observation().local_cleanup_complete
                    || live.owner.observation().session_revoked
                    || !live.owner.retained_errors().is_empty()
                    || live.owner.errors_truncated()
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "original broker native owners or I/O remain unresolved",
            ));
        }
        let view = projection(&self.store)?;
        if view.entry_blocked() {
            return Err(invalid("original entry preparation is pending/quarantined"));
        }
        if view.requests().any(|request| {
            view.query(request).map_or(true, |state| {
                !state.original_retired()
                    || state.quarantined
                    || state.effect == BrokerEffect::Unknown
            })
        }) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "original accepted broker operation remains unknown or unsettled",
            ));
        }
        let record = Record::new(
            u16::try_from(self.store.records().len())
                .map_err(|_| invalid("broker ledger capacity"))?,
            Entry::LeaseClosed {
                lease,
                disposition: v2::ledger::Disposition::AwaitingExternalReset,
            },
        )
        .map_err(io::Error::other)?;
        self.poisoned = true;
        self.store.append(record).map_err(store_error)?;
        self.poisoned = false;
        self.store
            .restart_view()
            .query_lease(lease)
            .map_err(|_| invalid("closed original broker lease missing"))
    }
    pub(crate) fn retirement_continuity(&self) -> io::Result<Option<BrokerOperationIdentity>> {
        if self.poisoned || self.session_revoked || !self.errors.is_empty() {
            return Ok(None);
        }
        let Some(live) = &self.owner else {
            return Ok(None);
        };
        let view = projection(&self.store)?;
        let original = view
            .original(&live.operation)
            .ok_or_else(|| invalid("original live request missing"))?;
        let identity = original.identity().map_err(io::Error::other)?;
        let observed = live.owner.observation();
        if self.query(&identity)?.original_retired()
            && observed.local_cleanup_complete
            && !observed.session_revoked
            && live.owner.retained_errors().is_empty()
            && !live.owner.errors_truncated()
        {
            Ok(Some(identity))
        } else {
            Ok(None)
        }
    }
    fn validate_next_activation(&self, request: &DesktopRequest) -> io::Result<()> {
        if self.poisoned || self.session_revoked || !self.errors.is_empty() {
            return Err(invalid("original runtime authority revoked or uncertain"));
        }
        let view = projection(&self.store)?;
        view.validate_broker_generation(request)
            .map_err(io::Error::other)?;
        let previous = view
            .requests()
            .filter(|r| matches!(r.action, DesktopAction::Activate { .. }))
            .last();
        if let Some(previous) = previous {
            let identity = previous.identity().map_err(io::Error::other)?;
            if self.retirement_continuity()?.as_ref() != Some(&identity) {
                return Err(invalid(
                    "previous original owner/root/scope continuity absent",
                ));
            }
        } else if self.owner.is_some() || !self.retired.is_empty() {
            return Err(invalid("unmapped retained original owner"));
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn prepare_generation(
        &mut self,
        request: &DesktopRequest,
    ) -> io::Result<Option<LinuxGenerationHandoff>> {
        self.validate_next_activation(request)?;
        let Some(live) = &mut self.owner else {
            return Ok(None);
        };
        if live.handoff_started {
            return Ok(None);
        }
        let scope = match &mut live.owner {
            RetainedActivation::Owned(owner) => Some(LinuxGenerationHandoff::OwnedScope(
                owner.take_settled_scope_for_next_generation()?,
            )),
            RetainedActivation::LinuxGio(owner) => Some(LinuxGenerationHandoff::NativeBroker(
                Box::new(owner.take_settled_broker()?),
            )),
        };
        live.handoff_started = true;
        Ok(scope)
    }
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn prepare_generation(&mut self, request: &DesktopRequest) -> io::Result<()> {
        self.validate_next_activation(request)?;
        if let Some(live) = &mut self.owner {
            live.handoff_started = true;
        }
        Ok(())
    }
    fn archive_retired_original(&mut self, request: &DesktopRequest) -> io::Result<()> {
        self.validate_next_activation(request)?;
        if self.retired.len() >= v2::MAX_OPERATIONS {
            return Err(invalid("original generation archive capacity"));
        }
        #[cfg(target_os = "linux")]
        if let Some(live) = &self.owner
            && !live.handoff_started
        {
            return Err(invalid(
                "original Linux scope/native token was not handed back",
            ));
        }
        if let Some(live) = self.owner.take() {
            self.retired.push(live);
        }
        Ok(())
    }
    /// The caller has already verified original retained entry/root/artifact
    /// authority and the helper/controller's committed mapping. Native creation
    /// occurs only after this method has synced the complete original request.
    pub fn activate(
        &mut self,
        request: DesktopRequest,
        launch: &mut Option<OwnedLaunch>,
    ) -> io::Result<BrokerState> {
        request.validate().map_err(io::Error::other)?;
        if let Some(original) = projection(&self.store)?.original(&request.helper_operation) {
            if original != &request {
                return Err(invalid("broker replay conflict"));
            }
            return self.query(&request.identity().map_err(io::Error::other)?);
        }
        let DesktopAction::Activate {
            mode: DesktopMode::OwnedExecutable,
            expectation_digest,
            close_binding,
            ..
        } = &request.action
        else {
            return Err(invalid("owned executable mode required"));
        };
        let planned = launch
            .as_ref()
            .ok_or_else(|| invalid("missing original owned launch"))?;
        validate_session(&self.header, &planned.session)?;
        owned_lifetime::validate_backend(
            request.owned_lifetime_definition(),
            &planned.session,
            &self.header.os,
        )?;
        self.validate_next_activation(&request)?;
        if request.binding_digest != self.header.digest().map_err(io::Error::other)?.to_hex()
            || planned.binding.ready.lease != request.broker_lease
            || planned.binding.ready.slot != request.slot.as_str()
            || planned.binding.ready.start_or_activation_operation != request.helper_operation
            || planned.binding.ready.action_digest
                != *request.digest().map_err(io::Error::other)?.as_bytes()
            || planned.binding.ready.nonce
                != *v2::BindingId::parse(&request.activation_nonce)
                    .map_err(io::Error::other)?
                    .as_bytes()
            || planned.binding.ready.expectation_digest
                != *v2::Digest::parse(expectation_digest)
                    .map_err(io::Error::other)?
                    .as_bytes()
            || planned.binding.close.start_or_activation_operation != close_binding.operation
            || planned.binding.close.action_digest
                != *v2::Digest::parse(&close_binding.action_digest)
                    .map_err(io::Error::other)?
                    .as_bytes()
            || planned.binding.close.nonce
                != *v2::BindingId::parse(&close_binding.nonce)
                    .map_err(io::Error::other)?
                    .as_bytes()
            || planned.binding.ready.resource_binding_digest
                != *v2::Digest::parse(&request.prior_binding_digest)
                    .map_err(io::Error::other)?
                    .as_bytes()
        {
            return Err(invalid("owned launch changed durable activation"));
        }
        planned.binding.validate()?;
        self.archive_retired_original(&request)?;
        self.commit(BrokerEvent::Accepted {
            request_digest: request.digest().map_err(io::Error::other)?.to_hex(),
            request: request.clone(),
        })?;
        #[cfg(feature = "test-support")]
        self.source_boundary(SourceInterruption::AfterAcceptance)?;
        let launch = launch
            .take()
            .ok_or_else(|| invalid("owned launch already consumed"))?;
        #[cfg(target_os = "linux")]
        let spawned = OwnedExecutable::spawn_in_dedicated_broker(
            launch.scope,
            launch.session,
            &launch.command,
            launch.binding,
            launch.ready_channel,
            launch.close_channel,
        );
        #[cfg(not(target_os = "linux"))]
        let spawned = OwnedExecutable::spawn(
            launch.session,
            &launch.command,
            launch.binding,
            launch.ready_channel,
            launch.close_channel,
        );
        let acknowledged = spawned.is_ok();
        let owner = match spawned {
            Ok(owner) => owner,
            Err(failure) => {
                self.retain_error(failure.error);
                *failure.owner
            }
        };
        self.owner = Some(LiveOwner {
            operation: request.helper_operation.clone(),
            owner: RetainedActivation::Owned(Box::new(owner)),
            activation_deadline: None,
            close_deadline: None,
            ready_deadline: None,
            initial_readiness: None,
            handoff_started: false,
        });
        #[cfg(feature = "test-support")]
        self.source_boundary(SourceInterruption::AfterSpawn)?;
        let evidence = self
            .owner
            .as_ref()
            .expect("retained original owner")
            .owner
            .observation()
            .evidence;
        let receipt = BrokerEffectReceipt {
            evidence,
            acknowledged,
            receipt_digest: request.digest().map_err(io::Error::other)?.to_hex(),
        };
        self.commit(BrokerEvent::EffectRecorded {
            operation: request.helper_operation.clone(),
            receipt,
        })?;
        if !acknowledged {
            self.commit(BrokerEvent::Quarantined {
                operation: request.helper_operation.clone(),
            })?;
        }
        #[cfg(feature = "test-support")]
        self.source_boundary(SourceInterruption::AfterReceipt)?;
        self.persist_lifetime()?;
        self.query(&request.identity().map_err(io::Error::other)?)
    }
    /// The closed native arm is prepared by the admitted platform bridge only.
    /// Accepted + initial authority are synced before the first native effect.
    pub fn activate_prepared(
        &mut self,
        request: DesktopRequest,
        launch: &mut Option<PreparedActivation>,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<BrokerState> {
        request.validate().map_err(io::Error::other)?;
        if let Some(original) = projection(&self.store)?.original(&request.helper_operation) {
            if original != &request {
                return Err(invalid("broker replay conflict"));
            }
            return self.query(&request.identity().map_err(io::Error::other)?);
        }
        if matches!(launch, Some(PreparedActivation::Owned(_))) {
            let Some(PreparedActivation::Owned(owned)) = launch.take() else {
                unreachable!()
            };
            let mut owned = Some(*owned);
            let result = self.activate(request, &mut owned);
            if let Some(owned) = owned {
                *launch = Some(PreparedActivation::Owned(Box::new(owned)));
            }
            return result;
        }
        let DesktopAction::Activate {
            mode: DesktopMode::NativeActivation,
            expectation_digest,
            close_binding,
            ..
        } = &request.action
        else {
            return Err(invalid("closed native activation required"));
        };
        let planned = launch
            .as_ref()
            .ok_or_else(|| invalid("native preparation unavailable"))?;
        if planned.mode() != DesktopMode::NativeActivation
            || !self
                .header
                .allowed_modes
                .contains(&DesktopMode::NativeActivation)
        {
            return Err(invalid("native mode not admitted"));
        }
        validate_session_binding(&self.header, planned.session())?;
        let binding = planned.binding();
        self.validate_next_activation(&request)?;
        if request.binding_digest != self.header.digest().map_err(io::Error::other)?.to_hex()
            || binding.ready.lease != request.broker_lease
            || binding.ready.slot != request.slot.as_str()
            || binding.ready.start_or_activation_operation != request.helper_operation
            || binding.ready.action_digest
                != *request.digest().map_err(io::Error::other)?.as_bytes()
            || binding.ready.nonce
                != *v2::BindingId::parse(&request.activation_nonce)
                    .map_err(io::Error::other)?
                    .as_bytes()
            || binding.ready.expectation_digest
                != *v2::Digest::parse(expectation_digest)
                    .map_err(io::Error::other)?
                    .as_bytes()
            || binding.close.start_or_activation_operation != close_binding.operation
            || binding.close.action_digest
                != *v2::Digest::parse(&close_binding.action_digest)
                    .map_err(io::Error::other)?
                    .as_bytes()
            || binding.close.nonce
                != *v2::BindingId::parse(&close_binding.nonce)
                    .map_err(io::Error::other)?
                    .as_bytes()
            || binding.ready.resource_binding_digest
                != *v2::Digest::parse(&request.prior_binding_digest)
                    .map_err(io::Error::other)?
                    .as_bytes()
        {
            return Err(invalid("native launch changed durable original activation"));
        }
        binding.validate()?;
        let state = planned
            .initial_lifetime()
            .ok_or_else(|| invalid("native lifetime ceiling absent"))?;
        self.archive_retired_original(&request)?;
        self.commit(BrokerEvent::Accepted {
            request_digest: request.digest().map_err(io::Error::other)?.to_hex(),
            request: request.clone(),
        })?;
        self.commit(BrokerEvent::NativeLifetimeRecorded {
            operation: request.helper_operation.clone(),
            state,
        })?;
        #[cfg(feature = "test-support")]
        self.source_boundary(SourceInterruption::AfterAcceptance)?;
        let prepared = launch
            .take()
            .ok_or_else(|| invalid("native preparation consumed"))?;
        let (mut owner, error) = prepared.start(deadline, cancel);
        let timely = crate::broker_transport::native_policy::native_return_timely(
            deadline,
            Instant::now(),
            cancel.load(std::sync::atomic::Ordering::Acquire),
        );
        let uncertain = owner
            .observation()
            .native_lifetime
            .as_ref()
            .is_some_and(|state| {
                state.instance == NativeInstanceState::Unknown
                    || state.primary == NativePrimaryState::Unknown
                    || state.local_resources == NativeLocalState::Unknown
            });
        if error.is_some() || !timely || uncertain {
            owner.mark_uncertain();
        }
        self.owner = Some(LiveOwner {
            operation: request.helper_operation.clone(),
            owner,
            activation_deadline: Some(deadline),
            close_deadline: None,
            ready_deadline: None,
            initial_readiness: None,
            handoff_started: false,
        });
        if let Some(error) = error {
            self.retain_error(error);
        }
        self.persist_lifetime()?;
        if !timely || uncertain || !self.errors.is_empty() {
            self.commit(BrokerEvent::Quarantined {
                operation: request.helper_operation.clone(),
            })?;
        }
        #[cfg(feature = "test-support")]
        self.source_boundary(SourceInterruption::AfterSpawn)?;
        self.query(&request.identity().map_err(io::Error::other)?)
    }
    /// Pending authentication is not a terminal Unknown receipt. All progress
    /// belongs to the same durably accepted action and original semantic deadline.
    pub fn poll_activation(
        &mut self,
        identity: &BrokerOperationIdentity,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Option<BrokerState>> {
        let current = self.query(identity)?;
        if current.quarantined || matches!(current.effect, BrokerEffect::Receipt { .. }) {
            return Ok(Some(current));
        }
        let live = self
            .owner
            .as_mut()
            .filter(|o| o.operation == identity.helper_operation)
            .ok_or_else(|| invalid("original native activation owner lost"))?;
        let observed = crate::broker_transport::native_policy::poll_original_activation(
            live.activation_deadline,
            deadline,
            |original_deadline| live.owner.poll_ready(original_deadline, cancel),
        );
        let Some(observed) = self.native_result(&identity.helper_operation, observed)? else {
            return Ok(None);
        };
        if observed.state == theseus_host::services::readiness::ReadinessState::Incomplete {
            self.owner
                .as_mut()
                .expect("retained original")
                .owner
                .mark_uncertain();
            self.persist_lifetime()?;
            self.commit(BrokerEvent::Quarantined {
                operation: identity.helper_operation.clone(),
            })?;
        } else {
            self.owner
                .as_mut()
                .expect("original owner")
                .initial_readiness = Some(observed.clone());
            self.commit(BrokerEvent::EffectRecorded {
                operation: identity.helper_operation.clone(),
                receipt: BrokerEffectReceipt {
                    evidence: DesktopEvidence::NativeActivation,
                    acknowledged: observed.state
                        == theseus_host::services::readiness::ReadinessState::Ready,
                    receipt_digest: identity.request_digest.clone(),
                },
            })?;
            self.persist_lifetime()?;
            #[cfg(feature = "test-support")]
            self.source_boundary(SourceInterruption::AfterReceipt)?;
        }
        self.query(identity).map(Some)
    }
    #[cfg(target_os = "macos")]
    pub(crate) fn has_retained_owner(&self) -> bool {
        self.owner.is_some()
    }
    pub fn pump_native_events(&mut self) -> io::Result<()> {
        let Some(live) = self.owner.as_mut() else {
            return Ok(());
        };
        let operation = live.operation.clone();
        let result = live.owner.pump();
        self.native_result(&operation, result)
    }
    pub fn query(&self, identity: &BrokerOperationIdentity) -> io::Result<BrokerState> {
        let view = projection(&self.store)?;
        let original = view
            .original(&identity.helper_operation)
            .ok_or_else(|| invalid("missing original broker operation"))?;
        if &original.identity().map_err(io::Error::other)? != identity {
            return Err(invalid("changed original broker identity"));
        }
        let mut state = view.query(original).map_err(io::Error::other)?;
        let original_activation = match &original.action {
            DesktopAction::RequestClose {
                original_activation_operation,
                ..
            } => original_activation_operation,
            _ => &identity.helper_operation,
        };
        let durable_quarantine = view.is_quarantined(&identity.helper_operation)
            || view.is_quarantined(original_activation);
        if let Some(live) = self
            .owner
            .iter()
            .chain(self.retired.iter())
            .find(|live| &live.operation == original_activation)
        {
            let observed = live.owner.observation();
            let activation = view
                .original(original_activation)
                .ok_or_else(|| invalid("original activation request missing"))?;
            if activation.owned_lifetime_definition().is_some() {
                // A durable Lost owner cannot be restored by a cached local observation.
                if state.owner != BrokerOwner::Lost {
                    let old = state
                        .owned_lifetime
                        .as_ref()
                        .ok_or_else(|| invalid("accepted owned lifetime axis missing"))?;
                    let next = owned_lifetime::observe(activation, &live.owner, old)?;
                    state
                        .record_owned_lifetime(next)
                        .map_err(io::Error::other)?;
                }
            } else {
                state.owner = if observed.local_cleanup_complete {
                    BrokerOwner::Settled
                } else {
                    BrokerOwner::Retained
                };
                state.primary = observed.primary;
                state.descendants = observed.descendants;
                state.cleanup_complete = observed.cleanup_complete;
                state.native_lifetime = observed.native_lifetime;
            }
            state.quarantined = durable_quarantine || self.poisoned || observed.session_revoked;
        }
        if self.poisoned {
            state.quarantined = true;
        }
        Ok(state)
    }
    pub fn probe_ready(
        &mut self,
        identity: &BrokerOperationIdentity,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<theseus_host::services::readiness::ReadinessObservation> {
        self.query(identity)?;
        let live = self
            .owner
            .as_mut()
            .filter(|o| o.operation == identity.helper_operation)
            .ok_or_else(|| invalid("original native owner lost"))?;
        let result = live.owner.probe_ready(deadline, cancel);
        self.native_result(&identity.helper_operation, result)
    }
    /// The caller has published this exact fresh probe nonce before dispatch.
    /// Pending native channel state cannot be replaced; a new activation is never
    /// constructed by a readiness transition.
    pub fn probe_ready_with_nonce(
        &mut self,
        identity: &BrokerOperationIdentity,
        nonce: v2::BindingId,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<theseus_host::services::readiness::ReadinessObservation> {
        self.query(identity)?;
        let live = self
            .owner
            .as_mut()
            .filter(|o| o.operation == identity.helper_operation)
            .ok_or_else(|| invalid("original native owner lost"))?;
        if let Some(observed) = crate::broker_transport::native_policy::initial_native_sample(
            live.owner.observation().native_lifetime.is_some(),
            live.owner.current_ready_probe()?.challenge.nonce,
            *nonce.as_bytes(),
            &live.initial_readiness,
        ) {
            return Ok(observed);
        }
        if live.owner.current_ready_probe()?.challenge.nonce != *nonce.as_bytes() {
            let mut next = live.owner.current_ready_probe()?.clone();
            next.challenge.nonce = *nonce.as_bytes();
            live.owner.begin_fresh_ready_probe(next)?;
            live.initial_readiness = None;
        }
        let deadline = crate::broker_transport::native_policy::original_probe_deadline(
            &mut live.ready_deadline,
            *nonce.as_bytes(),
            deadline,
        );
        let result = live.owner.probe_ready(deadline, cancel);
        self.native_result(&identity.helper_operation, result)
    }
    /// Bounded advance of the original nonce; None is scheduling pending.
    pub fn poll_ready_with_nonce(
        &mut self,
        identity: &BrokerOperationIdentity,
        nonce: v2::BindingId,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Option<theseus_host::services::readiness::ReadinessObservation>> {
        self.query(identity)?;
        let live = self
            .owner
            .as_mut()
            .filter(|o| o.operation == identity.helper_operation)
            .ok_or_else(|| invalid("original native owner lost"))?;
        if let Some(observed) = crate::broker_transport::native_policy::initial_native_sample(
            live.owner.observation().native_lifetime.is_some(),
            live.owner.current_ready_probe()?.challenge.nonce,
            *nonce.as_bytes(),
            &live.initial_readiness,
        ) {
            return Ok(Some(observed));
        }
        if live.owner.current_ready_probe()?.challenge.nonce != *nonce.as_bytes() {
            let mut next = live.owner.current_ready_probe()?.clone();
            next.challenge.nonce = *nonce.as_bytes();
            live.owner.begin_fresh_ready_probe(next)?;
            live.initial_readiness = None;
        }
        let deadline = crate::broker_transport::native_policy::original_probe_deadline(
            &mut live.ready_deadline,
            *nonce.as_bytes(),
            deadline,
        );
        let result = live.owner.poll_ready(deadline, cancel);
        self.native_result(&identity.helper_operation, result)
    }
    /// Existing worker output framing/budgets remain authoritative. The broker
    /// reads only the original retained pipes, never opens a new process.
    pub fn try_output(
        &mut self,
        identity: &BrokerOperationIdentity,
        channel: v2::Channel,
        bytes: &mut [u8],
    ) -> io::Result<usize> {
        self.query(identity)?;
        let live = self
            .owner
            .as_mut()
            .filter(|o| o.operation == identity.helper_operation)
            .ok_or_else(|| invalid("original output owner lost"))?;
        let result = match channel {
            v2::Channel::Stdout => live.owner.try_stdout(bytes),
            v2::Channel::Stderr => live.owner.try_stderr(bytes),
        };
        if result.as_ref().is_err_and(|e| {
            matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Unsupported
            )
        }) {
            return result;
        }
        self.native_result(&identity.helper_operation, result)
    }
    pub fn request_close(
        &mut self,
        request: DesktopRequest,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<BrokerState> {
        self.close_inner(request, deadline, cancel, false)?
            .ok_or_else(|| invalid("blocking close unexpectedly yielded"))
    }
    pub fn poll_request_close(
        &mut self,
        request: DesktopRequest,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Option<BrokerState>> {
        self.close_inner(request, deadline, cancel, true)
    }
    fn close_inner(
        &mut self,
        request: DesktopRequest,
        deadline: Instant,
        cancel: &AtomicBool,
        poll_only: bool,
    ) -> io::Result<Option<BrokerState>> {
        request.validate().map_err(io::Error::other)?;
        let view = projection(&self.store)?;
        let mut accepted = false;
        if let Some(original) = view.original(&request.helper_operation) {
            if original != &request {
                return Err(invalid("broker close replay conflict"));
            }
            let state = self.query(&request.identity().map_err(io::Error::other)?)?;
            if state.quarantined
                || state.owner != BrokerOwner::Retained
                || matches!(state.effect, BrokerEffect::Receipt { .. })
            {
                return Ok(Some(state));
            }
            accepted = true;
        }
        let DesktopAction::RequestClose {
            original_activation_operation,
            ..
        } = &request.action
        else {
            return Err(invalid("original close required"));
        };
        let original = view
            .original(original_activation_operation)
            .ok_or_else(|| invalid("missing original activation"))?;
        if original.close_request().map_err(io::Error::other)? != request {
            return Err(invalid("close policy changed"));
        }
        if !self
            .owner
            .as_ref()
            .is_some_and(|o| o.operation == *original_activation_operation)
        {
            return Err(invalid("original close owner lost"));
        }
        if !accepted {
            self.commit(BrokerEvent::Accepted {
                request_digest: request.digest().map_err(io::Error::other)?.to_hex(),
                request: request.clone(),
            })?;
        }
        let live = self
            .owner
            .as_mut()
            .ok_or_else(|| invalid("original close owner lost"))?;
        let deadline = *live.close_deadline.get_or_insert(deadline);
        let observed = if poll_only {
            live.owner.poll_request_close(deadline, cancel)
        } else {
            live.owner.request_close(deadline, cancel).map(Some)
        };
        let evidence = live.owner.observation().evidence;
        let Some(observation) = self.native_result(&request.helper_operation, observed)? else {
            return Ok(None);
        };
        // A cancelled/deadline exchange retains its original pending channel;
        // it is not converted into a false terminal close acknowledgement.
        if observation.state == theseus_host::services::readiness::ReadinessState::Incomplete {
            return self
                .query(&request.identity().map_err(io::Error::other)?)
                .map(Some);
        }
        self.commit(BrokerEvent::EffectRecorded {
            operation: request.helper_operation.clone(),
            receipt: BrokerEffectReceipt {
                evidence,
                acknowledged: observation.state
                    == theseus_host::services::readiness::ReadinessState::Ready,
                receipt_digest: request.digest().map_err(io::Error::other)?.to_hex(),
            },
        })?;
        self.persist_lifetime()?;
        self.query(&request.identity().map_err(io::Error::other)?)
            .map(Some)
    }
    /// Cancel never creates RequestClose/Activate or a fresh operation ID. Only
    /// the original live owner is controlled, including after lost replies.
    pub fn cancel(&mut self, identity: &BrokerOperationIdentity) -> io::Result<BrokerState> {
        let current = self.query(identity)?;
        if current.owner != BrokerOwner::Retained {
            return Ok(current);
        }
        let live = self
            .owner
            .as_mut()
            .filter(|o| o.operation == identity.helper_operation)
            .ok_or_else(|| invalid("original native owner lost"))?;
        let result = live.owner.try_cleanup();
        self.native_result(&identity.helper_operation, result)?;
        self.persist_lifetime()?;
        self.query(identity)
    }
    pub fn poll_original(&mut self, identity: &BrokerOperationIdentity) -> io::Result<BrokerState> {
        let current = self.query(identity)?;
        if !self
            .owner
            .as_ref()
            .is_some_and(|live| live.operation == identity.helper_operation)
        {
            return Ok(current);
        }

        if current.native_lifetime.is_some()
            && !current.quarantined
            && current.effect == BrokerEffect::Unknown
            && let Some(deadline) = self
                .owner
                .as_ref()
                .filter(|o| o.operation == identity.helper_operation)
                .and_then(|o| o.activation_deadline)
            && let Some(state) =
                self.poll_activation(identity, deadline, &AtomicBool::new(false))?
        {
            return Ok(state);
        }
        if let Some(live) = self
            .owner
            .as_mut()
            .filter(|o| o.operation == identity.helper_operation)
        {
            let result = live.owner.observe_primary();
            self.native_result(&identity.helper_operation, result)?;
        }
        self.persist_lifetime()?;
        self.query(identity)
    }
    pub fn observe_session(&mut self, observed: Option<&SessionBinding>) -> io::Result<()> {
        if observed.is_none() {
            self.session_revoked = true;
        }
        let result = match self.owner.as_mut() {
            Some(live) => live.owner.observe_session(observed),
            None => Ok(()),
        };
        if result.is_err()
            && let Some(operation) = self.owner.as_ref().map(|live| live.operation.clone())
            && !projection(&self.store)?.is_quarantined(&operation)
        {
            self.commit(BrokerEvent::Quarantined { operation })?;
        }
        self.persist_lifetime()?;
        result
    }
    fn persist_lifetime(&mut self) -> io::Result<()> {
        let Some(live) = self.owner.as_ref() else {
            return Ok(());
        };
        let observed = live.owner.observation();
        let view = projection(&self.store)?;
        let original = view
            .original(&live.operation)
            .ok_or_else(|| invalid("original activation request missing"))?;
        let event = if original.owned_lifetime_definition().is_some() {
            let durable = view.query(original).map_err(io::Error::other)?;
            let old = durable
                .owned_lifetime
                .as_ref()
                .ok_or_else(|| invalid("accepted owned lifetime axis missing"))?;
            if durable.owner == BrokerOwner::Lost {
                return Err(invalid("lost original owned lifetime cannot progress"));
            }
            BrokerEvent::OwnedLifetimeRecorded {
                operation: live.operation.clone(),
                state: owned_lifetime::observe(original, &live.owner, old)?,
            }
        } else if let Some(state) = observed.native_lifetime {
            BrokerEvent::NativeLifetimeRecorded {
                operation: live.operation.clone(),
                state,
            }
        } else {
            BrokerEvent::LifetimeRecorded {
                operation: live.operation.clone(),
                primary: observed.primary,
                descendants: observed.descendants,
                cleanup_complete: observed.cleanup_complete,
            }
        };
        // Query polling is read-only unless independent lifetime facts changed.
        if self
            .store
            .records()
            .iter()
            .rev()
            .find_map(|r| match r.entry() {
                Entry::Broker {
                    event:
                        e @ (BrokerEvent::LifetimeRecorded { .. }
                        | BrokerEvent::NativeLifetimeRecorded { .. }
                        | BrokerEvent::OwnedLifetimeRecorded { .. }),
                } => Some(e),
                _ => None,
            })
            == Some(&event)
        {
            return Ok(());
        }
        self.commit(event)
    }
}
fn aggregate_cleanup(observed: &theseus_host::desktop::owned::LifetimeObservation) -> bool {
    observed.local_cleanup_complete
        && (observed.primary_exited || observed.descendants == DescendantScope::NotCreated)
        && matches!(
            observed.descendants,
            DescendantScope::Joined | DescendantScope::NotCreated
        )
}
fn descendants(scope: DescendantScope) -> BrokerDescendants {
    match scope {
        DescendantScope::NotCreated => BrokerDescendants::NotCreated,
        DescendantScope::Pending => BrokerDescendants::Pending,
        DescendantScope::Joined => BrokerDescendants::Joined,
        DescendantScope::Unknown => BrokerDescendants::Unknown,
    }
}
fn principal(value: &theseus_host::principal::CurrentPrincipal) -> BrokerPrincipal {
    match value {
        theseus_host::principal::CurrentPrincipal::Posix {
            effective_uid,
            effective_gid,
        } => BrokerPrincipal::Posix {
            effective_uid: *effective_uid,
            effective_gid: *effective_gid,
        },
        theseus_host::principal::CurrentPrincipal::Windows { token_user_sid } => {
            BrokerPrincipal::Windows {
                token_user_sid: token_user_sid.clone(),
            }
        }
    }
}
fn validate_session(header: &IntegrationBrokerHeader, session: &BoundSession) -> io::Result<()> {
    session
        .authorize(theseus_host::desktop::session::DesktopMode::OwnedExecutable)
        .map_err(io::Error::from)?;
    validate_session_binding(header, session.binding())
}
/// Protocol projection of the host session's broker endpoint, compared
/// exactly with the admitted header endpoint.
fn session_endpoint(
    endpoint: &theseus_host::desktop::session::BrokerEndpoint,
) -> io::Result<v2::LocalEndpoint> {
    use theseus_host::desktop::session::BrokerEndpoint;
    Ok(match endpoint {
        BrokerEndpoint::Unix(path) => v2::LocalEndpoint::UnixPath {
            path: path.as_str().into(),
        },
        // The protocol's canonical NamedPipe path is the full closed
        // `\\.\pipe\theseus.<name>` form (see `validate_endpoint`), so the
        // header compares against the full host pipe name, never its suffix.
        BrokerEndpoint::Pipe(name) => {
            let endpoint = v2::LocalEndpoint::NamedPipe {
                path: name.as_str().into(),
            };
            v2::broker::validate_endpoint(&endpoint)
                .map_err(|_| invalid("invalid closed broker pipe"))?;
            endpoint
        }
    })
}
fn validate_session_binding(
    header: &IntegrationBrokerHeader,
    b: &SessionBinding,
) -> io::Result<()> {
    use theseus_host::desktop::session::SuppliedDisplay as HostDisplay;
    let endpoint = session_endpoint(&b.broker_endpoint)?;
    // A workspace dependency can unify Host's test-support independently of
    // this crate. Such source-only authority must still refuse shipping code.
    #[allow(unreachable_patterns)]
    let display = match &b.display {
        HostDisplay::X11 {
            display,
            authority_file,
            bus,
        } => SuppliedDisplay::X11 {
            display: display.clone(),
            authority_file: authority_file.clone(),
            bus: bus.as_str().into(),
        },
        HostDisplay::Wayland {
            socket,
            runtime_directory,
            bus,
        } => SuppliedDisplay::Wayland {
            socket: socket.as_str().into(),
            runtime_directory: runtime_directory.clone(),
            bus: bus.as_str().into(),
        },
        HostDisplay::MacOsGui { domain } => SuppliedDisplay::MacOsGui {
            domain: domain.clone(),
        },
        HostDisplay::WindowsInteractive {
            session_id,
            window_station,
            desktop,
        } => SuppliedDisplay::WindowsInteractive {
            session_id: *session_id,
            window_station: window_station.clone(),
            desktop: desktop.clone(),
        },
        #[cfg(feature = "test-support")]
        HostDisplay::SourceOwnedHeadless => SuppliedDisplay::SourceOwnedHeadless,
        #[cfg(not(feature = "test-support"))]
        _ => {
            return Err(invalid(
                "source-only headless session is not shipping authority",
            ));
        }
    };
    if b.target_id != header.target_id
        || b.ownership_id
            != *v2::BindingId::parse(&header.ownership_id)
                .map_err(io::Error::other)?
                .as_bytes()
        || b.provider_epoch
            != *v2::Digest::parse(&header.provider_epoch)
                .map_err(io::Error::other)?
                .as_bytes()
        || b.session_id != header.session_id
        || b.session_epoch
            != *v2::Digest::parse(&header.session_epoch)
                .map_err(io::Error::other)?
                .as_bytes()
        || b.broker_header_digest != *header.digest().map_err(io::Error::other)?.as_bytes()
        || b.manifest_digest
            != *v2::Digest::parse(&header.manifest_digest)
                .map_err(io::Error::other)?
                .as_bytes()
        || endpoint != header.broker_endpoint
        || principal(&b.broker_principal) != header.broker_principal
        || principal(&b.helper_principal) != header.helper_principal
        || display != header.display
    {
        return Err(invalid("supplied broker session mismatch"));
    }
    Ok(())
}

#[cfg(test)]
mod session_endpoint_tests {
    use super::*;
    use theseus_host::{desktop::session::BrokerEndpoint, services::readiness::NamedPipe};
    #[test]
    fn windows_broker_pipe_projects_to_the_canonical_full_header_endpoint() {
        let full = r"\\.\pipe\theseus.824e7d58760e034d3ebabc77-broker";
        let projected =
            session_endpoint(&BrokerEndpoint::Pipe(NamedPipe::new(full.into()).unwrap())).unwrap();
        // Exactly the header form the integration setup publishes.
        assert_eq!(
            projected,
            v2::LocalEndpoint::NamedPipe { path: full.into() }
        );
        assert!(v2::broker::validate_endpoint(&projected).is_ok());
        assert_ne!(
            projected,
            v2::LocalEndpoint::NamedPipe {
                path: "824e7d58760e034d3ebabc77-broker".into()
            }
        );
    }
}
