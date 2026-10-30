//! Typed recipes and offline validation. Environment forwarding is a run-preflight concern.
pub(crate) mod mcp;
pub use mcp::{
    McpCheckpointContract, McpClientClaim, McpDelivery, McpLifecycleContract, McpRegistrationState,
    validate_mcp_contract,
};
mod downgrade;
pub(crate) mod plugin;
pub use plugin::{PluginCheckpointContract, PluginContract, PluginScope, validate_plugin_contract};
mod interrupted_rollback;
pub(crate) mod json;
mod rollback;
mod types;
pub use json::{JsonAssertion, JsonPredicate, validate_json_assertions};
pub(crate) mod validate;
pub use downgrade::{DowngradeContract, validate_downgrade_contract};
pub use interrupted_rollback::{
    InterruptedRollbackContract, validate_interrupted_rollback_contract,
};
pub use types::*;
pub use validate::{ValidatedRecipe, load, normalize_container_path, parse_bytes};

pub use rollback::{
    BackupScope, ExpectedFailureContract, RestorationPolicy, RollbackContract,
    RollbackRecoveryContract, validate_rollback_contract, validate_rollback_recovery,
};
