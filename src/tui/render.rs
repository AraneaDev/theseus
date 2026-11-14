use super::{
    App, Focus, Tab, safe_text,
    theme::{self, Hint, REQUIRED, hint},
};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Tabs, Wrap},
};
pub fn render(frame: &mut Frame, app: &App) {
    if super::native::render(frame, app) {
        return;
    }
    if super::preview_edit::render(frame, app) {
        return;
    }
    if app.comparison.is_some() {
        super::comparison::render(frame, app);
        return;
    }
    let area = frame.area();
    let narrow = area.width < 85;
    let header = header(app, area.width);
    // Borders plus one or two wrapped lines; never a reserved empty line.
    let header_height = (header.line_count(area.width.saturating_sub(2)) as u16).clamp(3, 4);
    let footer = theme::footer(&footer_status(app), &hints(app), area.width);
    let footer_height = theme::footer_height(&footer, area.width);
    let main = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Min(1),
        Constraint::Length(footer_height),
    ])
    .split(area);
    frame.render_widget(header, main[0]);
    if app.report.is_none() {
        let title = if app.has_started() {
            "Live evidence"
        } else if super::preview_edit::dirty(app) {
            "ACCEPTED recipe preview (draft UNSAVED)"
        } else {
            "Recipe / command and environment preview"
        };
        let body = if app.has_started() {
            Paragraph::new(app.pending_text())
        } else {
            Paragraph::new(app.preview_lines())
        };
        frame.render_widget(
            body.block(theme::plain_panel(title, true))
                .wrap(Wrap { trim: false })
                .scroll((app.scroll, 0)),
            main[1],
        );
    } else {
        let parts = Layout::default()
            .direction(if narrow {
                Direction::Vertical
            } else {
                Direction::Horizontal
            })
            .constraints(if narrow {
                vec![Constraint::Length(4), Constraint::Min(1)]
            } else {
                vec![Constraint::Length(30), Constraint::Min(1)]
            })
            .split(main[1]);
        selectors(frame, app, parts[0], narrow);
        evidence(frame, app, parts[1]);
    }
    frame.render_widget(footer, main[2]);
}
/// Outcome once, coloured counts, and the run id dimmed at the right.
fn header(app: &App, width: u16) -> Paragraph<'static> {
    let mut block = Block::default()
        .title("Theseus")
        .borders(Borders::ALL)
        .border_style(theme::dim());
    let line = if let Some(r) = &app.report {
        let c = r.counts();
        let budget = (width as usize).saturating_sub(16) / 2;
        let id = projection(&r.run_id, 256).replace(['\n', '\t'], " ");
        let id = if id.chars().count() > budget {
            let tail = id
                .chars()
                .rev()
                .take(budget.saturating_sub(1))
                .collect::<Vec<_>>();
            format!("…{}", tail.into_iter().rev().collect::<String>())
        } else {
            id
        };
        if budget >= 6 {
            block = block.title(
                Line::from(Span::styled(
                    format!(" run {} ", safe_text(&id)),
                    theme::dim(),
                ))
                .right_aligned(),
            );
        }
        let mut spans = vec![
            theme::status_word(&app.outcome().to_string(), true),
            // The label only fits on wide terminals; narrow ones keep every count.
            Span::styled(
                if width >= 85 {
                    "  │ checks  "
                } else {
                    "  │ "
                },
                theme::dim(),
            ),
        ];
        for (i, (word, count)) in [
            ("passed", c.passed_checks),
            ("failed", c.failed_checks),
            ("review", c.review_findings),
            ("incomplete", c.incomplete_checks),
            ("skipped", c.skipped_checks),
        ]
        .into_iter()
        .enumerate()
        {
            if i > 0 {
                spans.push(Span::raw("  "));
            }
            // The outcome already carries the glyph; counts use colour only.
            let (_, color) = theme::status(word);
            let style = if count == 0 {
                theme::dim()
            } else {
                Style::default().fg(color)
            };
            spans.push(Span::styled(format!("{word} {count}"), style));
        }
        Line::from(spans)
    } else if app.has_started() {
        Line::raw("Theseus | Live evidence pending or unavailable")
    } else if super::preview_edit::dirty(app) {
        Line::styled(
            "Theseus | UNSAVED draft — accepted preview below; no commands executed",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Line::raw("Theseus | Preview — no commands executed")
    };
    Paragraph::new(line).block(block).wrap(Wrap { trim: false })
}
fn footer_status(app: &App) -> String {
    if super::preview_edit::dirty(app) {
        "UNSAVED draft".into()
    } else if app.editing_filter {
        format!(
            "Filter (Enter/Esc apply): {}_",
            projection(&app.filter_draft, 512)
        )
    } else {
        projection(&app.status, 512)
    }
}
/// Only keys that act in the current mode and focus.
fn hints(app: &App) -> Vec<Hint> {
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
    if app.report.is_some() {
        // Chunk and log hints describe the current selection, not the last frame.
        app.viewport.borrow_mut().update(app, None);
    }
    if app.report.is_none() {
        if !app.has_started() {
            if app.recipe.is_some() && !app.work_consumed {
                hints.push(hint("r", "run", REQUIRED));
                hints.push(hint("p", "profiles", REQUIRED));
                hints.push(hint("s", "scenarios", REQUIRED));
            }
            hints.push(hint("↑↓/PgUp/Dn", "scroll", 3));
            if super::preview_edit::eligible(app) {
                hints.push(hint("e", "edit", 2));
            }
        } else {
            hints.push(hint("PgUp/Dn", "scroll", 3));
        }
    } else {
        hints.push(hint("Tab/Enter", "focus", REQUIRED));
        if app.focus != Focus::Scenario {
            hints.push(hint("Esc", "back", 5));
        }
        if app.focus == Focus::Evidence {
            hints.push(hint("←→", "tabs", 1));
        }
        hints.push(hint("↑↓", "select", 2));
        hints.push(hint("/", "filter", 3));
        let view = app.viewport.borrow();
        if view.has_chunks() {
            hints.push(hint("[ ]", "chunks", 2));
        }
        hints.push(hint("PgUp/Dn", "scroll", 4));
        if app.tab == Tab::Logs && view.selected_log {
            hints.push(hint("m", "metadata", 3));
            hints.push(hint("^Home/End", "log", 6));
        }
    }
    if app.active {
        hints.push(hint("c", "cancel", REQUIRED));
    }
    hints.push(hint("q", "quit", REQUIRED));
    hints
}
fn selectors(frame: &mut Frame, app: &App, area: Rect, narrow: bool) {
    let parts = Layout::default()
        .direction(if narrow {
            Direction::Horizontal
        } else {
            Direction::Vertical
        })
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(area);
    let scenario_start = app.scenario_index.saturating_sub(32).min(
        app.report
            .as_ref()
            .map_or(0, |r| r.scenarios.len())
            .saturating_sub(64),
    );
    let stage_start = app.stage_index.saturating_sub(32).min(
        app.scenario()
            .map_or(0, |s| s.stages.len())
            .saturating_sub(64),
    );
    let scenarios = app
        .report
        .as_ref()
        .map(|r| {
            r.scenarios
                .iter()
                .skip(scenario_start)
                .take(64)
                .map(|s| {
                    (
                        format!("{}/{} {}", s.profile, s.scenario, s.outcome),
                        s.outcome.to_string(),
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    theme::status_list(
        frame,
        parts[0],
        theme::panel("Scenarios", app.focus == Focus::Scenario),
        scenarios
            .iter()
            .map(|(t, w)| (t.clone(), w.as_str()))
            .collect(),
        app.scenario_index.saturating_sub(scenario_start),
    );
    let stages = app
        .scenario()
        .map(|s| {
            s.stages
                .iter()
                .skip(stage_start)
                .take(64)
                .map(|s| {
                    (
                        format!("{} {}", projection(&s.name, 256), s.outcome),
                        s.outcome.to_string(),
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    theme::status_list(
        frame,
        parts[1],
        theme::panel("Stages", app.focus == Focus::Stage),
        stages
            .iter()
            .map(|(t, w)| (t.clone(), w.as_str()))
            .collect(),
        app.stage_index.saturating_sub(stage_start),
    );
}
fn evidence(frame: &mut Frame, app: &App, area: Rect) {
    let parts = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(area);
    let counts = app.viewport.borrow_mut().tab_counts(app);
    let filtered = !app.filter.is_empty();
    let active = app.evidence_len();
    let titles = Tab::ALL.iter().zip(counts).map(|(tab, count)| {
        let name = match tab {
            Tab::Stages => "Stages",
            Tab::Findings => "Findings",
            Tab::Files => "Files",
            Tab::Commands => "Commands",
            Tab::Logs => "Logs",
        };
        // Under a filter only the active tab's matching count is known.
        let count = if *tab == app.tab {
            Some(active)
        } else if filtered {
            None
        } else {
            Some(count)
        };
        let mut spans = vec![Span::raw(name)];
        if let Some(count) = count {
            spans.push(Span::styled(format!(" {count}"), theme::dim()));
        }
        Line::from(spans)
    });
    frame.render_widget(
        Tabs::new(titles).select(app.tab.index()).highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        ),
        parts[0],
    );
    record_panes(frame, app, parts[1], app.focus == Focus::Evidence);
}
/// All strings in outer chrome are bounded before interpolation/sanitization.
pub(super) fn projection(text: &str, bytes: usize) -> String {
    super::evidence::Record::Text(text).summary(bytes)
}
pub(super) fn record_panes(frame: &mut Frame, app: &App, area: Rect, focused: bool) {
    let mut view = app.viewport.borrow_mut();
    view.update(app, Some(area.width));
    let count = view.len(app);
    let selected = app.evidence_index.min(count.saturating_sub(1));
    // The list takes what its loaded rows need (at least 3, at most about half),
    // and the selected detail takes the rest.
    let rows_needed = count.clamp(1, view.rows.len().max(1)) as u16 + 2;
    let list_height = rows_needed.clamp(3, (area.height.saturating_sub(2) / 2).max(3));
    let regions = Layout::vertical([
        Constraint::Length(list_height),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .split(area);
    // Flattening is restricted to <=64 already bounded projections. The selected
    // detail retains every newline and is separately chunked without elision.
    let rows = view
        .rows
        .iter()
        .map(|row| ListItem::new(row.text.replace('\n', " | ")))
        .collect::<Vec<_>>();
    let mut state = ListState::default().with_selected(Some(selected.saturating_sub(view.start)));
    frame.render_stateful_widget(
        List::new(rows)
            .block(theme::panel("Records — Home/End select", focused))
            .highlight_symbol("> ")
            .highlight_style(Style::default().add_modifier(Modifier::BOLD)),
        regions[0],
        &mut state,
    );
    let title = format!(
        "Detail — record {} / {}",
        if count == 0 { 0 } else { selected + 1 },
        count
    );
    let detail = Paragraph::new(view.body.as_str())
        .block(theme::plain_panel(&title, focused))
        .wrap(Wrap { trim: false });
    let inner = theme::plain_panel(&title, focused).inner(regions[1]);
    let maximum = detail
        .line_count(inner.width)
        .saturating_sub(inner.height as usize)
        .min(u16::MAX as usize) as u16;
    frame.render_widget(detail.scroll((view.scroll.min(maximum), 0)), regions[1]);
    frame.render_widget(
        Paragraph::new(Span::styled(view.label.clone(), theme::dim())).wrap(Wrap { trim: false }),
        regions[2],
    );
}
