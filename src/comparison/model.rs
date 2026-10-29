//! Serializable suite declarations and evidence. Persistence validates source refs separately.
use crate::{
    model::{ProfileName, RunOutcome, RunReport, ScenarioKind},
    snapshot::FileChange,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const COMPARISON_SCHEMA_VERSION: u32 = 1;
pub const MAX_MANIFEST_BYTES: u64 = 1_048_576;
pub const MAX_COMPARISON_CELLS: usize = 8192;
pub const MAX_CELL_DIFFERENCES: usize = 10_000;
pub const MAX_SUITE_DIFFERENCES: usize = 100_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuiteManifest {
    pub schema_version: u32,
    pub name: String,
    pub baseline: String,
    pub profiles: Vec<ProfileName>,
    pub scenarios: Vec<ScenarioKind>,
    pub variants: Vec<Variant>,
    pub scopes: Vec<Scope>,
    pub stages: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Variant {
    pub id: String,
    pub recipe: PathBuf,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub id: String,
    pub profile: ProfileName,
    pub path: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonStatus {
    Equivalent,
    Different,
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeComparison {
    pub status: ComparisonStatus,
    /// Relative keys beneath the scope; the scope object itself has key `.`.
    pub differences: Vec<FileChange>,
    pub diagnostics: Vec<String>,
}
impl ScopeComparison {
    pub(crate) fn unknown(reason: impl Into<String>) -> Self {
        Self {
            status: ComparisonStatus::Unknown,
            differences: vec![],
            diagnostics: vec![reason.into()],
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceReport {
    pub variant_id: String,
    pub report: Option<RunReport>,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComparisonCell {
    pub variant_id: String,
    pub profile: ProfileName,
    pub scenario: ScenarioKind,
    pub stage: String,
    pub scope_id: String,
    pub comparison: ScopeComparison,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComparisonReport {
    pub cells: Vec<ComparisonCell>,
    pub outcome: RunOutcome,
    pub diagnostics: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceReference {
    pub variant_id: String,
    /// Fixed relative form `sources/<variant>/<run-id>`; validated by the store.
    pub path: String,
    pub report_digest: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComparisonArtifact {
    pub schema_version: u32,
    pub suite_id: String,
    pub manifest: SuiteManifest,
    pub sources: Vec<SourceReference>,
    pub report: ComparisonReport,
    pub cancelled: bool,
}
