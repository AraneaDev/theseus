use super::*;
use crate::{
    checks::{self, CommandObservation},
    recipe::{Check, CommandSpec, Expectation},
    report::ReportStore,
    runner::{CommandRequest, ExecutedCommand},
    snapshot::{self, Snapshot},
};

pub(super) struct ScenarioContext<'a, E, F> {
    pub recipe: &'a ValidatedRecipe,
    pub options: &'a RunOptions,
    pub cancel: &'a CancellationToken,
    pub environment: &'a mut E,
    pub store: &'a ReportStore,
    pub report: &'a mut RunReport,
    pub index: usize,
    pub journal: &'a mut RunJournal,
    pub emit: &'a mut F,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Availability {
    Running,
    Stopped,
    Unavailable,
}
pub(super) struct State {
    pub(super) resource: OwnedResource,
    pub(super) availability: Availability,
    pub(super) forward: bool,
    observations_verified: bool,
    pub(super) baseline: Vec<Snapshot>,
    pub(super) previous: Vec<Snapshot>,
    pub(super) configured: Option<Vec<Snapshot>>,
    pub(super) before_reinstall: Option<Vec<Snapshot>>,
    pub(super) before_interrupt: Option<Vec<Snapshot>>,
    pub(super) before_downgrade: Option<Vec<Snapshot>>,
    pub(super) sequence: usize,
    pub(super) before_upgrade: Option<Vec<Snapshot>>,
    pub(super) before_attempt: Option<Vec<Snapshot>>,
    pub(super) rollback_backup_authorized: bool,
}
pub(super) fn scenario<E: Environment, F: FnMut(EngineEvent)>(
    mut context: ScenarioContext<'_, E, F>,
) -> Result<()> {
    let c = &mut context;
    if c.cancel.is_cancelled() {
        let mut stage = StageReport::new("cancelled");
        stage.outcome = StageOutcome::Cancelled;
        stage.skip_reason = Some("cancelled before scenario creation".into());
        c.report.scenarios[c.index].stages.push(stage);
        c.report.cancelled = true;
        return Ok(());
    }
    let resource =
        c.environment
            .create(&c.report.run_id, &c.report.scenarios[c.index].id, c.recipe)?;
    c.report.scenarios[c.index].container_id = Some(resource.container_id.clone());
    c.journal.resources.push(resource.clone());
    c.store.save_journal(c.journal)?;
    c.save(EventKind::ScenarioStarted, None)?;
    c.environment.setup(&resource, c.recipe, c.cancel)?;
    let mut state = State {
        resource,
        availability: Availability::Running,
        forward: true,
        observations_verified: true,
        baseline: vec![],
        previous: vec![],
        configured: None,
        before_reinstall: None,
        before_interrupt: None,
        before_downgrade: None,
        sequence: 0,
        before_upgrade: None,
        before_attempt: None,
        rollback_backup_authorized: false,
    };
    let mut stage = StageReport::new("baseline");
    c.capture(&mut state, &mut stage);
    state.baseline = stage.snapshots.clone();
    c.finish(stage)?;
    let kind = c.report.scenarios[c.index].scenario;
    match kind {
        ScenarioKind::PluginLifecycle => super::plugin::run(c, &mut state)?,
        ScenarioKind::Downgrade => super::downgrade::run(c, &mut state)?,
        ScenarioKind::Rollback => super::rollback::run(c, &mut state)?,
        ScenarioKind::InterruptedRollback => super::interrupted_rollback::run(c, &mut state)?,
        ScenarioKind::InterruptedInstall | ScenarioKind::InterruptedUpgrade => {
            super::interruption::run(c, &mut state, kind)?
        }
        ScenarioKind::Clean | ScenarioKind::Upgrade | ScenarioKind::Reinstall => {
            let (first, first_cp) = match kind {
                ScenarioKind::Clean | ScenarioKind::Reinstall => {
                    (StepName::InstallTarget, Checkpoint::TargetInstalled)
                }
                ScenarioKind::Upgrade => (StepName::InstallBase, Checkpoint::BaseInstalled),
                ScenarioKind::InterruptedInstall
                | ScenarioKind::InterruptedUpgrade
                | ScenarioKind::Downgrade
                | ScenarioKind::Rollback
                | ScenarioKind::PluginLifecycle
                | ScenarioKind::InterruptedRollback => {
                    unreachable!("handled explicitly above")
                }
            };
            c.step(&mut state, first)?;
            c.checkpoint(&mut state, first_cp, false)?;
            c.step(&mut state, StepName::Configure)?;
            c.checkpoint(&mut state, Checkpoint::Configured, true)?;
            match kind {
                ScenarioKind::Upgrade => {
                    c.step(&mut state, StepName::Upgrade)?;
                    c.checkpoint(&mut state, Checkpoint::TargetInstalled, false)?;
                    c.step(&mut state, StepName::Exercise)?;
                }
                ScenarioKind::Clean => c.step(&mut state, StepName::Exercise)?,
                ScenarioKind::InterruptedInstall
                | ScenarioKind::InterruptedUpgrade
                | ScenarioKind::Downgrade
                | ScenarioKind::Rollback
                | ScenarioKind::PluginLifecycle
                | ScenarioKind::InterruptedRollback => {
                    unreachable!("handled explicitly above")
                }
                ScenarioKind::Reinstall => {
                    c.step_named(&mut state, StepName::Exercise, "exercise_before_reinstall")?;
                    let mut reference = StageReport::new("before_reinstall");
                    c.capture(&mut state, &mut reference);
                    state.before_reinstall = Some(reference.snapshots.clone());
                    c.finish(reference)?;
                    c.step(&mut state, StepName::Reinstall)?;
                    c.checkpoint(&mut state, Checkpoint::Reinstalled, false)?;
                    c.step_named(&mut state, StepName::Exercise, "exercise_after_reinstall")?;
                }
            }
        }
    }
    c.step(&mut state, StepName::Uninstall)?;
    c.checkpoint(&mut state, Checkpoint::Uninstalled, false)?;
    let mut final_stage = StageReport::new("final");
    c.capture(&mut state, &mut final_stage);
    let final_state = final_stage.snapshots.clone();
    c.finish(final_stage)?;
    let profile = &c.recipe.recipe.profiles[&c.report.scenarios[c.index].profile];
    c.report.scenarios[c.index].findings =
        checks::review_findings(&state.baseline, &final_state, &profile.checks);
    Ok(())
}
impl<E: Environment, F: FnMut(EngineEvent)> ScenarioContext<'_, E, F> {
    fn save(&mut self, kind: EventKind, stage: Option<String>) -> Result<()> {
        let sid = self.report.scenarios[self.index].id.clone();
        save_event(self.store, self.report, self.emit, kind, Some(sid), stage)
    }
    pub(super) fn start(&mut self, stage: &StageReport) -> Result<()> {
        let mut running = stage.clone();
        running.outcome = StageOutcome::Running;
        self.report.scenarios[self.index].stages.push(running);
        self.save(EventKind::StageStarted, Some(stage.name.clone()))
    }
    pub(super) fn update(&mut self, stage: &StageReport) -> Result<()> {
        *self.report.scenarios[self.index].stages.last_mut().unwrap() = stage.clone();
        self.save(EventKind::StageFinished, Some(stage.name.clone()))
    }
    pub(super) fn finish(&mut self, mut stage: StageReport) -> Result<()> {
        self.start(&stage)?;
        Self::outcome(&mut stage);
        self.update(&stage)
    }
    pub(super) fn outcome(stage: &mut StageReport) {
        if matches!(
            stage.outcome,
            StageOutcome::Skipped | StageOutcome::Cancelled
        ) {
            return;
        }
        stage.outcome = StageOutcome::Passed;
        stage.outcome = match stage.aggregate_outcome() {
            RunOutcome::Passed => StageOutcome::Passed,
            RunOutcome::Failed => StageOutcome::Failed,
            RunOutcome::Incomplete => StageOutcome::Incomplete,
            RunOutcome::Cancelled => StageOutcome::Cancelled,
        };
    }
    fn observation(&mut self, state: &State) -> Vec<Snapshot> {
        if state.availability != Availability::Running || self.cancel.is_cancelled() {
            return self
                .recipe
                .recipe
                .observe
                .iter()
                .map(|r| {
                    Snapshot::incomplete(
                        r,
                        "container stopped after timeout/cancellation; observation unavailable",
                    )
                })
                .collect();
        }
        self.environment
            .observe(
                &state.resource,
                &self.recipe.recipe.observe,
                self.recipe.recipe.limits,
                self.cancel,
            )
            .unwrap_or_else(|error| {
                self.recipe
                    .recipe
                    .observe
                    .iter()
                    .map(|r| Snapshot::incomplete(r, format!("{error:#}")))
                    .collect()
            })
    }
    pub(super) fn capture(&mut self, state: &mut State, stage: &mut StageReport) -> bool {
        stage.snapshots = self.observation(state);
        let complete = !stage.snapshots.is_empty()
            && stage.snapshots.iter().all(|snapshot| {
                !matches!(
                    snapshot.completeness,
                    snapshot::ObservationStatus::Incomplete { .. }
                )
            });
        // Every observation-bearing phase is a sticky prerequisite. In particular,
        // a later complete checkpoint/reference cannot authorize missing evidence.
        state.observations_verified &= complete;
        state.forward &= state.observations_verified;
        if stage.snapshots.is_empty() {
            stage
                .errors
                .push("required observation capture was empty".into());
        }
        for current in &stage.snapshots {
            if let Some(before) = state.baseline.iter().find(|b| b.root == current.root) {
                stage.baseline_diff.extend(snapshot::diff(before, current));
            }
            if let Some(before) = state.previous.iter().find(|b| b.root == current.root) {
                stage.stage_diff.extend(snapshot::diff(before, current));
            }
        }
        state.previous = stage.snapshots.clone();
        complete
    }
    pub(super) fn request(
        &self,
        state: &mut State,
        phase: String,
        command: &CommandSpec,
        identity: ExecutionIdentity,
        argv: Vec<String>,
        expected_exit: i32,
    ) -> CommandRequest {
        state.sequence += 1;
        CommandRequest {
            id: format!(
                "{}-{}",
                self.report.scenarios[self.index].id, state.sequence
            ),
            phase,
            identity,
            argv,
            env: command.env.clone(),
            pass_env: command.pass_env.clone(),
            timeout_seconds: command.timeout_seconds,
            expected_exit,
            log_limit: self.options.log_limit,
        }
    }
    pub(super) fn execute(
        &mut self,
        state: &mut State,
        request: CommandRequest,
        stage: &mut StageReport,
    ) -> ExecutedCommand {
        let mut executed = if self.cancel.is_cancelled() {
            let mut record = CommandRecord::new(&request.id, &request.phase);
            record.identity = request.identity.clone();
            record.argv = request.argv.clone();
            record.outcome = CommandOutcome::Cancelled;
            record.cancelled = true;
            ExecutedCommand {
                record,
                stdout: String::new(),
                stdout_utf8: false,
                stdout_complete: false,
            }
        } else {
            match self.environment.execute(
                &state.resource,
                request.clone(),
                self.store,
                self.cancel,
            ) {
                Ok(executed) => executed,
                Err(error) => {
                    let mut record = CommandRecord::new(request.id, request.phase);
                    record.identity = request.identity;
                    record.argv = request.argv;
                    record.outcome = CommandOutcome::Error;
                    record.error = Some(format!("{error:#}"));
                    ExecutedCommand {
                        record,
                        stdout: String::new(),
                        stdout_utf8: false,
                        stdout_complete: false,
                    }
                }
            }
        };
        if executed.record.timed_out || executed.record.cancelled {
            state.availability = Availability::Unavailable;
            if executed.record.cancelled {
                self.cancel.cancel();
                self.report.cancelled = true;
            }
            if let Err(error) = self.environment.stop(&state.resource) {
                let text = format!("stop descendants: {error:#}");
                stage.errors.push(text.clone());
                executed.record.error = Some(text);
            }
        }
        stage.commands.push(executed.record.clone());
        executed
    }
    pub(super) fn step(&mut self, state: &mut State, step: StepName) -> Result<()> {
        self.step_named(state, step, &step.to_string())
    }
    pub(super) fn step_named(
        &mut self,
        state: &mut State,
        step: StepName,
        name: &str,
    ) -> Result<()> {
        let mut stage = StageReport::new(name);
        stage.step = Some(step);
        if self.cancel.is_cancelled() {
            state.availability = Availability::Unavailable;
            self.report.cancelled = true;
        }
        if state.availability != Availability::Running
            || (step == StepName::CleanupBackup && !state.rollback_backup_authorized)
            || (!state.forward
                && step != StepName::Uninstall
                && !(step == StepName::CleanupBackup && state.rollback_backup_authorized))
        {
            stage.outcome = StageOutcome::Skipped;
            stage.skip_reason = Some(
                if state.availability != Availability::Running {
                    "container stopped after timeout/cancellation"
                } else {
                    "failed prerequisite"
                }
                .into(),
            );
            return self.finish(stage);
        }
        self.start(&stage)?;
        self.execute_step(state, step, name, &mut stage);
        self.capture(state, &mut stage);
        Self::outcome(&mut stage);
        self.update(&stage)
    }
    pub(super) fn execute_step(
        &mut self,
        state: &mut State,
        step: StepName,
        name: &str,
        stage: &mut StageReport,
    ) {
        let profile_name = self.report.scenarios[self.index].profile;
        let spec = self.recipe.recipe.profiles[&profile_name].steps[&step].clone();
        let installer = matches!(
            step,
            StepName::InstallBase
                | StepName::InstallTarget
                | StepName::Upgrade
                | StepName::Reinstall
                | StepName::Downgrade
                | StepName::Uninstall
                | StepName::RecoverInstall
                | StepName::RecoverUpgrade
                | StepName::Backup
                | StepName::AttemptUpgrade
                | StepName::Rollback
                | StepName::CleanupBackup
        );
        let identity = if installer && profile_name == ProfileName::System {
            ExecutionIdentity::root()
        } else {
            ExecutionIdentity::runtime()
        };
        let request = self.request(
            state,
            name.to_owned(),
            &spec,
            identity,
            vec!["/bin/sh".into(), "-c".into(), spec.script.clone()],
            0,
        );
        let executed = self.execute(state, request, stage);
        state.forward &= executed.record.outcome == CommandOutcome::Succeeded;
    }
    pub(super) fn checkpoint(
        &mut self,
        state: &mut State,
        checkpoint: Checkpoint,
        configured: bool,
    ) -> Result<()> {
        if self.cancel.is_cancelled() {
            state.availability = Availability::Unavailable;
            self.report.cancelled = true;
        }
        let blocked = !state.forward
            && matches!(
                checkpoint,
                Checkpoint::Interrupted
                    | Checkpoint::Recovered
                    | Checkpoint::KnownGood
                    | Checkpoint::BackupCreated
                    | Checkpoint::UpgradeFailed
                    | Checkpoint::RolledBack
            )
            || checkpoint == Checkpoint::BackupRemoved && !state.rollback_backup_authorized;
        let mut stage = StageReport::new(checkpoint.to_string());
        stage.checkpoint = Some(checkpoint);
        self.start(&stage)?;
        let complete = self.capture(state, &mut stage);
        let cleanup = matches!(
            checkpoint,
            Checkpoint::BackupRemoved | Checkpoint::Uninstalled
        );
        let blocked = blocked || !complete || (!state.observations_verified && !cleanup);
        if configured
            && complete
            && state.observations_verified
            && state.availability == Availability::Running
            && self.report.scenarios[self.index]
                .stages
                .iter()
                .any(|s| s.step == Some(StepName::Configure) && s.outcome != StageOutcome::Skipped)
        {
            state.configured = Some(stage.snapshots.clone());
        }
        if matches!(
            self.report.scenarios[self.index].scenario,
            ScenarioKind::Rollback | ScenarioKind::InterruptedRollback
        ) {
            let policy = self.report.scenarios[self.index]
                .rollback_recovery()
                .unwrap();
            {
                let derived = super::rollback_checks::derived(
                    policy,
                    checkpoint,
                    state.before_upgrade.as_deref().unwrap_or_default(),
                    &stage.snapshots,
                    &stage.name,
                );
                for mut check in derived {
                    if blocked {
                        check.outcome = CheckOutcome::Skipped;
                        check.message =
                            "required rollback prerequisite was skipped or failed".into();
                    } else if state.availability != Availability::Running {
                        check.outcome = CheckOutcome::Incomplete;
                        check.message = "container unavailable".into();
                    }
                    if !blocked {
                        state.forward &= check.outcome == CheckOutcome::Passed;
                    }
                    stage.checks.push(check);
                }
            }
        }
        let checks = self.recipe.recipe.profiles[&self.report.scenarios[self.index].profile]
            .checks
            .clone();
        let change_ids = self.report.scenarios[self.index]
            .rollback_recovery()
            .map(|p| p.change_checks.clone())
            .unwrap_or_default();
        for check in checks.iter().filter(|c| {
            c.checkpoint == checkpoint
                && (checkpoint != Checkpoint::Interrupted
                    || !matches!(c.expectation, Expectation::Changed { .. })
                    || change_ids.contains(&c.id))
        }) {
            let prerequisite_skipped = self.report.scenarios[self.index]
                .stages
                .iter()
                .rev()
                .find(|s| s.step.is_some())
                .is_some_and(|s| s.outcome == StageOutcome::Skipped);
            let retention_without_configuration =
                matches!(check.expectation, Expectation::Preserved { .. })
                    && state.configured.is_none()
                    && self.report.scenarios[self.index].stages.iter().any(|s| {
                        s.step == Some(StepName::Configure) && s.outcome == StageOutcome::Skipped
                    });
            let mut result = if state.availability == Availability::Running
                && (blocked || prerequisite_skipped || retention_without_configuration)
            {
                CheckResult {
                    mcp_required: matches!(check.expectation, Expectation::McpStdio { .. }),
                    mcp: None,
                    json_expectation: None,
                    json_required: matches!(check.expectation, Expectation::JsonCommand { .. }),
                    command_expectation: None,
                    id: check.id.clone(),
                    checkpoint,
                    outcome: CheckOutcome::Skipped,
                    message: "dependent prerequisite/configuration was deliberately skipped".into(),
                    path: check.expectation.path().map(str::to_owned),
                    command_ids: vec![],
                    backup: None,
                    comparison: None,
                }
            } else if state.availability != Availability::Running {
                CheckResult {
                    mcp_required: matches!(check.expectation, Expectation::McpStdio { .. }),
                    mcp: None,
                    json_expectation: None,
                    json_required: matches!(check.expectation, Expectation::JsonCommand { .. }),
                    command_expectation: None,
                    id: check.id.clone(),
                    checkpoint,
                    outcome: CheckOutcome::Incomplete,
                    message: "container stopped after timeout/cancellation".into(),
                    path: check.expectation.path().map(str::to_owned),
                    command_ids: vec![],
                    backup: None,
                    comparison: None,
                }
            } else {
                self.check(state, check, &mut stage)
            };
            if matches!(
                check.expectation,
                Expectation::Unchanged { .. } | Expectation::Changed { .. }
            ) {
                result.comparison = ComparisonReference::for_checkpoint(checkpoint);
            }
            state.forward &= result.outcome == CheckOutcome::Passed;
            stage.checks.push(result);
            self.update(&stage)?;
        }
        Self::outcome(&mut stage);
        state.forward &=
            stage.aggregate_outcome() == RunOutcome::Passed && !stage.snapshots.is_empty();
        self.update(&stage)?;
        let mut after = StageReport::new(format!("{checkpoint}:after_checks"));
        self.capture(state, &mut after);
        Self::outcome(&mut after);
        // Required observations are sticky prerequisites, including check side effects.
        // Later complete references cannot authorize forward work after missing evidence.
        state.forward &=
            after.aggregate_outcome() == RunOutcome::Passed && !after.snapshots.is_empty();
        self.finish(after)
    }
    fn command_expectation(
        &self,
        check: &Check,
        executed: &ExecutedCommand,
    ) -> Option<CommandExpectationEvidence> {
        let scenario = &self.report.scenarios[self.index];
        let plugin_component = scenario.plugin.as_ref().is_some_and(|p| {
            p.checkpoints
                .values()
                .any(|c| c.components.contains(&check.id))
        });
        if scenario.scenario != ScenarioKind::InterruptedRollback && !plugin_component {
            return None;
        }

        let required = plugin_component
            || matches!(
                (&check.checkpoint, &check.expectation),
                (
                    Checkpoint::BaseInstalled,
                    Expectation::Executable {
                        expected_exit: 0,
                        ..
                    }
                ) | (
                    Checkpoint::KnownGood,
                    Expectation::Command {
                        expected_exit: 0,
                        ..
                    }
                )
            )
            || self.report.scenarios[self.index]
                .rollback_recovery()
                .is_some_and(|p| {
                    p.runtime_checks.contains(&check.id) || p.data_checks.contains(&check.id)
                });
        if !required {
            return None;
        }
        let (expected_exit, marker, executable) = match &check.expectation {
            Expectation::Command {
                expected_exit,
                stdout_contains: Some(marker),
                ..
            } => (*expected_exit, marker, None),
            Expectation::Executable {
                name,
                resolved_path,
                args,
                expected_exit,
                stdout_contains: Some(marker),
                ..
            } => (
                *expected_exit,
                marker,
                Some(ExecutableExpectationEvidence {
                    name: name.clone(),
                    resolved_path: resolved_path.clone(),
                    args: args.clone(),
                }),
            ),
            _ => return None,
        };
        let matched = observed(executed).complete
            && marker.len() as u64 <= self.options.log_limit
            && executed.stdout.contains(marker);
        Some(CommandExpectationEvidence {
            expected_exit,
            stdout_contains: marker.clone(),
            stdout_matched: matched,
            stdout_match: if matched {
                executed
                    .stdout
                    .find(marker)
                    .map(|offset| executed.stdout[offset..offset + marker.len()].to_string())
            } else {
                None
            },
            executable,
        })
    }
    fn check(&mut self, state: &mut State, check: &Check, stage: &mut StageReport) -> CheckResult {
        let command_start = stage.commands.len();
        let mut command_expectation = None;
        let mut result = match &check.expectation {
            Expectation::McpStdio { probe } => {
                let spec = CommandSpec {
                    script: String::new(),
                    timeout_seconds: probe.timeout_seconds,
                    env: probe.env.clone(),
                    pass_env: vec![],
                };
                let request = self.request(
                    state,
                    format!("check:{}", check.id),
                    &spec,
                    ExecutionIdentity::runtime(),
                    probe.argv.clone(),
                    0,
                );
                let observed = match self.environment.probe_mcp(
                    &state.resource,
                    request.clone(),
                    probe.clone(),
                    self.store,
                    self.cancel,
                ) {
                    Ok(observed) => observed,
                    Err(error) => {
                        state.availability = Availability::Unavailable;
                        let mut record = CommandRecord::new(request.id, request.phase);
                        record.identity = request.identity;
                        record.argv = request.argv;
                        record.environment = record.identity.public_environment();
                        record.environment.extend(request.env);
                        record.error = Some(format!("{error:#}"));
                        record.cancelled = self.cancel.is_cancelled();
                        record.outcome = if record.cancelled {
                            CommandOutcome::Cancelled
                        } else {
                            CommandOutcome::Error
                        };
                        if let Err(error) = self.environment.stop(&state.resource) {
                            stage
                                .errors
                                .push(format!("stop after MCP infrastructure error: {error:#}"));
                        }
                        crate::runner::ExecutedMcp {
                            command: ExecutedCommand {
                                record,
                                stdout: String::new(),
                                stdout_complete: false,
                                stdout_utf8: false,
                            },
                            evidence: crate::mcp::McpSession::new(probe.clone())
                                .expect("validated probe")
                                .finish(crate::mcp::McpShutdown::Unverified),
                            container_available: false,
                        }
                    }
                };
                if !observed.container_available {
                    state.availability = Availability::Unavailable;
                }
                if observed.command.record.cancelled {
                    self.cancel.cancel();
                    self.report.cancelled = true;
                }
                stage.commands.push(observed.command.record.clone());
                checks::evaluate_mcp(check, &observed)
            }
            Expectation::JsonCommand {
                script,
                timeout_seconds,
                env,
                pass_env,
                expected_exit,
                max_stdout_bytes,
                ..
            } => {
                let spec = CommandSpec {
                    script: script.clone(),
                    timeout_seconds: *timeout_seconds,
                    env: env.clone(),
                    pass_env: pass_env.clone(),
                };
                let mut request = self.request(
                    state,
                    format!("check:{}", check.id),
                    &spec,
                    ExecutionIdentity::runtime(),
                    vec!["/bin/sh".into(), "-c".into(), script.clone()],
                    *expected_exit,
                );
                request.log_limit = request.log_limit.min(*max_stdout_bytes);
                let executed = self.execute(state, request, stage);
                checks::evaluate_json_command(
                    check,
                    &checks::JsonCommandObservation {
                        exit_status: executed.record.exit_status,
                        stdout: &executed.stdout,
                        complete: executed.stdout_complete
                            && !executed.record.timed_out
                            && !executed.record.cancelled,
                        utf8: executed.stdout_utf8,
                        error: executed.record.error.as_deref(),
                    },
                )
            }
            Expectation::Command {
                script,
                timeout_seconds,
                env,
                pass_env,
                expected_exit,
                ..
            } => {
                let spec = CommandSpec {
                    script: script.clone(),
                    timeout_seconds: *timeout_seconds,
                    env: env.clone(),
                    pass_env: pass_env.clone(),
                };
                let request = self.request(
                    state,
                    format!("check:{}", check.id),
                    &spec,
                    ExecutionIdentity::runtime(),
                    vec!["/bin/sh".into(), "-c".into(), script.clone()],
                    *expected_exit,
                );
                let executed = self.execute(state, request, stage);
                command_expectation = self.command_expectation(check, &executed);
                checks::evaluate_command(check, &observed(&executed))
            }
            Expectation::Executable {
                name,
                args,
                timeout_seconds,
                env,
                pass_env,
                expected_exit,
                ..
            } => {
                let spec = CommandSpec {
                    script: String::new(),
                    timeout_seconds: *timeout_seconds,
                    env: env.clone(),
                    pass_env: pass_env.clone(),
                };
                let request = self.request(
                    state,
                    format!("resolve:{}", check.id),
                    &spec,
                    ExecutionIdentity::runtime(),
                    vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        "command -v \"$1\"".into(),
                        "theseus-resolve".into(),
                        name.clone(),
                    ],
                    0,
                );
                let resolution = self.execute(state, request, stage);
                let resolved = resolution.stdout.trim().to_string();
                if state.availability != Availability::Running
                    || resolution.record.error.is_some()
                    || !resolution.stdout_complete
                    || !resolved.starts_with('/')
                    || resolved.contains('\n')
                    || resolution.record.exit_status != Some(0)
                {
                    checks::evaluate_executable(check, None, &observed(&resolution))
                } else {
                    let mut argv = vec![resolved.clone()];
                    argv.extend(args.iter().cloned());
                    let request = self.request(
                        state,
                        format!("check:{}", check.id),
                        &spec,
                        ExecutionIdentity::runtime(),
                        argv,
                        *expected_exit,
                    );
                    let executed = self.execute(state, request, stage);
                    command_expectation = self.command_expectation(check, &executed);
                    checks::evaluate_executable(check, Some(&resolved), &observed(&executed))
                }
            }
            Expectation::Unchanged { path } => {
                let mut stability = checks::compare_unchanged(
                    path,
                    match check.checkpoint {
                        Checkpoint::Reinstalled => state.before_reinstall.as_deref(),
                        Checkpoint::RolledBack => state.before_upgrade.as_deref(),
                        Checkpoint::Interrupted | Checkpoint::Recovered => {
                            state.before_interrupt.as_deref()
                        }
                        Checkpoint::Downgraded | Checkpoint::DowngradeRejected => {
                            state.before_downgrade.as_deref()
                        }
                        _ => None,
                    }
                    .unwrap_or_default(),
                    &stage.snapshots,
                );
                if check.checkpoint == Checkpoint::DowngradeRejected
                    && stability.outcome == CheckOutcome::Passed
                    && matches!(&self.report.scenarios[self.index].downgrade,
                        Some(crate::recipe::DowngradeContract::SafeRejection { binary_checks, .. }) if binary_checks.contains(&check.id))
                    && state.before_downgrade.as_ref().is_some_and(|snapshots| {
                        snapshots.iter().any(|snapshot| {
                            snapshot
                                .entries
                                .get(path)
                                .is_some_and(|entry| entry.kind == FileKind::Directory)
                        })
                    })
                {
                    stability.outcome = CheckOutcome::Failed;
                    stability.message =
                        "binary scope must be a file or symlink before downgrade".into();
                }
                if check.checkpoint == Checkpoint::RolledBack
                    && self.report.scenarios[self.index]
                        .rollback_recovery()
                        .is_some_and(|p| p.binary_checks.contains(&check.id))
                    && state.before_upgrade.as_ref().is_some_and(|snapshots| {
                        snapshots.iter().any(|s| {
                            s.entries.get(path).is_some_and(|e| {
                                !matches!(e.kind, FileKind::File | FileKind::Symlink)
                            })
                        })
                    })
                {
                    stability.outcome = CheckOutcome::Failed;
                    stability.message =
                        "binary scope must be a file or symlink before upgrade".into();
                }
                CheckResult {
                    mcp_required: matches!(check.expectation, Expectation::McpStdio { .. }),
                    mcp: None,
                    json_expectation: None,
                    json_required: matches!(check.expectation, Expectation::JsonCommand { .. }),
                    command_expectation: None,
                    id: check.id.clone(),
                    checkpoint: check.checkpoint,
                    outcome: stability.outcome,
                    message: stability.message,
                    path: Some(path.clone()),
                    command_ids: vec![],
                    backup: None,
                    comparison: None,
                }
            }
            Expectation::Changed { path } => {
                let r = checks::compare_changed(
                    path,
                    if check.checkpoint == Checkpoint::Interrupted {
                        state.before_interrupt.as_deref()
                    } else {
                        state.before_attempt.as_deref()
                    }
                    .unwrap_or_default(),
                    &stage.snapshots,
                );
                CheckResult {
                    mcp_required: matches!(check.expectation, Expectation::McpStdio { .. }),
                    mcp: None,
                    json_expectation: None,
                    json_required: matches!(check.expectation, Expectation::JsonCommand { .. }),
                    command_expectation: None,
                    id: check.id.clone(),
                    checkpoint: check.checkpoint,
                    outcome: r.outcome,
                    message: r.message,
                    path: Some(path.clone()),
                    command_ids: vec![],
                    backup: None,
                    comparison: None,
                }
            }
            _ => checks::evaluate_filesystem(check, &stage.snapshots, state.configured.as_deref()),
        };
        result.command_expectation = command_expectation;
        result.command_ids = stage.commands[command_start..]
            .iter()
            .map(|c| c.id.clone())
            .collect();
        result
    }
}
pub(super) fn observed(executed: &ExecutedCommand) -> CommandObservation {
    CommandObservation {
        exit_status: executed.record.exit_status,
        stdout: executed.stdout.clone(),
        complete: executed.stdout_complete
            && !executed.record.timed_out
            && !executed.record.cancelled
            && executed.record.error.is_none(),
        error: executed.record.error.clone(),
    }
}
