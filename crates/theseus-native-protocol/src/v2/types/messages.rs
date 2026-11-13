//! Message, request, reply, view, outcome and envelope data types.
use super::super::{Error, Role, SessionId};
use super::validate::validate_message;
use super::{
    BindingId, Channel, Cleanup, CloseCode, Digest, FailureCode, FileBytes, FileCreateFailureCode,
    InspectFailure, IntentId, LaunchPhase, LeafName, LeaseId, LogCode, NativeDescription,
    NativeStatus, NotStartedReason, OperationId, OperationKind, Os, PrefixAuthorityFailure,
    Principal, ProgramId, ProgramList, RefusalCode, RunReason, SpawnCleanup, UnknownReason,
    WorkspaceMarker,
};
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    Hello {
        role: Role,
    },
    Request {
        request: Request,
    },
    Response {
        reply_to: u64,
        result: Reply,
    },
    Output {
        lease: LeaseId,
        operation: OperationId,
        channel: Channel,
        offset: u64,
        bytes: FileBytes,
    },
    Log {
        lease: LeaseId,
        operation: OperationId,
        code: LogCode,
    },
    Closed {
        code: CloseCode,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    InspectOwnedEntry {
        lease: LeaseId,
        slot: super::super::ResourceSlot,
        generation: Option<super::super::broker::DesktopActivationTransition>,
        sut: Option<Box<super::super::broker::DesktopSutRunWitness>>,
    },
    PrepareOwnedEntry {
        preparation: Box<super::super::broker::EntryPreparation>,
    },
    BrokerBind {
        original: Box<super::super::broker::BrokerBind>,
    },
    QueryBrokerBind {
        original: Box<super::super::broker::BrokerBind>,
    },
    DesktopAction {
        request: Box<super::super::broker::DesktopRequest>,
    },
    InspectDesktop {
        lease: LeaseId,
        slot: super::super::ResourceSlot,
        generation: Option<super::super::broker::DesktopActivationTransition>,
    },
    QueryDesktop {
        identity: Box<super::super::broker::BrokerOperationIdentity>,
    },
    CancelDesktop {
        identity: Box<super::super::broker::BrokerOperationIdentity>,
    },
    ProbeDesktopReadiness {
        identity: Box<super::super::broker::BrokerOperationIdentity>,
        nonce: BindingId,
        deadline_ms: u32,
    },
    ProbeReadiness {
        deadline_ms: u32,
        lease: LeaseId,
        slot: super::super::ResourceSlot,
        operation: OperationId,
        action_digest: Digest,
        prior_binding_digest: Digest,
        nonce: BindingId,
        definition_role: String,
        artifact_role: String,
    },
    ServiceAction {
        lease: LeaseId,
        operation: OperationId,
        slot: super::super::ResourceSlot,
        prior_binding_digest: Digest,
        action: super::super::ServiceAction,
    },
    InspectService {
        lease: LeaseId,
        slot: super::super::ResourceSlot,
    },
    Describe,
    Acquire {
        intent: IntentId,
        binding: BindingId,
        manifest_digest: Digest,
    },
    QueryAcquire {
        intent: IntentId,
        binding: BindingId,
        manifest_digest: Digest,
    },
    CreateFile {
        lease: LeaseId,
        operation: OperationId,
        path: LeafName,
        bytes: FileBytes,
    },
    ReadFile {
        lease: LeaseId,
        path: LeafName,
        offset: u64,
        length: u16,
    },
    InspectNativeFile {
        lease: LeaseId,
        path: LeafName,
    },
    InspectFile {
        lease: LeaseId,
        path: LeafName,
    },
    Run {
        lease: LeaseId,
        operation: OperationId,
        program: ProgramId,
    },
    QueryOperation {
        lease: LeaseId,
        operation: OperationId,
    },
    Cancel {
        lease: LeaseId,
        operation: OperationId,
    },
    Release {
        lease: LeaseId,
    },
    QueryLease {
        lease: LeaseId,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    OwnedEntryPrepared {
        preparation: Box<super::super::broker::EntryPreparation>,
    },
    BrokerDescription {
        header: Box<super::super::broker::IntegrationBrokerHeader>,
    },
    BrokerBinding {
        receipt: Box<super::super::broker::BrokerBindingReceipt>,
    },
    DesktopState {
        identity: Box<super::super::broker::BrokerOperationIdentity>,
        state: Box<super::super::broker::BrokerState>,
    },
    DesktopObservation {
        lease: LeaseId,
        observation: Box<super::super::broker::DesktopObservation>,
    },
    DesktopReadiness {
        identity: Box<super::super::broker::BrokerOperationIdentity>,
        observation: Box<super::super::ReadinessObservation>,
    },
    ReadinessObservation {
        lease: LeaseId,
        observation: Box<super::super::ReadinessObservation>,
    },
    ServiceObservation {
        lease: LeaseId,
        observation: Box<super::super::ServiceObservation>,
    },
    IntegrationDescription {
        description: Box<super::super::IntegrationDescription>,
    },
    NativeDescription {
        description: Box<NativeDescription>,
    },
    NativeFileInfo {
        lease: LeaseId,
        path: LeafName,
        length: u64,
        sha256: Digest,
    },
    FileAbsent {
        lease: LeaseId,
        path: LeafName,
    },
    PrefixAuthorityRevoked {
        lease: LeaseId,
        reason: PrefixAuthorityFailure,
    },
    Description {
        binding: BindingId,
        manifest_digest: Digest,
        workspace_marker: WorkspaceMarker,
        os: Os,
        helper_principal: Principal,
        programs: ProgramList,
    },
    Acquired {
        intent: IntentId,
        binding: BindingId,
        manifest_digest: Digest,
        state: AcquireView,
    },
    Lease {
        lease: LeaseId,
        state: LeaseView,
    },
    Operation {
        lease: LeaseId,
        operation: OperationId,
        state: OperationView,
    },
    FileBytes {
        lease: LeaseId,
        path: LeafName,
        offset: u64,
        bytes: FileBytes,
        eof: bool,
    },
    FileInfo {
        lease: LeaseId,
        path: LeafName,
        length: u16,
        digest: Digest,
    },
    Refused {
        code: RefusalCode,
        /// Present only with `OwnershipPending` from a refused manager
        /// inspection; a closed step name, never native text.
        inspect_failure: Option<InspectFailure>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AcquireView {
    NotFound,
    Lease { lease: LeaseId, state: LeaseView },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeaseView {
    Held {
        intent: IntentId,
    },
    AwaitingExternalReset {
        intent: IntentId,
    },
    Quarantined {
        intent: IntentId,
        reason: UnknownReason,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OperationView {
    Missing,
    Accepted { operation_kind: OperationKind },
    Running,
    Finished { outcome: Outcome },
    Unknown { reason: UnknownReason },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    ServiceActionCompleted {
        outcome: super::super::ServiceOutcome,
    },
    FileCreated {
        length: u16,
        digest: Digest,
    },
    NotStarted {
        reason: NotStartedReason,
    },
    FileCreateFailed {
        code: FileCreateFailureCode,
    },
    RunCompleted {
        reason: RunReason,
        status: NativeStatus,
        cleanup: Cleanup,
        stdout_bytes: u64,
        stderr_bytes: u64,
        truncated: bool,
    },
    LaunchFailed {
        phase: LaunchPhase,
        code: FailureCode,
        cleanup: SpawnCleanup,
    },
}

/// A locally validated envelope; direction, sequence and correlation belong to SessionGuard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Envelope {
    session: SessionId,
    sequence: u64,
    message: Message,
}
impl Envelope {
    pub fn new(session: SessionId, sequence: u64, message: Message) -> Result<Self, Error> {
        validate_message(&message)?;
        Ok(Self {
            session,
            sequence,
            message,
        })
    }
    pub fn session(&self) -> &SessionId {
        &self.session
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn message(&self) -> &Message {
        &self.message
    }
}
