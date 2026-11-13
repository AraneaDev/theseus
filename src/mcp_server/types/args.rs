//! Tool argument DTOs and their serde defaults.
use super::*;

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
    Errors,
    Services,
    Readiness,
    IntegrationChecks,
    IntegrationDispositions,
    Desktop
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
