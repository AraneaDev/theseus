//! Pristine preview editing; the accepted document changes only after Save Copy succeeds.
use super::{
    App,
    editor::{Editor, EditorAction},
};
use crate::preview::{DocumentKind, DraftDocument, PreviewSource, ValidatedDocument};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Frame, text::Line, widgets::Paragraph};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Editor,
    Preview,
    SaveName,
    DiscardConfirm,
    QuitConfirm,
}

pub(super) struct Session {
    source: PreviewSource,
    accepted: String,
    editor: Editor,
    mode: Mode,
    dirty: bool,
    name: String,
    name_cursor: usize,
}

pub(super) fn dirty(app: &App) -> bool {
    app.edit_session.as_ref().is_some_and(|s| s.dirty)
}

/// Whether keys are text or modal answers (editor, Save Copy name, confirm).
pub(super) fn modal(app: &App) -> bool {
    app.edit_session
        .as_ref()
        .is_some_and(|s| s.mode != Mode::Preview)
}

pub(super) fn blocks_work(app: &App) -> bool {
    app.edit_session
        .as_ref()
        .is_some_and(|s| s.dirty || s.mode != Mode::Preview)
}

pub(super) fn eligible(app: &App) -> bool {
    !app.active
        && !app.work_consumed
        && !app.quit_requested
        && app.terminal_error.is_none()
        && app.report.is_none()
        && (app.recipe.is_some()
            || app
                .comparison
                .as_ref()
                .is_some_and(|v| v.suite.is_some() && v.report.is_none()))
}

fn source(app: &App) -> Option<PreviewSource> {
    if let Some(view) = &app.comparison {
        let suite = view.suite.as_ref()?;
        Some(PreviewSource {
            path: suite.path.clone(),
            digest: suite.digest.clone(),
            kind: DocumentKind::Suite,
        })
    } else {
        let recipe = app.recipe.as_ref()?;
        Some(PreviewSource {
            path: recipe.path.clone(),
            digest: recipe.digest.clone(),
            kind: DocumentKind::Recipe,
        })
    }
}

fn open(app: &mut App) {
    app.start_requested = false;
    if let Some(session) = &mut app.edit_session {
        session.mode = Mode::Editor;
        return;
    }
    let Some(source) = source(app) else { return };
    match DraftDocument::open(source.clone()).and_then(|doc| {
        let accepted = doc.text.clone();
        Ok(Session {
            source,
            accepted,
            editor: Editor::new(doc.text)?,
            mode: Mode::Editor,
            dirty: false,
            name: String::new(),
            name_cursor: 0,
        })
    }) {
        Ok(session) => app.edit_session = Some(session),
        Err(error) => app.status = format!("Cannot edit preview: {error:#}"),
    }
}

fn draft(session: &Session) -> DraftDocument {
    DraftDocument {
        source: session.source.clone(),
        text: session.editor.text().to_owned(),
    }
}

fn name_key(session: &mut Session, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc => {
            session.mode = Mode::Editor;
            return false;
        }
        KeyCode::Enter => return true,
        KeyCode::Left => {
            session.name_cursor = session.name[..session.name_cursor]
                .char_indices()
                .next_back()
                .map_or(0, |(i, _)| i)
        }
        KeyCode::Right => {
            if let Some(c) = session.name[session.name_cursor..].chars().next() {
                session.name_cursor += c.len_utf8();
            }
        }
        KeyCode::Home => session.name_cursor = 0,
        KeyCode::End => session.name_cursor = session.name.len(),
        KeyCode::Backspace => {
            if let Some((i, _)) = session.name[..session.name_cursor]
                .char_indices()
                .next_back()
            {
                session.name.drain(i..session.name_cursor);
                session.name_cursor = i;
            }
        }
        KeyCode::Delete => {
            if let Some(c) = session.name[session.name_cursor..].chars().next() {
                session
                    .name
                    .drain(session.name_cursor..session.name_cursor + c.len_utf8());
            }
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            let mut bytes = [0; 4];
            insert_name(session, c.encode_utf8(&mut bytes));
        }
        _ => {}
    }
    false
}

fn insert_name(session: &mut Session, text: &str) {
    if text.len() > 128usize.saturating_sub(session.name.len()) {
        session
            .editor
            .set_diagnostic("Save Copy name exceeds 128 bytes");
        return;
    }
    session.name.insert_str(session.name_cursor, text);
    session.name_cursor += text.len();
}

fn display_safe(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { '�' } else { c })
        .collect()
}

fn save(app: &mut App) {
    let Some(session) = &mut app.edit_session else {
        return;
    };
    let name = session.name.clone();
    match draft(session).save_copy(&name) {
        Ok(ValidatedDocument::Recipe(recipe)) => {
            if let Some(options) = &mut app.options {
                options
                    .profiles
                    .retain(|profile| recipe.recipe.profiles.contains_key(profile));
                if options.profiles.is_empty() {
                    options.profiles = recipe.recipe.profiles.keys().copied().collect();
                }
                let declared = recipe.declared_scenarios();
                options
                    .scenarios
                    .retain(|scenario| declared.contains(scenario));
                if options.scenarios.is_empty() {
                    options.scenarios = declared;
                }
            }
            app.status = format!("Saved preview copy: {}", recipe.path.display());
            app.recipe = Some(recipe);
            app.edit_session = None;
        }
        Ok(ValidatedDocument::Suite(suite)) => {
            if let Some(view) = &mut app.comparison {
                view.manifest = suite.manifest.clone();
                view.suite = Some(suite);
            }
            app.status = format!(
                "Saved preview copy: {}",
                app.comparison
                    .as_ref()
                    .unwrap()
                    .suite
                    .as_ref()
                    .unwrap()
                    .path
                    .display()
            );
            app.edit_session = None;
        }
        Err(error) => session.editor.set_diagnostic(&format!(
            "Save Copy failed: {name} adjacent to accepted source: {error:#}"
        )),
    }
    app.start_requested = false;
}

