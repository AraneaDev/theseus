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
struct State {
    resource: OwnedResource,
    dead: bool,
    forward: bool,
    baseline: Vec<Snapshot>,
    previous: Vec<Snapshot>,
    configured: Option<Vec<Snapshot>>,
    sequence: usize,
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
        dead: false,
        forward: true,
        baseline: vec![],
        previous: vec![],
        configured: None,
        sequence: 0,
    };
    let baseline = c.observation(&state);
    state.baseline = baseline.clone();
    state.previous = baseline.clone();
    let mut stage = StageReport::new("baseline");
    stage.snapshots = baseline;
    c.finish(stage)?;
    let kind = c.report.scenarios[c.index].scenario;
    let first = if kind == ScenarioKind::Clean {
        StepName::InstallTarget
    } else {
        StepName::InstallBase
    };
    let first_cp = if kind == ScenarioKind::Clean {
        Checkpoint::TargetInstalled
    } else {
        Checkpoint::BaseInstalled
    };
    c.step(&mut state, first)?;
    c.checkpoint(&mut state, first_cp, false)?;
    c.step(&mut state, StepName::Configure)?;
    c.checkpoint(&mut state, Checkpoint::Configured, true)?;
    if kind == ScenarioKind::Upgrade {
        c.step(&mut state, StepName::Upgrade)?;
        c.checkpoint(&mut state, Checkpoint::TargetInstalled, false)?;
    }
    c.step(&mut state, StepName::Exercise)?;
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
    fn start(&mut self, stage: &StageReport) -> Result<()> {
        let mut running = stage.clone();
        running.outcome = StageOutcome::Running;
        self.report.scenarios[self.index].stages.push(running);
        self.save(EventKind::StageStarted, Some(stage.name.clone()))
    }
    fn update(&mut self, stage: &StageReport) -> Result<()> {
        *self.report.scenarios[self.index].stages.last_mut().unwrap() = stage.clone();
        self.save(EventKind::StageFinished, Some(stage.name.clone()))
    }
    fn finish(&mut self, mut stage: StageReport) -> Result<()> {
        self.start(&stage)?;
        Self::outcome(&mut stage);
        self.update(&stage)
    }
    fn outcome(stage: &mut StageReport) {
        if stage.outcome == StageOutcome::Skipped {
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
        if state.dead {
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
    fn capture(&mut self, state: &mut State, stage: &mut StageReport) {
        stage.snapshots = self.observation(state);
        for current in &stage.snapshots {
            if let Some(before) = state.baseline.iter().find(|b| b.root == current.root) {
                stage.baseline_diff.extend(snapshot::diff(before, current));
            }
            if let Some(before) = state.previous.iter().find(|b| b.root == current.root) {
                stage.stage_diff.extend(snapshot::diff(before, current));
            }
        }
        state.previous = stage.snapshots.clone();
    }
    fn request(
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
    fn execute(
        &mut self,
        state: &mut State,
        request: CommandRequest,
        stage: &mut StageReport,
    ) -> ExecutedCommand {
        let mut executed = match self.environment.execute(
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
                    stdout_complete: false,
                }
            }
        };
        if executed.record.timed_out || executed.record.cancelled {
            state.dead = true;
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
    fn step(&mut self, state: &mut State, step: StepName) -> Result<()> {
        let mut stage = StageReport::new(step.to_string());
        stage.step = Some(step);
        if state.dead || (!state.forward && step != StepName::Uninstall) {
            stage.outcome = StageOutcome::Skipped;
            stage.skip_reason = Some(
                if state.dead {
                    "container stopped after timeout/cancellation"
                } else {
                    "failed prerequisite"
                }
                .into(),
            );
            return self.finish(stage);
        }
        self.start(&stage)?;
        let profile_name = self.report.scenarios[self.index].profile;
        let spec = self.recipe.recipe.profiles[&profile_name].steps[&step].clone();
        let installer = matches!(
            step,
            StepName::InstallBase
                | StepName::InstallTarget
                | StepName::Upgrade
                | StepName::Uninstall
        );
        let identity = if installer && profile_name == ProfileName::System {
            ExecutionIdentity::root()
        } else {
            ExecutionIdentity::runtime()
        };
        let request = self.request(
            state,
            step.to_string(),
            &spec,
            identity,
            vec!["/bin/sh".into(), "-c".into(), spec.script.clone()],
            0,
        );
        let executed = self.execute(state, request, &mut stage);
        state.forward &= executed.record.outcome == CommandOutcome::Succeeded;
        self.capture(state, &mut stage);
        Self::outcome(&mut stage);
        self.update(&stage)
    }
    fn checkpoint(
        &mut self,
        state: &mut State,
        checkpoint: Checkpoint,
        configured: bool,
    ) -> Result<()> {
        let mut stage = StageReport::new(checkpoint.to_string());
        stage.checkpoint = Some(checkpoint);
        self.start(&stage)?;
        self.capture(state, &mut stage);
        if configured
            && !state.dead
            && self.report.scenarios[self.index]
                .stages
                .iter()
                .any(|s| s.step == Some(StepName::Configure) && s.outcome != StageOutcome::Skipped)
        {
            state.configured = Some(stage.snapshots.clone());
        }
        let checks = self.recipe.recipe.profiles[&self.report.scenarios[self.index].profile]
            .checks
            .clone();
        for check in checks.iter().filter(|c| c.checkpoint == checkpoint) {
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
            let result = if !state.dead && (prerequisite_skipped || retention_without_configuration)
            {
                CheckResult {
                    id: check.id.clone(),
                    checkpoint,
                    outcome: CheckOutcome::Skipped,
                    message: "dependent prerequisite/configuration was deliberately skipped".into(),
                    path: check.expectation.path().map(str::to_owned),
                    command_ids: vec![],
                }
            } else if state.dead {
                CheckResult {
                    id: check.id.clone(),
                    checkpoint,
                    outcome: CheckOutcome::Incomplete,
                    message: "container stopped after timeout/cancellation".into(),
                    path: check.expectation.path().map(str::to_owned),
                    command_ids: vec![],
                }
            } else {
                self.check(state, check, &mut stage)
            };
            state.forward &= result.outcome == CheckOutcome::Passed;
            stage.checks.push(result);
            self.update(&stage)?;
        }
        Self::outcome(&mut stage);
        self.update(&stage)?;
        let mut after = StageReport::new(format!("{checkpoint}:after_checks"));
        self.capture(state, &mut after);
        self.finish(after)
    }
    fn check(&mut self, state: &mut State, check: &Check, stage: &mut StageReport) -> CheckResult {
        let command_start = stage.commands.len();
        let mut result = match &check.expectation {
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
                if state.dead
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
                    checks::evaluate_executable(check, Some(&resolved), &observed(&executed))
                }
            }
            _ => checks::evaluate_filesystem(check, &stage.snapshots, state.configured.as_deref()),
        };
        result.command_ids = stage.commands[command_start..]
            .iter()
            .map(|c| c.id.clone())
            .collect();
        result
    }
}
fn observed(executed: &ExecutedCommand) -> CommandObservation {
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
