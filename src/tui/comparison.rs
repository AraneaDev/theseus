//! Suite state and workbench rendering; lifecycle execution stays in the shared worker.
use super::{
    App, safe_text,
    theme::{self, HELP_ONLY, Hint, REQUIRED, hint},
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
        spans.push(theme::status_word(&super::live_outcome_word(app), true));
    }
    spans.push(Span::raw(suffix));
    if let Some(elapsed) = app.run_clock() {
        let clock = super::render::clock(elapsed);
        spans.push(if app.active {
            Span::styled(
                format!(" | {} running {clock}", super::render::spinner(elapsed)),
                theme::tone(ratatui::style::Color::Cyan),
            )
        } else {
            Span::styled(format!(" | ran {clock}"), theme::dim())
        });
    }
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
        let doc = suite_preview(app, view);
        frame.render_widget(
            Paragraph::new(doc.lines())
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
fn plural(count: usize, word: &str) -> String {
    format!("{count} {word}{}", if count == 1 { "" } else { "s" })
}
/// Comparison cells the declared suite derives: every non-baseline variant ×
/// profile × scenario × stage × scope declared for that profile. Arithmetic on
/// the manifest only; nothing is loaded or run.
pub(super) fn planned_cells(m: &SuiteManifest) -> usize {
    let candidates = m.variants.iter().filter(|v| v.id != m.baseline).count();
    let scoped_profiles = m
        .profiles
        .iter()
        .map(|p| m.scopes.iter().filter(|s| s.profile == *p).count())
        .sum::<usize>();
    candidates * scoped_profiles * m.scenarios.len() * m.stages.len()
}
/// Labelled suite sections in the recipe preview's style; never executes.
fn suite_preview(app: &App, view: &ComparisonView) -> super::preview_doc::Doc {
    use super::preview_doc::{Doc, Tone};
    let m = &view.manifest;
    let mut doc = Doc::new();
    if let Some(error) = &app.terminal_error {
        doc.plain(&bounded(error)).plain("\n\n");
    }
    let join = |items: Vec<String>| {
        if items.is_empty() {
            "none".to_string()
        } else {
            items.join(", ")
        }
    };
    let runs = m.variants.len() * m.profiles.len() * m.scenarios.len();
    doc.push(Tone::Label, "Will run:  ")
        .push(
            Tone::Name,
            &format!(
                "{} × {} × {} = {}",
                plural(m.variants.len(), "variant"),
                plural(m.profiles.len(), "profile"),
                plural(m.scenarios.len(), "scenario"),
                plural(runs, "lifecycle run"),
            ),
        )
        .dim(&format!(
            " · {} compared against {}\n",
            plural(planned_cells(m), "cell"),
            bounded(&m.baseline)
        ))
        .plain("\n")
        .heading("Selection")
        .label(
            "  Profiles:  ",
            &join(m.profiles.iter().map(|p| p.to_string()).collect()),
        )
        .plain("\n")
        .label(
            "  Scenarios: ",
            &join(m.scenarios.iter().map(|p| p.to_string()).collect()),
        )
        .plain("\n")
        .label(
            "  Pull:      ",
            &view.options.as_ref().is_some_and(|o| o.pull).to_string(),
        )
        .plain("\n\n")
        .heading("Stages");
    for stage in &m.stages {
        doc.plain(&format!(
            "  {}\n",
            bounded(stage).replace(['\n', '\t'], " ")
        ));
    }
    // Pad names to the longest (bounded) id so paths line up.
    let pad = |ids: &mut dyn Iterator<Item = &String>| {
        ids.map(|id| bounded(id).chars().count())
            .max()
            .unwrap_or(0)
            .min(32)
    };
    let scope_pad = pad(&mut m.scopes.iter().map(|s| &s.id));
    doc.plain("\n").heading("Scopes");
    for scope in &m.scopes {
        doc.push(
            Tone::Name,
            &format!(
                "  {:<scope_pad$}",
                bounded(&scope.id).replace(['\n', '\t'], " ")
            ),
        )
        .dim(&format!("  {:<6} ", scope.profile.to_string()))
        .plain(&bounded(&scope.path))
        .plain("\n");
    }
    let variant_pad = pad(&mut m.variants.iter().map(|v| &v.id));
    doc.plain("\n").heading("Variants");
    for variant in &m.variants {
        doc.push(
            Tone::Name,
            &format!(
                "  {:<variant_pad$}",
                bounded(&variant.id).replace(['\n', '\t'], " ")
            ),
        )
        .dim("  ")
        .plain(&bounded(&variant.recipe.display().to_string()));
        if variant.id == m.baseline {
            doc.plain("  ").push(Tone::Selected, " baseline ");
        }
        doc.plain("\n");
    }
    doc
}
/// Only keys that act in the current comparison mode and focus.
pub(super) fn hints(app: &App, view: &ComparisonView) -> Vec<Hint> {
    use theme::Group::*;
    if super::preview_edit::dirty(app) {
        return super::render::draft_hints();
    }
    if app.editing_filter {
        return vec![];
    }
    let mut hints = Vec::new();
    if view.report.is_some() {
        hints.push(hint(Navigate, "Tab", "cells/evidence", REQUIRED));
        hints.push(hint(Navigate, "↑↓", "select", 2));
        hints.push(hint(Navigate, "j/k", "select", HELP_ONLY));
        hints.push(hint(Navigate, "Home/End", "first/last", 5));
        let chunks = view.evidence_focus && app.viewport.borrow().has_chunks();
        hints.push(hint(
            Evidence,
            "[ ]",
            if chunks {
                "chunks"
            } else {
                "previous/next chunk"
            },
            if chunks { 2 } else { HELP_ONLY },
        ));
        hints.push(hint(Evidence, "PgUp/Dn", "scroll", 4));
        hints.push(hint(Evidence, "/", "filter", 3));
    } else if !app.has_started() {
        if view.suite.is_some() && !app.work_consumed {
            hints.push(hint(Run, "r", "run", REQUIRED));
        }
        hints.push(hint(Navigate, "↑↓", "scroll", 3));
        hints.push(hint(Navigate, "j/k", "scroll a line", HELP_ONLY));
        hints.push(hint(Navigate, "PgUp/Dn", "scroll a page", HELP_ONLY));
        if super::preview_edit::eligible(app) {
            super::render::editor_hints(&mut hints);
        }
    }
    super::render::common_hints(&mut hints, app);
    hints
}
