use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use std::{io::Write, path::Path};
use theseus::{
    model::*,
    tui::{App, Focus, Tab, input, render},
};
fn key(app: &mut App, code: KeyCode) {
    input(app, KeyEvent::new(code, KeyModifiers::NONE));
}
fn fixture(root: &Path) -> App {
    let mut report = RunReport::new("literal-failed");
    let mut scenario =
        ScenarioReport::new("user-upgrade", ProfileName::User, ScenarioKind::Upgrade);
    let mut first = StageReport::new("install_base");
    first.outcome = StageOutcome::Passed;
    first.errors.push("base-only evidence".into());
    let mut second = StageReport::new("target_installed");
    second.outcome = StageOutcome::Incomplete;
    second.checks.push(CheckResult {
        id: "target-path".into(),
        checkpoint: Checkpoint::TargetInstalled,
        outcome: CheckOutcome::Failed,
        message: "old executable wins on PATH".into(),
        path: None,
        command_ids: vec![],
    });
    second.errors.push("observation could not finish".into());
    let mut command = CommandRecord::new("cmd1", "version");
    command.stdout = Some(LogReference {
        path: "logs/out".into(),
        bytes: 100_000,
        truncated: true,
    });
    second.commands.push(command);
    scenario.stages = vec![first, second];
    scenario.errors.push("scenario infrastructure error".into());
    scenario.findings.push(Finding {
        kind: FindingKind::Leftover,
        path: Some("/bin/stale".into()),
        message: "unclaimed leftover".into(),
    });
    report.errors.push("run infrastructure error".into());
    report.scenarios.push(scenario);
    App::offline(report, root.to_path_buf())
}
fn screen(app: &App, w: u16, h: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| render(f, app)).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect()
}
#[test]
fn stage_selection_changes_real_evidence_and_narrow_layout_retains_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fixture(dir.path());
    app.focus = Focus::Stage;
    assert!(screen(&app, 120, 36).contains("base-only evidence"));
    key(&mut app, KeyCode::Down);
    let wide = screen(&app, 120, 36);
    assert!(wide.contains("old executable wins on PATH"));
    assert!(!wide.contains("base-only evidence"));
    assert!(screen(&app, 52, 24).contains("old executable wins on PATH"));
    assert_eq!(app.stage_index, 1);
}
#[test]
fn findings_filter_and_focus_navigation_are_usable() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fixture(dir.path());
    app.stage_index = 1;
    app.focus = Focus::Evidence;
    app.tab = Tab::Findings;
    let text = screen(&app, 120, 40);
    for expected in [
        "old executable wins on PATH",
        "observation could not finish",
        "unclaimed leftover",
        "scenario infrastructure error",
        "run infrastructure error",
    ] {
        assert!(text.contains(expected), "missing {expected}");
    }
    key(&mut app, KeyCode::Char('/'));
    for c in "unclaimed".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    key(&mut app, KeyCode::Enter);
    let filtered = screen(&app, 120, 40);
    assert!(filtered.contains("unclaimed leftover"));
    assert!(!filtered.contains("old executable wins on PATH"));
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.focus, Focus::Stage);
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.focus, Focus::Evidence);
    key(&mut app, KeyCode::Right);
    assert_eq!(app.tab, Tab::Files);
}
#[test]
fn logs_are_loaded_through_private_bounded_storage_and_controls_are_inert() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("run");
    let store = theseus::report::create(&root).unwrap();
    let mut out = store.create_log("logs/out").unwrap();
    out.write_all(&vec![b'x'; 100_000]).unwrap();
    let mut app = fixture(&root);
    app.stage_index = 1;
    app.tab = Tab::Logs;
    app.refresh_logs();
    assert!(app.log_text.len() < 70_000);
    assert!(app.log_text.contains("truncated"));
    std::fs::remove_file(root.join("logs/out")).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", root.join("logs/out")).unwrap();
    app.refresh_logs();
    assert!(app.log_text.contains("Cannot read"));
    assert!(!app.log_text.contains("root:x:"));
    assert_eq!(
        theseus::tui::safe_text("hello\u{1b}[31m\rworld"),
        "hello�[31m�world"
    );
}
#[test]
fn preview_selection_requires_explicit_start_and_offline_cannot_run() {
    let recipe = theseus::recipe::load(Path::new("examples/standalone/theseus.toml")).unwrap();
    let options = theseus::engine::RunOptions::all(&recipe);
    let mut app = App::preview(recipe, options);
    assert!(!app.start_requested);
    let text = screen(&app, 120, 40);
    assert!(text.contains("Preview"));
    assert!(text.contains("install_target"));
    key(&mut app, KeyCode::Char('p'));
    assert_eq!(
        app.options.as_ref().unwrap().profiles,
        vec![ProfileName::User]
    );
    key(&mut app, KeyCode::Char('p'));
    assert_eq!(
        app.options.as_ref().unwrap().profiles,
        vec![ProfileName::System]
    );
    key(&mut app, KeyCode::Char('p'));
    assert_eq!(
        app.options.as_ref().unwrap().profiles,
        vec![ProfileName::User, ProfileName::System]
    );
    key(&mut app, KeyCode::Char('s'));
    assert_eq!(
        app.options.as_ref().unwrap().scenarios,
        vec![ScenarioKind::Clean]
    );
    key(&mut app, KeyCode::Char('s'));
    assert_eq!(
        app.options.as_ref().unwrap().scenarios,
        vec![ScenarioKind::Upgrade]
    );
    key(&mut app, KeyCode::Char('s'));
    assert_eq!(
        app.options.as_ref().unwrap().scenarios,
        vec![ScenarioKind::Clean, ScenarioKind::Upgrade]
    );
    key(&mut app, KeyCode::Char('r'));
    assert!(app.start_requested);
    let dir = tempfile::tempdir().unwrap();
    let mut offline = fixture(dir.path());
    key(&mut offline, KeyCode::Char('r'));
    assert!(!offline.start_requested);
}
#[test]
fn preflight_report_errors_remain_visible_without_scenarios() {
    let mut report = RunReport::new("preflight");
    report.errors.push("Docker image unavailable".into());
    let app = App::offline(report, std::path::PathBuf::new());
    assert!(screen(&app, 100, 25).contains("Docker image unavailable"));
}
#[test]
fn completed_runs_cannot_accidentally_execute_again() {
    let recipe = theseus::recipe::load(Path::new("examples/standalone/theseus.toml")).unwrap();
    let mut app = App::preview(recipe.clone(), theseus::engine::RunOptions::all(&recipe));
    app.set_report(RunReport::new("done"), std::path::PathBuf::new());
    key(&mut app, KeyCode::Char('r'));
    assert!(!app.start_requested);
}
#[test]
fn actual_pty_startup_exit_and_engine_error_restore_terminal() {
    let output = std::process::Command::new("python3")
        .arg("tests/support/tui_pty.py")
        .arg(env!("CARGO_BIN_EXE_theseus"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
fn files_and_commands_show_literal_metadata_for_the_selected_stage() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fixture(dir.path());
    let stage = &mut app.report.as_mut().unwrap().scenarios[0].stages[1];
    let entry = theseus::snapshot::FileEntry {
        kind: FileKind::File,
        mode: 0o755,
        uid: 10001,
        gid: 10001,
        size: 7,
        sha256: Some("literal-sha".into()),
        link_target: None,
    };
    stage.stage_diff.push(theseus::snapshot::FileChange {
        path: "/bin/upgraded".into(),
        kind: theseus::snapshot::ChangeKind::Modified,
        before: None,
        after: Some(entry),
    });
    stage.commands[0].argv = vec!["sample-cli".into(), "--version".into()];
    app.stage_index = 1;
    app.tab = Tab::Files;
    let text = screen(&app, 120, 40);
    assert!(text.contains("/bin/upgraded"));
    assert!(text.contains("literal-sha"));
    assert!(text.contains("uid: 10001"));
    app.tab = Tab::Commands;
    let text = screen(&app, 120, 40);
    assert!(text.contains("sample-cli"));
    assert!(text.contains("--version"));
    assert!(text.contains("10001:10001"));
    app.focus = Focus::Stage;
    key(&mut app, KeyCode::Up);
    assert!(!screen(&app, 120, 40).contains("sample-cli"));
}
#[test]
fn controls_cancel_active_runs_and_scroll_without_changing_selection() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fixture(dir.path());
    app.stage_index = 1;
    key(&mut app, KeyCode::PageDown);
    assert_eq!(app.scroll, 10);
    assert_eq!(app.stage_index, 1);
    app.active = true;
    key(&mut app, KeyCode::Char('c'));
    assert!(app.cancel_requested);
    assert!(!app.quit_requested);
    key(&mut app, KeyCode::Char('q'));
    assert!(app.quit_requested);
    assert!(app.cancel_requested);
}
#[test]
fn live_report_refresh_loads_newly_available_selected_log() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("run");
    let store = theseus::report::create(&root).unwrap();
    let mut app = fixture(&root);
    app.stage_index = 1;
    app.tab = Tab::Logs;
    app.refresh_logs();
    assert!(app.log_text.contains("Cannot read"));
    store
        .write_log("logs/out", b"new live evidence", 100)
        .unwrap();
    app.set_report(app.report.clone().unwrap(), root);
    assert!(app.log_text.contains("new live evidence"));
}
#[test]
fn narrow_summary_keeps_review_and_incomplete_counts_visible() {
    let dir = tempfile::tempdir().unwrap();
    let app = fixture(dir.path());
    let text = screen(&app, 52, 24);
    assert!(text.contains("review 1"));
    assert!(text.contains("incomplete 0"));
}
#[test]
fn selection_stays_visible_after_word_wrapped_and_wide_records() {
    let mut report = RunReport::new("wrapping-selection");
    let mut scenario = ScenarioReport::new("user-clean", ProfileName::User, ScenarioKind::Clean);
    let mut stage = StageReport::new("baseline");
    for i in 0..40 {
        stage
            .errors
            .push(format!("{i}: {}", vec!["a".repeat(45); 3].join(" ")));
    }
    stage.errors.push("SELECTED-END-EVIDENCE".into());
    scenario.stages.push(stage);
    report.scenarios.push(scenario);
    let mut app = App::offline(report, "/tmp".into());
    app.focus = Focus::Evidence;
    app.evidence_index = 41;
    for (width, height) in [(120, 30), (52, 24)] {
        assert!(
            screen(&app, width, height).contains("SELECTED-END-EVIDENCE"),
            "selection disappeared at {width} columns"
        );
    }
    for error in &mut app.report.as_mut().unwrap().scenarios[0].stages[0].errors[..40] {
        *error = format!(
            "{}\t{}\n{}",
            "界".repeat(45),
            "b ".repeat(45),
            "e\u{301}".repeat(45)
        );
    }
    for (width, height) in [(120, 30), (52, 24)] {
        assert!(
            screen(&app, width, height).contains("SELECTED-END-EVIDENCE"),
            "Unicode selection disappeared at {width} columns"
        );
    }
}
