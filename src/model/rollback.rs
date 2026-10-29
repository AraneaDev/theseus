//! Rollback proof metadata and fail-closed context checks. Lifecycle identifiers
//! and matching policy/flow validation are added with the rollback controller.
use super::{CommandOutcome, ScenarioReport, StageReport};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedFailureEvidence {
    pub expected_exit: i32,
    pub stdout_contains: String,
    pub stdout_matched: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupReference {
    pub source_path: String,
    pub stored_path: String,
}
impl StageReport {
    pub(super) fn unmatched_rollback_evidence(&self) -> bool {
        self.commands.iter().any(|command| {
            command.expected_failure.is_some() || command.outcome == CommandOutcome::ExpectedFailure
        }) || self.checks.iter().any(|check| check.backup.is_some())
    }
}
impl ScenarioReport {
    pub(super) fn unmatched_rollback_policy(&self) -> bool {
        self.rollback.is_some()
    }
}

/// Every rollback observation name is reserved behind report schema five,
/// regardless of which side of a comparison contains it.
pub(crate) fn rollback_reference(stage: &str) -> bool {
    matches!(
        stage,
        "before_upgrade"
            | "before_attempt"
            | "before_rollback"
            | "known_good"
            | "backup_created"
            | "upgrade_failed"
            | "rolled_back"
            | "backup_removed"
    )
}
