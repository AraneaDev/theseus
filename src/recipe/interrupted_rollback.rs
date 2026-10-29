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
