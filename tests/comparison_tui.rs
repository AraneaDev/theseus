#[allow(dead_code)]
#[path = "support/comparison.rs"]
mod support;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{Terminal, backend::TestBackend};
use theseus::{
    comparison::{self, *},
    tui::{App, input, render},
};
fn key(app: &mut App, k: KeyCode) {
    input(app, KeyEvent::from(k));
}
fn screen(app: &App, width: u16, height: u16) -> String {
    let mut t = Terminal::new(TestBackend::new(width, height)).unwrap();
    t.draw(|f| render(f, app)).unwrap();
    t.backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect()
}
#[test]
fn preview_shows_explicit_manifest_and_quit_does_not_run_or_edit_it() {
    let t = tempfile::tempdir().unwrap();
    let path = support::recipe_suite(t.path());
    let suite = comparison::load(&path).unwrap();
    let mut app = App::comparison_preview(
        suite,
        SuiteRunOptions {
            output_root: t.path().join("output"),
            pull: true,
        },
    );
    let text = screen(&app, 140, 42);
    for s in [
        "methods",
        "Baseline: copy",
        "archive",
        "/app",
        "configured",
        "missing_stage",
        "pull: true",
        "no commands executed",
    ] {
        assert!(text.contains(s), "missing {s}: {text}");
    }
    key(&mut app, KeyCode::Char('p'));
    key(&mut app, KeyCode::Char('s'));
    key(&mut app, KeyCode::Char('q'));
    assert!(app.quit_requested);
    assert!(!app.start_requested);
    assert!(!t.path().join("output").exists());
    assert_eq!(app.exit_code(), 0);
}
#[test]
fn offline_context_rows_unknown_and_source_recovery_are_visible_without_recipes() {
    let t = tempfile::tempdir().unwrap();
    let root = support::saved(t.path());
    let mut app = App::comparison_offline(
        comparison::load_document(&root.join("comparison.json")).unwrap(),
        root.join("comparison.json"),
    );
    assert_eq!(app.exit_code(), 1);
    let text = screen(&app, 180, 48);
    for s in [
        "Baseline: copy",
        "/app",
        "Equivalent",
        "sha256:archive",
        "archive: failed",
        "theseus report",
        "sources/archive/run-archive/report.json",
    ] {
        assert!(text.contains(s), "missing {s}: {text}");
    }
    support::add_unknown_stage(&root);
    app = App::comparison_offline(comparison::load_document(&root).unwrap(), root.clone());
    key(&mut app, KeyCode::Down);
    let text = screen(&app, 90, 34);
    assert!(text.contains("Unknown"));
    assert!(text.contains("missing stage missing_stage"));
    key(&mut app, KeyCode::Char('r'));
    assert!(!app.start_requested);
}
#[test]
fn hostile_terminal_text_is_escaped_in_suite_and_selected_evidence() {
    let t = tempfile::tempdir().unwrap();
    let root = support::saved(t.path());
    let mut doc = comparison::load_document(&root).unwrap();
    doc.manifest.name = "hostile\x1b[2J".into();
    doc.report.cells[0]
        .comparison
        .diagnostics
        .push("reason\x1b]52;copy\x07".into());
    let app = App::comparison_offline(doc, root);
    let text = screen(&app, 140, 42);
    assert!(!text.contains('\x1b'));
    assert!(!text.contains('\x07'));
    assert!(text.contains("reason�"));
}

#[test]
fn selected_difference_displays_relative_key_and_both_recorded_metadata_sides() {
    use theseus::{
        model::FileKind,
        snapshot::{ChangeKind, FileChange, FileEntry},
    };
    let t = tempfile::tempdir().unwrap();
    let root = support::saved(t.path());
    let mut document = comparison::load_document(&root).unwrap();
    let before = FileEntry {
        kind: FileKind::File,
        mode: 0o644,
        uid: 10001,
        gid: 10001,
        size: 7,
        sha256: Some("baseline-sha".into()),
        link_target: None,
    };
    let mut after = before.clone();
    after.sha256 = Some("candidate-sha".into());
    after.mode = 0o755;
    document.report.cells[0].comparison.status = ComparisonStatus::Different;
    document.report.cells[0]
        .comparison
        .differences
        .push(FileChange {
            path: "bin/tool".into(),
            kind: ChangeKind::Modified,
            before: Some(before),
            after: Some(after),
        });
    let mut app = App::comparison_offline(document, root);
    let text = screen(&app, 180, 48);
    for expected in [
        "bin/tool",
        "Different",
        "baseline-sha",
        "candidate-sha",
        "uid: 10001",
        "Before:",
        "After:",
    ] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
    key(&mut app, KeyCode::PageDown);
    assert_eq!(app.scroll, 10);
    key(&mut app, KeyCode::PageUp);
    assert_eq!(app.scroll, 0);
}
#[test]
fn large_difference_display_keeps_source_paths_reachable_and_labels_ui_elision() {
    use theseus::snapshot::{ChangeKind, FileChange};
    let t = tempfile::tempdir().unwrap();
    let root = support::saved(t.path());
    let mut document = comparison::load_document(&root).unwrap();
    let cell = &mut document.report.cells[0];
    cell.comparison.status = ComparisonStatus::Different;
    cell.comparison.differences = (0..101)
        .map(|i| FileChange {
            path: format!("item-{i:03}"),
            kind: ChangeKind::Added,
            before: None,
            after: None,
        })
        .collect();
    document.report.sources[0].error =
        Some(format!("{}tail-that-must-be-elided", "x".repeat(1000)));
    let mut app = App::comparison_offline(document, root.clone());
    let text = screen(&app, 180, 70);
    for expected in [
        "sources/copy/run-copy/report.json",
        "theseus cleanup",
        "showing 100 of 101",
        "UI prefix limit",
        "elided;",
        &format!("theseus comparison {} --json", root.display()),
    ] {
        assert!(text.contains(expected), "missing {expected}");
    }
    assert!(!text.contains("tail-that-must-be-elided"));
    key(&mut app, KeyCode::PageDown);
    let text = screen(&app, 180, 400);
    assert!(!text.contains("item-100"));
    assert_eq!(
        app.comparison
            .as_ref()
            .unwrap()
            .report
            .as_ref()
            .unwrap()
            .cells[0]
            .comparison
            .differences
            .len(),
        101,
        "presentation must not truncate evidence"
    );
}
