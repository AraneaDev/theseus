#[path = "support/native.rs"]
mod native;
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
// Independent expected host-shell quoting; command execution below also checks
// that apostrophes, spaces and metacharacters survive as a single native argument.
fn shell_path(path: &std::path::Path) -> String {
    let text = path.to_string_lossy();
    if !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/_-.".contains(&b))
    {
        return text.into_owned();
    }
    #[cfg(unix)]
    let escaped = text.replace('\'', "'\"'\"'");
    #[cfg(windows)]
    let escaped = text.replace('\'', "''");
    format!("'{escaped}'")
}

fn source_report_path(root: &std::path::Path, variant: &str) -> std::path::PathBuf {
    // SourceReference paths are portable recorded keys, appended unchanged by the display.
    std::path::PathBuf::from(format!(
        "{}/sources/{variant}/run-{variant}/report.json",
        root.display()
    ))
}

// These two overview assertions require complete native commands in the 60%
// records pane. Narrow/detail reachability tests keep their fixed dimensions.
fn overview_width(root: &std::path::Path) -> u16 {
    let quoted_root = shell_path(root);
    let quoted_report = shell_path(&source_report_path(root, "archive"));
    let records = [
        format!("Source logs/commands: theseus report {quoted_report}"),
        format!("Recovery: theseus cleanup {quoted_root}"),
        format!("Complete evidence: theseus comparison {quoted_root} --json"),
    ];
    let cells = records
        .iter()
        .map(|s| ratatui::text::Line::from(s.as_str()).width())
        .max()
        .unwrap();
    let width = cells
        .checked_add(8)
        .and_then(|n| n.checked_mul(5))
        .expect("overview width overflow")
        .div_ceil(3)
        .max(180);
    assert!(
        width <= 4096,
        "native overview fixture exceeds bounded display: {width}"
    );
    u16::try_from(width).unwrap()
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
        "Pull:      true",
        "baseline",
        "Will run:",
        "no commands executed",
    ] {
        assert!(text.contains(s), "missing {s}: {text}");
    }
    key(&mut app, KeyCode::Char('j'));
    assert_eq!(app.scroll, 1);
    key(&mut app, KeyCode::Char('k'));
    assert_eq!(app.scroll, 0);
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
    let text = screen(&app, overview_width(&root), 48);
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
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::End);
    let text = screen(&app, 180, 48);
    for expected in [
        "bin/tool",
        "Different",
        "baseline-sha",
        "candidate-sha",
        "uid: 10001",
        "before:",
        "after:",
    ] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
    key(&mut app, KeyCode::PageDown);
    assert_eq!(app.scroll, 10);
    key(&mut app, KeyCode::PageUp);
    assert_eq!(app.scroll, 0);
}
#[test]
fn large_difference_display_keeps_all_differences_and_full_source_fields_reachable() {
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
    let text = screen(&app, overview_width(&root), 70);
    for expected in [
        format!(
            "theseus report {}",
            shell_path(&source_report_path(&root, "copy"))
        ),
        format!("theseus cleanup {}", shell_path(&root)),
        format!("theseus comparison {} --json", shell_path(&root)),
    ] {
        assert!(text.contains(&expected), "missing {expected}");
    }
    assert!(!text.contains("tail-that-must-be-elided"));
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::End);
    assert!(screen(&app, 180, 70).contains("item-100"));
    key(&mut app, KeyCode::Char('/'));
    for c in "tail-that-must-be-elided".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    key(&mut app, KeyCode::Enter);
    assert!(screen(&app, 180, 70).contains("tail-that-must-be-elided"));
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

#[test]
fn displayed_commands_preserve_spaced_quoted_shell_paths() {
    use std::fs;
    let t = tempfile::tempdir().unwrap();
    let parent = t.path().join("space ' $(touch INJECTED) ;");
    fs::create_dir(&parent).unwrap();
    let root = support::saved(&parent);
    let app = App::comparison_offline(comparison::load_document(&root).unwrap(), root.clone());
    let text = screen(&app, 600, 70);
    let bin = t.path().join("bin");
    fs::create_dir(&bin).unwrap();
    fs::copy(
        env!("CARGO_BIN_EXE_theseus"),
        bin.join(format!("theseus{}", std::env::consts::EXE_SUFFIX)),
    )
    .unwrap();
    for (prefix, suffix, expected) in [
        (
            "Source logs/commands: ",
            " --json",
            format!(
                "theseus report {}",
                shell_path(&source_report_path(&root, "copy"))
            ),
        ),
        (
            "Recovery: ",
            "",
            format!("theseus cleanup {}", shell_path(&root)),
        ),
        (
            "Complete evidence: ",
            "",
            format!("theseus comparison {} --json", shell_path(&root)),
        ),
    ] {
        let command = text
            .split(prefix)
            .nth(1)
            .unwrap()
            .split("  ")
            .next()
            .unwrap()
            .trim();
        assert_eq!(
            command, expected,
            "complete native command must be reachable"
        );
        let out = native::shell(&format!("{command}{suffix}"))
            .env(
                "PATH",
                std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(
                    &std::env::var_os("PATH").unwrap_or_default(),
                )))
                .unwrap(),
            )
            .current_dir(t.path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{command}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        if !suffix.is_empty() || prefix == "Complete evidence: " {
            let _: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        }
        assert!(!t.path().join("INJECTED").exists());
    }
}
