use super::{App, Focus, safe_text};
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
                projection(&r.run_id, 256),
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
                vec![Constraint::Length(4), Constraint::Min(1)]
            } else {
                vec![Constraint::Length(30), Constraint::Min(1)]
            })
            .split(main[1]);
        selectors(frame, app, parts[0], narrow);
        evidence(frame, app, parts[1]);
    }
    let hint = if app.editing_filter {
        format!(
            "Filter (Enter/Esc apply): {}_",
            projection(&app.filter_draft, 512)
        )
    } else {
        format!(
            "{} | Tab/Enter focus Esc back ↑↓/j/k select ←→ tabs / filter Home/End select [ ] chunks PgUp/Dn scroll CtrlHome/End log m metadata | r run c cancel q quit",
            projection(&app.status, 512)
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
                    ListItem::new(safe_text(&format!(
                        "{}/{} {}",
                        s.profile, s.scenario, s.outcome
                    )))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut state =
        ListState::default().with_selected(Some(app.scenario_index.saturating_sub(scenario_start)));
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
                .skip(stage_start)
                .take(64)
                .map(|s| {
                    ListItem::new(safe_text(&format!(
                        "{} {}",
                        projection(&s.name, 256),
                        s.outcome
                    )))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut state =
        ListState::default().with_selected(Some(app.stage_index.saturating_sub(stage_start)));
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
    let parts = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(area);
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
    let regions = Layout::vertical([
        Constraint::Length((area.height / 4).clamp(3, 10)),
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
            .block(block("Records — Home/End select", focused))
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
        .block(block(&title, focused))
        .wrap(Wrap { trim: false });
    let inner = block(&title, focused).inner(regions[1]);
    let maximum = detail
        .line_count(inner.width)
        .saturating_sub(inner.height as usize)
        .min(u16::MAX as usize) as u16;
    frame.render_widget(detail.scroll((view.scroll.min(maximum), 0)), regions[1]);
    frame.render_widget(
        Paragraph::new(view.label.as_str()).wrap(Wrap { trim: false }),
        regions[2],
    );
}
