//! Recovery policy for an interrupted attempt, with no ordinary failure receipt.
use super::{RollbackRecoveryContract, validate_rollback_recovery};
use anyhow::Result;
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InterruptedRollbackContract {
    #[serde(flatten)]
    pub recovery: RollbackRecoveryContract,
}
impl<'de> Deserialize<'de> for InterruptedRollbackContract {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        // Decode the strict shared wire directly, including rejection of failure.
        Ok(Self {
            recovery: RollbackRecoveryContract::deserialize(deserializer)?,
        })
    }
}

pub fn validate_interrupted_rollback_contract(
    contract: &InterruptedRollbackContract,
) -> Result<()> {
    validate_rollback_recovery(&contract.recovery)
}

pub(super) fn validate_profile(
    profile: &super::Profile,
    observe: &[String],
    scenarios: &[crate::model::ScenarioKind],
) -> Result<()> {
    let policy = profile
        .interrupted_rollback
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("interrupted_rollback requires explicit policy"))?;
    validate_interrupted_rollback_contract(policy)?;
    for check in &profile.checks {
        anyhow::ensure!(
            !(check.checkpoint == crate::model::Checkpoint::Interrupted
                && matches!(check.expectation, super::Expectation::Changed { .. }))
                || policy.recovery.change_checks.contains(&check.id),
            "Changed at Interrupted must be declared in interrupted_rollback change_checks"
        );
    }
    super::rollback::validate_recovery_profile(
        profile,
        observe,
        &policy.recovery,
        crate::model::Checkpoint::Interrupted,
        scenarios.contains(&crate::model::ScenarioKind::Rollback)
            && profile.rollback.as_ref().is_some_and(|p| {
                matches!(
                    p.recovery.restoration,
                    super::RestorationPolicy::RestoreData { .. }
                )
            }),
    )
}
