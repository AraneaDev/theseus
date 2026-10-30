use crate::{
    engine::RunOptions,
    model::*,
    recipe::{Expectation, ValidatedRecipe},
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
    pub(super) viewport: std::cell::RefCell<super::view::View>,
    pub(super) view_revision: u64,
    pub(super) work_consumed: bool,
    pub(super) filter_draft: String,
    pub(super) edit_session: Option<super::preview_edit::Session>,
    pub comparison: Option<super::comparison::ComparisonView>,
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
            viewport: Default::default(),
            view_revision: 0,
            work_consumed: false,
            filter_draft: String::new(),
            edit_session: None,
            comparison: None,
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
    pub(super) fn has_started(&self) -> bool {
        self.work_consumed || self.active || self.terminal_error.is_some()
    }
    fn missing_outcome(&self) -> RunOutcome {
        if self.has_started() {
            RunOutcome::Incomplete
        } else {
            RunOutcome::Passed
        }
    }
    pub(super) fn pending_text(&self) -> String {
        let project = super::render::projection;
        let mut text = format!(
            "Live evidence pending or unavailable.\nSaved evidence root: {}\n{}",
            project(&self.root.display().to_string(), 4096),
            project(&self.status, 4096)
        );
        if let Some(error) = &self.terminal_error {
            text.push_str(&format!("\n{}", project(error, 4096)));
        }
        safe_text(&text)
    }
    pub fn outcome(&self) -> RunOutcome {
        if self.terminal_error.is_some() {
            RunOutcome::Incomplete
        } else if let Some(view) = &self.comparison {
            view.report
                .as_ref()
                .map_or(self.missing_outcome(), |r| r.outcome)
        } else {
            self.report
                .as_ref()
                .map_or(self.missing_outcome(), |r| r.outcome)
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
    pub(super) fn pending_root(&mut self, root: PathBuf) {
        if self.root != root {
            self.report = None;
            if let Some(view) = &mut self.comparison {
                view.report = None;
            }
            self.root = root;
            self.view_revision = self.view_revision.wrapping_add(1);
            *self.viewport.borrow_mut() = Default::default();
            self.log_text.clear();
            self.scroll = 0;
        }
    }
    pub fn set_report(&mut self, report: RunReport, root: PathBuf) {
        self.view_revision = self.view_revision.wrapping_add(1);
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
                if options
                    .scenarios
                    .contains(&ScenarioKind::InterruptedRollback)
                {
                    text += "Interrupted rollback artifact roles: base = known-good; target = interrupted candidate.\nInterruption: ContainerSigkill at synchronous THESEUS_INTERRUPT_BARRIER; abrupt whole-container stop, same-ID restart without setup or input copying.\nOriginal reference: before_upgrade after KnownGood checks; Changed: before_interrupt -> interrupted before checks; restoration: before_upgrade -> rolled_back.\nFresh before_rollback revalidates backup after Interrupted check effects.\n";
                    if let Some(command) = profile.steps.get(&StepName::InterruptUpgrade) {
                        text += &format!(
                            "Candidate barrier/stop budget: {}s\n",
                            command.timeout_seconds
                        );
                    }
                    if let Some(command) = profile.steps.get(&StepName::Rollback) {
                        text += &format!("Explicit rollback command: {}\n", command.script);
                    }
                    if let Some(policy) = &profile.interrupted_rollback {
                        text += &format!(
                            "Changed checks: {:?}; binary checks: {:?}; runtime checks: {:?}; data readers: {:?}\n",
                            policy.recovery.change_checks,
                            policy.recovery.binary_checks,
                            policy.recovery.runtime_checks,
                            policy.recovery.data_checks
                        );
                        match &policy.recovery.restoration {
                            crate::recipe::RestorationPolicy::BinaryOnly => {
                                text += "Policy: binary_only; known-good runtime must read retained partially changed data.\n"
                            }
                            crate::recipe::RestorationPolicy::RestoreData {
                                backup_root,
                                scopes,
                            } => {
                                text += &format!(
                                    "Policy: restore_data; backup root {backup_root}\nDerived checks: __theseus_rollback_root_absent (Backup before command); __theseus_rollback_backup_removed (BackupRemoved)\n"
                                );
                                for scope in scopes {
                                    text += &format!(
                                        "Scope {}: {} -> {backup_root}/{}\n",
                                        scope.id, scope.source, scope.stored
                                    );
                                    for (phase, label) in [
                                        ("BackupCreated", "source_stable"),
                                        ("BackupCreated", "backup_matches"),
                                        ("Interrupted", "backup_intact"),
                                        ("before_rollback", "backup_ready"),
                                        ("RolledBack", "restored"),
                                        ("RolledBack", "backup_retained"),
                                    ] {
                                        text += &format!(
                                            "Derived {phase}: __theseus_rollback_{label}_{}\n",
                                            scope.id
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                if options.scenarios.contains(&ScenarioKind::Rollback) {
                    text += "Rollback artifact roles: base = known-good; target = failed candidate.\nKnown-good reference: before_upgrade after KnownGood checks; Changed: before_attempt -> upgrade_failed; restore: before_upgrade -> rolled_back.\n";
                    if let Some(policy) = &profile.rollback {
                        text += &format!(
                            "Expected failure: attempt_upgrade exit={} marker={:?}\nChanged checks: {:?}; binary checks: {:?}; runtime checks: {:?}; data readers: {:?}\n",
                            policy.failure.expected_exit,
                            policy.failure.stdout_contains,
                            policy.recovery.change_checks,
                            policy.recovery.binary_checks,
                            policy.recovery.runtime_checks,
                            policy.recovery.data_checks
                        );
                        match &policy.recovery.restoration {
                            crate::recipe::RestorationPolicy::BinaryOnly => {
                                text += "Policy: binary_only; known-good runtime must read retained data.\n"
                            }
                            crate::recipe::RestorationPolicy::RestoreData {
                                backup_root,
                                scopes,
                            } => {
                                text += &format!(
                                    "Policy: restore_data; backup root {backup_root}\nDerived checks: __theseus_rollback_root_absent (Backup before command, before_upgrade reference); __theseus_rollback_backup_removed (BackupRemoved)\n"
                                );
                                for scope in scopes {
                                    text += &format!(
                                        "Scope {}: {} -> {backup_root}/{}\n",
                                        scope.id, scope.source, scope.stored
                                    );
                                    for (phase, label) in [
                                        ("BackupCreated", "source_stable"),
                                        ("BackupCreated", "backup_matches"),
                                        ("UpgradeFailed", "backup_intact"),
                                        ("before_rollback", "backup_ready"),
                                        ("RolledBack", "restored"),
                                        ("RolledBack", "backup_retained"),
                                    ] {
                                        text += &format!(
                                            "Derived {phase}: __theseus_rollback_{label}_{}\n",
                                            scope.id
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                if options.scenarios.contains(&ScenarioKind::Downgrade) {
                    text += "Downgrade artifact roles: target = newer installed; base = older requested.\nCompare before_downgrade (after newer exercise) to the declared policy checkpoint before checks. Preserved uses configured.\n";
                    if let Some(policy) = &profile.downgrade {
                        match policy {
                            crate::recipe::DowngradeContract::Compatible { data_checks } => {
                                text += &format!(
                                    "Policy: compatible; older runtime and data reader checks: {data_checks:?}\n"
                                );
                            }
                            crate::recipe::DowngradeContract::SafeRejection {
                                expected_exit,
                                stdout_contains,
                                binary_checks,
                                data_checks,
                            } => {
                                text += &format!(
                                    "Policy: safe_rejection; expected refusal exit={expected_exit}, stdout marker={stdout_contains:?}\nBinary scopes: {binary_checks:?}; data scopes: {data_checks:?}\n"
                                );
                            }
                        }
                    }
                    for step in [StepName::Exercise, StepName::ExerciseDowngraded] {
                        if let Some(command) = profile.steps.get(&step) {
                            text += &format!("Downgrade runtime {step}: {}\n", command.script);
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
        let catalog = super::evidence::Catalog::for_app(self);
        let index = super::evidence::FilterIndex::new(&catalog, &self.filter);
        index
            .records(&catalog)
            .map(|selected| selected.record.text())
            .collect()
    }
    pub fn refresh_logs(&mut self) {
        // Explicit refresh also checks same-path replacement and retries errors.
        self.viewport.borrow_mut().force_refresh();
        self.sync_view();
    }
    pub(super) fn sync_view(&mut self) {
        let mut view = self.viewport.borrow_mut();
        view.update(self, None);
        self.log_text = if self.tab == Tab::Logs && view.raw_log() {
            format!("{}\n{}", view.label, view.body)
        } else {
            String::new()
        };
    }
    pub(super) fn evidence_len(&self) -> usize {
        self.viewport.borrow_mut().len(self)
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
