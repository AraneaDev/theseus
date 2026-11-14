//! Opt-in (`--mouse`) pointer input. Clicks and wheel map through the panel
//! rectangles the renderer recorded for the last drawn frame, so hit-testing
//! never re-derives layout.
use super::{App, Focus, Tab};
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

/// A list's visible rows: `area` holds one row per line, the first of which is
/// absolute item `first`; rows at or past `end` are empty space.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ListHit {
    pub area: Rect,
    pub first: usize,
    pub end: usize,
}
impl ListHit {
    /// From a drawn window of rows starting at absolute item `start`:
    /// its row area, the list's scroll offset within it and its row count.
    pub fn drawn(start: usize, (area, offset, count): (Rect, usize, usize)) -> Self {
        Self {
            area,
            first: start + offset,
            end: start + count,
        }
    }
    fn row(&self, at: Position) -> Option<usize> {
        if !self.area.contains(at) {
            return None;
        }
        let index = self.first + usize::from(at.y - self.area.y);
        (index < self.end).then_some(index)
    }
}
/// Panels of the last drawn frame. Reset before every draw, so a panel that
/// is not on screen (another mode, a narrow layout) is never clickable.
#[derive(Clone, Debug, Default)]
pub(super) struct Hits {
    pub scenarios: Option<ListHit>,
    pub stages: Option<ListHit>,
    pub records: Option<ListHit>,
    /// Comparison cell rows.
    pub cells: Option<ListHit>,
    pub tabs: Vec<(Rect, Tab)>,
    /// Native evidence selector tabs, by selector index.
    pub native_tabs: Vec<(Rect, usize)>,
    /// Scrollable text: the selected detail, a preview or pending body.
    pub scroll: Option<Rect>,
}

/// Where each `Tabs` title (with its one-column padding) is drawn: ratatui
/// writes `pad title pad` per title with a one-column divider between them.
pub(super) fn tab_areas(area: Rect, widths: impl IntoIterator<Item = usize>) -> Vec<Rect> {
    let mut x = area.x;
    let mut out = Vec::new();
    for width in widths {
        if x >= area.right() {
            break;
        }
        let span = u16::try_from(width + 2).unwrap_or(u16::MAX);
        let end = x.saturating_add(span).min(area.right());
        out.push(Rect::new(x, area.y, end - x, 1));
        x = end.saturating_add(1);
    }
    out
}

const WHEEL_LINES: u16 = 3;

/// Apply one mouse event. Nothing acts while the editor or a confirmation is
/// open or the filter draft is being typed; with help open a click closes it.
pub(super) fn input(app: &mut App, event: MouseEvent) {
    let at = Position::new(event.column, event.row);
    let click = matches!(event.kind, MouseEventKind::Down(MouseButton::Left));
    let wheel = match event.kind {
        MouseEventKind::ScrollDown => Some(true),
        MouseEventKind::ScrollUp => Some(false),
        _ => None,
    };
    if app.help {
        if click {
            app.help = false;
        }
        return;
    }
    if app.editing_filter || super::preview_edit::modal(app) || (!click && wheel.is_none()) {
        return;
    }
    let hits = app.hits.borrow().clone();
    if let Some(down) = wheel {
        wheel_at(app, &hits, at, down);
    } else {
        click_at(app, &hits, at);
    }
    if app.native.is_none() {
        app.sync_view();
    }
}

fn scroll_by(app: &mut App, down: bool) {
    app.scroll = if down {
        app.scroll.saturating_add(WHEEL_LINES)
    } else {
        app.scroll.saturating_sub(WHEEL_LINES)
    };
}

