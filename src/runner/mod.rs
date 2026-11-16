//! Container boundary shared by the engine and deterministic environment tests.
mod artifacts;
mod docker;
pub(crate) mod process;
use crate::{
    engine::CancellationToken,
    model::*,
    recipe::{Limits, ValidatedRecipe},
    report::ReportStore,
    snapshot::Snapshot,
};
use anyhow::Result;
pub use docker::{Docker, ROLE_LABEL, SCENARIO_LABEL};

/// Whether a journaled resource is a companion of the scenario: owned beside
/// its guest (a private Docker daemon), and removed after it.
pub(crate) fn is_companion(resource: &OwnedResource, scenario_id: &str) -> bool {
    resource.labels.contains_key(ROLE_LABEL)
        && resource.labels.get(SCENARIO_LABEL).map(String::as_str) == Some(scenario_id)
}
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
    pub stdout_utf8: bool,
}
pub struct ExecutedMcp {
    pub command: ExecutedCommand,
    pub evidence: crate::mcp::McpEvidence,
    pub container_available: bool,
}
/// Observation evidence and a separate infrastructure failure channel. Expected
/// bounded/no-follow gaps stay in snapshots; transport errors must not disappear
/// when an adapter preserves partial observations for the ordinary run viewer.
pub struct ObservationBatch {
    pub snapshots: Vec<Snapshot>,
    pub infrastructure_error: Option<String>,
}
pub trait Environment {
    fn prepare(
        &mut self,
        recipe: &ValidatedRecipe,
        pull: bool,
        cancel: &CancellationToken,
    ) -> Result<PreparedEnvironment>;
    /// Creation only, before the guest's own. A companion is owned beside the
    /// scenario's guest (a private Docker daemon the recipe declares); the
    /// engine journals it before the guest is created and removes it after it.
    fn create_companion(
        &mut self,
        _run_id: &str,
        _scenario_id: &str,
        _recipe: &ValidatedRecipe,
    ) -> Result<Option<OwnedResource>> {
        Ok(None)
    }
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
    fn probe_mcp(
        &mut self,
        _resource: &OwnedResource,
        _request: CommandRequest,
        _probe: crate::mcp::McpProbe,
        _store: &ReportStore,
        _cancel: &CancellationToken,
    ) -> Result<ExecutedMcp> {
        anyhow::bail!("environment does not support bounded MCP probing")
    }
    /// Deliberate interruption is opt-in; unsupported environments fail closed.
    fn execute_interrupted(
        &mut self,
        _resource: &OwnedResource,
        _request: CommandRequest,
        _store: &ReportStore,
        _cancel: &CancellationToken,
    ) -> Result<ExecutedCommand> {
        anyhow::bail!("environment does not support verified interruption")
    }
    fn restart(
        &mut self,
        _resource: &OwnedResource,
        _cancel: &CancellationToken,
    ) -> Result<RestartEvidence> {
        anyhow::bail!("environment does not support verified restart")
    }
    fn observe(
        &mut self,
        resource: &OwnedResource,
        roots: &[String],
        limits: Limits,
        cancel: &CancellationToken,
    ) -> Result<Vec<Snapshot>>;
    fn observe_with_status(
        &mut self,
        resource: &OwnedResource,
        roots: &[String],
        limits: Limits,
        cancel: &CancellationToken,
    ) -> Result<ObservationBatch> {
        self.observe(resource, roots, limits, cancel)
            .map(|snapshots| ObservationBatch {
                snapshots,
                infrastructure_error: None,
            })
    }
    /// Stops container descendants after timeout/cancellation. Must verify ownership.
    fn stop(&mut self, resource: &OwnedResource) -> Result<()>;
    /// Idempotent removal, with ownership verified before any destructive action.
    fn remove(&mut self, resource: &OwnedResource) -> Result<()>;
}
