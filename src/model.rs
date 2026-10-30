//! Stable, serializable identifiers and evidence shared by every interface.
mod interrupted_rollback;
mod json;
pub(crate) mod plugin;
pub use json::{
    JsonAssertionObservation, JsonCommandEvidence, JsonParseStatus, JsonValueObservation,
};
mod rollback;
mod rollback_validation;
pub use interrupted_rollback::{CommandExpectationEvidence, ExecutableExpectationEvidence};
pub(crate) use rollback::rollback_reference;
pub use rollback::{BackupReference, ExpectedFailureEvidence};
use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! named_enum {
    ($name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(match self { $(Self::$variant => $text),+ })
            }
        }
    };
}
named_enum!(ProfileName { User => "user", System => "system" });
named_enum!(ScenarioKind { Clean => "clean", Upgrade => "upgrade", Reinstall => "reinstall", InterruptedInstall => "interrupted_install", InterruptedUpgrade => "interrupted_upgrade", Downgrade => "downgrade", Rollback => "rollback", InterruptedRollback => "interrupted_rollback", PluginLifecycle => "plugin_lifecycle" });
named_enum!(StepName { InstallBase => "install_base", InstallTarget => "install_target", Configure => "configure", Upgrade => "upgrade", Exercise => "exercise", Reinstall => "reinstall", Uninstall => "uninstall", InterruptInstall => "interrupt_install", RecoverInstall => "recover_install", InterruptUpgrade => "interrupt_upgrade", RecoverUpgrade => "recover_upgrade", Downgrade => "downgrade", ExerciseDowngraded => "exercise_downgraded", Backup => "backup", AttemptUpgrade => "attempt_upgrade", Rollback => "rollback", ExerciseRolledBack => "exercise_rolled_back", CleanupBackup => "cleanup_backup", DisablePlugin => "disable_plugin", EnablePlugin => "enable_plugin" });
named_enum!(Checkpoint { BaseInstalled => "base_installed", Configured => "configured", TargetInstalled => "target_installed", Uninstalled => "uninstalled", Reinstalled => "reinstalled", Interrupted => "interrupted", Recovered => "recovered", Downgraded => "downgraded", DowngradeRejected => "downgrade_rejected", KnownGood => "known_good", BackupCreated => "backup_created", UpgradeFailed => "upgrade_failed", RolledBack => "rolled_back", BackupRemoved => "backup_removed", PluginDisabled => "plugin_disabled", PluginEnabled => "plugin_enabled" });
/// One applicability map for selection, orchestration and unperformed evidence.
pub fn checkpoint_applies(scenario: ScenarioKind, checkpoint: Checkpoint) -> bool {
    match checkpoint {
        Checkpoint::BaseInstalled => matches!(
            scenario,
            ScenarioKind::Upgrade
                | ScenarioKind::PluginLifecycle
                | ScenarioKind::InterruptedUpgrade
                | ScenarioKind::Rollback
                | ScenarioKind::InterruptedRollback
        ),
        Checkpoint::TargetInstalled => matches!(
            scenario,
            ScenarioKind::Clean
                | ScenarioKind::Upgrade
                | ScenarioKind::Reinstall
                | ScenarioKind::Downgrade
                | ScenarioKind::PluginLifecycle
        ),
        Checkpoint::Reinstalled => scenario == ScenarioKind::Reinstall,
        Checkpoint::KnownGood
        | Checkpoint::BackupCreated
        | Checkpoint::RolledBack
        | Checkpoint::BackupRemoved => matches!(
            scenario,
            ScenarioKind::Rollback | ScenarioKind::InterruptedRollback
        ),
        Checkpoint::UpgradeFailed => scenario == ScenarioKind::Rollback,
        Checkpoint::Downgraded | Checkpoint::DowngradeRejected => {
            scenario == ScenarioKind::Downgrade
        }
        Checkpoint::Interrupted if scenario == ScenarioKind::InterruptedRollback => true,
        Checkpoint::Interrupted | Checkpoint::Recovered => matches!(
            scenario,
            ScenarioKind::InterruptedInstall | ScenarioKind::InterruptedUpgrade
        ),
        Checkpoint::PluginDisabled | Checkpoint::PluginEnabled => {
            scenario == ScenarioKind::PluginLifecycle
        }
        Checkpoint::Configured => scenario != ScenarioKind::PluginLifecycle,
        Checkpoint::Uninstalled => true,
    }
}
/// Backup checkpoints belong only to the active restoration policy.
/// A profile may share backup commands with a different declared matrix entry.
pub fn checkpoint_applies_with_recovery(
    scenario: ScenarioKind,
    checkpoint: Checkpoint,
    recovery: Option<&crate::recipe::RollbackRecoveryContract>,
) -> bool {
    checkpoint_applies(scenario, checkpoint)
        && (!matches!(
            checkpoint,
            Checkpoint::BackupCreated | Checkpoint::BackupRemoved
        ) || !recovery
            .is_some_and(|p| matches!(p.restoration, crate::recipe::RestorationPolicy::BinaryOnly)))
}
/// Policy-aware applicability; old scenarios retain the two-argument semantics.
pub fn checkpoint_applies_with_policy(
    scenario: ScenarioKind,
    checkpoint: Checkpoint,
    policy: Option<&crate::recipe::DowngradeContract>,
) -> bool {
    checkpoint_applies(scenario, checkpoint)
        && match checkpoint {
            Checkpoint::Downgraded => matches!(
                policy,
                Some(crate::recipe::DowngradeContract::Compatible { .. })
            ),
            Checkpoint::DowngradeRejected => matches!(
                policy,
                Some(crate::recipe::DowngradeContract::SafeRejection { .. })
            ),
            _ => true,
        }
}
named_enum!(FileKind { File => "file", Directory => "directory", Symlink => "symlink" });
named_enum!(CheckOutcome { Passed => "passed", Failed => "failed", Incomplete => "incomplete", Skipped => "skipped" });
named_enum!(FindingKind { Leftover => "leftover", BaselineDeletion => "baseline_deletion", ChangedBaseline => "changed_baseline", ObservationIncomplete => "observation_incomplete" });
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mcp_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<crate::mcp::McpEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_expectation: Option<JsonCommandEvidence>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub json_required: bool,
    pub id: String,
    pub checkpoint: Checkpoint,
    pub outcome: CheckOutcome,
    pub message: String,
    pub path: Option<String>,
    pub command_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comparison: Option<ComparisonReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<BackupReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_expectation: Option<CommandExpectationEvidence>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparisonReference {
    pub before_stage: String,
    pub after_stage: String,
}
impl ComparisonReference {
    pub fn for_checkpoint(checkpoint: Checkpoint) -> Option<Self> {
        match checkpoint {
            Checkpoint::Reinstalled => Some(Self::reinstall()),
            Checkpoint::RolledBack => Some(Self {
                before_stage: "before_upgrade".into(),
                after_stage: "rolled_back".into(),
            }),
            Checkpoint::UpgradeFailed => Some(Self {
                before_stage: "before_attempt".into(),
                after_stage: "upgrade_failed".into(),
            }),
            Checkpoint::Downgraded | Checkpoint::DowngradeRejected => Some(Self {
                before_stage: "before_downgrade".into(),
                after_stage: checkpoint.to_string(),
            }),
            Checkpoint::Interrupted | Checkpoint::Recovered => Some(Self {
                before_stage: "before_interrupt".into(),
                after_stage: checkpoint.to_string(),
            }),
            _ => None,
        }
    }
    pub fn reinstall() -> Self {
        Self {
            before_stage: "before_reinstall".into(),
            after_stage: "reinstalled".into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub kind: FindingKind,
    pub path: Option<String>,
    pub message: String,
}

pub const REPORT_SCHEMA_VERSION: u32 = 8;
pub const JOURNAL_SCHEMA_VERSION: u32 = 1;
named_enum!(RunOutcome { Passed => "passed", Failed => "failed", Incomplete => "incomplete", Cancelled => "cancelled" });
impl RunOutcome {
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Passed => 0,
            Self::Failed => 1,
            Self::Incomplete => 2,
            Self::Cancelled => 130,
        }
    }
    pub fn aggregate(outcomes: impl IntoIterator<Item = Self>) -> Self {
        outcomes
            .into_iter()
            .max_by_key(|o| match o {
                Self::Passed => 0,
                Self::Failed => 1,
                Self::Incomplete => 2,
                Self::Cancelled => 3,
            })
            .unwrap_or(Self::Incomplete)
    }
}
named_enum!(StageOutcome { Pending => "pending", Running => "running", Passed => "passed", Failed => "failed", Incomplete => "incomplete", Cancelled => "cancelled", Skipped => "skipped" });
named_enum!(CommandOutcome { ExpectedFailure => "expected_failure", ExpectedRejection => "expected_rejection", Interrupted => "interrupted", Succeeded => "succeeded", Failed => "failed", TimedOut => "timed_out", Cancelled => "cancelled", Error => "error", Skipped => "skipped" });
named_enum!(InterruptionMechanism { ContainerSigkill => "container_sigkill" });
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterruptionEvidence {
    pub mechanism: InterruptionMechanism,
    pub readiness_verified: bool,
    pub stopped_verified: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectionEvidence {
    pub expected_exit: i32,
    pub stdout_contains: String,
    pub stdout_matched: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestartEvidence {
    pub container_id: String,
    pub running_verified: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TeardownStatus {
    Pending,
    Complete,
    Failed { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionIdentity {
    pub uid: u32,
    pub gid: u32,
    /// Account home used for defaults; effective HOME is recorded in environment
    /// or identified by name in forwarded_environment.
    pub home: String,
}
impl ExecutionIdentity {
    pub fn runtime() -> Self {
        Self {
            uid: 10001,
            gid: 10001,
            home: "/home/theseus".into(),
        }
    }
    pub fn root() -> Self {
        Self {
            uid: 0,
            gid: 0,
            home: "/root".into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogReference {
    pub path: String,
    pub bytes: u64,
    pub truncated: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandRecord {
    pub id: String,
    pub phase: String,
    pub identity: ExecutionIdentity,
    /// Only public literals and public execution defaults. Never forwarded secret values.
    pub environment: std::collections::BTreeMap<String, String>,
    /// Explicitly forwarded names; values are never saved as metadata.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forwarded_environment: Vec<String>,
    pub workdir: String,
    /// Structured argv; a recipe shell script is one argument to /bin/sh -c.
    pub argv: Vec<String>,
    pub started_at: String,
    pub duration_ms: u64,
    pub exit_status: Option<i32>,
    pub timed_out: bool,
    pub cancelled: bool,
    pub outcome: CommandOutcome,
    pub stdout: Option<LogReference>,
    pub stderr: Option<LogReference>,
    pub error: Option<String>,
    pub skip_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interruption: Option<InterruptionEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejection: Option<RejectionEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_failure: Option<ExpectedFailureEvidence>,
}
impl CommandRecord {
    pub fn new(id: impl Into<String>, phase: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            phase: phase.into(),
            identity: ExecutionIdentity::runtime(),
            environment: Default::default(),
            forwarded_environment: Vec::new(),
            workdir: "/workspace".into(),
            argv: Vec::new(),
            started_at: String::new(),
            duration_ms: 0,
            exit_status: None,
            timed_out: false,
            cancelled: false,
            outcome: CommandOutcome::Skipped,
            stdout: None,
            stderr: None,
            error: None,
            skip_reason: None,
            interruption: None,
            rejection: None,
            expected_failure: None,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageReport {
    pub name: String,
    pub step: Option<StepName>,
    pub checkpoint: Option<Checkpoint>,
    pub outcome: StageOutcome,
    pub commands: Vec<CommandRecord>,
    pub checks: Vec<CheckResult>,
    pub snapshots: Vec<crate::snapshot::Snapshot>,
    pub baseline_diff: Vec<crate::snapshot::FileChange>,
    pub stage_diff: Vec<crate::snapshot::FileChange>,
    pub errors: Vec<String>,
    pub skip_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<RestartEvidence>,
}
impl StageReport {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            step: None,
            checkpoint: None,
            outcome: StageOutcome::Pending,
            commands: Vec::new(),
            checks: Vec::new(),
            snapshots: Vec::new(),
            baseline_diff: Vec::new(),
            stage_diff: Vec::new(),
            errors: Vec::new(),
            skip_reason: None,
            restart: None,
        }
    }
    pub fn verified_interruption(&self) -> bool {
        matches!(
            self.step,
            Some(StepName::InterruptInstall | StepName::InterruptUpgrade)
        ) && self.step.is_some_and(|step| self.name == step.to_string())
            && self.commands.len() == 1
            && self.errors.is_empty()
            && self.commands.iter().all(|command| {
                command.phase == self.name
                    && command.expected_failure.is_none()
                    && command.rejection.is_none()
                    && command.outcome == CommandOutcome::Interrupted
                    && command.exit_status.is_none()
                    && command.error.is_none()
                    && !command.timed_out
                    && !command.cancelled
                    && command
                        .interruption
                        .as_ref()
                        .is_some_and(|proof| proof.readiness_verified && proof.stopped_verified)
            })
    }
    pub fn verified_rejection(&self) -> bool {
        self.step == Some(StepName::Downgrade)
            && self.name == "downgrade"
            && self.commands.len() == 1
            && self.errors.is_empty()
            && self.commands.iter().all(|command| {
                command.phase == "downgrade"
                    && command.outcome == CommandOutcome::ExpectedRejection
                    && !command.timed_out
                    && !command.cancelled
                    && command.error.is_none()
                    && command.interruption.is_none()
                    && command.stdout.as_ref().is_none_or(|log| !log.truncated)
                    && command.rejection.as_ref().is_some_and(|proof| {
                        (1..=125).contains(&proof.expected_exit)
                            && command.exit_status == Some(proof.expected_exit)
                            && !proof.stdout_contains.trim().is_empty()
                            && proof.stdout_matched
                    })
            })
    }
    pub fn aggregate_outcome(&self) -> RunOutcome {
        let initial = match self.outcome {
            StageOutcome::Passed | StageOutcome::Skipped => RunOutcome::Passed,
            StageOutcome::Failed => RunOutcome::Failed,
            StageOutcome::Cancelled => RunOutcome::Cancelled,
            _ => RunOutcome::Incomplete,
        };
        let mut outcomes = vec![initial];
        if self.unmatched_rollback_evidence()
            || !self.valid_json_checks()
            || !self.valid_mcp_checks()
        {
            outcomes.push(RunOutcome::Incomplete);
        }
        if self.restart.as_ref().is_some_and(|proof| {
            !proof.running_verified
                || self.name != "restart_after_interrupt"
                || proof.container_id.is_empty()
        }) || (self.name == "restart_after_interrupt"
            && self.outcome != StageOutcome::Skipped
            && self.restart.is_none())
            || !self.errors.is_empty()
            || self.snapshots.iter().any(|s| {
                matches!(
                    s.completeness,
                    crate::snapshot::ObservationStatus::Incomplete { .. }
                )
            })
        {
            outcomes.push(RunOutcome::Incomplete);
        }
        for check in &self.checks {
            outcomes.push(match check.outcome {
                CheckOutcome::Passed | CheckOutcome::Skipped => RunOutcome::Passed,
                CheckOutcome::Failed => RunOutcome::Failed,
                CheckOutcome::Incomplete => RunOutcome::Incomplete,
            });
        }
        for command in &self.commands {
            if command.rejection.is_some()
                && (command.outcome != CommandOutcome::ExpectedRejection
                    || !self.verified_rejection())
            {
                outcomes.push(RunOutcome::Incomplete);
            }
            outcomes.push(if command.cancelled {
                RunOutcome::Cancelled
            } else if command.timed_out || command.error.is_some() {
                RunOutcome::Incomplete
            } else {
                match command.outcome {
                    CommandOutcome::Succeeded | CommandOutcome::Skipped => RunOutcome::Passed,
                    CommandOutcome::Failed => RunOutcome::Failed,
                    CommandOutcome::Cancelled => RunOutcome::Cancelled,
                    CommandOutcome::ExpectedFailure if self.verified_failure() => {
                        RunOutcome::Passed
                    }
                    CommandOutcome::ExpectedRejection if self.verified_rejection() => {
                        RunOutcome::Passed
                    }
                    CommandOutcome::ExpectedRejection | CommandOutcome::ExpectedFailure => {
                        RunOutcome::Incomplete
                    }
                    CommandOutcome::Interrupted if self.verified_interruption() => {
                        RunOutcome::Passed
                    }
                    CommandOutcome::Interrupted
                    | CommandOutcome::TimedOut
                    | CommandOutcome::Error => RunOutcome::Incomplete,
                }
            });
        }
        RunOutcome::aggregate(outcomes)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioReport {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<crate::recipe::PluginContract>,
    pub id: String,
    pub profile: ProfileName,
    pub scenario: ScenarioKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downgrade: Option<crate::recipe::DowngradeContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback: Option<crate::recipe::RollbackContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupted_rollback: Option<crate::recipe::InterruptedRollbackContract>,
    pub outcome: RunOutcome,
    pub container_id: Option<String>,
    pub stages: Vec<StageReport>,
    pub findings: Vec<Finding>,
    pub teardown: TeardownStatus,
    pub errors: Vec<String>,
}
impl ScenarioReport {
    pub(crate) fn rollback_recovery(&self) -> Option<&crate::recipe::RollbackRecoveryContract> {
        self.rollback
            .as_ref()
            .map(|p| &p.recovery)
            .or_else(|| self.interrupted_rollback.as_ref().map(|p| &p.recovery))
    }
    pub fn new(id: impl Into<String>, profile: ProfileName, scenario: ScenarioKind) -> Self {
        Self {
            id: id.into(),
            profile,
            scenario,
            plugin: None,
            downgrade: None,
            rollback: None,
            interrupted_rollback: None,
            outcome: RunOutcome::Incomplete,
            container_id: None,
            stages: Vec::new(),
            findings: Vec::new(),
            teardown: TeardownStatus::Pending,
            errors: Vec::new(),
        }
    }
    pub fn recompute_outcome(&mut self) {
        self.outcome = self.aggregate_outcome();
    }
    pub fn aggregate_outcome(&self) -> RunOutcome {
        let mut outcomes = self
            .stages
            .iter()
            .map(StageReport::aggregate_outcome)
            .collect::<Vec<_>>();
        outcomes.push(plugin::scenario_outcome(self));
        if self.unmatched_rollback_policy() || self.unmatched_interrupted_rollback_policy() {
            outcomes.push(RunOutcome::Incomplete);
        }
        for (index, stage) in self.stages.iter().enumerate() {
            let interrupt_step = match self.scenario {
                ScenarioKind::InterruptedInstall => Some(StepName::InterruptInstall),
                ScenarioKind::InterruptedUpgrade | ScenarioKind::InterruptedRollback => {
                    Some(StepName::InterruptUpgrade)
                }
                _ => None,
            };
            if stage.restart.as_ref().is_some_and(|proof| {
                Some(&proof.container_id) != self.container_id.as_ref()
                    || !self.stages[..index].last().is_some_and(|previous| {
                        previous.step == interrupt_step && previous.verified_interruption()
                    })
            }) || (stage
                .commands
                .iter()
                .any(|command| command.outcome == CommandOutcome::Interrupted)
                && stage.step != interrupt_step)
            {
                outcomes.push(RunOutcome::Incomplete);
            }
        }
        if self.downgrade.is_some() != (self.scenario == ScenarioKind::Downgrade)
            || self
                .downgrade
                .as_ref()
                .is_some_and(|policy| crate::recipe::validate_downgrade_contract(policy).is_err())
        {
            outcomes.push(RunOutcome::Incomplete);
        }
        for stage in &self.stages {
            if stage.checkpoint.is_some_and(|cp| {
                (self.scenario == ScenarioKind::Downgrade
                    || matches!(cp, Checkpoint::Downgraded | Checkpoint::DowngradeRejected))
                    && !checkpoint_applies_with_policy(self.scenario, cp, self.downgrade.as_ref())
            }) || stage.checks.iter().any(|check| {
                (self.scenario == ScenarioKind::Downgrade
                    || matches!(
                        check.checkpoint,
                        Checkpoint::Downgraded | Checkpoint::DowngradeRejected
                    ))
                    && !checkpoint_applies_with_policy(
                        self.scenario,
                        check.checkpoint,
                        self.downgrade.as_ref(),
                    )
            }) {
                outcomes.push(RunOutcome::Incomplete);
            }
            for command in &stage.commands {
                if command.outcome == CommandOutcome::ExpectedRejection
                    || command.rejection.is_some()
                {
                    let matches_policy = match (&self.downgrade, &command.rejection) {
                        (
                            Some(crate::recipe::DowngradeContract::SafeRejection {
                                expected_exit,
                                stdout_contains,
                                ..
                            }),
                            Some(proof),
                        ) => {
                            self.scenario == ScenarioKind::Downgrade
                                && stage.verified_rejection()
                                && proof.expected_exit == *expected_exit
                                && proof.stdout_contains == *stdout_contains
                        }
                        _ => false,
                    };
                    if !matches_policy {
                        outcomes.push(RunOutcome::Incomplete);
                    }
                }
            }
        }
        // Completed successful runs need their policy checkpoint and every named
        // contract. Failed/cancelled runs and intermediate saves may lack them.
        if self.scenario == ScenarioKind::Downgrade
            && matches!(self.teardown, TeardownStatus::Complete)
            && RunOutcome::aggregate(outcomes.iter().copied()) == RunOutcome::Passed
            && let Some(policy) = &self.downgrade
        {
            let (checkpoint, ids) = match policy {
                crate::recipe::DowngradeContract::Compatible { data_checks } => (
                    Checkpoint::Downgraded,
                    data_checks.iter().collect::<Vec<_>>(),
                ),
                crate::recipe::DowngradeContract::SafeRejection {
                    binary_checks,
                    data_checks,
                    ..
                } => (
                    Checkpoint::DowngradeRejected,
                    binary_checks.iter().chain(data_checks).collect(),
                ),
            };
            let checkpoints = self
                .stages
                .iter()
                .filter(|s| s.checkpoint == Some(checkpoint))
                .collect::<Vec<_>>();
            let attempts = self
                .stages
                .iter()
                .filter(|s| s.step == Some(StepName::Downgrade))
                .collect::<Vec<_>>();
            let attempt_valid = attempts.len() == 1
                && attempts[0].outcome == StageOutcome::Passed
                && match policy {
                    crate::recipe::DowngradeContract::SafeRejection { .. } => {
                        attempts[0].verified_rejection()
                    }
                    crate::recipe::DowngradeContract::Compatible { .. } => {
                        attempts[0].name == "downgrade"
                            && attempts[0].commands.len() == 1
                            && attempts[0].commands.iter().all(|command| {
                                command.phase == "downgrade"
                                    && command.outcome == CommandOutcome::Succeeded
                                    && command.exit_status == Some(0)
                                    && command.rejection.is_none()
                            })
                    }
                };
            if !attempt_valid
                || checkpoints.len() != 1
                || checkpoints.iter().any(|s| {
                    s.outcome != StageOutcome::Passed
                        || s.checks
                            .iter()
                            .any(|check| check.outcome != CheckOutcome::Passed)
                })
                || ids.iter().any(|id| {
                    let checks = checkpoints
                        .iter()
                        .flat_map(|s| &s.checks)
                        .filter(|c| &c.id == *id && c.checkpoint == checkpoint)
                        .collect::<Vec<_>>();
                    checks.len() != 1 || checks[0].outcome != CheckOutcome::Passed
                })
            {
                outcomes.push(RunOutcome::Incomplete);
            }
        }
        if self.stages.is_empty()
            || !matches!(self.teardown, TeardownStatus::Complete)
            || !self.errors.is_empty()
            || self
                .findings
                .iter()
                .any(|f| f.kind == FindingKind::ObservationIncomplete)
        {
            outcomes.push(RunOutcome::Incomplete);
        }
        RunOutcome::aggregate(outcomes)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactFileDigest {
    pub path: String,
    pub sha256: String,
    pub size: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactDigest {
    pub name: String,
    pub sha256: String,
    pub files: Vec<ArtifactFileDigest>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunReport {
    pub schema_version: u32,
    pub run_id: String,
    pub recipe_path: String,
    pub recipe_digest: String,
    pub image: String,
    pub image_id: Option<String>,
    pub network: crate::recipe::Network,
    pub artifacts: Vec<ArtifactDigest>,
    pub selected_profiles: Vec<ProfileName>,
    pub selected_scenarios: Vec<ScenarioKind>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub outcome: RunOutcome,
    pub cancelled: bool,
    pub scenarios: Vec<ScenarioReport>,
    pub errors: Vec<String>,
}
impl RunReport {
    pub fn new(run_id: impl Into<String>) -> Self {
        Self {
            schema_version: REPORT_SCHEMA_VERSION,
            run_id: run_id.into(),
            recipe_path: String::new(),
            recipe_digest: String::new(),
            image: String::new(),
            image_id: None,
            network: crate::recipe::Network::None,
            artifacts: Vec::new(),
            selected_profiles: Vec::new(),
            selected_scenarios: Vec::new(),
            started_at: String::new(),
            finished_at: None,
            outcome: RunOutcome::Incomplete,
            cancelled: false,
            scenarios: Vec::new(),
            errors: Vec::new(),
        }
    }
    pub fn recompute_outcome(&mut self) {
        for scenario in &mut self.scenarios {
            scenario.recompute_outcome();
        }
        self.outcome = self.aggregate_outcome();
    }
    pub fn aggregate_outcome(&self) -> RunOutcome {
        let mut outcomes = self
            .scenarios
            .iter()
            .map(ScenarioReport::aggregate_outcome)
            .collect::<Vec<_>>();
        outcomes.push(plugin::artifact_outcome(self));
        if self.cancelled {
            outcomes.push(RunOutcome::Cancelled);
        }
        let missing_matrix_entry = self.selected_profiles.iter().any(|profile| {
            self.selected_scenarios.iter().any(|kind| {
                self.scenarios
                    .iter()
                    .filter(|s| s.profile == *profile && s.scenario == *kind)
                    .count()
                    != 1
            })
        });
        if self.scenarios.is_empty() || !self.errors.is_empty() || missing_matrix_entry {
            outcomes.push(RunOutcome::Incomplete);
        }
        RunOutcome::aggregate(outcomes)
    }
    pub fn counts(&self) -> ResultCounts {
        let mut counts = ResultCounts::default();
        for scenario in &self.scenarios {
            counts.review_findings += scenario.findings.len();
            for stage in &scenario.stages {
                counts.commands += stage.commands.len();
                for check in &stage.checks {
                    match check.outcome {
                        CheckOutcome::Passed => counts.passed_checks += 1,
                        CheckOutcome::Failed => counts.failed_checks += 1,
                        CheckOutcome::Incomplete => counts.incomplete_checks += 1,
                        CheckOutcome::Skipped => counts.skipped_checks += 1,
                    }
                }
            }
        }
        counts
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResultCounts {
    pub passed_checks: usize,
    pub failed_checks: usize,
    pub incomplete_checks: usize,
    pub skipped_checks: usize,
    pub review_findings: usize,
    pub commands: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnedResource {
    pub container_id: String,
    pub labels: std::collections::BTreeMap<String, String>,
    pub removed: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunJournal {
    pub schema_version: u32,
    pub run_id: String,
    pub resources: Vec<OwnedResource>,
    pub cleanup_errors: Vec<String>,
}

impl StageReport {
    pub(crate) fn valid_mcp_checks(&self) -> bool {
        self.checks.iter().all(|check| {
            if check.mcp.is_some() && !check.mcp_required {
                return false;
            }
            if check.mcp.as_ref().is_some_and(|p| !p.valid()) {
                return false;
            }
            if !check.mcp_required || check.outcome != CheckOutcome::Passed {
                return true;
            }
            let Some(proof) = &check.mcp else {
                return false;
            };
            let matching: Vec<_> = self
                .commands
                .iter()
                .filter(|c| check.command_ids.first() == Some(&c.id))
                .collect();
            if check.command_ids.len() != 1 || matching.len() != 1 || !proof.verified_pass() {
                return false;
            }
            let command = matching[0];
            let mut environment = ExecutionIdentity::runtime().public_environment();
            environment.extend(proof.probe.env.clone());
            command.phase == format!("check:{}", check.id)
                && command.identity.uid == 10001
                && command.identity.gid == 10001
                && command.identity.home == "/home/theseus"
                && command.argv == proof.probe.argv
                && command.environment == environment
                && command.forwarded_environment.is_empty()
                && command.outcome == CommandOutcome::Succeeded
                && command.exit_status == Some(0)
                && !command.timed_out
                && !command.cancelled
                && command.error.is_none()
                && command.interruption.is_none()
                && command.expected_failure.is_none()
                && command.rejection.is_none()
                && command
                    .stdout
                    .as_ref()
                    .is_some_and(|log| !log.truncated && log.bytes == proof.stdout_bytes)
                && command.stderr.is_some()
        })
    }
}

impl ExecutionIdentity {
    pub(crate) fn public_environment(&self) -> std::collections::BTreeMap<String, String> {
        std::collections::BTreeMap::from([
            ("HOME".into(), self.home.clone()),
            ("XDG_CONFIG_HOME".into(), format!("{}/.config", self.home)),
            (
                "XDG_DATA_HOME".into(),
                format!("{}/.local/share", self.home),
            ),
            ("XDG_CACHE_HOME".into(), format!("{}/.cache", self.home)),
            (
                "PATH".into(),
                "/home/theseus/.local/bin:/usr/local/bin:/usr/bin:/bin".into(),
            ),
            (
                "USER".into(),
                if self.uid == 0 { "root" } else { "theseus" }.into(),
            ),
            (
                "LOGNAME".into(),
                if self.uid == 0 { "root" } else { "theseus" }.into(),
            ),
        ])
    }
}