fn wheel_at(app: &mut App, hits: &Hits, at: Position, down: bool) {
    let step = |index: usize, len: usize| {
        if down {
            index.saturating_add(1).min(len.saturating_sub(1))
        } else {
            index.saturating_sub(1)
        }
    };
    let over = |hit: &Option<ListHit>| hit.is_some_and(|h| h.area.contains(at));
    if over(&hits.scenarios) {
        let len = app.report.as_ref().map_or(0, |r| r.scenarios.len());
        select_scenario(app, step(app.scenario_index, len), false);
    } else if over(&hits.stages) {
        let len = app.scenario().map_or(0, |s| s.stages.len());
        select_stage(app, step(app.stage_index, len), false);
    } else if over(&hits.cells) {
        let len = app
            .comparison
            .as_ref()
            .and_then(|v| v.report.as_ref())
            .map_or(0, |r| r.cells.len());
        let row = app.comparison.as_ref().map_or(0, |v| v.row);
        select_cell(app, step(row, len), false);
    } else if over(&hits.records) {
        let len = app.evidence_len();
        select_record(app, step(app.evidence_index, len), false);
    } else if hits.scroll.is_some_and(|r| r.contains(at)) {
        scroll_by(app, down);
    }
}

fn click_at(app: &mut App, hits: &Hits, at: Position) {
    if let Some(i) = hits.scenarios.and_then(|h| h.row(at)) {
        select_scenario(app, i, true);
    } else if let Some(i) = hits.stages.and_then(|h| h.row(at)) {
        select_stage(app, i, true);
    } else if let Some(i) = hits.cells.and_then(|h| h.row(at)) {
        select_cell(app, i, true);
    } else if let Some(i) = hits.records.and_then(|h| h.row(at)) {
        select_record(app, i, true);
    } else if let Some(&(_, index)) = hits.native_tabs.iter().find(|(r, _)| r.contains(at)) {
        // Native evidence shows one record at a time (no rows to click).
        if let Some(view) = app.native.as_mut()
            && view.select(index)
        {
            app.scroll = 0;
        }
    } else if let Some((_, tab)) = hits.tabs.iter().find(|(r, _)| r.contains(at)) {
        app.navigated |= app.report.is_some();
        app.focus = Focus::Evidence;
        if app.tab != *tab {
            app.tab = *tab;
            app.evidence_index = 0;
        }
        app.scroll = 0;
    }
}

fn select_scenario(app: &mut App, index: usize, focus: bool) {
    let Some(len) = app.report.as_ref().map(|r| r.scenarios.len()) else {
        return;
    };
    let index = index.min(len.saturating_sub(1));
    app.navigated = true;
    if focus {
        app.focus = Focus::Scenario;
    }
    if index != app.scenario_index {
        app.scenario_index = index;
        app.stage_index = 0;
        app.evidence_index = 0;
        app.scroll = 0;
    }
}
fn select_stage(app: &mut App, index: usize, focus: bool) {
    let Some(len) = app.scenario().map(|s| s.stages.len()) else {
        return;
    };
    let index = index.min(len.saturating_sub(1));
    app.navigated = true;
    if focus {
        app.focus = Focus::Stage;
    }
    if index != app.stage_index {
        app.stage_index = index;
        app.evidence_index = 0;
        app.scroll = 0;
    }
}
fn select_cell(app: &mut App, index: usize, focus: bool) {
    let Some(view) = app.comparison.as_mut() else {
        return;
    };
    let len = view.report.as_ref().map_or(0, |r| r.cells.len());
    let index = index.min(len.saturating_sub(1));
    if focus {
        view.evidence_focus = false;
    }
    if index != view.row {
        view.row = index;
        app.evidence_index = 0;
        app.scroll = 0;
    }
}
fn select_record(app: &mut App, index: usize, focus: bool) {
    let index = index.min(app.evidence_len().saturating_sub(1));
    app.navigated |= app.report.is_some();
    if focus {
        if let Some(view) = app.comparison.as_mut() {
            view.evidence_focus = true;
        } else {
            app.focus = Focus::Evidence;
        }
    }
    if index != app.evidence_index {
        app.evidence_index = index;
        app.scroll = 0;
    }
}