/// Consume preview editing controls before filtering, paging or normal workbench input.
pub(super) fn input(app: &mut App, key: KeyEvent) -> bool {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return false;
    }
    if !eligible(app) {
        app.edit_session = None;
        return false;
    }
    if app.editing_filter
        && app
            .edit_session
            .as_ref()
            .is_none_or(|s| s.mode == Mode::Preview)
    {
        return false;
    }
    let plain = !key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
    if app.edit_session.is_none() {
        if plain && key.code == KeyCode::Char('e') {
            open(app);
            return true;
        }
        return false;
    }
    let session = app.edit_session.as_mut().unwrap();
    match session.mode {
        Mode::Editor => {
            let mut recompute = false;
            let action = session.editor.key(key);
            match action {
                EditorAction::Validate => {
                    let result = draft(session).validate();
                    session.editor.set_diagnostic(&match result {
                        Ok(_) => "Draft valid; use CtrlS to Save Copy".into(),
                        Err(e) => format!("Invalid draft: {e:#}"),
                    });
                }
                EditorAction::SaveCopy => {
                    session.name = session
                        .source
                        .path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string()
                        + ".edited.toml";
                    session.name_cursor = session.name.len();
                    session.editor.set_diagnostic("");
                    session.mode = Mode::SaveName;
                    app.start_requested = false;
                }
                EditorAction::Close => {
                    session.mode = Mode::Preview;
                    app.start_requested = false;
                }
                EditorAction::None => {
                    recompute = matches!(
                        key.code,
                        KeyCode::Char(_)
                            | KeyCode::Enter
                            | KeyCode::Tab
                            | KeyCode::Backspace
                            | KeyCode::Delete
                    ) && !key.modifiers.contains(KeyModifiers::CONTROL)
                }
            }
            if recompute {
                session.dirty = session.editor.text() != session.accepted;
            }
            true
        }
        Mode::SaveName => {
            if name_key(session, key) {
                save(app);
            }
            true
        }
        Mode::DiscardConfirm | Mode::QuitConfirm => {
            match key.code {
                KeyCode::Esc => session.mode = Mode::Preview,
                KeyCode::Enter if session.mode == Mode::DiscardConfirm => {
                    app.edit_session = None;
                    app.status = "Draft discarded; accepted preview restored".into();
                    app.start_requested = false;
                }
                KeyCode::Enter => {
                    app.edit_session = None;
                    app.quit_requested = true;
                    app.start_requested = false;
                }
                _ => {}
            }
            true
        }
        Mode::Preview if !plain => false,
        Mode::Preview => match key.code {
            KeyCode::Char('e') => {
                session.mode = Mode::Editor;
                app.start_requested = false;
                true
            }
            KeyCode::Char('d') if session.dirty => {
                session.mode = Mode::DiscardConfirm;
                app.start_requested = false;
                true
            }
            KeyCode::Char('q') if session.dirty => {
                session.mode = Mode::QuitConfirm;
                app.start_requested = false;
                true
            }
            KeyCode::Char('r' | 'p' | 's') if session.dirty => true,
            _ => false,
        },
    }
}

pub(super) fn paste(app: &mut App, text: &str) {
    if !eligible(app) {
        return;
    }
    let Some(session) = &mut app.edit_session else {
        return;
    };
    match session.mode {
        Mode::Editor => {
            session.editor.paste(text);
            session.dirty = session.editor.text() != session.accepted;
        }
        Mode::SaveName => insert_name(session, text),
        _ => {}
    }
}

/// Modal drawing supersedes accepted preview chrome; closed dirty drafts use its own banner.
pub(super) fn render(frame: &mut Frame, app: &App) -> bool {
    let Some(session) = &app.edit_session else {
        return false;
    };
    match session.mode {
        Mode::Preview => false,
        Mode::Editor => {
            session.editor.draw(
                frame,
                frame.area(),
                &session.source.path.display().to_string(),
                session.dirty,
            );
            true
        }
        Mode::SaveName => {
            let name = display_safe(&session.name);
            let text = format!(
                "SAVE COPY {}\nSource: {}\n\nFilename: {}\n{}\nEnter save | Esc return to editor",
                if session.dirty {
                    "UNSAVED"
                } else {
                    "unchanged"
                },
                display_safe(&super::render::projection(
                    &session.source.path.display().to_string(),
                    256
                )),
                name,
                display_safe(session.editor.diagnostic())
            );
            frame.render_widget(Paragraph::new(text), frame.area());
            let x = Line::raw(format!(
                "Filename: {}",
                display_safe(&session.name[..session.name_cursor])
            ))
            .width();
            if frame.area().height > 3 && x < frame.area().width as usize {
                frame.set_cursor_position((frame.area().x + x as u16, frame.area().y + 3));
            }
            true
        }
        Mode::DiscardConfirm | Mode::QuitConfirm => {
            let action = if session.mode == Mode::DiscardConfirm {
                "Discard unsaved draft?"
            } else {
                "Discard unsaved draft and quit?"
            };
            frame.render_widget(
                Paragraph::new(format!("{action}\nEnter confirm | Esc retain draft")),
                frame.area(),
            );
            true
        }
    }
}
