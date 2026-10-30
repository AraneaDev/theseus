use super::{App, Focus, Tab, safe_text};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Tabs, Wrap},
};
fn block(title: &str, focused: bool) -> Block<'_> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused {
            Color::Cyan
        } else {
            Color::DarkGray
        }))
}
pub fn render(frame: &mut Frame, app: &App) {
    if app.comparison.is_some() {
        super::comparison::render(frame, app);
        return;
    }
    let area = frame.area();
    let narrow = area.width < 85;
    let main = Layout::vertical([
        Constraint::Length(if narrow { 4 } else { 3 }),
        Constraint::Min(1),
        Constraint::Length(3),
    ])
    .split(area);
    let summary = app
        .report
        .as_ref()
        .map(|r| {
            let c = r.counts();
            if narrow {
                return format!(
                    "{} | passed {} failed {} review {}\nincomplete {} skipped {}",
                    app.outcome(),
                    c.passed_checks,
                    c.failed_checks,
                    c.review_findings,
                    c.incomplete_checks,
                    c.skipped_checks
                );
            }
            format!(
                "{} | {} | passed {} failed {} review {} incomplete {} skipped {}",
                r.run_id,
                app.outcome(),
                c.passed_checks,
                c.failed_checks,
                c.review_findings,
                c.incomplete_checks,
                c.skipped_checks
            )
        })
        .unwrap_or_else(|| "Theseus | Preview — no commands executed".into());
    frame.render_widget(
        Paragraph::new(safe_text(&summary))
            .block(block("Theseus", false))
            .wrap(Wrap { trim: false }),
        main[0],
    );
    if app.report.is_none() {
        frame.render_widget(
            Paragraph::new(app.preview_text())
                .block(block("Recipe / command and environment preview", true))
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
                vec![Constraint::Length(7), Constraint::Min(1)]
            } else {
                vec![Constraint::Length(30), Constraint::Min(1)]
            })
            .split(main[1]);
        selectors(frame, app, parts[0], narrow);
        evidence(frame, app, parts[1]);
    }
    let hint = if app.editing_filter {
        format!("Filter: {}_ | Enter apply / Esc finish", app.filter)
    } else {
        format!(
            "{} | Tab/Enter focus Esc back ↑↓/j/k select ←→ tabs / filter PgUp/Dn scroll | r run c cancel q quit",
            app.status
        )
    };
    frame.render_widget(
        Paragraph::new(safe_text(&hint)).wrap(Wrap { trim: false }),
        main[2],
    );
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
    let scenarios = app
        .report
        .as_ref()
        .map(|r| {
            r.scenarios
                .iter()
                .map(|s| {
                    ListItem::new(safe_text(&format!(
                        "{}/{} {}",
                        s.profile, s.scenario, s.outcome
                    )))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut state = ListState::default().with_selected(Some(app.scenario_index));
    frame.render_stateful_widget(
        List::new(scenarios)
            .block(block("Scenarios", app.focus == Focus::Scenario))
            .highlight_symbol("> ")
            .highlight_style(Style::default().add_modifier(Modifier::BOLD)),
        parts[0],
        &mut state,
    );
    let stages = app
        .scenario()
        .map(|s| {
            s.stages
                .iter()
                .map(|s| ListItem::new(safe_text(&format!("{} {}", s.name, s.outcome))))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut state = ListState::default().with_selected(Some(app.stage_index));
    frame.render_stateful_widget(
        List::new(stages)
            .block(block("Stages", app.focus == Focus::Stage))
            .highlight_symbol("> ")
            .highlight_style(Style::default().add_modifier(Modifier::BOLD)),
        parts[1],
        &mut state,
    );
}
fn evidence(frame: &mut Frame, app: &App, area: Rect) {
    let parts = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).split(area);
    frame.render_widget(
        Tabs::new(["Stages", "Findings", "Files", "Commands", "Logs"])
            .select(app.tab.index())
            .highlight_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        parts[0],
    );
    let items = app.items();
    let mut text = String::new();
    let mut selected_start = 0;
    for (i, item) in items.iter().enumerate() {
        if i == app.evidence_index {
            selected_start = text.len();
        }
        text += &format!(
            "{} {}\n",
            if i == app.evidence_index { ">" } else { " " },
            item
        );
    }
    if text.is_empty() {
        text = "No evidence for this stage/filter.\n".into();
    }
    if app.tab == Tab::Logs {
        text = format!(
            "{}\n{}",
            items
                .get(app.evidence_index)
                .map_or("No stored log", String::as_str),
            app.log_text
        );
    }
    let evidence_block = block("Evidence", app.focus == Focus::Evidence);
    // Measure the exact displayed prefix with Ratatui's own word/grapheme wrapping
    // and the same inner width, prefix markers and Wrap used for rendering.
    let before = if app.focus == Focus::Evidence && app.tab != Tab::Logs && selected_start > 0 {
        Paragraph::new(&text[..selected_start])
            .wrap(Wrap { trim: false })
            .line_count(evidence_block.inner(parts[1]).width)
    } else {
        0
    };
    // Paragraph scroll is u16; for longer prefixes start directly at the selected
    // record instead of silently clamping its position outside the viewport.
    let (text, offset) = if before > u16::MAX as usize {
        (&text[selected_start..], 0)
    } else {
        (text.as_str(), before as u16)
    };
    frame.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .scroll((offset.saturating_add(app.scroll), 0))
            .block(evidence_block),
        parts[1],
    );
}
