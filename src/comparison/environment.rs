//! Preserve infrastructure errors that the engine converts into lifecycle evidence.
//! Every call/result is forwarded unchanged; the engine alone controls execution.
use crate::{
    engine::CancellationToken,
    model::{CommandOutcome, OwnedResource, RestartEvidence},
    recipe::{Limits, ValidatedRecipe},
    report::ReportStore,
    runner::{CommandRequest, Environment, ExecutedCommand, PreparedEnvironment},
    snapshot::Snapshot,
};
use anyhow::Result;

pub(super) struct TrackedEnvironment<'a, E> {
    pub inner: &'a mut E,
    pub error: Option<String>,
}
impl<E> TrackedEnvironment<'_, E> {
    fn command(
        &mut self,
        operation: &str,
        result: Result<ExecutedCommand>,
    ) -> Result<ExecutedCommand> {
        if self.error.is_none()
            && let Ok(executed) = &result
            && executed.record.outcome == CommandOutcome::Error
        {
            self.error = Some(format!(
                "infrastructure {operation}: {}",
                executed
                    .record
                    .error
                    .as_deref()
                    .unwrap_or("command control failed")
            ));
        }
        self.record(operation, result)
    }
    fn record<T>(&mut self, operation: &str, result: Result<T>) -> Result<T> {
        if self.error.is_none()
            && let Err(error) = &result
        {
            self.error = Some(format!("infrastructure {operation}: {error:#}"));
        }
        result
    }
}
impl<E: Environment> Environment for TrackedEnvironment<'_, E> {
    fn prepare(
        &mut self,
        recipe: &ValidatedRecipe,
        pull: bool,
        cancel: &CancellationToken,
    ) -> Result<PreparedEnvironment> {
        let result = self.inner.prepare(recipe, pull, cancel);
        self.record("prepare", result)
    }
    fn create(
        &mut self,
        run: &str,
        scenario: &str,
        recipe: &ValidatedRecipe,
    ) -> Result<OwnedResource> {
        let result = self.inner.create(run, scenario, recipe);
        self.record("create", result)
    }
    fn setup(
        &mut self,
        resource: &OwnedResource,
        recipe: &ValidatedRecipe,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let result = self.inner.setup(resource, recipe, cancel);
        self.record("setup", result)
    }
    fn execute(
        &mut self,
        resource: &OwnedResource,
        request: CommandRequest,
        store: &ReportStore,
        cancel: &CancellationToken,
    ) -> Result<ExecutedCommand> {
        let result = self.inner.execute(resource, request, store, cancel);
        self.command("execute", result)
    }
    fn execute_interrupted(
        &mut self,
        resource: &OwnedResource,
        request: CommandRequest,
        store: &ReportStore,
        cancel: &CancellationToken,
    ) -> Result<ExecutedCommand> {
        let result = self
            .inner
            .execute_interrupted(resource, request, store, cancel);
        self.command("execute interrupted", result)
    }
    fn restart(
        &mut self,
        resource: &OwnedResource,
        cancel: &CancellationToken,
    ) -> Result<RestartEvidence> {
        let result = self.inner.restart(resource, cancel);
        self.record("restart", result)
    }
    fn observe(
        &mut self,
        resource: &OwnedResource,
        roots: &[String],
        limits: Limits,
        cancel: &CancellationToken,
    ) -> Result<Vec<Snapshot>> {
        let result = self
            .inner
            .observe_with_status(resource, roots, limits, cancel)
            .map(|batch| {
                if self.error.is_none()
                    && let Some(error) = batch.infrastructure_error
                {
                    self.error = Some(format!("infrastructure observe: {error}"));
                }
                batch.snapshots
            });
        self.record("observe", result)
    }
    fn stop(&mut self, resource: &OwnedResource) -> Result<()> {
        let result = self.inner.stop(resource);
        self.record("stop", result)
    }
    fn remove(&mut self, resource: &OwnedResource) -> Result<()> {
        let result = self.inner.remove(resource);
        self.record("remove", result)
    }
}
