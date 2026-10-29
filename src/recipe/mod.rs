//! Typed recipes and offline validation. Environment forwarding is a run-preflight concern.
mod downgrade;
mod rollback;
mod types;
pub(crate) mod validate;
pub use downgrade::{DowngradeContract, validate_downgrade_contract};
pub use types::*;
pub use validate::{ValidatedRecipe, load, normalize_container_path};

pub use rollback::{
    BackupScope, ExpectedFailureContract, RestorationPolicy, RollbackContract,
    validate_rollback_contract,
};
