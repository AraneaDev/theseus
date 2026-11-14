use super::{
    App, Focus, Tab, safe_text,
    theme::{self, Group, HELP, HELP_ONLY, Hint, REQUIRED, hint},
};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Tabs, Wrap},
};
pub fn render(frame: &mut Frame, app: &App) {
    mode(frame, app);
    if app.help {
        theme::help(frame, &help_hints(app));
    }
}
/// Every binding of the current mode: the footer's hints plus help-only ones.
fn help_hints(app: &App) -> Vec<Hint> {
    if let Some(view) = &app.native {
        super::native::hints(app, view)
    } else if let Some(view) = &app.comparison {
        super::comparison::hints(app, view)
    } else {
        hints(app)
    }
}
/// Bindings shared by every mode that can open help.
pub(super) fn common_hints(hints: &mut Vec<Hint>, app: &App) {
    if app.active {
        hints.push(hint(Group::Run, "c", "cancel", REQUIRED));
    }
    hints.push(HELP);
    hints.push(hint(Group::Run, "q", "quit", REQUIRED));
    hints.push(hint(Group::Run, "Ctrl-C", "exit immediately", HELP_ONLY));
}
/// Dirty-draft preview keys, shared by recipe and suite previews.
pub(super) fn draft_hints() -> Vec<Hint> {
    vec![
        hint(Group::Editing, "e", "edit", REQUIRED),
        hint(Group::Editing, "d", "discard", REQUIRED),
        hint(Group::Run, "q", "discard/quit", REQUIRED),
        hint(
            Group::Run,
            "r/p/s",
            "disabled until Save Copy or discard",
            1,
        ),
        HELP,
    ]
}
/// Editor keys, listed in help only: `?` is literal text inside the editor.
pub(super) fn editor_hints(hints: &mut Vec<Hint>) {
    hints.push(hint(Group::Editing, "e", "edit raw TOML", 2));
    for (key, label) in [
        ("Ctrl-T", "validate draft (in editor)"),
        ("Ctrl-S", "Save Copy (in editor)"),
        ("Esc", "close editor, keep draft"),
    ] {
        hints.push(hint(Group::Editing, key, label, HELP_ONLY));
    }
}
fn mode(frame: &mut Frame, app: &App) {
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
        let rows = selector_rows(app);
        let parts = Layout::default()
            .direction(if narrow {
                Direction::Vertical
            } else {
                Direction::Horizontal
            })
            .constraints(if narrow {
                vec![Constraint::Length(4), Constraint::Min(1)]
            } else {
                vec![
                    Constraint::Length(selector_width(&rows, area.width)),
                    Constraint::Min(1),
                ]
            })
            .split(main[1]);
        selectors(frame, app, parts[0], narrow, rows);
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
            // While a run is active the report is partial; say so instead of
            // showing its provisional "incomplete" outcome.
            theme::status_word(&super::live_outcome_word(app), true),
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
        // Activity stays on the counts line when it fits; otherwise it gets its
        // own line so the spinner, stage and clock are never split apart.
        let mut activity = run_activity(app);
        let fits = Line::from(spans.clone()).width() + Line::from(activity.clone()).width()
            <= width.saturating_sub(2) as usize;
        if fits || activity.is_empty() {
            spans.extend(activity);
            vec![Line::from(spans)]
        } else {
            activity.remove(0);
            vec![Line::from(spans), Line::from(activity)]
        }
    } else if app.has_started() {
        vec![Line::raw("Theseus | Live evidence pending or unavailable")]
    } else if super::preview_edit::dirty(app) {
        vec![Line::styled(
            "Theseus | UNSAVED draft — accepted preview below; no commands executed",
            theme::tone(Color::Yellow).add_modifier(Modifier::BOLD),
        )]
    } else {
        vec![Line::raw("Theseus | Preview — no commands executed")]
    };
    Paragraph::new(line).block(block).wrap(Wrap { trim: false })
}
pub(super) fn clock(elapsed: std::time::Duration) -> String {
    let secs = elapsed.as_secs();
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}
/// A spinner frame that advances with the run clock (the loop redraws ~20/s).
pub(super) fn spinner(elapsed: std::time::Duration) -> &'static str {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    FRAMES[(elapsed.as_millis() / 100 % FRAMES.len() as u128) as usize]
}
/// The stage the persisted report shows as running, wherever the selection is;
/// otherwise what the latest engine event named (the report can lag behind).
fn running_stage(app: &App) -> Option<(String, Option<String>)> {
    let one_line = |text: &str| projection(text, 64).replace(['\n', '\t'], " ");
    app.report
        .as_ref()
        .and_then(|report| {
            report.scenarios.iter().find_map(|s| {
                s.stages
                    .iter()
                    .find(|stage| stage.outcome == crate::model::StageOutcome::Running)
                    .map(|stage| {
                        (
                            format!("{}/{}", s.profile, s.scenario),
                            Some(one_line(&stage.name)),
                        )
                    })
            })
        })
        .or_else(|| {
            app.live_stage.as_ref().map(|(id, stage)| {
                // Name a known scenario the same way the selector does.
                let scenario = app
                    .report
                    .as_ref()
                    .and_then(|r| r.scenarios.iter().find(|s| s.id == *id))
                    .map_or_else(|| one_line(id), |s| format!("{}/{}", s.profile, s.scenario));
                (scenario, stage.as_deref().map(one_line))
            })
        })
}
/// Header suffix: what is running now and for how long, or the final duration.
fn run_activity(app: &App) -> Vec<Span<'static>> {
    let Some(elapsed) = app.run_clock() else {
        return vec![];
    };
    let mut spans = vec![Span::styled("  │ ", theme::dim())];
    if app.active {
        let accent = theme::tone(Color::Cyan);
        spans.push(Span::styled(format!("{} ", spinner(elapsed)), accent));
        match running_stage(app) {
            Some((scenario, Some(stage))) => {
                spans.push(Span::styled(format!("{} · ", safe_text(&scenario)), accent));
                spans.push(Span::styled(
                    safe_text(&stage),
                    accent.add_modifier(Modifier::BOLD),
                ));
            }
            Some((scenario, None)) => {
                spans.push(Span::styled(format!("{} · ", safe_text(&scenario)), accent));
                spans.push(Span::styled("between stages", accent));
            }
            None => spans.push(Span::styled("running", accent)),
        }
        spans.push(Span::styled(format!("  {}", clock(elapsed)), theme::dim()));
    } else {
        spans.push(Span::styled(
            format!("ran {}", clock(elapsed)),
            theme::dim(),
        ));
    }
    spans
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
/// Help-only entries (`HELP_ONLY`) complete the `?` overlay from the same list.
pub(super) fn hints(app: &App) -> Vec<Hint> {
    use Group::*;
    if super::preview_edit::dirty(app) {
        return draft_hints();
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
                hints.push(hint(Run, "r", "run", REQUIRED));
                hints.push(hint(Run, "p", "profiles", REQUIRED));
                hints.push(hint(Run, "s", "scenarios", REQUIRED));
            }
            hints.push(hint(Navigate, "↑↓/PgUp/Dn", "scroll", 3));
            hints.push(hint(Navigate, "j/k", "scroll a line", HELP_ONLY));
            if super::preview_edit::eligible(app) {
                editor_hints(&mut hints);
            }
        } else {
            hints.push(hint(Navigate, "PgUp/Dn", "scroll", 3));
        }
    } else {
        hints.push(hint(Navigate, "Tab/Enter", "focus", REQUIRED));
        if app.focus != Focus::Scenario {
            hints.push(hint(Navigate, "Esc", "back", 5));
        }
        if app.focus == Focus::Evidence {
            hints.push(hint(Evidence, "←→", "tabs", 1));
        } else {
            hints.push(hint(Evidence, "←→", "tabs (evidence focus)", HELP_ONLY));
        }
        hints.push(hint(Navigate, "↑↓", "select", 2));
        hints.push(hint(Navigate, "j/k", "select", HELP_ONLY));
        hints.push(hint(Navigate, "Home/End", "first/last row", HELP_ONLY));
        hints.push(hint(Evidence, "/", "filter", 3));
        let view = app.viewport.borrow();
        let chunks = view.has_chunks();
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
        let log = app.tab == Tab::Logs && view.selected_log;
        hints.push(hint(
            Logs,
            "m",
            if log {
                "metadata"
            } else {
                "raw/metadata (selected log)"
            },
            if log { 3 } else { HELP_ONLY },
        ));
        hints.push(hint(
            Logs,
            "^Home/End",
            if log { "log" } else { "first/last log chunk" },
            if log { 6 } else { HELP_ONLY },
        ));
    }
    common_hints(&mut hints, app);
    hints
}
/// Bounded (<= 64 each) scenario and stage rows with their status words, plus
/// the window starts. Row text keeps the trailing status word that terminal
/// harnesses match (`> rolled_back passed`).
pub(super) struct SelectorRows {
    scenario_start: usize,
    stage_start: usize,
    scenarios: Vec<(String, String)>,
    stages: Vec<(String, String)>,
}
/// Left column wide enough for the longest visible row (border, glyph and
/// selection marker included), kept between 22% and 45% of the width.
fn selector_width(rows: &SelectorRows, width: u16) -> u16 {
    let longest = rows
        .scenarios
        .iter()
        .chain(&rows.stages)
        .map(|(text, _)| Span::raw(safe_text(text)).width())
        .max()
        .unwrap_or(0);
    let needed = u16::try_from(longest + 6).unwrap_or(u16::MAX);
    // Widen past the former fixed 30 columns only while the evidence side keeps
    // at least 70, so one-line evidence records never wrap sooner than before.
    let high = (width * 45 / 100).min(width.saturating_sub(70)).max(30);
    let low = (width * 22 / 100).min(high);
    needed.clamp(low, high)
}
fn selector_rows(app: &App) -> SelectorRows {
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
                    // While live, the scenario holding the running stage shows
                    // the running glyph; its text keeps the recorded outcome.
                    let running = app.active
                        && s.stages
                            .iter()
                            .any(|st| st.outcome == crate::model::StageOutcome::Running);
                    (
                        format!("{}/{} {}", s.profile, s.scenario, s.outcome),
                        if running {
                            "running".to_string()
                        } else {
                            s.outcome.to_string()
                        },
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
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
    SelectorRows {
        scenario_start,
        stage_start,
        scenarios,
        stages,
    }
}
fn selectors(frame: &mut Frame, app: &App, area: Rect, narrow: bool, rows: SelectorRows) {
    let parts = Layout::default()
        .direction(if narrow {
            Direction::Horizontal
        } else {
            Direction::Vertical
        })
        .constraints(if narrow {
            // Side by side in the stacked strip: scenarios get what their
            // longest row needs, within 25%..35%, and stages (never less than
            // the former 65%) the rest.
            let longest = rows
                .scenarios
                .iter()
                .map(|(text, _)| Span::raw(safe_text(text)).width())
                .max()
                .unwrap_or(0);
            let needed = u16::try_from(longest + 6).unwrap_or(u16::MAX);
            let (low, high) = (area.width / 4, area.width * 35 / 100);
            [
                Constraint::Length(needed.clamp(low, high.max(low))),
                Constraint::Min(1),
            ]
        } else {
            [Constraint::Percentage(35), Constraint::Percentage(65)]
        })
        .split(area);
    fn list(rows: &[(String, String)]) -> Vec<(String, &str)> {
        rows.iter().map(|(t, w)| (t.clone(), w.as_str())).collect()
    }
    theme::status_list(
        frame,
        parts[0],
        theme::panel("Scenarios", app.focus == Focus::Scenario),
        list(&rows.scenarios),
        app.scenario_index.saturating_sub(rows.scenario_start),
    );
    theme::status_list(
        frame,
        parts[1],
        theme::panel("Stages", app.focus == Focus::Stage),
        list(&rows.stages),
        app.stage_index.saturating_sub(rows.stage_start),
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
            theme::tone(Color::Cyan).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
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
    // Highlighting scans only these bounded rows and the one visible chunk.
    let needle = theme::needle(&app.filter);
    let rows = view
        .rows
        .iter()
        .map(|row| ListItem::new(theme::highlighted(row.text.replace('\n', " | "), &needle)))
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
    // Same line split as `Text::from(&str)`, so wrapping and scroll bounds match.
    let body = if needle.is_empty() {
        Text::from(view.body.as_str())
    } else {
        Text::from(
            view.body
                .lines()
                .map(|line| theme::highlighted(line.to_owned(), &needle))
                .collect::<Vec<_>>(),
        )
    };
    let detail = Paragraph::new(body)
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
