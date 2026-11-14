use crate::{engine::RunOptions, model::*, recipe::ValidatedRecipe};
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
    pub native: Option<super::native::NativeView>,
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
    /// Wall-clock start of the explicit run and its frozen duration once the
    /// worker returns; presentation only, never persisted as evidence.
    pub(super) run_started: Option<std::time::Instant>,
    pub(super) run_elapsed: Option<std::time::Duration>,
    /// Scenario id and stage named by the latest engine event, if any.
    pub(super) live_stage: Option<(String, Option<String>)>,
    pub start_requested: bool,
    pub cancel_requested: bool,
    pub quit_requested: bool,
    /// The `?` key reference is open; it swallows every other key.
    pub help: bool,
    /// The user moved a selection or focus; completion then keeps their place.
    pub(super) navigated: bool,
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
            native: None,
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
            run_started: None,
            run_elapsed: None,
            live_stage: None,
            start_requested: false,
            cancel_requested: false,
            quit_requested: false,
            help: false,
            navigated: false,
        }
    }
    /// A saved report as opened from the CLI: the first scenario and stage
    /// needing attention is pre-selected.
    pub fn open_saved(report: RunReport, root: PathBuf) -> Self {
        let mut app = Self::offline(report, root);
        app.select_attention();
        app
    }
    /// Select the first failed, else incomplete, else review-finding scenario
    /// and its first such stage. Leaves the selection alone when all passed.
    pub(super) fn select_attention(&mut self) {
        let Some((scenario, stage)) = self.report.as_ref().and_then(first_attention) else {
            return;
        };
        self.scenario_index = scenario;
        self.stage_index = stage;
        self.evidence_index = 0;
        self.scroll = 0;
    }
    pub fn native_offline(report: crate::native::model::NativeReport, root: PathBuf) -> Self {
        let mut app = Self::offline(RunReport::new("unused"), root);
        app.report = None;
        app.native = Some(super::native::NativeView::offline(report));
        app.status = "Offline native report".into();
        app
    }
    pub fn set_native_report(&mut self, report: crate::native::model::NativeReport, root: PathBuf) {
        if let Some(view) = &mut self.native {
            view.set_report(report);
        } else {
            self.native = Some(super::native::NativeView::offline(report));
        }
        self.root = root;
        self.view_revision = self.view_revision.wrapping_add(1);
    }
    pub fn integration_offline(
        report: crate::native::integration::model::NativeIntegrationReport,
        root: PathBuf,
    ) -> Self {
        let mut app = Self::offline(RunReport::new("unused"), root);
        app.report = None;
        app.native = Some(super::native::NativeView::integration_offline(report));
        app.status = "Offline native integration report".into();
        app
    }
    pub fn set_integration_report(
        &mut self,
        report: crate::native::integration::model::NativeIntegrationReport,
        root: PathBuf,
    ) {
        if let Some(view) = &mut self.native {
            view.set_integration_report(report);
        } else {
            self.native = Some(super::native::NativeView::integration_offline(report));
        }
        self.root = root;
        self.view_revision = self.view_revision.wrapping_add(1);
    }
    pub fn native_preview(text: String) -> Self {
        let mut app = Self::offline(RunReport::new("unused"), PathBuf::new());
        app.report = None;
        app.native = Some(super::native::NativeView::preview(text));
        app.status = "Native preview".into();
        app
    }
    pub fn preview(recipe: ValidatedRecipe, options: RunOptions) -> Self {
        let mut app = Self::offline(RunReport::new("preview"), PathBuf::new());
        app.report = None;
        app.recipe = Some(recipe);
        app.options = Some(options);
        app.status = "Preview — press r to start".into();
        app
    }
    /// Time since the explicit start, frozen when the worker returned.
    pub(super) fn run_clock(&self) -> Option<std::time::Duration> {
        self.run_elapsed
            .or_else(|| self.run_started.map(|start| start.elapsed()))
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
        } else if let Some(view) = &self.native {
            view.outcome()
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
    /// Plain-text form of the styled preview; both come from one document.
    pub fn preview_text(&self) -> String {
        super::preview_doc::build(self).text()
    }
    pub(super) fn preview_lines(&self) -> Vec<ratatui::text::Line<'static>> {
        super::preview_doc::build(self).lines()
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
/// The first scenario/stage needing attention, by precedence failed >
/// incomplete > review. A stage qualifies by its outcome or any of its checks;
/// a scenario whose own outcome qualifies without such a stage selects its
/// first stage. Review findings are per scenario, so they select its first stage.
pub(super) fn first_attention(report: &RunReport) -> Option<(usize, usize)> {
    let ranked = |stage_outcome, check_outcome, run_outcome| {
        report
            .scenarios
            .iter()
            .enumerate()
            .find_map(|(i, scenario)| {
                scenario
                    .stages
                    .iter()
                    .position(|stage| {
                        stage.outcome == stage_outcome
                            || stage.checks.iter().any(|c| c.outcome == check_outcome)
                    })
                    .or((scenario.outcome == run_outcome).then_some(0))
                    .map(|stage| (i, stage))
            })
    };
    ranked(
        StageOutcome::Failed,
        CheckOutcome::Failed,
        RunOutcome::Failed,
    )
    .or_else(|| {
        ranked(
            StageOutcome::Incomplete,
            CheckOutcome::Incomplete,
            RunOutcome::Incomplete,
        )
    })
    .or_else(|| {
        report
            .scenarios
            .iter()
            .position(|s| !s.findings.is_empty())
            .map(|i| (i, 0))
    })
}
/// Header outcome word: "running" while a run is active (its report is still
/// partial and would otherwise read as "incomplete"), else the observed outcome.
pub(super) fn live_outcome_word(app: &App) -> String {
    if app.active {
        "running".into()
    } else {
        app.outcome().to_string()
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
