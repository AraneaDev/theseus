//! Native evidence has its own display domain; no Docker identity projection.
use super::App;
use crate::{
    model::RunOutcome,
    native::{
        model::NativeReport,
        view::{Selector, evidence_page},
    },
};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    widgets::{Block, Borders, Paragraph, Wrap},
};

pub struct NativeView {
    pub(super) report: Option<NativeReport>,
    execution: Option<(
        crate::native::recipe::ValidatedNativeRecipe,
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
            report: Some(report),
            execution: None,
            preview: String::new(),
            selector: 0,
            record: 0,
            byte: 0,
        }
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
        self.execution = Some((recipe, options, launch));
    }
    pub(super) fn take_execution(
        &mut self,
    ) -> anyhow::Result<
        Option<(
            crate::native::recipe::ValidatedNativeRecipe,
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
        recipe.validate_selection(&options.scenarios, &options.limits)?;
        let (recipe, options, launch) = self.execution.take().unwrap();
        Ok(Some((recipe, options, launch.unwrap())))
    }
    pub(super) fn set_report(&mut self, report: NativeReport) {
        self.report = Some(report);
        self.byte = 0;
    }
    pub fn outcome(&self) -> RunOutcome {
        self.report
            .as_ref()
            .map_or(RunOutcome::Passed, |r| r.outcome)
    }
    fn text(&self) -> String {
        let value = if let Some(report) = &self.report {
            evidence_page(report, SELECTORS[self.selector], self.record, 1)
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
        format!(
            "Selector: {:?} | record {} | byte {}\n{}\n\nleft/right: selector | up/down: record | n/p: text page | r: run | c: cancel | q: quit",
            SELECTORS[self.selector],
            self.record,
            start,
            super::safe_text(&value[start..end])
        )
    }
}
pub fn render(frame: &mut Frame, app: &App) -> bool {
    let Some(view) = &app.native else {
        return false;
    };
    frame.render_widget(
        Paragraph::new(format!(
            "{}\n{}",
            super::safe_text(&app.status),
            view.text()
        ))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Native catalog evidence"),
        )
        .wrap(Wrap { trim: false })
        .scroll((app.scroll, 0)),
        frame.area(),
    );
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
            view.selector = (view.selector + if key.code == KeyCode::Right { 1 } else { 5 }) % 6;
            view.record = 0;
            view.byte = 0;
            app.scroll = 0;
        }
        KeyCode::Down => {
            if let Some(report) = &view.report
                && evidence_page(report, SELECTORS[view.selector], view.record, 1)
                    .is_ok_and(|p| p.next_offset.is_some())
            {
                view.record += 1;
                view.byte = 0;
            }
        }
        KeyCode::Up => {
            view.record = view.record.saturating_sub(1);
            view.byte = 0;
        }
        KeyCode::Char('n') => view.byte = view.byte.saturating_add(4096),
        KeyCode::Char('p') => view.byte = view.byte.saturating_sub(4096),
        KeyCode::PageDown => app.scroll = app.scroll.saturating_add(10),
        KeyCode::PageUp => app.scroll = app.scroll.saturating_sub(10),
        _ => (),
    }
    true
}
