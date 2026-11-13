//! Bounded tool result views.
use super::*;

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
    Service,
    Readiness,
    IntegrationCheck,
    IntegrationDisposition,
    Desktop,
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
    pub integration: Option<crate::native::integration::view::Summary>,
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
