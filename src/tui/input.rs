use super::{App, Focus, Tab};
use crate::model::{ProfileName, ScenarioKind};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
pub fn input(app: &mut App, key: KeyEvent) {
    if key.kind == KeyEventKind::Release {
        return;
    }
    if app.editing_filter {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => app.editing_filter = false,
            KeyCode::Backspace => {
                app.filter.pop();
            }
            KeyCode::Char(c) => app.filter.push(c),
            _ => {}
        }
        app.evidence_index = 0;
        app.scroll = 0;
        app.refresh_logs();
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
        KeyCode::Char('r') if !app.active && app.recipe.is_some() && app.report.is_none() => {
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
            if let Some(options) = &mut app.options {
                options.scenarios = match options.scenarios.as_slice() {
                    [ScenarioKind::Clean] => vec![ScenarioKind::Upgrade],
                    [ScenarioKind::Upgrade] => vec![ScenarioKind::Clean, ScenarioKind::Upgrade],
                    _ => vec![ScenarioKind::Clean],
                };
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
        KeyCode::Char('/') => {
            app.editing_filter = true;
            app.filter.clear();
            app.evidence_index = 0;
            app.scroll = 0;
        }
        KeyCode::PageDown => app.scroll = app.scroll.saturating_add(10),
        KeyCode::PageUp => app.scroll = app.scroll.saturating_sub(10),
        KeyCode::Down | KeyCode::Char('j') | KeyCode::Up | KeyCode::Char('k') => {
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
                Focus::Evidence => app.items().len(),
            };
            let index = match app.focus {
                Focus::Scenario => &mut app.scenario_index,
                Focus::Stage => &mut app.stage_index,
                Focus::Evidence => &mut app.evidence_index,
            };
            *index = if down {
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
    app.refresh_logs();
}
