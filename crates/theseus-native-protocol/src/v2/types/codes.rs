//! Closed protocol code enums.
use super::SidText;
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
/// Closed, fixed classification of a refused manager inspection step. It
/// names only which helper step failed; it never carries native text, error
/// codes, paths or identities, and never changes the refusal semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InspectFailure {
    SlotUndeclared,
    ReadinessBind,
    ReadinessRenewal,
    ManagerConnect,
    UninstallTransition,
    TransitionDefinition,
    TransitionAdmission,
    DeletionProgress,
    ManagerQuery,
    RegistrationQualification,
    ActivationPid,
    ObservationBinding,
    Unclassified,
}
impl InspectFailure {
    pub const ALL: [Self; 13] = [
        Self::SlotUndeclared,
        Self::ReadinessBind,
        Self::ReadinessRenewal,
        Self::ManagerConnect,
        Self::UninstallTransition,
        Self::TransitionDefinition,
        Self::TransitionAdmission,
        Self::DeletionProgress,
        Self::ManagerQuery,
        Self::RegistrationQualification,
        Self::ActivationPid,
        Self::ObservationBinding,
        Self::Unclassified,
    ];
    pub const fn name(self) -> &'static str {
        match self {
            Self::SlotUndeclared => "slot_undeclared",
            Self::ReadinessBind => "readiness_bind",
            Self::ReadinessRenewal => "readiness_renewal",
            Self::ManagerConnect => "manager_connect",
            Self::UninstallTransition => "uninstall_transition",
            Self::TransitionDefinition => "transition_definition",
            Self::TransitionAdmission => "transition_admission",
            Self::DeletionProgress => "deletion_progress",
            Self::ManagerQuery => "manager_query",
            Self::RegistrationQualification => "registration_qualification",
            Self::ActivationPid => "activation_pid",
            Self::ObservationBinding => "observation_binding",
            Self::Unclassified => "unclassified",
        }
    }
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
