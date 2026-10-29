use crate::{
    engine::RunOptions,
    model::*,
    recipe::{Expectation, ValidatedRecipe},
    report,
};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Scenario,
    Stage,
    Evidence,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Stages,
    Findings,
    Files,
    Commands,
    Logs,
}
impl Tab {
    pub const ALL: [Self; 5] = [
        Self::Stages,
        Self::Findings,
        Self::Files,
        Self::Commands,
        Self::Logs,
    ];
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).unwrap()
    }
}
/// UI state contains evidence and selections only; execution belongs to engine::run.
pub struct App {
    pub recipe: Option<ValidatedRecipe>,
    pub options: Option<RunOptions>,
    pub report: Option<RunReport>,
    pub root: PathBuf,
    pub scenario_index: usize,
    pub stage_index: usize,
    pub evidence_index: usize,
    pub focus: Focus,
    pub tab: Tab,
    pub filter: String,
    pub editing_filter: bool,
    pub scroll: u16,
    pub log_text: String,
    pub status: String,
    /// Authoritative worker failure, separate from the last persisted evidence.
    pub terminal_error: Option<String>,
    pub active: bool,
    pub start_requested: bool,
    pub cancel_requested: bool,
    pub quit_requested: bool,
}
impl App {
    pub fn offline(report: RunReport, root: PathBuf) -> Self {
        Self {
            recipe: None,
            options: None,
            report: Some(report),
            root,
            scenario_index: 0,
            stage_index: 0,
            evidence_index: 0,
            focus: Focus::Scenario,
            tab: Tab::Stages,
            filter: String::new(),
            editing_filter: false,
            scroll: 0,
            log_text: String::new(),
            status: "Offline report".into(),
            terminal_error: None,
            active: false,
            start_requested: false,
            cancel_requested: false,
            quit_requested: false,
        }
    }
    pub fn preview(recipe: ValidatedRecipe, options: RunOptions) -> Self {
        let mut app = Self::offline(RunReport::new("preview"), PathBuf::new());
        app.report = None;
        app.recipe = Some(recipe);
        app.options = Some(options);
        app.status = "Preview — press r to start".into();
        app
    }
    pub fn outcome(&self) -> RunOutcome {
        if self.terminal_error.is_some() {
            RunOutcome::Incomplete
        } else {
            self.report
                .as_ref()
                .map_or(RunOutcome::Passed, |r| r.outcome)
        }
    }
    pub fn exit_code(&self) -> i32 {
        self.outcome().exit_code()
    }
    pub fn scenario(&self) -> Option<&ScenarioReport> {
        self.report.as_ref()?.scenarios.get(self.scenario_index)
    }
    pub fn stage(&self) -> Option<&StageReport> {
        self.scenario()?.stages.get(self.stage_index)
    }
    pub fn set_report(&mut self, report: RunReport, root: PathBuf) {
        self.report = Some(report);
        self.root = root;
        self.scenario_index = self.scenario_index.min(
            self.report
                .as_ref()
                .unwrap()
                .scenarios
                .len()
                .saturating_sub(1),
        );
        self.stage_index = self.stage_index.min(
            self.scenario()
                .map_or(0, |s| s.stages.len().saturating_sub(1)),
        );
        self.refresh_logs();
    }
    pub fn preview_text(&self) -> String {
        let (Some(recipe), Some(options)) = (&self.recipe, &self.options) else {
            return String::new();
        };
        let mut text = format!(
            "Preview: {}\nImage: {} | network: {:?} | pull: {}\nProfiles: {:?}   Scenarios: {:?}\np: cycle profiles | s: cycle scenarios | r: run explicitly\nRuntime UID/GID 10001:10001 | system installation 0:0\nHOME=/home/theseus XDG_CONFIG_HOME=/home/theseus/.config\nXDG_DATA_HOME=/home/theseus/.local/share XDG_CACHE_HOME=/home/theseus/.cache\nPATH=/home/theseus/.local/bin:/usr/local/bin:/usr/bin:/bin\nWorking directory: /workspace | root installation HOME=/root\nObserve: {:?}\nInputs: {:?}\n",
            recipe.recipe.name,
            recipe.recipe.image,
            recipe.recipe.network,
            options.pull,
            options.profiles,
            options.scenarios,
            recipe.recipe.observe,
            recipe.recipe.artifacts
        );
        for name in &options.profiles {
            if let Some(profile) = recipe.recipe.profiles.get(name) {
                text += &format!("\n[{name}]\n");
                if options.scenarios.iter().any(|kind| {
                    matches!(
                        kind,
                        ScenarioKind::InterruptedInstall | ScenarioKind::InterruptedUpgrade
                    )
                }) {
                    text += "Interruption: ContainerSigkill at synchronous THESEUS_INTERRUPT_BARRIER; abrupt whole-container stop, same-ID restart without setup or input copying.\nPartial interrupted checks run after restart and before explicit recovery.\nUnchanged compares before_interrupt -> interrupted/recovered (before checks). Preserved uses configured.\n";
                    for (kind, interrupt, recover) in [
                        (
                            ScenarioKind::InterruptedInstall,
                            StepName::InterruptInstall,
                            StepName::RecoverInstall,
                        ),
                        (
                            ScenarioKind::InterruptedUpgrade,
                            StepName::InterruptUpgrade,
                            StepName::RecoverUpgrade,
                        ),
                    ] {
                        if options.scenarios.contains(&kind) {
                            text += match kind {
                                ScenarioKind::InterruptedInstall => {
                                    "before_interrupt: before initial installation.\n"
                                }
                                ScenarioKind::InterruptedUpgrade => {
                                    "before_interrupt: after first exercise.\n"
                                }
                                _ => unreachable!(),
                            };
                            if let Some(command) = profile.steps.get(&interrupt) {
                                text += &format!(
                                    "{kind} barrier/stop budget: {}s\n",
                                    command.timeout_seconds
                                );
                            }
                            if let Some(command) = profile.steps.get(&recover) {
                                text += &format!("Explicit recovery command: {}\n", command.script);
                            }
                        }
                    }
                }
                if options.scenarios.contains(&ScenarioKind::Reinstall) {
                    text += "Reinstall rehearsal: install_target, configure, exercise_before_reinstall, before_reinstall, reinstall, reinstalled, exercise_after_reinstall, uninstall.\nBoth exercise stages run the same exercise command as UID/GID 10001:10001.\n";
                    if let Some(command) = profile.steps.get(&StepName::Reinstall) {
                        text +=
                            &format!("Repeated target installation command: {}\n", command.script);
                    }
                    let stable: Vec<_> = profile
                        .checks
                        .iter()
                        .filter(|check| check.checkpoint == Checkpoint::Reinstalled)
                        .filter_map(|check| match &check.expectation {
                            Expectation::Unchanged { path } => Some(path.as_str()),
                            _ => None,
                        })
                        .collect();
                    text += &format!(
                        "Declared stable scopes (path and descendants): {stable:?}\nCompare before_reinstall (after first exercise) to reinstalled (before checks and second exercise).\n"
                    );
                }
                for (step, command) in &profile.steps {
                    text += &format!(
                        "{step} ({}s) env={:?} pass_env names={:?}\n{}\n",
                        command.timeout_seconds, command.env, command.pass_env, command.script
                    );
                }
                for check in &profile.checks {
                    text += &format!(
                        "check {} @ {}: {:?}\n",
                        check.id, check.checkpoint, check.expectation
                    );
                }
            }
        }
        safe_text(&text)
    }
    /// One item per evidence record. Filtering never mutates the stored report.
    pub fn items(&self) -> Vec<String> {
        let Some(stage) = self.stage() else {
            let mut items = self
                .report
                .as_ref()
                .map(|r| r.errors.clone())
                .unwrap_or_default();
            if let Some(scenario) = self.scenario() {
                items.extend(scenario.errors.iter().cloned());
                items.push(format!("Teardown: {:?}", scenario.teardown));
            }
            return self.filter_items(items);
        };
        let mut items = Vec::new();
        if matches!(self.tab, Tab::Stages | Tab::Commands) {
            if let Some(proof) = &stage.restart {
                items.push(format!(
                    "Restart: {} running={}",
                    proof.container_id, proof.running_verified
                ));
            }
            for command in &stage.commands {
                if let Some(proof) = &command.interruption {
                    items.push(format!(
                        "Interruption {:?}: readiness={} stopped={}",
                        proof.mechanism, proof.readiness_verified, proof.stopped_verified
                    ));
                    if command.outcome == CommandOutcome::Failed
                        && command.exit_status.is_some()
                        && !proof.readiness_verified
                    {
                        items.push(format!(
                            "Barrier not reached: ordinary completion exit {:?}",
                            command.exit_status
                        ));
                    }
                }
            }
        }
        match self.tab {
            Tab::Stages => {
                items.push(format!("{}: {}", stage.name, stage.outcome));
                items.extend(stage.checks.iter().map(check_text));
                items.extend(stage.errors.iter().cloned());
                if let Some(reason) = &stage.skip_reason {
                    items.push(format!("Skipped: {reason}"));
                }
                for snap in &stage.snapshots {
                    items.push(format!(
                        "Observation {}: {:?} ({} entries)",
                        snap.root,
                        snap.completeness,
                        snap.entries.len()
                    ));
                }
            }
            Tab::Findings => {
                items.extend(stage.checks.iter().map(check_text));
                items.extend(stage.errors.iter().cloned());
                if let Some(scenario) = self.scenario() {
                    items.extend(
                        scenario
                            .findings
                            .iter()
                            .map(|f| format!("Review {} {:?}: {}", f.kind, f.path, f.message)),
                    );
                    items.extend(scenario.errors.iter().cloned());
                    items.push(format!("Teardown: {:?}", scenario.teardown));
                }
                if let Some(report) = &self.report {
                    items.extend(report.errors.iter().cloned());
                }
                for snapshot in &stage.snapshots {
                    if !matches!(
                        snapshot.completeness,
                        crate::snapshot::ObservationStatus::Complete
                    ) {
                        items.push(format!("{} {:?}", snapshot.root, snapshot.completeness));
                    }
                }
            }
            Tab::Files => {
                items.extend(
                    stage
                        .checks
                        .iter()
                        .filter(|check| check.comparison.is_some())
                        .map(check_text),
                );
                for (label, changes) in [
                    ("Stage", &stage.stage_diff),
                    ("Baseline", &stage.baseline_diff),
                ] {
                    for change in changes {
                        items.push(format!(
                            "{label} {:?} {}\nbefore: {:?}\nafter: {:?}",
                            change.kind, change.path, change.before, change.after
                        ));
                    }
                }
                for snapshot in &stage.snapshots {
                    items.push(format!(
                        "Observation {} {:?}",
                        snapshot.root, snapshot.completeness
                    ));
                    items.extend(snapshot.entries.iter().map(|(p, e)| format!("{p}: {e:?}")));
                }
            }
            Tab::Commands => {
                items.extend(stage.commands.iter().map(|c|format!("{} {}: {}\nargv: {:?}\nUID:GID {}:{} account home={} cwd={}\nenv: {:?}\nforwarded env (values hidden): {:?}\nUTC {} | {}ms | exit {:?} | timeout={} cancelled={}\nstdout: {:?}\nstderr: {:?}\nerror: {:?} skipped: {:?}",c.id,c.phase,c.outcome,c.argv,c.identity.uid,c.identity.gid,c.identity.home,c.workdir,c.environment,c.forwarded_environment,c.started_at,c.duration_ms,c.exit_status,c.timed_out,c.cancelled,c.stdout,c.stderr,c.error,c.skip_reason)));
            }
            Tab::Logs => {
                for command in &stage.commands {
                    for (stream, log) in [("stdout", &command.stdout), ("stderr", &command.stderr)]
                    {
                        if let Some(log) = log {
                            items.push(format!(
                                "{} {stream}: {} ({} bytes, truncated={})",
                                command.id, log.path, log.bytes, log.truncated
                            ));
                        }
                    }
                }
            }
        }
        self.filter_items(items)
    }
    fn filter_items(&self, items: Vec<String>) -> Vec<String> {
        let needle = self.filter.to_lowercase();
        let mut items: Vec<_> = items
            .into_iter()
            .map(|s| safe_text(&s))
            .filter(|s| s.to_lowercase().contains(&needle))
            .collect();
        // A stale evidence filter must never hide a terminal infrastructure error.
        if let Some(error) = &self.terminal_error {
            items.insert(0, safe_text(error));
        }
        items
    }
    pub fn refresh_logs(&mut self) {
        self.log_text.clear();
        if self.tab != Tab::Logs {
            return;
        }
        let Some(item) = self.items().get(self.evidence_index).cloned() else {
            return;
        };
        let log = self
            .stage()
            .into_iter()
            .flat_map(|s| &s.commands)
            .flat_map(|c| c.stdout.iter().chain(&c.stderr))
            .find(|l| item.contains(&format!(": {} (", safe_text(&l.path))))
            .cloned();
        if let Some(log) = log {
            self.log_text = match report::read_log(&self.root, &log.path, 64 * 1024) {
                Ok(read) => format!(
                    "{}\n{}",
                    if read.truncated || log.truncated {
                        "[truncated: showing at most 64 KiB]"
                    } else {
                        "[complete stored stream]"
                    },
                    safe_text(&String::from_utf8_lossy(&read.bytes))
                ),
                Err(error) => format!("Cannot read log: {error:#}"),
            };
        }
    }
}
/// Prevent saved command output and recipe text from injecting terminal controls.
pub fn safe_text(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                '�'
            } else {
                c
            }
        })
        .collect()
}

fn check_text(check: &CheckResult) -> String {
    let mut text = format!("{} {}: {}", check.id, check.outcome, check.message);
    if let Some(path) = &check.path {
        text += &format!("\nScope: {path}");
    }
    if let Some(comparison) = &check.comparison {
        text += &format!(
            "\nComparison: {} -> {}",
            comparison.before_stage, comparison.after_stage
        );
    }
    text
}
