//! Native evidence has its own display domain; no Docker identity projection.
use super::{
    App,
    theme::{self, Hint, REQUIRED, hint},
};
use crate::{
    model::RunOutcome,
    native::{
        integration::model::{NativeIntegrationReport, NativeReportVersion},
        model::NativeReport,
        view::{Selector, evidence_page},
    },
};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Modifier},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Tabs, Wrap},
};

pub struct NativeView {
    pub(super) report: Option<NativeReportVersion>,
    execution: Option<(
        crate::engine::domain::SelectedRecipe,
        crate::engine::domain::NativeRunOptions,
        Option<crate::native::launch::LaunchAuthority>,
    )>,
    preview: String,
    selector: usize,
    record: usize,
    byte: usize,
}
const SELECTORS: [Selector; 6] = [
    Selector::Overview,
    Selector::NativeFiles,
    Selector::Commands,
    Selector::Dispositions,
    Selector::Identities,
    Selector::Failures,
];
impl NativeView {
    pub fn offline(report: NativeReport) -> Self {
        Self {
            report: Some(NativeReportVersion::CatalogV1(report)),
            execution: None,
            preview: String::new(),
            selector: 0,
            record: 0,
            byte: 0,
        }
    }
    pub fn integration_offline(report: NativeIntegrationReport) -> Self {
        let mut view = Self::preview(String::new());
        view.report = Some(NativeReportVersion::IntegrationV2(report));
        view
    }
    pub fn preview(preview: String) -> Self {
        Self {
            report: None,
            execution: None,
            preview,
            selector: 0,
            record: 0,
            byte: 0,
        }
    }
    pub fn set_execution(
        &mut self,
        recipe: crate::native::recipe::ValidatedNativeRecipe,
        options: crate::engine::domain::NativeRunOptions,
        launch: Option<crate::native::launch::LaunchAuthority>,
    ) {
        self.execution = Some((
            crate::engine::domain::SelectedRecipe::Native(recipe),
            options,
            launch,
        ));
    }
    pub fn set_integration_execution(
        &mut self,
        recipe: crate::native::integration::recipe::ValidatedIntegrationRecipe,
        options: crate::engine::domain::NativeRunOptions,
        launch: Option<crate::native::launch::LaunchAuthority>,
    ) {
        self.execution = Some((
            crate::engine::domain::SelectedRecipe::NativeIntegration(recipe),
            options,
            launch,
        ));
    }
    pub(super) fn take_execution(
        &mut self,
    ) -> anyhow::Result<
        Option<(
            crate::engine::domain::SelectedRecipe,
            crate::engine::domain::NativeRunOptions,
            crate::native::launch::LaunchAuthority,
        )>,
    > {
        let Some((recipe, options, launch)) = self.execution.as_ref() else {
            return Ok(None);
        };
        anyhow::ensure!(
            launch.is_some(),
            "native execution requires explicit --native-target"
        );
        match recipe {
            crate::engine::domain::SelectedRecipe::Native(r) => {
                r.validate_selection(&options.scenarios, &options.limits)?
            }
            crate::engine::domain::SelectedRecipe::NativeIntegration(r) => {
                r.validate_selection(&options.scenarios, &options.limits)?
            }
            _ => anyhow::bail!("native execution backend mismatch"),
        }
        let (recipe, options, launch) = self.execution.take().unwrap();
        Ok(Some((recipe, options, launch.unwrap())))
    }
    pub(super) fn set_report(&mut self, report: NativeReport) {
        if matches!(self.report, Some(NativeReportVersion::IntegrationV2(_))) {
            self.selector = 0;
            self.record = 0;
        }
        self.report = Some(NativeReportVersion::CatalogV1(report));
        self.byte = 0;
    }
    pub(super) fn set_integration_report(&mut self, report: NativeIntegrationReport) {
        if matches!(self.report, Some(NativeReportVersion::CatalogV1(_))) {
            self.selector = 0;
            self.record = 0;
        }
        self.report = Some(NativeReportVersion::IntegrationV2(report));
        self.byte = 0;
    }
    fn page(&self) -> anyhow::Result<crate::native::view::Page> {
        match self.report.as_ref().unwrap() {
            NativeReportVersion::CatalogV1(report) => evidence_page(
                report,
                SELECTORS[self.selector % SELECTORS.len()],
                self.record,
                1,
            ),
            NativeReportVersion::IntegrationV2(report) => {
                use crate::native::integration::view::{Selector as S, evidence_page};
                let selectors = [
                    S::Overview,
                    S::Services,
                    S::Readiness,
                    S::Checks,
                    S::Dispositions,
                    S::Desktop,
                    S::NativeFiles,
                    S::Commands,
                    S::Identities,
                    S::Failures,
                ];
                evidence_page(report, selectors[self.selector], self.record, 1)
            }
        }
    }
    fn integration_detail(
        &self,
    ) -> Option<anyhow::Result<crate::native::integration::view::DetailPage>> {
        let NativeReportVersion::IntegrationV2(report) = self.report.as_ref()? else {
            return None;
        };
        use crate::native::integration::view::{Selector as S, detail_page};
        let selectors = [
            S::Overview,
            S::Services,
            S::Readiness,
            S::Checks,
            S::Dispositions,
            S::Desktop,
            S::NativeFiles,
            S::Commands,
            S::Identities,
            S::Failures,
        ];
        Some(detail_page(
            report,
            selectors[self.selector],
            self.record,
            self.byte,
            4096,
        ))
    }
    pub fn outcome(&self) -> RunOutcome {
        self.report
            .as_ref()
            .map_or(RunOutcome::Passed, |r| r.consumer_outcome())
    }
    /// Outcome line and bounded, sanitized evidence body.
    fn text(&self) -> (Option<String>, String) {
        let (value, start) = if let Some(page) = self.integration_detail() {
            match page {
                Ok(page) => (
                    String::from_utf8(page.bytes).expect("detail pages preserve UTF-8"),
                    page.byte_offset,
                ),
                Err(error) => (format!("Native evidence unavailable: {error}"), 0),
            }
        } else {
            let value = if self.report.is_some() {
                self.page()
                    .and_then(|page| Ok(serde_json::to_string_pretty(&page)?))
                    .unwrap_or_else(|e| format!("Native evidence unavailable: {e}"))
            } else {
                self.preview.clone()
            };
            let mut start = self.byte.min(value.len());
            while !value.is_char_boundary(start) {
                start -= 1;
            }
            let mut end = (start + 4096).min(value.len());
            while !value.is_char_boundary(end) {
                end -= 1;
            }
            (value[start..end].to_owned(), start)
        };
        let outcome = self
            .report
            .as_ref()
            .map(|r| r.consumer_outcome().to_string());
        let body = format!(
            "Selector: {} | record {} | byte {}\n{}",
            self.selector_names()[self.selector],
            self.record,
            start,
            super::safe_text(&value)
        );
        (outcome, body)
    }
    /// Switch to selector `index` (as ←/→ reaching it does), restarting at
    /// its first record and byte. An out-of-range index does nothing.
    pub(super) fn select(&mut self, index: usize) -> bool {
        if index >= self.selector_names().len() {
            return false;
        }
        self.selector = index;
        self.record = 0;
        self.byte = 0;
        true
    }
    fn selector_names(&self) -> &'static [&'static str] {
        if matches!(self.report, Some(NativeReportVersion::IntegrationV2(_))) {
            &[
                "Overview",
                "Services",
                "Readiness",
                "Checks",
                "Dispositions",
                "Desktop",
                "NativeFiles",
                "Commands",
                "Identities",
                "Failures",
            ]
        } else {
            &[
                "Overview",
                "NativeFiles",
                "Commands",
                "Dispositions",
                "Identities",
                "Failures",
            ]
        }
    }
}
/// Only keys that act now: selectors and records need a report, r is the
/// single explicit start, c applies while running.
pub(super) fn hints(app: &App, view: &NativeView) -> Vec<Hint> {
    use theme::Group::*;
    let mut hints = Vec::new();
    if view.report.is_some() {
        hints.push(hint(Navigate, "←→", "selector", REQUIRED));
        hints.push(hint(Navigate, "↑↓", "record", 1));
    }
    hints.push(hint(Evidence, "n/p", "text page", 2));
    hints.push(hint(Evidence, "PgUp/Dn", "scroll", 3));
    if !app.active && !app.work_consumed && view.report.is_none() {
        hints.push(hint(Run, "r", "run", REQUIRED));
    }
    super::render::common_hints(&mut hints, app);
    hints
}
pub fn render(frame: &mut Frame, app: &App) -> bool {
    let Some(view) = &app.native else {
        return false;
    };
    let (outcome, body) = view.text();
    let area = frame.area();
    // Header: outcome glyph once (or the preview notice), as in the run view.
    let mut header = match &outcome {
        // Glyph first, so the exact `Outcome: <word>` text stays intact for
        // readers and for the real-manager acceptance checks that match it.
        Some(outcome) => {
            let (glyph, color) = theme::status(outcome);
            let style = theme::tone(color).add_modifier(Modifier::BOLD);
            vec![
                Span::styled(format!("{glyph} "), style),
                Span::raw("Outcome: "),
                Span::styled(super::safe_text(outcome), style),
            ]
        }
        None => vec![Span::raw("Preview — no commands executed")],
    };
    if let Some(elapsed) = app.run_clock() {
        let clock = super::render::clock(elapsed);
        header.push(if app.active {
            Span::styled(
                format!("  │ {} running {clock}", super::render::spinner(elapsed)),
                theme::tone(Color::Cyan),
            )
        } else {
            Span::styled(format!("  │ ran {clock}"), theme::dim())
        });
    }
    let hints = hints(app, view);
    let status = super::render::projection(&app.status, 512);
    let footer = theme::footer(&status, &hints, area.width);
    let tabs_height = u16::from(view.report.is_some());
    let regions = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(tabs_height),
        Constraint::Min(1),
        Constraint::Length(theme::footer_height(&footer, area.width)),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new(Line::from(header)).block(
            Block::default()
                .title("Theseus native")
                .borders(Borders::ALL)
                .border_style(theme::dim()),
        ),
        regions[0],
    );
    if view.report.is_some() {
        let names = view.selector_names();
        app.hits.borrow_mut().native_tabs =
            super::mouse::tab_areas(regions[1], names.iter().map(|n| n.chars().count()))
                .into_iter()
                .zip(0..names.len())
                .collect();
        frame.render_widget(
            Tabs::new(names.iter().copied())
                .select(view.selector)
                .highlight_style(
                    theme::tone(Color::Cyan).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                ),
            regions[1],
        );
    }
    let mut lines = body.split('\n');
    let mut text = vec![Line::styled(
        lines.next().unwrap_or_default().to_owned(),
        theme::dim(),
    )];
    text.extend(lines.map(|l| Line::raw(l.to_owned())));
    frame.render_widget(
        Paragraph::new(text)
            .block(theme::plain_panel("Native evidence", true))
            .wrap(Wrap { trim: false })
            .scroll((app.scroll, 0)),
        regions[2],
    );
    app.hits.borrow_mut().scroll = Some(regions[2]);
    frame.render_widget(footer, regions[3]);
    true
}
pub fn input(app: &mut App, key: KeyEvent) -> bool {
    let Some(view) = &mut app.native else {
        return false;
    };
    match key.code {
        KeyCode::Char('q') => {
            app.quit_requested = true;
            if app.active {
                app.cancel_requested = true;
            }
        }
        KeyCode::Char('c') if app.active => app.cancel_requested = true,
        KeyCode::Char('r') if !app.active && !app.work_consumed && view.report.is_none() => {
            app.start_requested = true
        }
        KeyCode::Left | KeyCode::Right => {
            let count = view.selector_names().len();
            let step = if key.code == KeyCode::Right {
                1
            } else {
                count - 1
            };
            view.select((view.selector + step) % count);
            app.scroll = 0;
        }
        KeyCode::Down => {
            let more = if let Some(page) = view.integration_detail() {
                page.is_ok_and(|p| p.next_record_offset.is_some())
            } else {
                view.report.is_some() && view.page().is_ok_and(|p| p.next_offset.is_some())
            };
            if more {
                view.record += 1;
                view.byte = 0;
                app.scroll = 0;
            }
        }
        KeyCode::Up => {
            view.record = view.record.saturating_sub(1);
            view.byte = 0;
            app.scroll = 0;
        }
        KeyCode::Char('n') => {
            if let Some(page) = view.integration_detail() {
                if let Ok(page) = page
                    && let Some(next) = page.next_byte_offset
                {
                    view.byte = next;
                    app.scroll = 0;
                }
            } else {
                view.byte = view.byte.saturating_add(4096);
            }
        }
        KeyCode::Char('p') => {
            view.byte = view.byte.saturating_sub(4096);
            app.scroll = 0;
        }
        KeyCode::PageDown => app.scroll = app.scroll.saturating_add(10),
        KeyCode::PageUp => app.scroll = app.scroll.saturating_sub(10),
        _ => (),
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    fn draw(app: &App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|f| super::super::render(f, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }
    #[test]
    fn saved_integration_readiness_renders_real_widgets_without_execution_authority() {
        let report = crate::native::integration::view::tests::report();
        let mut app = App::integration_offline(report, "/saved-evidence".into());
        let overview = draw(&app);
        assert!(overview.contains("native_integration_v1"));
        assert!(overview.contains("incomplete"));
        input(&mut app, KeyCode::Right.into());
        input(&mut app, KeyCode::Right.into());
        let readiness = draw(&app);
        assert!(readiness.contains("Readiness"));
        assert!(readiness.contains("readiness response unavailable"));
        assert!(!app.start_requested);
        assert_eq!(app.outcome(), RunOutcome::Incomplete);
        assert!(
            app.native
                .as_mut()
                .unwrap()
                .take_execution()
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn native_chrome_matches_the_workbench_and_offers_only_live_keys() {
        let preview = App::native_preview("Native catalog preview\n\u{1b}[31mred".into());
        let text = draw(&preview);
        assert!(text.contains("Preview — no commands executed"));
        assert!(text.contains("Native preview | n/p text page"));
        assert!(text.contains("r run"));
        assert!(!text.contains("selector"), "no selectors before a report");
        assert!(!text.contains('\u{1b}'));
        let report = crate::native::integration::view::tests::report();
        let mut app = App::integration_offline(report, "/saved-evidence".into());
        let text = draw(&app);
        assert!(text.contains("… Outcome: incomplete"));
        assert!(text.contains("Overview │ Services │ Readiness"));
        assert!(text.contains("←→ selector") && !text.contains("r run"));
        input(&mut app, KeyCode::Right.into());
        assert!(draw(&app).contains("Selector: Services | record 0 | byte 0"));
    }
    fn rows(app: &App) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|f| super::super::render(f, app)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect()
            })
            .collect()
    }
    fn pointer(app: &mut App, kind: crossterm::event::MouseEventKind, column: u16, row: u16) {
        super::super::mouse::input(
            app,
            crossterm::event::MouseEvent {
                kind,
                column,
                row,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        );
    }
    /// Column and row of `needle` on the selector tab row.
    fn tab_at(screen: &[String], needle: &str) -> (u16, u16) {
        screen
            .iter()
            .enumerate()
            .find_map(|(y, row)| {
                (row.contains("Overview │"))
                    .then(|| row.find(needle))
                    .flatten()
                    .map(|x| (row[..x].chars().count() as u16, y as u16))
            })
            .unwrap_or_else(|| panic!("{needle} not on the tab row\n{}", screen.join("\n")))
    }
    #[test]
    fn native_selector_tabs_switch_on_click_and_wheel_still_scrolls() {
        use crossterm::event::{MouseButton, MouseEventKind::*};
        let click = Down(MouseButton::Left);
        let report = crate::native::integration::view::tests::report();
        let mut app = App::integration_offline(report, "/saved-evidence".into());
        // Every recorded rectangle covers its own drawn selector title.
        let screen = rows(&app);
        let tabs = app.hits.borrow().native_tabs.clone();
        assert_eq!(tabs.len(), 10);
        let names = app.native.as_ref().unwrap().selector_names();
        for (rect, index) in tabs {
            let text: String = screen[rect.y as usize]
                .chars()
                .skip(rect.x as usize)
                .take(rect.width as usize)
                .collect();
            assert_eq!(text.trim(), names[index], "{rect:?}");
        }
        // A click on a selector tab switches to it, like ←/→ reaching it.
        app.native.as_mut().unwrap().record = 1;
        app.scroll = 4;
        let (x, y) = tab_at(&screen, "Readiness");
        pointer(&mut app, click, x + 2, y);
        let view = app.native.as_ref().unwrap();
        assert_eq!((view.selector, view.record, view.byte), (2, 0, 0));
        assert_eq!(app.scroll, 0);
        let screen = rows(&app);
        assert!(
            screen
                .iter()
                .any(|r| r.contains("Selector: Readiness | record 0"))
        );
        assert!(screen.iter().any(|r| r.contains("… Outcome: incomplete")));
        // Clicking the evidence body (one record, not rows) changes nothing.
        let (x, y) = tab_at(&screen, "Overview");
        pointer(&mut app, click, x, y + 3);
        assert_eq!(app.native.as_ref().unwrap().selector, 2);
        // Wheel over the evidence still scrolls it; over the tabs it does not.
        pointer(&mut app, ScrollDown, x, y + 3);
        assert_eq!(app.scroll, 3);
        pointer(&mut app, ScrollDown, x, y);
        assert_eq!((app.scroll, app.native.as_ref().unwrap().selector), (3, 2));
        // Help open: a click on a tab only closes help.
        let screen = rows(&app);
        app.help = true;
        let (x, y) = tab_at(&screen, "Failures");
        pointer(&mut app, click, x, y);
        assert!(!app.help);
        assert_eq!(app.native.as_ref().unwrap().selector, 2);
        pointer(&mut app, click, x, y);
        assert_eq!(app.native.as_ref().unwrap().selector, 9);
        // A preview has no selector tabs to click.
        let preview = App::native_preview("no report".into());
        rows(&preview);
        assert!(preview.hits.borrow().native_tabs.is_empty());
    }
    #[test]
    fn large_readiness_details_and_following_record_remain_navigable() {
        let report = crate::native::integration::view::tests::large_readiness_report();
        let mut app = App::integration_offline(report, "/saved-large".into());
        input(&mut app, KeyCode::Right.into());
        input(&mut app, KeyCode::Right.into());
        assert!(!draw(&app).contains("Native evidence unavailable"));
        input(&mut app, KeyCode::Char('n').into());
        assert!(!draw(&app).contains("Native evidence unavailable"));
        app.scroll = 200;
        input(&mut app, KeyCode::Down.into());
        assert!(draw(&app).contains("ready-following"));
    }
}
