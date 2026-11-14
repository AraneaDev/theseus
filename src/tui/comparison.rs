//! Suite state and workbench rendering; lifecycle execution stays in the shared worker.
use super::{
    App, safe_text,
    theme::{self, Hint, REQUIRED, hint},
};
use crate::comparison::{
    ComparisonReport, LoadedComparison, SuiteManifest, SuiteRunOptions, ValidatedSuite,
};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use std::path::PathBuf;

pub struct ComparisonView {
    pub manifest: SuiteManifest,
    pub suite: Option<ValidatedSuite>,
    pub options: Option<SuiteRunOptions>,
    pub report: Option<ComparisonReport>,
    pub row: usize,
    pub evidence_focus: bool,
}
impl App {
    /// Preview is read-only until an explicit r reaches the shared workbench.
    pub fn comparison_preview(suite: ValidatedSuite, options: SuiteRunOptions) -> Self {
        let mut app = Self::offline(crate::model::RunReport::new("preview"), PathBuf::new());
        app.report = None;
        app.comparison = Some(ComparisonView {
            manifest: suite.manifest.clone(),
            suite: Some(suite),
            options: Some(options),
            report: None,
            row: 0,
            evidence_focus: false,
        });
        app.status = "Comparison preview — r starts the declared suite".into();
        app
    }
    /// Context and source paths must come from comparison::load_document.
    pub fn comparison_offline(document: LoadedComparison, root: PathBuf) -> Self {
        let mut app = Self::offline(crate::model::RunReport::new("comparison"), PathBuf::new());
        app.report = None;
        app.comparison = Some(ComparisonView {
            manifest: document.manifest,
            suite: None,
            options: None,
            report: None,
            row: 0,
            evidence_focus: false,
        });
        app.set_comparison_report(document.report, root);
        app.status = "Offline comparison".into();
        app
    }
    pub fn set_comparison_report(&mut self, report: ComparisonReport, root: PathBuf) {
        self.view_revision = self.view_revision.wrapping_add(1);
        if let Some(view) = &mut self.comparison {
            view.row = view.row.min(report.cells.len().saturating_sub(1));
            view.report = Some(report);
            self.root = if root.file_name().is_some_and(|n| n == "comparison.json") {
                root.parent()
                    .unwrap_or(std::path::Path::new("."))
                    .to_path_buf()
            } else {
                root
            };
        }
    }
}
pub(super) fn input(app: &mut App, key: KeyEvent) {
    let evidence_focus = app.comparison.as_ref().unwrap().evidence_focus;
    match key.code {
        KeyCode::Char('q') => {
            app.quit_requested = true;
            if app.active {
                app.cancel_requested = true;
            }
        }
        KeyCode::Char('c') if app.active => app.cancel_requested = true,
        KeyCode::Char('r')
            if !app.active
                && !app.work_consumed
                && !app.quit_requested
                && app
                    .comparison
                    .as_ref()
                    .is_some_and(|v| v.suite.is_some() && v.report.is_none()) =>
        {
            app.start_requested = true
        }
        KeyCode::Tab => {
            app.comparison.as_mut().unwrap().evidence_focus = !evidence_focus;
            app.scroll = 0;
        }
        KeyCode::PageDown => app.scroll = app.scroll.saturating_add(10),
        KeyCode::PageUp => app.scroll = app.scroll.saturating_sub(10),
        KeyCode::Home
        | KeyCode::End
        | KeyCode::Down
        | KeyCode::Char('j')
        | KeyCode::Up
        | KeyCode::Char('k') => {
            let down = matches!(key.code, KeyCode::Down | KeyCode::Char('j'));
            if app.comparison.as_ref().unwrap().report.is_none() {
                app.scroll = if down {
                    app.scroll.saturating_add(1)
                } else {
                    app.scroll.saturating_sub(1)
                };
                return;
            }
            let len = if evidence_focus {
                app.evidence_len()
            } else {
                app.comparison
                    .as_ref()
                    .unwrap()
                    .report
                    .as_ref()
                    .map_or(0, |r| r.cells.len())
            };
            let index = if evidence_focus {
                &mut app.evidence_index
            } else {
                &mut app.comparison.as_mut().unwrap().row
            };
            *index = match key.code {
                KeyCode::Home => 0,
                KeyCode::End => len.saturating_sub(1),
                _ if down => index.saturating_add(1).min(len.saturating_sub(1)),
                _ => index.saturating_sub(1),
            };
            if !evidence_focus {
                app.evidence_index = 0;
            }
            app.scroll = 0;
        }
        _ => {}
    }
    app.sync_view();
}
fn bounded(text: &str) -> String {
    super::render::projection(text, 512)
}
fn panel(title: &str) -> Block<'_> {
    Block::default().title(title).borders(Borders::ALL)
}
fn heading_field(label: &str, value: &str, width: usize, limit: usize) -> String {
    let available = width.saturating_sub(label.len());
    let value = super::render::projection(value, available.min(limit).max(4));
    // A saved stage can contain a newline. Keep each projected field on its
    // reserved row, with the complete original still available in Cell detail.
    format!("{label}{}", value.replace(['\n', '\t'], " "))
}
pub(super) fn render(frame: &mut Frame, app: &App) {
    let view = app.comparison.as_ref().unwrap();
    let m = &view.manifest;
    let status = if super::preview_edit::dirty(app) {
        "UNSAVED draft".to_string()
    } else if app.editing_filter {
        format!("Filter (Enter/Esc apply): {}_", bounded(&app.filter_draft))
    } else {
        bounded(&app.status)
    };
    if view.report.is_some() {
        app.viewport.borrow_mut().update(app, None);
    }
    let width = frame.area().width;
    let footer = theme::footer(&status, &hints(app, view), width);
    let layout = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(theme::footer_height(&footer, width)),
    ])
    .split(frame.area());
    let suffix = if view.report.is_none() && app.has_started() {
        " | Live evidence pending or unavailable"
    } else if view.report.is_none() {
        if super::preview_edit::dirty(app) {
            " | UNSAVED draft — accepted preview; no commands executed"
        } else {
            " | Preview — no commands executed"
        }
    } else {
        ""
    };
    let mut spans = vec![
        Span::styled(
            safe_text(&bounded(&m.name)),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(safe_text(&format!(" | Baseline: {}", bounded(&m.baseline)))),
    ];
    // Before a report exists there is no observed outcome to show; a default
    // "passed" in a preview would read as a result.
    if view.report.is_some() {
        spans.push(Span::raw(" | "));
        spans.push(theme::status_word(&app.outcome().to_string(), true));
    }
    spans.push(Span::raw(suffix));
    let title = Line::from(spans);
    frame.render_widget(
        Paragraph::new(title)
            .block(panel("Theseus comparison"))
            .wrap(Wrap { trim: false }),
        layout[0],
    );
    if let Some(report) = &view.report {
        let narrow = frame.area().width < 100;
        let parts = if narrow {
            // At minimum terminal height, give the selected Cell detail room
            // even though the cell selector itself cannot fit on screen.
            let selector_height = if layout[1].height < 18 { 0 } else { 6 };
            Layout::vertical([Constraint::Length(selector_height), Constraint::Min(1)])
                .split(layout[1])
        } else {
            Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                .split(layout[1])
        };
        let row_start = view
            .row
            .saturating_sub(32)
            .min(report.cells.len().saturating_sub(64));
        let rows = report
            .cells
            .iter()
            .skip(row_start)
            .take(64)
            .map(|c| {
                (
                    format!(
                        "{} {}/{}/{} {} {:?}",
                        bounded(&c.variant_id),
                        c.profile,
                        c.scenario,
                        bounded(&c.stage),
                        bounded(&c.scope_id),
                        c.comparison.status
                    ),
                    format!("{:?}", c.comparison.status),
                )
            })
            .collect::<Vec<_>>();
        theme::status_list(
            frame,
            parts[0],
            theme::panel("Comparison rows — Tab evidence", !view.evidence_focus),
            rows.iter().map(|(t, w)| (t.clone(), w.as_str())).collect(),
            view.row.saturating_sub(row_start),
        );
        let heading_width = parts[1].width.saturating_sub(2) as usize;
        let compact = parts[1].height < 16;
        let heading = report
            .cells
            .get(view.row)
            .map(|c| {
                let path = m
                    .scopes
                    .iter()
                    .find(|s| s.id == c.scope_id)
                    .map_or("<unknown scope>", |s| s.path.as_str());
                if compact {
                    format!(
                        "V:{} P:{} C:{}\nT:{} S:{} {:?}",
                        super::render::projection(&c.variant_id, 8),
                        c.profile,
                        super::render::projection(&c.scenario.to_string(), 10),
                        super::render::projection(&c.stage, 10).replace(['\n', '\t'], " "),
                        super::render::projection(&c.scope_id, 8),
                        c.comparison.status
                    )
                } else if heading_width < 68 {
                    let identity = format!("Profile: {} | Scenario: {}", c.profile, c.scenario);
                    let identity = if identity.len() <= heading_width {
                        identity
                    } else {
                        format!("Profile: {}\nScenario: {}", c.profile, c.scenario)
                    };
                    format!(
                        "{}\n{}\n{}\n{}\n{}\n{:?}",
                        heading_field("Variant: ", &c.variant_id, heading_width, 32),
                        identity,
                        heading_field("Stage: ", &c.stage, heading_width, 32),
                        heading_field("Scope: ", &c.scope_id, heading_width, 24),
                        heading_field("Path: ", path, heading_width, 48),
                        c.comparison.status
                    )
                } else {
                    let project = super::render::projection;
                    format!(
                        "Variant: {} | {}/{} | Stage: {}\nScope {}: {} | {:?}",
                        project(&c.variant_id, 32),
                        c.profile,
                        c.scenario,
                        project(&c.stage, 32),
                        project(&c.scope_id, 24),
                        project(path, 48),
                        c.comparison.status
                    )
                }
            })
            .unwrap_or_default();
        let lines = Paragraph::new(heading.as_str())
            .wrap(Wrap { trim: false })
            .line_count(parts[1].width.saturating_sub(2));
        let heading_height = u16::try_from(lines.saturating_add(2)).unwrap_or(u16::MAX);
        let detail = Layout::vertical([Constraint::Length(heading_height), Constraint::Min(1)])
            .split(parts[1]);
        frame.render_widget(
            Paragraph::new(heading)
                .wrap(Wrap { trim: false })
                .block(panel(if compact {
                    "Selected cell — full in Cell detail"
                } else {
                    "Selected cell"
                })),
            detail[0],
        );
        super::render::record_panes(frame, app, detail[1], view.evidence_focus);
    } else if app.has_started() {
        frame.render_widget(
            Paragraph::new(app.pending_text())
                .block(panel("Live evidence"))
                .wrap(Wrap { trim: false })
                .scroll((app.scroll, 0)),
            layout[1],
        );
    } else {
        let join = |items: Vec<String>| items.join(", ");
        let mut context = format!(
            "Profiles: {} | Scenarios: {}\nStages: {}\n",
            join(m.profiles.iter().map(|p| p.to_string()).collect()),
            join(m.scenarios.iter().map(|p| p.to_string()).collect()),
            join(m.stages.iter().map(|s| bounded(s)).collect())
        );
        for scope in &m.scopes {
            context += &format!(
                "Scope {}: {} {}\n",
                scope.id,
                scope.profile,
                bounded(&scope.path)
            );
        }
        context += &format!(
            "pull: {}\n\nVariants:\n",
            view.options.as_ref().is_some_and(|o| o.pull)
        );
        for variant in &m.variants {
            context += &format!(
                "{}: {}\n",
                variant.id,
                bounded(&variant.recipe.display().to_string())
            );
        }
        if let Some(error) = &app.terminal_error {
            context = format!("{}\n\n{context}", bounded(error));
        }
        frame.render_widget(
            Paragraph::new(safe_text(&context))
                .block(panel(if super::preview_edit::dirty(app) {
                    "ACCEPTED suite preview (draft UNSAVED)"
                } else {
                    "Explicit suite preview — r to start"
                }))
                .wrap(Wrap { trim: false })
                .scroll((app.scroll, 0)),
            layout[1],
        );
    }
    frame.render_widget(footer, layout[2]);
}
/// Only keys that act in the current comparison mode and focus.
fn hints(app: &App, view: &ComparisonView) -> Vec<Hint> {
    if super::preview_edit::dirty(app) {
        return vec![
            hint("e", "edit", REQUIRED),
            hint("d", "discard", REQUIRED),
            hint("q", "discard/quit", REQUIRED),
            hint("r/p/s", "disabled until Save Copy or discard", 1),
        ];
    }
    if app.editing_filter {
        return vec![];
    }
    let mut hints = Vec::new();
    if view.report.is_some() {
        hints.push(hint("Tab", "cells/evidence", REQUIRED));
        hints.push(hint("↑↓", "select", 2));
        hints.push(hint("Home/End", "first/last", 5));
        if view.evidence_focus && app.viewport.borrow().has_chunks() {
            hints.push(hint("[ ]", "chunks", 2));
        }
        hints.push(hint("PgUp/Dn", "scroll", 4));
        hints.push(hint("/", "filter", 3));
    } else if !app.has_started() {
        if view.suite.is_some() && !app.work_consumed {
            hints.push(hint("r", "run", REQUIRED));
        }
        hints.push(hint("↑↓", "scroll", 3));
        if super::preview_edit::eligible(app) {
            hints.push(hint("e", "edit", 2));
        }
    }
    if app.active {
        hints.push(hint("c", "cancel", REQUIRED));
    }
    hints.push(hint("q", "quit", REQUIRED));
    hints
}
