//! Rollback policy/proof validation shared by live aggregation and offline loading.
use super::rollback_validation::*;
use super::*;
use crate::recipe::{RestorationPolicy, RollbackContract};
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
    pub fn verified_failure(&self) -> bool {
        self.step == Some(StepName::AttemptUpgrade)
            && self.name == "attempt_upgrade"
            && self.errors.is_empty()
            && self.commands.len() == 1
            && self.commands.iter().all(|c| {
                valid_invocation(&c.argv, Invocation::ShellScript)
                    && c.phase == "attempt_upgrade"
                    && c.outcome == CommandOutcome::ExpectedFailure
                    && !c.cancelled
                    && !c.timed_out
                    && c.error.is_none()
                    && c.interruption.is_none()
                    && c.rejection.is_none()
                    && c.stdout.as_ref().is_none_or(|log| !log.truncated)
                    && c.expected_failure.as_ref().is_some_and(|p| {
                        (1..=125).contains(&p.expected_exit)
                            && c.exit_status == Some(p.expected_exit)
                            && p.stdout_matched
                            && !p.stdout_contains.trim().is_empty()
                    })
            })
    }
    pub(super) fn unmatched_rollback_evidence(&self) -> bool {
        self.commands.iter().any(|c| {
            (c.expected_failure.is_some() || c.outcome == CommandOutcome::ExpectedFailure)
                && !self.verified_failure()
        }) || self.checks.iter().any(|c| {
            c.backup.is_some()
                && (!c.id.starts_with("__theseus_rollback_")
                    || c.comparison.as_ref().is_none_or(|r| {
                        r.before_stage != "before_upgrade" || r.after_stage != self.name
                    }))
        })
    }
}
impl ScenarioReport {
    pub(super) fn unmatched_rollback_policy(&self) -> bool {
        if self.scenario == ScenarioKind::InterruptedRollback {
            return self.rollback.is_some();
        }
        if self.scenario != ScenarioKind::Rollback {
            return self.rollback.is_some()
                || self.stages.iter().any(|s| {
                    matches!(
                        s.step,
                        Some(
                            StepName::Backup
                                | StepName::AttemptUpgrade
                                | StepName::Rollback
                                | StepName::ExerciseRolledBack
                                | StepName::CleanupBackup
                        )
                    ) || s.checkpoint.is_some_and(|c| {
                        matches!(
                            c,
                            Checkpoint::KnownGood
                                | Checkpoint::BackupCreated
                                | Checkpoint::UpgradeFailed
                                | Checkpoint::RolledBack
                                | Checkpoint::BackupRemoved
                        )
                    }) || s.checks.iter().any(|c| {
                        matches!(
                            c.checkpoint,
                            Checkpoint::KnownGood
                                | Checkpoint::BackupCreated
                                | Checkpoint::UpgradeFailed
                                | Checkpoint::RolledBack
                                | Checkpoint::BackupRemoved
                        )
                    }) || rollback_reference(&s.name)
                        || s.commands.iter().any(|c| {
                            c.expected_failure.is_some()
                                || c.outcome == CommandOutcome::ExpectedFailure
                        })
                        || s.checks.iter().any(|c| {
                            c.backup.is_some()
                                || c.comparison.as_ref().is_some_and(|r| {
                                    rollback_reference(&r.before_stage)
                                        || rollback_reference(&r.after_stage)
                                })
                        })
                });
        }
        let Some(policy) = &self.rollback else {
            return true;
        };
        if crate::recipe::validate_rollback_contract(policy).is_err() || self.downgrade.is_some() {
            return true;
        }
        for stage in &self.stages {
            if stage.checkpoint.is_some_and(|cp| {
                !checkpoint_applies_with_recovery(
                    ScenarioKind::Rollback,
                    cp,
                    Some(&policy.recovery),
                )
            }) {
                return true;
            }
            if stage.checks.iter().any(|c| {
                !checkpoint_applies_with_recovery(
                    ScenarioKind::Rollback,
                    c.checkpoint,
                    Some(&policy.recovery),
                )
            }) {
                return true;
            }
            for command in &stage.commands {
                if (command.expected_failure.is_some()
                    || command.outcome == CommandOutcome::ExpectedFailure)
                    && (!stage.verified_failure()
                        || command.expected_failure.as_ref().is_none_or(|p| {
                            p.expected_exit != policy.failure.expected_exit
                                || p.stdout_contains != policy.failure.stdout_contains
                        }))
                {
                    return true;
                }
            }
            for check in &stage.checks {
                if check.id.starts_with("__theseus_rollback_")
                    && !generated_context(&policy.recovery, stage, check)
                {
                    return true;
                }
                if check.backup.is_some() && !check.id.starts_with("__theseus_rollback_") {
                    return true;
                }
            }
        }
        // Only require the full lifecycle when all observed evidence claims success
        // and teardown is complete; failures and intermediate saves may skip tails.
        let completed_success = matches!(self.teardown, TeardownStatus::Complete)
            && self.errors.is_empty()
            && !self.stages.is_empty()
            && self
                .stages
                .iter()
                .all(|s| s.aggregate_outcome() == RunOutcome::Passed);
        completed_success && !self.complete_rollback(policy)
    }
    fn complete_rollback(&self, policy: &RollbackContract) -> bool {
        let restore = matches!(
            policy.recovery.restoration,
            RestorationPolicy::RestoreData { .. }
        );
        let mut names = vec![
            "baseline",
            "install_base",
            "base_installed",
            "base_installed:after_checks",
            "configure",
            "configured",
            "configured:after_checks",
            "exercise_before_upgrade",
            "known_good",
            "known_good:after_checks",
            "before_upgrade",
        ];
        if restore {
            names.extend(["backup", "backup_created", "backup_created:after_checks"]);
        }
        names.extend([
            "before_attempt",
            "attempt_upgrade",
            "upgrade_failed",
            "upgrade_failed:after_checks",
            "before_rollback",
            "rollback",
            "rolled_back",
            "rolled_back:after_checks",
            "exercise_rolled_back",
        ]);
        if restore {
            names.extend([
                "cleanup_backup",
                "backup_removed",
                "backup_removed:after_checks",
            ]);
        }
        names.extend([
            "uninstall",
            "uninstalled",
            "uninstalled:after_checks",
            "final",
        ]);
        if self
            .stages
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            != names
        {
            return false;
        }
        self.complete_rollback_recovery(&policy.recovery, Checkpoint::UpgradeFailed)
    }
}
/// Every new observation reference has a report-five feature floor.
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
