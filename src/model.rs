//! Stable, serializable identifiers and evidence shared by every interface.
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
named_enum!(ScenarioKind { Clean => "clean", Upgrade => "upgrade" });
named_enum!(StepName { InstallBase => "install_base", InstallTarget => "install_target", Configure => "configure", Upgrade => "upgrade", Exercise => "exercise", Uninstall => "uninstall" });
named_enum!(Checkpoint { BaseInstalled => "base_installed", Configured => "configured", TargetInstalled => "target_installed", Uninstalled => "uninstalled" });
named_enum!(FileKind { File => "file", Directory => "directory", Symlink => "symlink" });
named_enum!(CheckOutcome { Passed => "passed", Failed => "failed", Incomplete => "incomplete", Skipped => "skipped" });
named_enum!(FindingKind { Leftover => "leftover", BaselineDeletion => "baseline_deletion", ChangedBaseline => "changed_baseline", ObservationIncomplete => "observation_incomplete" });
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    pub id: String,
    pub checkpoint: Checkpoint,
    pub outcome: CheckOutcome,
    pub message: String,
    pub path: Option<String>,
    pub command_ids: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub kind: FindingKind,
    pub path: Option<String>,
    pub message: String,
}

pub const REPORT_SCHEMA_VERSION: u32 = 1;
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
named_enum!(CommandOutcome { Succeeded => "succeeded", Failed => "failed", TimedOut => "timed_out", Cancelled => "cancelled", Error => "error", Skipped => "skipped" });
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
}
impl CommandRecord {
    pub fn new(id: impl Into<String>, phase: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            phase: phase.into(),
            identity: ExecutionIdentity::runtime(),
            environment: Default::default(),
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
        }
    }
    pub fn aggregate_outcome(&self) -> RunOutcome {
        let initial = match self.outcome {
            StageOutcome::Passed | StageOutcome::Skipped => RunOutcome::Passed,
            StageOutcome::Failed => RunOutcome::Failed,
            StageOutcome::Cancelled => RunOutcome::Cancelled,
            _ => RunOutcome::Incomplete,
        };
        let mut outcomes = vec![initial];
        if !self.errors.is_empty()
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
            outcomes.push(if command.cancelled {
                RunOutcome::Cancelled
            } else if command.timed_out || command.error.is_some() {
                RunOutcome::Incomplete
            } else {
                match command.outcome {
                    CommandOutcome::Succeeded | CommandOutcome::Skipped => RunOutcome::Passed,
                    CommandOutcome::Failed => RunOutcome::Failed,
                    CommandOutcome::Cancelled => RunOutcome::Cancelled,
                    CommandOutcome::TimedOut | CommandOutcome::Error => RunOutcome::Incomplete,
                }
            });
        }
        RunOutcome::aggregate(outcomes)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioReport {
    pub id: String,
    pub profile: ProfileName,
    pub scenario: ScenarioKind,
    pub outcome: RunOutcome,
    pub container_id: Option<String>,
    pub stages: Vec<StageReport>,
    pub findings: Vec<Finding>,
    pub teardown: TeardownStatus,
    pub errors: Vec<String>,
}
impl ScenarioReport {
    pub fn new(id: impl Into<String>, profile: ProfileName, scenario: ScenarioKind) -> Self {
        Self {
            id: id.into(),
            profile,
            scenario,
            outcome: RunOutcome::Incomplete,
            container_id: None,
            stages: Vec::new(),
            findings: Vec::new(),
            teardown: TeardownStatus::Pending,
            errors: Vec::new(),
        }
    }
    pub fn recompute_outcome(&mut self) {
        let mut outcomes = self
            .stages
            .iter()
            .map(StageReport::aggregate_outcome)
            .collect::<Vec<_>>();
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
        self.outcome = RunOutcome::aggregate(outcomes);
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
        let mut outcomes = self.scenarios.iter().map(|s| s.outcome).collect::<Vec<_>>();
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
        self.outcome = RunOutcome::aggregate(outcomes);
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
