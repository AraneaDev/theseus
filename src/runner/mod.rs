//! Container boundary shared by the engine and deterministic environment tests.
mod artifacts;
mod docker;
mod process;
use crate::{
    engine::CancellationToken,
    model::*,
    recipe::{Limits, ValidatedRecipe},
    report::ReportStore,
    snapshot::Snapshot,
};
use anyhow::Result;
pub use docker::Docker;
use std::collections::BTreeMap;

pub struct PreparedEnvironment {
    pub image_id: String,
    pub artifacts: Vec<ArtifactDigest>,
}
#[derive(Debug, Clone)]
pub struct CommandRequest {
    pub id: String,
    pub phase: String,
    pub identity: ExecutionIdentity,
    /// Container argv, never a host shell string.
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub pass_env: Vec<String>,
    pub timeout_seconds: u64,
    pub expected_exit: i32,
    pub log_limit: u64,
}
pub struct ExecutedCommand {
    pub record: CommandRecord,
    /// Bounded by log_limit; no unbounded command output in events.
    pub stdout: String,
    pub stdout_complete: bool,
}
pub trait Environment {
    fn prepare(
        &mut self,
        recipe: &ValidatedRecipe,
        pull: bool,
        cancel: &CancellationToken,
    ) -> Result<PreparedEnvironment>;
    /// Creation only. The engine journals this resource before setup/start/copy.
    fn create(
        &mut self,
        run_id: &str,
        scenario_id: &str,
        recipe: &ValidatedRecipe,
    ) -> Result<OwnedResource>;
    fn setup(
        &mut self,
        resource: &OwnedResource,
        recipe: &ValidatedRecipe,
        cancel: &CancellationToken,
    ) -> Result<()>;
    fn execute(
        &mut self,
        resource: &OwnedResource,
        request: CommandRequest,
        store: &ReportStore,
        cancel: &CancellationToken,
    ) -> Result<ExecutedCommand>;
    fn observe(
        &mut self,
        resource: &OwnedResource,
        roots: &[String],
        limits: Limits,
        cancel: &CancellationToken,
    ) -> Result<Vec<Snapshot>>;
    /// Stops container descendants after timeout/cancellation. Must verify ownership.
    fn stop(&mut self, resource: &OwnedResource) -> Result<()>;
    /// Idempotent removal, with ownership verified before any destructive action.
    fn remove(&mut self, resource: &OwnedResource) -> Result<()>;
}
