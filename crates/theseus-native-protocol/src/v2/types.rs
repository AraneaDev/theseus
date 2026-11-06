use super::{Error, MAX_FILE_BYTES, MAX_PROGRAMS, Role, SessionId};
fn parse_hex<const N: usize>(v: &str) -> Result<[u8; N], Error> {
    if v.len() != N * 2 {
        return Err(Error::InvalidValue);
    }
    fn nibble(v: u8) -> Result<u8, Error> {
        match v {
            b'0'..=b'9' => Ok(v - b'0'),
            b'a'..=b'f' => Ok(v - b'a' + 10),
            _ => Err(Error::InvalidValue),
        }
    }
    let mut out = [0; N];
    let (pairs, _) = v.as_bytes().as_chunks::<2>();
    for (target, pair) in out.iter_mut().zip(pairs) {
        *target = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Ok(out)
}
fn hex(v: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(v.len() * 2);
    for b in v {
        out.push(H[(b >> 4) as usize] as char);
        out.push(H[(b & 15) as usize] as char);
    }
    out
}
macro_rules! identity {
    ($name:ident,$len:literal) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name([u8; $len]);
        impl $name {
            pub fn from_bytes(v: [u8; $len]) -> Self {
                Self(v)
            }
            pub fn parse(v: &str) -> Result<Self, Error> {
                parse_hex(v).map(Self)
            }
            pub fn as_bytes(&self) -> &[u8; $len] {
                &self.0
            }
            pub fn to_hex(&self) -> String {
                hex(&self.0)
            }
        }
    };
}
identity!(BindingId, 16);
identity!(WorkspaceMarker, 16);
identity!(IntentId, 16);
identity!(LeaseId, 16);
identity!(OperationId, 16);
identity!(Digest, 32);
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProgramId(String);
impl ProgramId {
    pub fn new(v: &str) -> Result<Self, Error> {
        let b = v.as_bytes();
        if b.is_empty()
            || b.len() > 32
            || !b[0].is_ascii_lowercase()
            || !b
                .iter()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_-".contains(c))
        {
            return Err(Error::InvalidValue);
        }
        Ok(Self(v.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeafName(String);
impl LeafName {
    pub fn new(v: &str) -> Result<Self, Error> {
        let b = v.as_bytes();
        if b.is_empty()
            || b.len() > 64
            || !b[0].is_ascii_alphanumeric()
            || b.last() == Some(&b'.')
            || !b
                .iter()
                .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(c))
        {
            return Err(Error::InvalidValue);
        }
        let stem = v.split('.').next().ok_or(Error::InvalidValue)?;
        if ["CON", "PRN", "AUX", "NUL"]
            .iter()
            .any(|r| stem.eq_ignore_ascii_case(r))
            || (stem.len() == 4
                && (stem[..3].eq_ignore_ascii_case("COM") || stem[..3].eq_ignore_ascii_case("LPT"))
                && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        {
            return Err(Error::InvalidValue);
        }
        Ok(Self(v.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
/// Lexical metadata only; native SID validation remains a host obligation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidText(String);
impl SidText {
    pub fn new(v: &str) -> Result<Self, Error> {
        if !(5..=256).contains(&v.len())
            || !v.starts_with("S-1-")
            || !v.as_bytes()[4..]
                .iter()
                .all(|c| c.is_ascii_hexdigit() || b"x-".contains(c))
        {
            return Err(Error::InvalidValue);
        }
        Ok(Self(v.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileBytes(Vec<u8>);
impl FileBytes {
    pub fn new(v: &[u8]) -> Result<Self, Error> {
        if v.len() > MAX_FILE_BYTES {
            return Err(Error::LimitExceeded);
        }
        Ok(Self(v.to_vec()))
    }
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgramList(Vec<ProgramId>);
impl ProgramList {
    pub fn new(v: Vec<ProgramId>) -> Result<Self, Error> {
        if v.is_empty() || v.len() > MAX_PROGRAMS {
            return Err(Error::LimitExceeded);
        }
        if v.windows(2).any(|p| p[0] >= p[1]) {
            return Err(Error::InvalidValue);
        }
        Ok(Self(v))
    }
    pub fn as_slice(&self) -> &[ProgramId] {
        &self.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Linux,
    Macos,
    Windows,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Stdout,
    Stderr,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationKind {
    ServiceAction,
    CreateFile,
    Run,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunReason {
    Natural,
    CancelRequested,
    Deadline,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotStartedReason {
    CancelRequested,
    Deadline,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileCreateFailureCode {
    AlreadyExists,
    Permission,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchPhase {
    NeverCreated,
    CreatedSuspended,
    MayRun,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cleanup {
    PosixLeaderReapedGroupTerminationRequested,
    WindowsProcessExitedJobEmpty,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnCleanup {
    NoChildCreated,
    PosixLeaderReapedGroupTerminationRequested,
    WindowsProcessExitedJobEmpty,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureCode {
    Io,
    Permission,
    Unsupported,
    InvalidCatalog,
    NativeFailure,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnknownReason {
    HelperRestartedWithUnfinishedOperation,
    LedgerWriteUncertain,
    ProcessStateUnproved,
    FileEffectUncertain,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefusalCode {
    WrongBinding,
    WrongManifest,
    WrongLease,
    Unsupported,
    Busy,
    ReplayConflict,
    Capacity,
    LedgerUnavailable,
    LedgerUnknown,
    LeaseQuarantined,
    AwaitingExternalReset,
    OwnershipPending,
    InvalidRequest,
    NotFound,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogCode {
    OutputTruncated,
    CancelRequested,
    DeadlineExpired,
    CleanupPending,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseCode {
    Normal,
    ProtocolError,
    SequenceExhausted,
    PeerLost,
    ShutdownDeadline,
    LedgerUnavailable,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Principal {
    Posix {
        effective_uid: u32,
        effective_gid: u32,
    },
    Windows {
        token_user_sid: SidText,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeStatus {
    PosixExit { code: u8 },
    PosixSignal { signal: u16 },
    WindowsExit { code: u32 },
}
/// Closed native capability set; no fallback or unknown capabilities are accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeCapabilities;
impl NativeCapabilities {
    pub const NAMES: [&'static str; 4] = [
        "catalog_run",
        "helper_current_account",
        "settled_leaf_absence",
        "native_stream_inspect_64m",
    ];
}
pub const MAX_NATIVE_FILE_BYTES: u64 = 64 * 1024 * 1024;
pub const NATIVE_INSPECT_BUFFER_BYTES: usize = 64 * 1024;
/// Domain-separated commitment to the catalog's ordered base and target artifacts.
pub fn artifact_commitment(
    base_length: u64,
    base: Digest,
    target_length: u64,
    target: Digest,
) -> Result<Digest, super::Error> {
    use sha2::{Digest as _, Sha256};
    if !(1..=MAX_NATIVE_FILE_BYTES).contains(&base_length)
        || !(1..=MAX_NATIVE_FILE_BYTES).contains(&target_length)
    {
        return Err(super::Error::InvalidValue);
    }
    let mut hash = Sha256::new();
    hash.update(b"theseus-native-artifacts-v1\0");
    hash.update(base_length.to_be_bytes());
    hash.update(base.0);
    hash.update(target_length.to_be_bytes());
    hash.update(target.0);
    Ok(Digest::from_bytes(hash.finalize().into()))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelperProfile {
    Fixture,
    Native {
        provider_epoch: Digest,
    },
    Integration {
        provider_epoch: Digest,
        core_manifest_digest: Digest,
        integration_manifest_digest: Digest,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Provenance<T> {
    Observed { value: T, source: String },
    ProviderReported { value: T, provider: String },
    Unavailable { reason: String },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeDescription {
    pub binding: BindingId,
    pub manifest_digest: Digest,
    pub workspace_marker: WorkspaceMarker,
    pub os: Os,
    pub helper_principal: Principal,
    pub programs: ProgramList,
    pub capabilities: NativeCapabilities,
    pub provider_epoch: Digest,
    pub artifact_commitment: Digest,
    pub build: Provenance<String>,
    pub os_build: Provenance<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrefixAuthorityFailure {
    Missing,
    Replaced,
    Renamed,
    MappingUnproved,
}
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
        slot: super::ResourceSlot,
        generation: Option<super::broker::DesktopActivationTransition>,
        sut: Option<Box<super::broker::DesktopSutRunWitness>>,
    },
    PrepareOwnedEntry {
        preparation: Box<super::broker::EntryPreparation>,
    },
    BrokerBind {
        original: Box<super::broker::BrokerBind>,
    },
    QueryBrokerBind {
        original: Box<super::broker::BrokerBind>,
    },
    DesktopAction {
        request: Box<super::broker::DesktopRequest>,
    },
    InspectDesktop {
        lease: LeaseId,
        slot: super::ResourceSlot,
        generation: Option<super::broker::DesktopActivationTransition>,
    },
    QueryDesktop {
        identity: Box<super::broker::BrokerOperationIdentity>,
    },
    CancelDesktop {
        identity: Box<super::broker::BrokerOperationIdentity>,
    },
    ProbeDesktopReadiness {
        identity: Box<super::broker::BrokerOperationIdentity>,
        nonce: BindingId,
        deadline_ms: u32,
    },
    ProbeReadiness {
        deadline_ms: u32,
        lease: LeaseId,
        slot: super::ResourceSlot,
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
        slot: super::ResourceSlot,
        prior_binding_digest: Digest,
        action: super::ServiceAction,
    },
    InspectService {
        lease: LeaseId,
        slot: super::ResourceSlot,
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
        preparation: Box<super::broker::EntryPreparation>,
    },
    BrokerDescription {
        header: Box<super::broker::IntegrationBrokerHeader>,
    },
    BrokerBinding {
        receipt: Box<super::broker::BrokerBindingReceipt>,
    },
    DesktopState {
        identity: Box<super::broker::BrokerOperationIdentity>,
        state: Box<super::broker::BrokerState>,
    },
    DesktopObservation {
        lease: LeaseId,
        observation: Box<super::broker::DesktopObservation>,
    },
    DesktopReadiness {
        identity: Box<super::broker::BrokerOperationIdentity>,
        observation: Box<super::ReadinessObservation>,
    },
    ReadinessObservation {
        lease: LeaseId,
        observation: Box<super::ReadinessObservation>,
    },
    ServiceObservation {
        lease: LeaseId,
        observation: Box<super::ServiceObservation>,
    },
    IntegrationDescription {
        description: Box<super::IntegrationDescription>,
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
        outcome: super::ServiceOutcome,
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
fn bounded_length(length: u16) -> Result<(), Error> {
    if usize::from(length) > MAX_FILE_BYTES {
        Err(Error::LimitExceeded)
    } else {
        Ok(())
    }
}
fn checked_end(offset: u64, length: usize) -> Result<(), Error> {
    offset
        .checked_add(length as u64)
        .map(|_| ())
        .ok_or(Error::InvalidValue)
}
fn validate_message(v: &Message) -> Result<(), Error> {
    match v {
        Message::Request {
            request: Request::PrepareOwnedEntry { preparation },
        } => preparation.validate(),
        Message::Request {
            request: Request::InspectOwnedEntry {
                generation, sut, ..
            },
        } => {
            if let Some(sut) = sut {
                sut.validate()?;
            }
            if let Some(generation) = generation {
                generation.validate()?;
                if Some(&generation.sut) != sut.as_deref() {
                    return Err(Error::InvalidValue);
                }
            }
            Ok(())
        }
        Message::Request {
            request: Request::BrokerBind { original } | Request::QueryBrokerBind { original },
        } => original.validate(),
        Message::Request {
            request: Request::DesktopAction { request },
        } => request.validate(),
        Message::Request {
            request: Request::QueryDesktop { identity } | Request::CancelDesktop { identity },
        } => identity.validate(),
        Message::Request {
            request:
                Request::ProbeDesktopReadiness {
                    identity,
                    nonce,
                    deadline_ms,
                },
        } => {
            identity.validate()?;
            if nonce.as_bytes() == &[0; 16] || !(1..=30000).contains(deadline_ms) {
                return Err(Error::InvalidValue);
            }
            Ok(())
        }
        Message::Request {
            request: Request::ReadFile { offset, length, .. },
        } => {
            bounded_length(*length)?;
            if *length == 0 {
                return Err(Error::InvalidValue);
            }
            checked_end(*offset, usize::from(*length))
        }
        Message::Request {
            request:
                Request::ProbeReadiness {
                    deadline_ms,
                    definition_role,
                    artifact_role,
                    ..
                },
        } => {
            if !(1..=30_000).contains(deadline_ms)
                || !["base", "configured", "target"].contains(&definition_role.as_str())
                || !["base", "target"].contains(&artifact_role.as_str())
            {
                return Err(Error::InvalidValue);
            }
            Ok(())
        }
        Message::Request {
            request:
                Request::InspectDesktop {
                    generation: Some(generation),
                    ..
                },
        } => generation.validate(),
        Message::Response { result, .. } => match result {
            Reply::OwnedEntryPrepared { preparation } => preparation.validate(),
            Reply::BrokerDescription { header } => header.validate(),
            Reply::BrokerBinding { receipt } => receipt.validate(),
            Reply::DesktopState { identity, state } => {
                identity.validate()?;
                state.validate()
            }
            Reply::DesktopObservation { observation, .. } => observation.validate(),
            Reply::DesktopReadiness {
                identity,
                observation,
            } => observation.validate_desktop(identity),
            Reply::Description {
                os,
                helper_principal,
                ..
            } => match (os, helper_principal) {
                (Os::Windows, Principal::Windows { .. })
                | (Os::Linux | Os::Macos, Principal::Posix { .. }) => Ok(()),
                _ => Err(Error::InvalidValue),
            },
            Reply::Acquired {
                intent,
                state: AcquireView::Lease { state, .. },
                ..
            } => {
                let nested_intent = match state {
                    LeaseView::Held { intent }
                    | LeaseView::AwaitingExternalReset { intent }
                    | LeaseView::Quarantined { intent, .. } => intent,
                };
                if intent != nested_intent {
                    return Err(Error::InvalidValue);
                }
                Ok(())
            }
            Reply::Operation {
                state: OperationView::Finished { outcome },
                ..
            } => validate_outcome(outcome),
            Reply::FileBytes {
                offset, bytes, eof, ..
            } => {
                if bytes.as_slice().is_empty() && !eof {
                    return Err(Error::InvalidValue);
                }
                checked_end(*offset, bytes.as_slice().len())
            }
            Reply::NativeDescription { description } => {
                const IDS: [&str; 8] = [
                    "check_base",
                    "check_data",
                    "check_target",
                    "configure",
                    "exercise",
                    "install_base",
                    "install_target",
                    "uninstall",
                ];
                if !description
                    .programs
                    .as_slice()
                    .iter()
                    .map(ProgramId::as_str)
                    .eq(IDS)
                {
                    return Err(Error::InvalidValue);
                }
                match (&description.os, &description.helper_principal) {
                    (Os::Windows, Principal::Windows { .. })
                    | (Os::Linux | Os::Macos, Principal::Posix { .. }) => Ok(()),
                    _ => Err(Error::InvalidValue),
                }
            }
            Reply::NativeFileInfo { length, .. } if *length > MAX_NATIVE_FILE_BYTES => {
                Err(Error::LimitExceeded)
            }
            Reply::FileInfo { length, .. } => bounded_length(*length),
            _ => Ok(()),
        },
        Message::Output { offset, bytes, .. } => {
            if bytes.as_slice().is_empty() {
                return Err(Error::InvalidValue);
            }
            checked_end(*offset, bytes.as_slice().len())
        }
        _ => Ok(()),
    }
}
/// Checks only facts carried by the outcome, without assuming a session OS.
pub(crate) fn validate_outcome(v: &Outcome) -> Result<(), Error> {
    // Infer an OS family only to reuse local validation. This establishes no session OS.
    let os = match v {
        Outcome::RunCompleted {
            status: NativeStatus::WindowsExit { .. },
            ..
        } => Os::Windows,
        Outcome::LaunchFailed {
            cleanup: SpawnCleanup::WindowsProcessExitedJobEmpty,
            ..
        } => Os::Windows,
        _ => Os::Linux,
    };
    validate_outcome_for_os(v, os)
}

/// Checks local outcome facts and their agreement with the independently fixed OS.
pub(crate) fn validate_outcome_for_os(v: &Outcome, os: Os) -> Result<(), Error> {
    let native_windows = match v {
        Outcome::RunCompleted { status, .. } => {
            Some(matches!(status, NativeStatus::WindowsExit { .. }))
        }
        Outcome::LaunchFailed {
            cleanup: SpawnCleanup::NoChildCreated,
            ..
        } => None,
        Outcome::LaunchFailed { cleanup, .. } => Some(matches!(
            cleanup,
            SpawnCleanup::WindowsProcessExitedJobEmpty
        )),
        _ => None,
    };
    if native_windows.is_some_and(|windows| windows != (os == Os::Windows)) {
        return Err(Error::InvalidValue);
    }
    match v {
        Outcome::FileCreated { length, .. } => bounded_length(*length),
        Outcome::RunCompleted {
            status, cleanup, ..
        } => {
            match status {
                NativeStatus::PosixSignal { signal } if !(1..=255).contains(signal) => {
                    return Err(Error::InvalidValue);
                }
                _ => {}
            }
            match (status, cleanup) {
                (NativeStatus::WindowsExit { .. }, Cleanup::WindowsProcessExitedJobEmpty)
                | (
                    NativeStatus::PosixExit { .. } | NativeStatus::PosixSignal { .. },
                    Cleanup::PosixLeaderReapedGroupTerminationRequested,
                ) => Ok(()),
                _ => Err(Error::InvalidValue),
            }
        }
        Outcome::LaunchFailed { phase, cleanup, .. } => match (phase, cleanup) {
            (LaunchPhase::NeverCreated, SpawnCleanup::NoChildCreated)
            | (LaunchPhase::CreatedSuspended, SpawnCleanup::WindowsProcessExitedJobEmpty)
            | (
                LaunchPhase::MayRun,
                SpawnCleanup::PosixLeaderReapedGroupTerminationRequested
                | SpawnCleanup::WindowsProcessExitedJobEmpty,
            ) => Ok(()),
            _ => Err(Error::InvalidValue),
        },
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_native_facts_match_the_fixed_os() {
        for os in [Os::Linux, Os::Macos, Os::Windows] {
            for (status, cleanup, windows) in [
                (
                    NativeStatus::PosixExit { code: 255 },
                    Cleanup::PosixLeaderReapedGroupTerminationRequested,
                    false,
                ),
                (
                    NativeStatus::PosixSignal { signal: 255 },
                    Cleanup::PosixLeaderReapedGroupTerminationRequested,
                    false,
                ),
                (
                    NativeStatus::WindowsExit { code: u32::MAX },
                    Cleanup::WindowsProcessExitedJobEmpty,
                    true,
                ),
            ] {
                let outcome = Outcome::RunCompleted {
                    reason: RunReason::Natural,
                    status,
                    cleanup,
                    stdout_bytes: 0,
                    stderr_bytes: 0,
                    truncated: false,
                };
                assert_eq!(
                    validate_outcome_for_os(&outcome, os).is_ok(),
                    (os == Os::Windows) == windows
                );
            }
            for (phase, cleanup, windows) in [
                (
                    LaunchPhase::NeverCreated,
                    SpawnCleanup::NoChildCreated,
                    None,
                ),
                (
                    LaunchPhase::MayRun,
                    SpawnCleanup::PosixLeaderReapedGroupTerminationRequested,
                    Some(false),
                ),
                (
                    LaunchPhase::MayRun,
                    SpawnCleanup::WindowsProcessExitedJobEmpty,
                    Some(true),
                ),
                (
                    LaunchPhase::CreatedSuspended,
                    SpawnCleanup::WindowsProcessExitedJobEmpty,
                    Some(true),
                ),
            ] {
                let outcome = Outcome::LaunchFailed {
                    phase,
                    cleanup,
                    code: FailureCode::NativeFailure,
                };
                assert_eq!(
                    validate_outcome_for_os(&outcome, os).is_ok(),
                    windows.is_none_or(|windows| (os == Os::Windows) == windows)
                );
            }
            assert!(
                validate_outcome_for_os(
                    &Outcome::NotStarted {
                        reason: NotStartedReason::Deadline
                    },
                    os
                )
                .is_ok()
            );
        }
        assert!(
            validate_outcome_for_os(
                &Outcome::RunCompleted {
                    reason: RunReason::Natural,
                    status: NativeStatus::PosixSignal { signal: 0 },
                    cleanup: Cleanup::PosixLeaderReapedGroupTerminationRequested,
                    stdout_bytes: 0,
                    stderr_bytes: 0,
                    truncated: false
                },
                Os::Linux
            )
            .is_err()
        );
    }
}

/// Immutable config3/store commitment. Component order and domain are protocol constants.
pub fn integration_manifest_digest(core: Digest, integration: Digest) -> Digest {
    use sha2::{Digest as _, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"theseus.native_integration_v1.manifest\0");
    hash.update(core.0);
    hash.update(integration.0);
    Digest::from_bytes(hash.finalize().into())
}

#[cfg(test)]
mod integration_manifest_tests {
    use super::*;
    #[test]
    fn full_integration_manifest_is_domain_separated_and_ordered() {
        let core = Digest::parse(&"02".repeat(32)).unwrap();
        let integration = Digest::parse(&"11".repeat(32)).unwrap();
        assert_eq!(
            integration_manifest_digest(core, integration).to_hex(),
            "cd64ea4df93a58dc89771b1f60cd2b829e3e45e0bdb78650ee6a672cd7971d3c"
        );
        assert_ne!(
            integration_manifest_digest(integration, core),
            integration_manifest_digest(core, integration)
        );
    }
}
