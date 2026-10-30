//! Preserve infrastructure errors that the engine converts into lifecycle evidence.
//! Every call/result is forwarded unchanged; the engine alone controls execution.
use crate::{
    engine::CancellationToken,
    model::{CommandOutcome, OwnedResource, RestartEvidence},
    recipe::{Limits, ValidatedRecipe},
    report::ReportStore,
    runner::{CommandRequest, Environment, ExecutedCommand, ExecutedMcp, PreparedEnvironment},
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
    fn probe_mcp(
        &mut self,
        resource: &OwnedResource,
        request: CommandRequest,
        probe: crate::mcp::McpProbe,
        store: &ReportStore,
        cancel: &CancellationToken,
    ) -> Result<ExecutedMcp> {
        let result = self
            .inner
            .probe_mcp(resource, request, probe, store, cancel);
        if self.error.is_none()
            && let Ok(observed) = &result
            && observed.command.record.outcome == CommandOutcome::Error
        {
            self.error = Some(format!(
                "infrastructure probe MCP: {}",
                observed
                    .command
                    .record
                    .error
                    .as_deref()
                    .unwrap_or("command control failed")
            ));
        }
        self.record("probe MCP", result)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        mcp::{McpProbe, McpSession, McpShutdown},
        model::CommandRecord,
    };
    struct ProbeFailure {
        outer: bool,
        calls: usize,
    }
    impl Environment for ProbeFailure {
        fn prepare(
            &mut self,
            _: &ValidatedRecipe,
            _: bool,
            _: &CancellationToken,
        ) -> Result<PreparedEnvironment> {
            unreachable!()
        }
        fn create(&mut self, _: &str, _: &str, _: &ValidatedRecipe) -> Result<OwnedResource> {
            unreachable!()
        }
        fn setup(
            &mut self,
            _: &OwnedResource,
            _: &ValidatedRecipe,
            _: &CancellationToken,
        ) -> Result<()> {
            unreachable!()
        }
        fn execute(
            &mut self,
            _: &OwnedResource,
            _: CommandRequest,
            _: &ReportStore,
            _: &CancellationToken,
        ) -> Result<ExecutedCommand> {
            unreachable!()
        }
        fn observe(
            &mut self,
            _: &OwnedResource,
            _: &[String],
            _: Limits,
            _: &CancellationToken,
        ) -> Result<Vec<Snapshot>> {
            unreachable!()
        }
        fn stop(&mut self, _: &OwnedResource) -> Result<()> {
            unreachable!()
        }
        fn remove(&mut self, _: &OwnedResource) -> Result<()> {
            unreachable!()
        }
        fn probe_mcp(
            &mut self,
            _: &OwnedResource,
            r: CommandRequest,
            p: McpProbe,
            _: &ReportStore,
            _: &CancellationToken,
        ) -> Result<ExecutedMcp> {
            self.calls += 1;
            assert_eq!(r.argv, p.argv);
            assert_eq!(r.env, p.env);
            assert!(r.pass_env.is_empty());
            if self.outer {
                anyhow::bail!("fixture transport failure");
            }
            let mut record = CommandRecord::new(r.id, r.phase);
            record.outcome = CommandOutcome::Error;
            record.error = Some("fixture command control failure".into());
            Ok(ExecutedMcp {
                command: ExecutedCommand {
                    record,
                    stdout: String::new(),
                    stdout_complete: false,
                    stdout_utf8: true,
                },
                evidence: McpSession::new(p)?.finish(McpShutdown::Unverified),
                container_available: false,
            })
        }
    }
    #[test]
    fn mcp_forwarding_retains_both_infrastructure_error_channels() {
        for outer in [true, false] {
            let mut inner = ProbeFailure { outer, calls: 0 };
            let mut tracked = TrackedEnvironment {
                inner: &mut inner,
                error: None,
            };
            let probe:McpProbe=toml::from_str("argv=['/bin/server']\nprotocol='2026-07-28'\nserver_name='fixture'\nserver_version='2'\ntools=[]").unwrap();
            let request = CommandRequest {
                id: "probe".into(),
                phase: "check:discovery".into(),
                identity: crate::model::ExecutionIdentity::runtime(),
                argv: probe.argv.clone(),
                env: probe.env.clone(),
                pass_env: vec![],
                timeout_seconds: 15,
                expected_exit: 0,
                log_limit: 1024,
            };
            let root = tempfile::tempdir().unwrap();
            let store = crate::report::create(&root.path().join("run")).unwrap();
            let resource = OwnedResource {
                container_id: "owned".into(),
                labels: Default::default(),
                removed: false,
            };
            let result = tracked.probe_mcp(
                &resource,
                request,
                probe,
                &store,
                &CancellationToken::default(),
            );
            assert_eq!(result.is_err(), outer);
            let error = tracked.error.as_ref().unwrap();
            assert!(error.contains(if outer {
                "fixture transport failure"
            } else {
                "fixture command control failure"
            }));
            if let Ok(observed) = result {
                assert!(!observed.container_available);
                assert_eq!(
                    observed.command.record.error.as_deref(),
                    Some("fixture command control failure")
                );
            }
            assert_eq!(inner.calls, 1);
        }
    }
}
