use super::*;
use crate::comparison::{self as suites, ComparisonResult, LoadedComparison, SuiteRunOptions};
use ratatui::{Terminal, backend::TestBackend};
fn screen(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| render(frame, app)).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}
#[allow(dead_code)]
mod fixtures {
    use crate as theseus;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/support/comparison.rs"
    ));
}
#[test]
fn suite_start_is_consumed_once_and_original_preview_matrix_is_unchanged() {
    let t = tempfile::tempdir().unwrap();
    let path = fixtures::recipe_suite(t.path());
    let suite = suites::load(&path).unwrap();
    let mut app = App::comparison_preview(
        suite,
        SuiteRunOptions {
            output_root: t.path().join("output"),
            pull: true,
        },
    );
    for k in ['p', 's', 'r', 'r'] {
        input(&mut app, KeyCode::Char(k).into());
    }
    let work = take_work(&mut app).unwrap().unwrap();
    let Work::Suite(suite, options) = work else {
        panic!("suite must dispatch through suite worker");
    };
    assert_eq!(
        suite.manifest.profiles,
        vec![crate::model::ProfileName::User]
    );
    assert_eq!(
        suite.manifest.scenarios,
        vec![crate::model::ScenarioKind::Clean]
    );
    assert!(options.pull);
    assert!(!app.start_requested);
    assert!(take_work(&mut app).unwrap().is_none());
    app.active = true;
    input(&mut app, KeyCode::Char('r').into());
    assert!(take_work(&mut app).unwrap().is_none());
    assert!(!t.path().join("output").exists());
}
#[test]
fn editing_recipe_retains_dirty_draft_and_blocks_stale_start() {
    let t = tempfile::tempdir().unwrap();
    fixtures::recipe_suite(t.path());
    let recipe = crate::recipe::load(&t.path().join("copy.toml")).unwrap();
    let mut app = App::preview(recipe.clone(), engine::RunOptions::all(&recipe));
    input(&mut app, KeyCode::Char('r').into());
    input(&mut app, KeyCode::Char('e').into());
    assert!(take_work(&mut app).unwrap().is_none());
    input(&mut app, KeyCode::Char('q').into());
    input(&mut app, KeyCode::Char('r').into());
    assert!(!app.quit_requested);
    assert!(!app.start_requested);
    input(&mut app, KeyCode::Esc.into());
    input(&mut app, KeyCode::Char('r').into());
    assert!(take_work(&mut app).unwrap().is_none());
    input(&mut app, KeyCode::Char('d').into());
    input(&mut app, KeyCode::Esc.into());
    assert!(take_work(&mut app).unwrap().is_none());
    input(&mut app, KeyCode::Char('d').into());
    input(&mut app, KeyCode::Enter.into());
    assert!(!app.start_requested);
    input(&mut app, KeyCode::Char('r').into());
    assert!(take_work(&mut app).unwrap().is_some());
}
#[test]
fn unchanged_editor_closes_before_launch_and_cannot_resume_after_launch() {
    let t = tempfile::tempdir().unwrap();
    fixtures::recipe_suite(t.path());
    let recipe = crate::recipe::load(&t.path().join("copy.toml")).unwrap();
    let mut app = App::preview(recipe.clone(), engine::RunOptions::all(&recipe));
    input(&mut app, KeyCode::Char('e').into());
    input(&mut app, KeyCode::Esc.into());
    input(&mut app, KeyCode::Char('r').into());
    assert!(take_work(&mut app).unwrap().is_some());
    input(&mut app, KeyCode::Char('e').into());
    input(&mut app, KeyCode::Char('q').into());
    assert!(app.quit_requested);
}
#[test]
fn recipe_validation_quit_confirmation_and_failed_save_retain_draft() {
    let t = tempfile::tempdir().unwrap();
    fixtures::recipe_suite(t.path());
    let recipe = crate::recipe::load(&t.path().join("copy.toml")).unwrap();
    let mut app = App::preview(recipe.clone(), engine::RunOptions::all(&recipe));
    input(&mut app, KeyCode::Char('e').into());
    preview_edit::paste(&mut app, "\n[");
    input(
        &mut app,
        KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
    );
    assert!(screen(&app).contains("Invalid draft"));
    input(
        &mut app,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
    );
    input(&mut app, KeyCode::Enter.into());
    assert!(screen(&app).contains("Save Copy failed"));
    input(&mut app, KeyCode::Esc.into());
    input(&mut app, KeyCode::Esc.into());
    input(&mut app, KeyCode::Char('q').into());
    assert!(!app.quit_requested);
    input(&mut app, KeyCode::Esc.into());
    assert!(take_work(&mut app).unwrap().is_none());
    input(&mut app, KeyCode::Char('q').into());
    input(&mut app, KeyCode::Enter.into());
    assert!(app.quit_requested);
    assert!(!t.path().join("copy.edited.toml").exists());
}
#[test]
fn recipe_save_copy_adopts_bytes_and_requires_fresh_run() {
    let t = tempfile::tempdir().unwrap();
    fixtures::recipe_suite(t.path());
    let recipe = crate::recipe::load(&t.path().join("copy.toml")).unwrap();
    let old = std::fs::read(&recipe.path).unwrap();
    let mut app = App::preview(recipe.clone(), engine::RunOptions::all(&recipe));
    input(&mut app, KeyCode::Char('r').into());
    input(&mut app, KeyCode::Char('e').into());
    preview_edit::paste(&mut app, "\n# café\n");
    input(
        &mut app,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
    );
    input(&mut app, KeyCode::Enter.into());
    let saved = t.path().join("copy.edited.toml").canonicalize().unwrap();
    assert_eq!(
        std::fs::read(&saved).unwrap(),
        [old.clone(), b"\n# caf\xc3\xa9\n".to_vec()].concat()
    );
    assert_eq!(std::fs::read(&recipe.path).unwrap(), old);
    assert_eq!(app.recipe.as_ref().unwrap().path, saved);
    assert!(!app.start_requested);
    assert!(take_work(&mut app).unwrap().is_none());
    input(&mut app, KeyCode::Char('r').into());
    let Some(Work::Run(adopted, _)) = take_work(&mut app).unwrap() else {
        panic!("saved recipe must launch")
    };
    assert_eq!(adopted.path, saved);
}
#[test]
fn save_copy_falls_back_to_newly_declared_recipe_selection() {
    let t = tempfile::tempdir().unwrap();
    fixtures::recipe_suite(t.path());
    let recipe = crate::recipe::load(&t.path().join("copy.toml")).unwrap();
    let mut options = engine::RunOptions::all(&recipe);
    options.profiles = vec![crate::model::ProfileName::System];
    options.scenarios = vec![crate::model::ScenarioKind::Upgrade];
    options.pull = true;
    let mut app = App::preview(recipe, options);
    input(&mut app, KeyCode::Char('e').into());
    input(
        &mut app,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
    );
    input(&mut app, KeyCode::Enter.into());
    let selected = app.options.as_ref().unwrap();
    assert_eq!(selected.profiles, vec![crate::model::ProfileName::User]);
    assert_eq!(selected.scenarios, vec![crate::model::ScenarioKind::Clean]);
    assert!(selected.pull);
    assert!(!app.start_requested);
}
#[test]
fn suite_save_copy_adopts_manifest_and_paste_outside_editor_is_inert() {
    let t = tempfile::tempdir().unwrap();
    let path = fixtures::recipe_suite(t.path());
    let mut app = App::comparison_preview(
        suites::load(&path).unwrap(),
        SuiteRunOptions {
            output_root: t.path().join("out"),
            pull: true,
        },
    );
    preview_edit::paste(&mut app, "rq\n");
    assert!(!app.start_requested && !app.quit_requested);
    input(&mut app, KeyCode::Char('e').into());
    preview_edit::paste(&mut app, "\n# suite edit\n");
    input(&mut app, KeyCode::Esc.into());
    input(&mut app, KeyCode::Char('r').into());
    assert!(take_work(&mut app).unwrap().is_none());
    input(&mut app, KeyCode::Char('e').into());
    input(
        &mut app,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
    );
    input(&mut app, KeyCode::Enter.into());
    let saved = t
        .path()
        .join("comparison.edited.toml")
        .canonicalize()
        .unwrap();
    assert!(saved.exists());
    assert_eq!(
        app.comparison
            .as_ref()
            .unwrap()
            .suite
            .as_ref()
            .unwrap()
            .path,
        saved
    );
    assert!(!app.start_requested);
    input(&mut app, KeyCode::Char('r').into());
    let Some(Work::Suite(suite, options)) = take_work(&mut app).unwrap() else {
        panic!("saved suite must launch")
    };
    assert_eq!(suite.path, saved);
    assert!(options.pull);
}
#[test]
fn save_copy_name_collision_and_invalid_name_keep_editor_and_original() {
    let t = tempfile::tempdir().unwrap();
    fixtures::recipe_suite(t.path());
    let recipe = crate::recipe::load(&t.path().join("copy.toml")).unwrap();
    let mut app = App::preview(recipe.clone(), engine::RunOptions::all(&recipe));
    input(&mut app, KeyCode::Char('e').into());
    preview_edit::paste(&mut app, "\n# retained\n");
    input(
        &mut app,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
    );
    for _ in 0..128 {
        input(&mut app, KeyCode::Backspace.into());
    }
    preview_edit::paste(&mut app, "copy.toml");
    input(&mut app, KeyCode::Enter.into());
    assert!(screen(&app).contains("Save Copy failed"));
    assert_eq!(app.recipe.as_ref().unwrap().path, recipe.path);
    for _ in 0..128 {
        input(&mut app, KeyCode::Backspace.into());
    }
    preview_edit::paste(&mut app, "../escape.toml");
    input(&mut app, KeyCode::Enter.into());
    assert!(screen(&app).contains("Save Copy failed"));
    assert!(!t.path().join("escape.toml").exists());
    for _ in 0..128 {
        input(&mut app, KeyCode::Backspace.into());
    }
    preview_edit::paste(&mut app, "copy.edited.toml");
    std::fs::write(t.path().join("copy.edited.toml"), b"existing").unwrap();
    input(&mut app, KeyCode::Enter.into());
    assert!(screen(&app).contains("Save Copy failed"));
    assert_eq!(
        std::fs::read(t.path().join("copy.edited.toml")).unwrap(),
        b"existing"
    );
    input(&mut app, KeyCode::Esc.into());
    assert!(screen(&app).contains("UNSAVED"));
}
#[test]
fn failed_save_copy_names_attempted_leaf_even_under_long_source_path() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("a".repeat(140)).join("b".repeat(140));
    std::fs::create_dir_all(&root).unwrap();
    fixtures::recipe_suite(&root);
    let recipe = crate::recipe::load(&root.join("copy.toml")).unwrap();
    let mut app = App::preview(recipe.clone(), engine::RunOptions::all(&recipe));
    std::fs::write(root.join("copy.edited.toml"), b"existing").unwrap();
    input(&mut app, KeyCode::Char('e').into());
    input(
        &mut app,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
    );
    input(&mut app, KeyCode::Enter.into());
    assert!(screen(&app).contains("Save Copy failed: copy.edited.toml"));
    assert_eq!(app.recipe.as_ref().unwrap().path, recipe.path);
    assert_eq!(
        std::fs::read(root.join("copy.edited.toml")).unwrap(),
        b"existing"
    );
}
#[test]
fn dirty_preview_labels_accepted_document_and_offline_preview_cannot_edit() {
    let t = tempfile::tempdir().unwrap();
    fixtures::recipe_suite(t.path());
    let recipe = crate::recipe::load(&t.path().join("copy.toml")).unwrap();
    let mut app = App::preview(recipe.clone(), engine::RunOptions::all(&recipe));
    input(&mut app, KeyCode::Char('e').into());
    preview_edit::paste(&mut app, "\n# draft\n");
    input(&mut app, KeyCode::Esc.into());
    let text = screen(&app);
    assert!(text.contains("UNSAVED draft"));
    assert!(text.contains("ACCEPTED recipe preview"));
    let mut offline = App::offline(
        crate::model::RunReport::new("offline"),
        t.path().to_path_buf(),
    );
    input(&mut offline, KeyCode::Char('e').into());
    assert!(offline.edit_session.is_none());
    app.set_report(crate::model::RunReport::new("done"), t.path().to_path_buf());
    input(&mut app, KeyCode::Char('e').into());
    assert!(app.edit_session.is_none());
}
#[test]
fn filter_keeps_literal_e_and_ctrl_c_bypasses_editor_modal() {
    let t = tempfile::tempdir().unwrap();
    fixtures::recipe_suite(t.path());
    let recipe = crate::recipe::load(&t.path().join("copy.toml")).unwrap();
    let mut app = App::preview(recipe.clone(), engine::RunOptions::all(&recipe));
    input(&mut app, KeyCode::Char('/').into());
    input(&mut app, KeyCode::Char('e').into());
    assert_eq!(app.filter_draft, "e");
    assert!(app.edit_session.is_none());
    input(&mut app, KeyCode::Esc.into());
    input(&mut app, KeyCode::Char('e').into());
    let terminate = CancellationToken::default();
    let mut presentation = Presentation::new(false, Duration::ZERO);
    handle_key(
        &mut app,
        &mut presentation,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        &CancellationToken::default(),
        &terminate,
        &mut None,
        Instant::now(),
    );
    assert!(terminate.is_cancelled());
}
#[test]
fn invalid_run_selection_does_not_consume_the_single_start() {
    let t = tempfile::tempdir().unwrap();
    fixtures::recipe_suite(t.path());
    let recipe = crate::recipe::load(&t.path().join("copy.toml")).unwrap();
    let mut options = engine::RunOptions::all(&recipe);
    options.profiles = vec![crate::model::ProfileName::System];
    let mut app = App::preview(recipe, options);
    app.start_requested = true;
    assert!(take_work(&mut app).is_err());
    app.options.as_mut().unwrap().profiles = vec![crate::model::ProfileName::User];
    app.start_requested = true;
    assert!(take_work(&mut app).unwrap().is_some());
    app.start_requested = true;
    assert!(take_work(&mut app).unwrap().is_none());
}
#[test]
fn suite_without_first_saved_report_cannot_launch_a_second_engine() {
    let t = tempfile::tempdir().unwrap();
    let path = fixtures::recipe_suite(t.path());
    let mut app = App::comparison_preview(
        suites::load(&path).unwrap(),
        SuiteRunOptions {
            output_root: t.path().join("output"),
            pull: false,
        },
    );
    app.start_requested = true;
    assert!(take_work(&mut app).unwrap().is_some());
    app.active = true;
    assert!(complete_work(&mut app, Err(anyhow!("first save failed"))).is_err());
    app.start_requested = true;
    assert!(
        take_work(&mut app).unwrap().is_none(),
        "an App must consume its engine start once even without a saved report"
    );
}
#[test]
fn suite_ready_completion_retains_cancel_deadline_and_persisted_failed_outcome() {
    let t = tempfile::tempdir().unwrap();
    let root = fixtures::saved(t.path());
    let document = suites::load_document(&root).unwrap();
    let report = document.report.clone();
    let mut app = App::comparison_offline(document, root.clone());
    app.active = true;
    let cancel = CancellationToken::default();
    let now = Instant::now();
    let mut deadline = None;
    let mut presentation = Presentation::new(true, Duration::ZERO);
    presentation.frame(Duration::from_secs(6), 100, 30);
    handle_key(
        &mut app,
        &mut presentation,
        KeyCode::Char('q').into(),
        &cancel,
        &CancellationToken::default(),
        &mut deadline,
        now,
    );
    assert!(cancel.is_cancelled());
    assert_eq!(deadline, Some(now + Duration::from_secs(5)));
    complete_work(
        &mut app,
        Ok(WorkResult::Suite(ComparisonResult {
            report,
            report_path: root.join("comparison.json"),
        })),
    )
    .unwrap();
    assert_eq!(
        quit_result(&app, &mut presentation, Duration::from_secs(7))
            .unwrap()
            .unwrap(),
        1
    );
}
#[test]
fn suite_error_quit_prioritizes_diagnostic_and_suite_recovery_over_cached_outcome() {
    let t = tempfile::tempdir().unwrap();
    let parent = t.path().join("space ' $(touch INJECTED) ;");
    std::fs::create_dir(&parent).unwrap();
    let root = fixtures::saved(&parent);
    let mut app = App::comparison_offline(suites::load_document(&root).unwrap(), root.clone());
    app.active = true;
    complete_work(&mut app, Err(anyhow!("suite final save failed"))).unwrap();
    input(&mut app, KeyCode::Char('q').into());
    let mut p = Presentation::new(false, Duration::ZERO);
    let error = quit_result(&app, &mut p, Duration::ZERO)
        .unwrap()
        .unwrap_err()
        .to_string();
    assert!(error.contains("suite final save failed"));
    assert!(error.contains(&root.join("comparison.json").display().to_string()));
    let command = error
        .lines()
        .find_map(|line| line.strip_prefix("Recovery: "))
        .unwrap();
    let bin = t.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    #[cfg(unix)]
    let out = {
        let receiver = bin.join("theseus");
        std::fs::write(
            &receiver,
            "#!/bin/sh\nprintf '%s\\n' \"$#\" \"$1\" \"$2\"\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&receiver, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::process::Command::new("/bin/sh")
            .args(["-c", command])
            .env("PATH", &bin)
            .current_dir(t.path())
            .output()
            .unwrap()
    };
    #[cfg(windows)]
    let out = {
        let receiver = bin.join("theseus.exe");
        std::fs::copy(
            std::env::var_os("THESEUS_HOST_TEST_FIXTURE")
                .expect("build trusted host fixture and set THESEUS_HOST_TEST_FIXTURE"),
            &receiver,
        )
        .unwrap();
        let powershell = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        std::process::Command::new(powershell)
            .args(["-NoProfile", "-NonInteractive", "-Command", command])
            .env("PATH", &bin)
            .current_dir(t.path())
            .output()
            .unwrap()
    };
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap().replace("\r\n", "\n"),
        format!("2\ncleanup\n{}\n", root.display())
    );
    assert!(!t.path().join("INJECTED").exists());
    assert_eq!(app.exit_code(), 2);
    assert!(unresolved(&app).contains("source journals"));
    let document = LoadedComparison {
        manifest: fixtures::manifest(),
        report: app.comparison.as_ref().unwrap().report.clone().unwrap(),
    };
    assert_eq!(document.report.outcome, crate::model::RunOutcome::Failed);
}
#[test]
fn suite_startup_paste_cannot_start_and_live_event_retains_validated_recovery_context() {
    let t = tempfile::tempdir().unwrap();
    let path = fixtures::recipe_suite(t.path());
    let mut app = App::comparison_preview(
        suites::load(&path).unwrap(),
        SuiteRunOptions {
            output_root: t.path().join("output"),
            pull: false,
        },
    );
    let mut presentation = Presentation::new(true, Duration::ZERO);
    let cancel = CancellationToken::default();
    let terminate = CancellationToken::default();
    let mut deadline = None;
    for k in ['r', 'p', 's', 'r'] {
        handle_key(
            &mut app,
            &mut presentation,
            KeyCode::Char(k).into(),
            &cancel,
            &terminate,
            &mut deadline,
            Instant::now(),
        );
    }
    assert!(!app.start_requested);
    assert!(take_work(&mut app).unwrap().is_none());
    assert!(!t.path().join("output").exists());
    app.start_requested = true;
    assert!(take_work(&mut app).unwrap().is_some());
    app.active = true;
    let root = fixtures::saved(t.path());
    let mut last = None;
    apply_event(
        &mut app,
        WorkEvent::Suite(suites::ComparisonEvent {
            report_path: root.join("comparison.json"),
            variant_id: Some("archive".into()),
            source: None,
        }),
        &mut last,
    );
    assert_eq!(last, Some(root.join("comparison.json")));
    assert_eq!(app.root, root);
    assert!(app.comparison.as_ref().unwrap().report.is_none());
    assert_eq!(app.outcome(), crate::model::RunOutcome::Incomplete);
    assert_eq!(app.comparison.as_ref().unwrap().manifest.baseline, "copy");
    input(&mut app, KeyCode::Char('r').into());
    assert!(take_work(&mut app).unwrap().is_none());
}
