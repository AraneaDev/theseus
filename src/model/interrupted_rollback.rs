//! Interrupted rollback has a distinct authorization and exact successful sequence.
use super::rollback_validation::{Invocation, generated_context_at, valid_invocation};
use super::*;
use crate::recipe::RestorationPolicy;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandExpectationEvidence {
    pub expected_exit: i32,
    pub stdout_contains: String,
    pub stdout_matched: bool,
    /// Observed matching literal only, bounded by the command output/log cap.
    /// This records match evidence; it is not tamper-proof log verification.
    pub stdout_match: Option<String>,
    pub executable: Option<ExecutableExpectationEvidence>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutableExpectationEvidence {
    pub name: String,
    pub resolved_path: String,
    pub args: Vec<String>,
}

impl StageReport {
    pub(crate) fn verified_rollback_interruption(&self) -> bool {
        self.verified_interruption()
            && self
                .commands
                .iter()
                .all(|c| valid_invocation(&c.argv, Invocation::ShellScript))
    }
}
impl ScenarioReport {
    /// Recheck completed proof without trusting cached success or cloning snapshots.
    pub(crate) fn verified_interrupted_rollback(&self) -> bool {
        self.scenario == ScenarioKind::InterruptedRollback
            && self.outcome == RunOutcome::Passed
            && matches!(self.teardown, TeardownStatus::Complete)
            && self.errors.is_empty()
            && !self.stages.is_empty()
            && self
                .stages
                .iter()
                .all(|s| s.aggregate_outcome() == RunOutcome::Passed)
            && !self.unmatched_interrupted_rollback_policy()
    }
    pub(super) fn unmatched_interrupted_rollback_policy(&self) -> bool {
        if self.scenario != ScenarioKind::InterruptedRollback {
            return self.interrupted_rollback.is_some()
                || self.stages.iter().flat_map(|s| &s.checks).any(|c| {
                    c.command_expectation.is_some()
                        && !(self.scenario == ScenarioKind::PluginLifecycle
                            && self.plugin.as_ref().is_some_and(|p| {
                                p.checkpoints
                                    .values()
                                    .any(|proof| proof.components.contains(&c.id))
                            }))
                });
        }
        let Some(policy) = &self.interrupted_rollback else {
            return true;
        };
        if self.rollback.is_some()
            || self.downgrade.is_some()
            || crate::recipe::validate_interrupted_rollback_contract(policy).is_err()
        {
            return true;
        }
        for stage in &self.stages {
            if stage.checkpoint.is_some_and(|cp| {
                !checkpoint_applies_with_recovery(self.scenario, cp, Some(&policy.recovery))
            }) || stage.checks.iter().any(|c| {
                !checkpoint_applies_with_recovery(
                    self.scenario,
                    c.checkpoint,
                    Some(&policy.recovery),
                )
            }) {
                return true;
            }
            if stage.step.is_some_and(|s| {
                matches!(
                    s,
                    StepName::AttemptUpgrade
                        | StepName::RecoverUpgrade
                        | StepName::RecoverInstall
                        | StepName::InterruptInstall
                )
            }) {
                return true;
            }
            for command in &stage.commands {
                if command.expected_failure.is_some()
                    || command.rejection.is_some()
                    || matches!(
                        command.outcome,
                        CommandOutcome::ExpectedFailure | CommandOutcome::ExpectedRejection
                    )
                {
                    return true;
                }
                if command.interruption.is_some() && stage.step != Some(StepName::InterruptUpgrade)
                {
                    return true;
                }
                if command.outcome == CommandOutcome::Interrupted
                    && (!stage.verified_interruption()
                        || !valid_invocation(&command.argv, Invocation::ShellScript))
                {
                    return true;
                }
            }
            for check in &stage.checks {
                if (check.id.starts_with("__theseus_rollback_")
                    && !generated_context_at(
                        &policy.recovery,
                        stage,
                        check,
                        Checkpoint::Interrupted,
                    ))
                    || (check.backup.is_some() && !check.id.starts_with("__theseus_rollback_"))
                {
                    return true;
                }
            }
        }
        let completed_success = matches!(self.teardown, TeardownStatus::Complete)
            && self.errors.is_empty()
            && !self.stages.is_empty()
            && self
                .stages
                .iter()
                .all(|s| s.aggregate_outcome() == RunOutcome::Passed);
        completed_success && !self.complete_interrupted_rollback(&policy.recovery)
    }
    fn complete_interrupted_rollback(
        &self,
        policy: &crate::recipe::RollbackRecoveryContract,
    ) -> bool {
        let restore = matches!(policy.restoration, RestorationPolicy::RestoreData { .. });
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
            "before_interrupt",
            "interrupt_upgrade",
            "restart_after_interrupt",
            "interrupted",
            "interrupted:after_checks",
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
        if self
            .stages
            .iter()
            .filter(|s| {
                matches!(
                    s.name.as_str(),
                    "interrupt_upgrade" | "restart_after_interrupt"
                )
            })
            .any(|s| !s.snapshots.is_empty())
        {
            return false;
        }
        let restart = self
            .stages
            .iter()
            .find(|s| s.name == "restart_after_interrupt")
            .unwrap();
        if restart.step.is_some()
            || restart.checkpoint.is_some()
            || !restart.commands.is_empty()
            || !restart.restart.as_ref().is_some_and(|p| {
                p.running_verified && Some(&p.container_id) == self.container_id.as_ref()
            })
        {
            return false;
        }
        if self.container_id.as_ref().is_none_or(|id| id.is_empty())
            || self
                .stages
                .iter()
                .any(|s| s.name != "restart_after_interrupt" && s.restart.is_some())
        {
            return false;
        }
        self.complete_rollback_recovery(policy, Checkpoint::Interrupted)
    }
}
