use super::{App, Focus, Tab};
use crate::model::ProfileName;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
pub fn input(app: &mut App, key: KeyEvent) {
    if key.kind == KeyEventKind::Release {
        return;
    }
    if super::preview_edit::input(app, key) {
        return;
    }
    app.viewport.borrow_mut().update(app, None);
    if filter_input(app, key) {
        return;
    }
    if page_input(app, key) {
        return;
    }
    if app.comparison.is_some() {
        super::comparison::input(app, key);
        return;
    }

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
                && app.recipe.is_some()
                && app.report.is_none() =>
        {
            app.start_requested = true
        }
        KeyCode::Char('p') if app.report.is_none() && !app.active => {
            if let (Some(options), Some(recipe)) = (&mut app.options, &app.recipe) {
                let all = recipe.recipe.profiles.keys().copied().collect::<Vec<_>>();
                let choices = [all, vec![ProfileName::User], vec![ProfileName::System]]
                    .into_iter()
                    .filter(|p| p.iter().all(|n| recipe.recipe.profiles.contains_key(n)))
                    .collect::<Vec<_>>();
                let i = choices
                    .iter()
                    .position(|p| *p == options.profiles)
                    .unwrap_or(0);
                options.profiles = choices[(i + 1) % choices.len()].clone();
            }
        }
        KeyCode::Char('s') if app.report.is_none() && !app.active => {
            if let (Some(options), Some(recipe)) = (&mut app.options, &app.recipe) {
                let all = recipe.declared_scenarios();
                let mut choices = vec![all.clone()];
                for scenario in all {
                    let individual = vec![scenario];
                    if !choices.contains(&individual) {
                        choices.push(individual);
                    }
                }
                let i = choices
                    .iter()
                    .position(|s| *s == options.scenarios)
                    .unwrap_or(0);
                options.scenarios = choices[(i + 1) % choices.len()].clone();
            }
        }
        KeyCode::Tab | KeyCode::Enter => {
            app.focus = match app.focus {
                Focus::Scenario => Focus::Stage,
                Focus::Stage => Focus::Evidence,
                Focus::Evidence => Focus::Scenario,
            };
            app.scroll = 0;
        }
        KeyCode::Esc => {
            app.focus = match app.focus {
                Focus::Evidence => Focus::Stage,
                _ => Focus::Scenario,
            };
            app.scroll = 0;
        }
        KeyCode::Left | KeyCode::Right if app.focus == Focus::Evidence => {
            let delta = if key.code == KeyCode::Right { 1 } else { 4 };
            app.tab = Tab::ALL[(app.tab.index() + delta) % 5];
            app.evidence_index = 0;
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
            if app.report.is_none() {
                app.scroll = if matches!(key.code, KeyCode::Down | KeyCode::Char('j')) {
                    app.scroll.saturating_add(1)
                } else {
                    app.scroll.saturating_sub(1)
                };
                return;
            }
            let down = matches!(key.code, KeyCode::Down | KeyCode::Char('j'));
            let len = match app.focus {
                Focus::Scenario => app.report.as_ref().map_or(0, |r| r.scenarios.len()),
                Focus::Stage => app.scenario().map_or(0, |s| s.stages.len()),
                Focus::Evidence => app.evidence_len(),
            };
            let index = match app.focus {
                Focus::Scenario => &mut app.scenario_index,
                Focus::Stage => &mut app.stage_index,
                Focus::Evidence => &mut app.evidence_index,
            };
            *index = if key.code == KeyCode::Home {
                0
            } else if key.code == KeyCode::End {
                len.saturating_sub(1)
            } else if down {
                index.saturating_add(1).min(len.saturating_sub(1))
            } else {
                index.saturating_sub(1)
            };
            if app.focus == Focus::Scenario {
                app.stage_index = 0;
            }
            if app.focus != Focus::Evidence {
                app.evidence_index = 0;
            }
            app.scroll = 0;
        }
        _ => {}
    }
    app.sync_view();
}

/// Filter edits are cheap drafts. Enter and Esc both apply the complete query.
pub(super) fn filter_input(app: &mut App, key: KeyEvent) -> bool {
    if app.editing_filter {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => {
                app.filter = std::mem::take(&mut app.filter_draft);
                app.editing_filter = false;
                app.viewport.borrow_mut().restart_detail();
                app.evidence_index = 0;
                app.scroll = 0;
                app.sync_view();
            }
            KeyCode::Backspace => {
                app.filter_draft.pop();
            }
            KeyCode::Char(c) => app.filter_draft.push(c),
            _ => {}
        }
        return true;
    }
    if key.code == KeyCode::Char('/') {
        app.editing_filter = true;
        app.filter_draft.clear();
        return true;
    }
    false
}
pub(super) fn page_input(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('m') => {
            if !app.viewport.borrow_mut().toggle_log_metadata(app) {
                return false;
            }
        }
        KeyCode::Char(']') => app.viewport.borrow_mut().next(app),
        KeyCode::Char('[') => app.viewport.borrow_mut().previous(app),
        KeyCode::Home | KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.viewport
                .borrow_mut()
                .log_edge(app, key.code == KeyCode::End);
        }
        _ => return false,
    }
    app.scroll = 0;
    app.sync_view();
    true
}
