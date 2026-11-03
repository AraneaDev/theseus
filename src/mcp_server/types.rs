//! Bounded public projections and private ownership-preserving worker handoffs.
use crate::model::{ProfileName, ResultCounts, RunOutcome, ScenarioKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fmt, path::PathBuf};
use theseus_host::fs::RelativePath;

pub(crate) const UINT_MAX: u64 = 9_007_199_254_740_991;
pub(crate) const LEGACY_VERSION: &str = "2025-11-25";
pub(crate) const MODERN_VERSION: &str = "2026-07-28";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ArgumentError;
impl fmt::Display for ArgumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid arguments")
    }
}
impl std::error::Error for ArgumentError {}

macro_rules! text_type {
    ($($name:ident),+ $(,)?) => {$(
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub(crate) struct $name(String);
        impl $name {
            pub fn parse(value: &str) -> Result<Self, ArgumentError> {
                if valid_text(stringify!($name), value) { Ok(Self(value.into())) } else { Err(ArgumentError) }
            }
            #[allow(dead_code, reason = "Validated text tokens share a uniform borrowing accessor.")]
            pub fn as_str(&self) -> &str { &self.0 }
        }
        impl TryFrom<String> for $name {
            type Error = ArgumentError;
            fn try_from(value: String) -> Result<Self, Self::Error> { Self::parse(&value) }
        }
        impl From<$name> for String { fn from(value: $name) -> Self { value.0 } }
    )+};
}
text_type!(Instance, Handle, Digest, Revision, Key, RecordId, Cursor);
fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn decimal(value: &str) -> bool {
    !value.is_empty()
        && (value == "0" || !value.starts_with('0'))
        && value.bytes().all(|b| b.is_ascii_digit())
}
fn valid_record(value: &str) -> bool {
    let c: Vec<_> = value.split('/').collect();
    match c.as_slice() {
        ["run" | "native_recipe"] => true,
        ["native", "leaves" | "limits"] => true,
        ["native", "artifact", "base" | "target"] => true,
        ["native", "check", i] => decimal(i),
        ["native", "step", step] => {
            serde_json::from_value::<crate::model::StepName>(Value::String((*step).into())).is_ok()
        }
        ["native_identity" | "native_disposition", i] => decimal(i),
        ["artifact", "base" | "target"] => true,
        ["env" | "observe" | "error" | "artifact", i] | ["s", i] => decimal(i),
        ["artifact", a, "file", b] => decimal(a) && decimal(b),
        ["p", "user" | "system", "check", i] => decimal(i),
        ["p", "user" | "system", "step", step] => {
            serde_json::from_value::<crate::model::StepName>(Value::String((*step).into())).is_ok()
        }
        [
            "p",
            "user" | "system",
            "policy",
            "mcp" | "plugin" | "downgrade" | "rollback" | "interrupted_rollback",
        ] => true,
        ["s", s, "stage" | "finding" | "error", i] => decimal(s) && decimal(i),
        [
            "s",
            s,
            "stage",
            t,
            "command" | "check" | "snapshot" | "baseline" | "change" | "error" | "native_file",
            i,
        ] => decimal(s) && decimal(t) && decimal(i),
        ["s", s, "stage", t, "snapshot", i, "entry", j] => {
            decimal(s) && decimal(t) && decimal(i) && decimal(j)
        }
        _ => false,
    }
}
fn valid_text(kind: &str, value: &str) -> bool {
    match kind {
        "Instance" => hex(value, 32),
        "Digest" | "Revision" => hex(value, 64),
        "Handle" => value.split_once(':').is_some_and(|(i, s)| {
            hex(i, 32) && decimal(s) && s.parse::<u8>().is_ok_and(|n| (1..=128).contains(&n))
        }),
        "Key" => !value.is_empty() && value.len() <= 128 && !value.contains('\0'),
        "RecordId" => value.len() <= 128 && value.is_ascii() && valid_record(value),
        "Cursor" => value.len() <= 256 && value.is_ascii(),
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub(crate) struct UInt(u64);
impl UInt {
    pub fn new(value: u64) -> Result<Self, ArgumentError> {
        if value <= UINT_MAX {
            Ok(Self(value))
        } else {
            Err(ArgumentError)
        }
    }
    pub fn get(self) -> u64 {
        self.0
    }
}
impl TryFrom<u64> for UInt {
    type Error = ArgumentError;
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<UInt> for u64 {
    fn from(value: UInt) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum RpcId {
    String(String),
    Integer(i64),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Era {
    Legacy,
    Modern,
}
#[derive(Debug, Clone)]
pub(crate) struct ReplyContext {
    pub era: Era,
    pub id: RpcId,
    pub instance: Instance,
}

macro_rules! named_enum {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub(crate) enum $name { $($variant),+ }
    };
}
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

fn required_nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(d)
}

fn optional_non_null<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[derive(Default)]
pub(crate) enum InspectSelector {
    #[default]
    Overview,
    Catalog {
        collection: InspectCollection,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "optional_non_null"
        )]
        profile: Option<ProfileName>,
    },
    Record {
        record_id: RecordId,
    },
}
named_enum!(InspectCollection {
    Steps,
    Checks,
    Artifacts,
    Environment,
    ObservedRoots,
    Policies
});
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InspectArgs {
    pub recipe_path: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    pub recipe_digest: Option<Digest>,
    #[serde(default)]
    pub selector: InspectSelector,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    pub limit: Option<UInt>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    pub cursor: Option<Cursor>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StartArgs {
    pub server_instance: Instance,
    pub request_key: Key,
    pub recipe_path: String,
    pub recipe_digest: Digest,
    #[serde(
        default,
        deserialize_with = "explicit_profiles",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub profiles: Vec<ProfileName>,
    pub scenarios: Vec<ScenarioKind>,
    #[serde(default)]
    pub pull: bool,
    #[serde(default = "default_log_limit")]
    pub log_limit: UInt,
}
fn explicit_profiles<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> Result<Vec<ProfileName>, D::Error> {
    let profiles = Vec::<ProfileName>::deserialize(decoder)?;
    if profiles.is_empty() {
        return Err(serde::de::Error::custom(
            "omit profiles for the native current-helper role",
        ));
    }
    Ok(profiles)
}
fn default_log_limit() -> UInt {
    UInt(16_777_216)
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListArgs {
    pub server_instance: Instance,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    pub limit: Option<UInt>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    pub cursor: Option<Cursor>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StatusArgs {
    pub server_instance: Instance,
    pub handle: Handle,
    #[serde(default = "default_refresh")]
    pub refresh: bool,
}
fn default_refresh() -> bool {
    true
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CancelArgs {
    pub server_instance: Instance,
    pub handle: Handle,
}

// Splitting catalogs by their required coordinates makes invalid selector combinations unrepresentable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum EvidenceSelector {
    Root(RootCatalog),
    Scenario(ScenarioCatalog),
    Stage(StageCatalog),
    Artifact(ArtifactCatalog),
    Snapshot(SnapshotCatalog),
    Record(RecordSelector),
    Log(LogSelector),
}
named_enum!(CatalogKind { Catalog });
named_enum!(RecordSelectorKind { Record });
named_enum!(LogKind { Log });
named_enum!(RootCollection {
    Run,
    Scenarios,
    Artifacts,
    Errors
});
named_enum!(ScenarioCollection {
    Stages,
    Findings,
    ScenarioErrors
});
named_enum!(StageCollection {
    Commands,
    Checks,
    Snapshots,
    BaselineChanges,
    StageChanges,
    StageErrors,
    NativeFiles
});
named_enum!(ArtifactCollection { ArtifactFiles });
named_enum!(SnapshotCollection { SnapshotEntries });
named_enum!(Stream { Stdout, Stderr });
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RootCatalog {
    pub kind: CatalogKind,
    pub collection: RootCollection,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScenarioCatalog {
    pub kind: CatalogKind,
    pub collection: ScenarioCollection,
    pub scenario_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StageCatalog {
    pub kind: CatalogKind,
    pub collection: StageCollection,
    pub scenario_id: String,
    pub stage_index: UInt,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArtifactCatalog {
    pub kind: CatalogKind,
    pub collection: ArtifactCollection,
    pub artifact_index: UInt,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SnapshotCatalog {
    pub kind: CatalogKind,
    pub collection: SnapshotCollection,
    pub scenario_id: String,
    pub stage_index: UInt,
    pub snapshot_index: UInt,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordSelector {
    pub kind: RecordSelectorKind,
    pub record_id: RecordId,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LogSelector {
    pub kind: LogKind,
    pub command_id: String,
    pub stream: Stream,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EvidenceArgs {
    pub server_instance: Instance,
    pub handle: Handle,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    pub revision: Option<Revision>,
    pub selector: EvidenceSelector,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    pub limit: Option<UInt>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    pub cursor: Option<Cursor>,
}
#[derive(Debug, Clone)]
pub(crate) enum ToolCall {
    RecipeInspect(InspectArgs),
    RunStart(StartArgs),
    RunList(ListArgs),
    RunStatus(StatusArgs),
    RunCancel(CancelArgs),
    RunEvidence(EvidenceArgs),
}
fn path_valid(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && RelativePath::parse(path).is_ok()
        && path.split('/').all(|component| {
            if component.ends_with(['.', ' '])
                || component
                    .chars()
                    .any(|c| c < ' ' || "\\:<>\"|?*".contains(c))
            {
                return false;
            }
            let stem = component
                .split('.')
                .next()
                .unwrap_or("")
                .to_ascii_uppercase();
            !matches!(
                stem.as_str(),
                "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
            ) && !["COM", "LPT"].iter().any(|prefix| {
                stem.strip_prefix(prefix).is_some_and(|n| {
                    matches!(
                        n,
                        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                    )
                })
            })
        })
}
fn bounded_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 4096
}
fn limit_valid(limit: Option<UInt>, maximum: u64) -> bool {
    limit.is_none_or(|n| (1..=maximum).contains(&n.get()))
}
fn same_instance(instance: &Instance, handle: &Handle) -> bool {
    handle
        .as_str()
        .split_once(':')
        .is_some_and(|(i, _)| i == instance.as_str())
}
impl ToolCall {
    pub fn validate(&self) -> Result<(), ArgumentError> {
        let valid = match self {
            Self::RecipeInspect(a) => {
                path_valid(&a.recipe_path)
                    && match &a.selector {
                        InspectSelector::Overview => a.limit.is_none() && a.cursor.is_none(),
                        InspectSelector::Catalog {
                            collection,
                            profile,
                        } => {
                            a.recipe_digest.is_some()
                                && limit_valid(a.limit, 32)
                                && (profile.is_none()
                                    || !matches!(
                                        collection,
                                        InspectCollection::Artifacts
                                            | InspectCollection::ObservedRoots
                                    ))
                        }
                        InspectSelector::Record { .. } => {
                            a.recipe_digest.is_some() && limit_valid(a.limit, 4096)
                        }
                    }
            }
            Self::RunStart(a) => {
                path_valid(&a.recipe_path)
                    && a.profiles.len() <= 2
                    && (1..=9).contains(&a.scenarios.len())
                    && a.profiles
                        .iter()
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        == a.profiles.len()
                    && a.scenarios
                        .iter()
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        == a.scenarios.len()
                    && (1..=16_777_216).contains(&a.log_limit.get())
            }
            Self::RunList(a) => limit_valid(a.limit, 32),
            Self::RunStatus(a) => same_instance(&a.server_instance, &a.handle),
            Self::RunCancel(a) => same_instance(&a.server_instance, &a.handle),
            Self::RunEvidence(a) => {
                same_instance(&a.server_instance, &a.handle)
                    && (a.cursor.is_none() || a.revision.is_some())
                    && match &a.selector {
                        EvidenceSelector::Root(_) | EvidenceSelector::Artifact(_) => {
                            limit_valid(a.limit, 32)
                        }
                        EvidenceSelector::Scenario(s) => {
                            bounded_id(&s.scenario_id) && limit_valid(a.limit, 32)
                        }
                        EvidenceSelector::Stage(s) => {
                            bounded_id(&s.scenario_id) && limit_valid(a.limit, 32)
                        }
                        EvidenceSelector::Snapshot(s) => {
                            bounded_id(&s.scenario_id) && limit_valid(a.limit, 32)
                        }
                        EvidenceSelector::Record(_) => {
                            a.revision.is_some() && limit_valid(a.limit, 4096)
                        }
                        EvidenceSelector::Log(s) => {
                            bounded_id(&s.command_id)
                                && a.revision.is_some()
                                && limit_valid(a.limit, 4096)
                        }
                    }
            }
        };
        if valid { Ok(()) } else { Err(ArgumentError) }
    }
}

named_enum!(RunState {
    Validating,
    Admitted,
    Running,
    Finished,
    StartError,
    WorkerError
});
named_enum!(CancellationReason {
    Client,
    RequestCancelled,
    RuntimeLimit,
    ServerShutdown
});
named_enum!(NativeCleanup {
    NotStarted,
    InProgress,
    Complete,
    Pending,
    Unknown
});
named_enum!(GuestCleanup {
    NotStarted,
    InProgress,
    Complete,
    Failed,
    Unknown
});
named_enum!(Launch {
    NeverCreated,
    CreatedSuspended,
    MayRun
});
named_enum!(Refresh {
    Loaded,
    Cached,
    Busy,
    Unavailable
});
named_enum!(Lifetime { ServerProcess });
named_enum!(Network { None, Bridge });
named_enum!(Encoding { JsonUtf8 });
named_enum!(RecordKind {
    NativeRecipe,
    NativeFile,
    NativeDisposition,
    NativeIdentity,
    RecipeStep,
    RecipeCheck,
    RecipeArtifact,
    RecipeEnvironment,
    RecipeObservedRoot,
    RecipePolicy,
    RunHeader,
    ScenarioHeader,
    StageHeader,
    Command,
    Check,
    Finding,
    ArtifactHeader,
    ArtifactFile,
    SnapshotHeader,
    SnapshotEntry,
    BaselineChange,
    StageChange,
    Error
});
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CleanupView {
    pub native: NativeCleanup,
    pub guest: GuestCleanup,
    #[serde(deserialize_with = "required_nullable")]
    pub launch: Option<Launch>,
    pub recovery_required: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum NativeDispositionView {
    NotStarted,
    Complete { receipt: TextPreview },
    Unknown { reason: TextPreview },
    Failed { reason: TextPreview },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeScenarioDispositions {
    pub scenario_id: TextPreview,
    pub local: NativeDispositionView,
    pub remote: NativeDispositionView,
    pub release: NativeDispositionView,
    pub reset: NativeDispositionView,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SavedView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_dispositions: Option<Vec<NativeScenarioDispositions>>,
    pub revision: Revision,
    pub schema_version: u8,
    pub outcome: RunOutcome,
    pub counts: Counts,
    #[serde(deserialize_with = "required_nullable")]
    pub finished_at: Option<TextPreview>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EngineResult {
    pub outcome: RunOutcome,
    pub counts: Counts,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunView {
    pub handle: Handle,
    pub admission_sequence: u8,
    pub state: RunState,
    pub cancellation_requested: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub cancellation_reason: Option<CancellationReason>,
    #[serde(deserialize_with = "required_nullable")]
    pub report_id: Option<TextPreview>,
    #[serde(deserialize_with = "required_nullable")]
    pub report_path: Option<TextPreview>,
    #[serde(deserialize_with = "required_nullable")]
    pub journal_path: Option<TextPreview>,
    #[serde(deserialize_with = "required_nullable")]
    pub saved: Option<SavedView>,
    #[serde(deserialize_with = "required_nullable")]
    pub engine_result: Option<EngineResult>,
    pub saved_hint_sequence: UInt,
    #[serde(deserialize_with = "required_nullable")]
    pub snapshot_hint_sequence: Option<UInt>,
    pub cleanup: CleanupView,
    #[serde(deserialize_with = "required_nullable")]
    pub error: Option<FailureError>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InspectArtifacts {
    #[serde(deserialize_with = "required_nullable")]
    pub base: Option<TextPreview>,
    pub target: TextPreview,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OverviewData {
    pub kind: OverviewKind,
    pub recipe_path: String,
    pub recipe_digest: Digest,
    pub schema_version: u8,
    pub name: TextPreview,
    pub image: TextPreview,
    pub network: Network,
    pub profiles: Vec<ProfileName>,
    pub scenarios: Vec<ScenarioKind>,
    pub artifacts: InspectArtifacts,
    pub lifetime: Lifetime,
    pub max_run_seconds: UInt,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeOverviewData {
    pub kind: NativeOverviewKind,
    pub recipe_path: String,
    pub recipe_digest: Digest,
    pub schema: u8,
    pub os: crate::native::recipe::NativeOs,
    pub role: String,
    pub scenarios: Vec<ScenarioKind>,
    pub lifetime: Lifetime,
    pub max_run_seconds: UInt,
}
named_enum!(NativeOverviewKind { NativeOverview });
named_enum!(OverviewKind { Overview });
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StartData {
    pub handle: Handle,
    pub state: RunState,
    pub replayed: bool,
    pub cancellation_requested: bool,
    pub lifetime: Lifetime,
    pub poll_after_ms: u16,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListData {
    pub runs: Vec<RunView>,
    pub high_water: u8,
    #[serde(deserialize_with = "required_nullable")]
    pub next_cursor: Option<Cursor>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StatusData {
    pub run: RunView,
    pub refresh: Refresh,
    pub poll_after_ms: u16,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CancelData {
    pub run: RunView,
    pub requested_now: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordSummary {
    pub record_id: RecordId,
    pub record_kind: RecordKind,
    #[serde(deserialize_with = "required_nullable")]
    pub source_id: Option<ShortPreview>,
    pub summary: ShortPreview,
    #[serde(deserialize_with = "required_nullable")]
    pub outcome: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CatalogPage {
    pub kind: CatalogKind,
    pub revision: Revision,
    pub records: Vec<RecordSummary>,
    #[serde(deserialize_with = "required_nullable")]
    pub next_cursor: Option<Cursor>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordPage {
    pub kind: RecordSelectorKind,
    pub revision: Revision,
    pub record_id: RecordId,
    pub record_kind: RecordKind,
    pub encoding: Encoding,
    pub offset: UInt,
    pub bytes: Vec<u8>,
    pub total_bytes: UInt,
    #[serde(deserialize_with = "required_nullable")]
    pub next_cursor: Option<Cursor>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LogPage {
    pub kind: LogKind,
    pub revision: Revision,
    pub command_id: String,
    pub stream: Stream,
    pub offset: UInt,
    pub bytes: Vec<u8>,
    pub recorded_bytes: UInt,
    pub current_bytes: UInt,
    pub readable_total: UInt,
    pub capture_truncated: bool,
    pub change_token: String,
    #[serde(deserialize_with = "required_nullable")]
    pub next_cursor: Option<Cursor>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum InspectData {
    NativeOverview(NativeOverviewData),
    Overview(OverviewData),
    Catalog(CatalogPage),
    Record(RecordPage),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum EvidenceData {
    Catalog(CatalogPage),
    Record(RecordPage),
    Log(LogPage),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum ToolData {
    RecipeInspect(InspectData),
    RunStart(StartData),
    RunList(ListData),
    RunStatus(StatusData),
    RunCancel(CancelData),
    RunEvidence(EvidenceData),
}

#[derive(Debug, Clone)]
pub(crate) struct SavedBinding {
    pub handle: Handle,
    pub report_id: String,
    pub child: RelativePath,
    pub hint_sequence: u64,
}
#[derive(Debug)]
pub(crate) struct FinalSummary {
    pub outcome: RunOutcome,
    pub counts: ResultCounts,
    pub report_path: PathBuf,
    pub journal_path: PathBuf,
}

#[derive(Debug)]
pub(crate) enum Method {
    Initialize,
    Discover,
    Ping,
    ToolsList,
    ToolCall(ToolCall),
    RejectedTool(ArgumentError),
}
#[derive(Debug)]
pub(crate) struct Request {
    pub id: RpcId,
    pub era: Era,
    pub method: Method,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Notification {
    Initialized,
    Cancel(RpcId),
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReplyId {
    Id(RpcId),
    Null,
    Omit,
}
#[derive(Debug)]
pub(crate) struct VersionData {
    pub requested: String,
    pub supported: Vec<String>,
}
#[derive(Debug)]
pub(crate) struct ProtocolError {
    pub code: i32,
    pub message: TextPreview,
    pub data: Option<VersionData>,
    pub id: ReplyId,
    pub close_after_send: bool,
}
#[derive(Debug)]
pub(crate) enum WireEvent {
    Request(Request),
    Notification(Notification),
    ProtocolError(ProtocolError),
    Ignore,
}
