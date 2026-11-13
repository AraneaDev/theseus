//! Error codes, previews and tool failure projections.
use super::*;

named_enum!(ErrorCode {
    InvalidArguments,
    InvalidCursor,
    UnknownRun,
    UnknownRecord,
    AmbiguousRecord,
    StaleInstance,
    IdempotencyConflict,
    RegistryFull,
    Busy,
    QueryBusy,
    RecipeChanged,
    RevisionChanged,
    LogChanged,
    RecipeInvalid,
    SourceTooLarge,
    UnsafePath,
    RootChanged,
    QueryTimeout,
    IoError,
    EvidenceInvalid,
    CancelledBeforeStart,
    WorkerError,
    CleanupPending,
    ResponseTooLarge,
    InternalError
});
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ErrorPolicy {
    pub retryable: bool,
    pub restart_pagination: bool,
}
impl ErrorCode {
    pub fn policy(self) -> ErrorPolicy {
        ErrorPolicy {
            retryable: matches!(
                self,
                Self::Busy
                    | Self::QueryBusy
                    | Self::QueryTimeout
                    | Self::IoError
                    | Self::EvidenceInvalid
            ),
            restart_pagination: matches!(
                self,
                Self::InvalidCursor
                    | Self::RecipeChanged
                    | Self::RevisionChanged
                    | Self::LogChanged
            ),
        }
    }
}
#[derive(Debug)]
pub(crate) struct QueryError {
    pub code: ErrorCode,
    pub source: anyhow::Error,
}
impl QueryError {
    pub fn new(code: ErrorCode, source: anyhow::Error) -> Self {
        Self { code, source }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Preview<const LIMIT: usize> {
    pub text: String,
    pub truncated: bool,
    pub lossy: bool,
}
pub(crate) type TextPreview = Preview<512>;
pub(crate) type ShortPreview = Preview<128>;
impl<const LIMIT: usize> Preview<LIMIT> {
    pub fn new(value: &str, lossy: bool) -> Self {
        let mut end = value.len().min(LIMIT);
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        Self {
            text: value[..end].into(),
            truncated: end < value.len(),
            lossy,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Counts {
    pub passed_checks: UInt,
    pub failed_checks: UInt,
    pub incomplete_checks: UInt,
    pub skipped_checks: UInt,
    pub review_findings: UInt,
    pub commands: UInt,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FailureError {
    pub code: ErrorCode,
    pub message: TextPreview,
    pub retryable: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub handle: Option<Handle>,
    pub restart_pagination: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Failure {
    pub format_version: u8,
    pub server_instance: Instance,
    pub ok: bool,
    pub error: FailureError,
}
impl Failure {
    pub fn new(
        instance: Instance,
        code: ErrorCode,
        message: TextPreview,
        handle: Option<Handle>,
    ) -> Self {
        let policy = code.policy();
        Self {
            format_version: 1,
            server_instance: instance,
            ok: false,
            error: FailureError {
                code,
                message,
                retryable: policy.retryable,
                handle,
                restart_pagination: policy.restart_pagination,
            },
        }
    }
}
pub(crate) type ToolResult = Result<ToolData, Failure>;
