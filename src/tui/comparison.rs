//! Suite state and workbench rendering; lifecycle execution stays in the shared worker.
use super::{App, safe_text};
use crate::comparison::{
    ComparisonReport, LoadedComparison, SuiteManifest, SuiteRunOptions, ValidatedSuite,
};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use std::path::PathBuf;

pub struct ComparisonView {
    pub manifest: SuiteManifest,
    pub suite: Option<ValidatedSuite>,
    pub options: Option<SuiteRunOptions>,
    pub report: Option<ComparisonReport>,
    pub row: usize,
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
        });
        app.set_comparison_report(document.report, root);
        app.status = "Offline comparison".into();
        app
    }
    pub fn set_comparison_report(&mut self, report: ComparisonReport, root: PathBuf) {
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
    let view = app.comparison.as_mut().unwrap();
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
                && !app.quit_requested
                && view.suite.is_some()
                && view.report.is_none() =>
        {
            app.start_requested = true
        }
        KeyCode::PageDown => app.scroll = app.scroll.saturating_add(10),
        KeyCode::PageUp => app.scroll = app.scroll.saturating_sub(10),
        KeyCode::Down | KeyCode::Char('j') | KeyCode::Up | KeyCode::Char('k') => {
            let down = matches!(key.code, KeyCode::Down | KeyCode::Char('j'));
            if let Some(report) = &view.report {
                view.row = if down {
                    view.row
                        .saturating_add(1)
                        .min(report.cells.len().saturating_sub(1))
                } else {
                    view.row.saturating_sub(1)
                };
                app.scroll = 0;
            } else {
                app.scroll = if down {
                    app.scroll.saturating_add(1)
                } else {
                    app.scroll.saturating_sub(1)
                };
            }
        }
        _ => {}
    }
}
// UI projections only. Full strings and every difference remain in offline JSON.
const DISPLAY_DIFFERENCES: usize = 100;
fn bounded(text: &str) -> String {
    let flat = safe_text(text).replace('\n', "\\n").replace('\t', "\\t");
    let mut chars = flat.chars();
    let prefix = chars.by_ref().take(512).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}… [elided; full text in JSON]")
    } else {
        prefix
    }
}
fn metadata(entry: &Option<crate::snapshot::FileEntry>) -> String {
    entry.as_ref().map_or_else(|| "None".into(), |e| format!(
        "kind: {:?}, mode: {:o}, uid: {}, gid: {}, size: {}, sha256: {:?}, link_target: {:?}",
        e.kind, e.mode, e.uid, e.gid, e.size, e.sha256.as_deref().map(bounded), e.link_target.as_deref().map(bounded)
    ))
}
fn panel(title: &str) -> Block<'_> {
    Block::default().title(title).borders(Borders::ALL)
}
pub(super) fn render(frame: &mut Frame, app: &App) {
    let view = app.comparison.as_ref().unwrap();
    let m = &view.manifest;
    let layout = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(3),
    ])
    .split(frame.area());
    let title = format!(
        "{} | Baseline: {} | {}{}",
        bounded(&m.name),
        m.baseline,
        app.outcome(),
        if view.report.is_none() {
            " | Preview — no commands executed"
        } else {
            ""
        }
    );
    frame.render_widget(
        Paragraph::new(safe_text(&title))
            .block(panel("Theseus comparison"))
            .wrap(Wrap { trim: false }),
        layout[0],
    );
    let mut context = format!(
        "Profiles: {:?} | Scenarios: {:?}\nStages: {:?}\n",
        m.profiles, m.scenarios, m.stages
    );
    for scope in &m.scopes {
        context += &format!(
            "Scope {}: {} {}\n",
            scope.id,
            scope.profile,
            bounded(&scope.path)
        );
    }
    if let Some(report) = &view.report {
        let narrow = frame.area().width < 100;
        let parts = if narrow {
            Layout::vertical([Constraint::Length(6), Constraint::Min(1)]).split(layout[1])
        } else {
            Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                .split(layout[1])
        };
        let rows = report
            .cells
            .iter()
            .map(|c| {
                ListItem::new(safe_text(&format!(
                    "{} {}/{}/{} {} {:?}",
                    c.variant_id,
                    c.profile,
                    c.scenario,
                    bounded(&c.stage),
                    c.scope_id,
                    c.comparison.status
                )))
            })
            .collect::<Vec<_>>();
        let mut state = ListState::default().with_selected(Some(view.row));
        frame.render_stateful_widget(
            List::new(rows)
                .block(panel("Comparison rows — ↑↓/j/k"))
                .highlight_symbol("> "),
            parts[0],
            &mut state,
        );
        let mut text = String::new();
        if let Some(error) = &app.terminal_error {
            text += &format!("{}\n\n", bounded(error));
        }
        if let Some(cell) = report.cells.get(view.row) {
            let path = m
                .scopes
                .iter()
                .find(|s| s.id == cell.scope_id)
                .map_or("<unknown scope>", |s| s.path.as_str());
            text += &format!(
                "{} vs {} | {} | {} | {}\nScope {}: {} | {:?}\n",
                cell.variant_id,
                m.baseline,
                cell.profile,
                cell.scenario,
                bounded(&cell.stage),
                cell.scope_id,
                bounded(path),
                cell.comparison.status
            );
            for reason in &cell.comparison.diagnostics {
                text += &format!("Unknown / diagnostic: {}\n", bounded(reason));
            }
        }
        text += &format!("\n{context}\nSources (exact recorded identities):\n");
        for source in &report.sources {
            text += &format!(
                "{}: {} | run {}\nImage: {}\nImage ID: {}\nRecipe digest: {}\n",
                source.variant_id,
                source.outcome,
                bounded(source.run_id.as_deref().unwrap_or("<pending>")),
                bounded(source.image.as_deref().unwrap_or("<pending>")),
                bounded(source.image_id.as_deref().unwrap_or("<pending>")),
                bounded(source.recipe_digest.as_deref().unwrap_or("<pending>"))
            );
            for artifact in &source.artifacts {
                text += &format!(
                    "Artifact {}: {} ({} files; file digests in JSON)\n",
                    bounded(&artifact.name),
                    bounded(&artifact.sha256),
                    artifact.files.len()
                );
            }
            if let Some(path) = &source.report_path {
                text += &format!(
                    "Source logs/commands: theseus report {}\n",
                    app.root.join(path).display()
                );
            }
            if let Some(error) = &source.error {
                text += &format!("Source error: {}\n", bounded(error));
            }
        }
        for diagnostic in &report.diagnostics {
            text += &format!("{}\n", bounded(diagnostic));
        }
        text += &format!("Recovery: theseus cleanup {}\n", app.root.display());
        if let Some(cell) = report.cells.get(view.row) {
            let total = cell.comparison.differences.len();
            text += &format!(
                "\nDifferences: showing {} of {}{}\nComplete evidence: theseus comparison {} --json\n",
                total.min(DISPLAY_DIFFERENCES),
                total,
                if total > DISPLAY_DIFFERENCES {
                    " (UI prefix limit)"
                } else {
                    ""
                },
                app.root.display()
            );
            for change in cell.comparison.differences.iter().take(DISPLAY_DIFFERENCES) {
                text += &format!(
                    "{} {:?}\nBefore: {}\nAfter: {}\n",
                    bounded(&change.path),
                    change.kind,
                    metadata(&change.before),
                    metadata(&change.after)
                );
            }
        }
        frame.render_widget(
            Paragraph::new(safe_text(&text))
                .block(panel("Evidence / source reports — PgUp/Dn scroll"))
                .wrap(Wrap { trim: false })
                .scroll((app.scroll, 0)),
            parts[1],
        );
    } else {
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
                .block(panel("Explicit suite preview — r to start"))
                .wrap(Wrap { trim: false })
                .scroll((app.scroll, 0)),
            layout[1],
        );
    }
    frame.render_widget(
        Paragraph::new(safe_text(&format!(
            "{} | ↑↓/j/k rows | PgUp/Dn evidence scroll | r run c cancel q quit",
            bounded(&app.status)
        )))
        .wrap(Wrap { trim: false }),
        layout[2],
    );
}
