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
        command_expectation: None,
        id: "target-path".into(),
        checkpoint: Checkpoint::TargetInstalled,
        outcome: CheckOutcome::Failed,
        message: "old executable wins on PATH".into(),
        path: None,
        command_ids: vec![],
        backup: None,
        comparison: None,
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
    app.focus = Focus::Evidence;
    key(&mut app, KeyCode::Down);
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
    stage.commands[0].forwarded_environment = vec!["PATH".into()];
    app.stage_index = 1;
    app.tab = Tab::Files;
    let text = screen(&app, 120, 40);
    assert!(text.contains("/bin/upgraded"));
    assert!(text.contains("literal-sha"));
    assert!(text.contains("uid: 10001"));
    app.tab = Tab::Commands;
    let text = screen(&app, 120, 40);
    assert!(text.contains("sample-cli"));
    assert!(text.contains("forwarded env (values hidden): [\"PATH\"]"));
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

#[test]
fn actual_pty_idle_preview_and_offline_signals_restore_terminal() {
    let output = std::process::Command::new("python3")
        .arg("tests/support/tui_signals.py")
        .arg(env!("CARGO_BIN_EXE_theseus"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    println!("{}", String::from_utf8_lossy(&output.stdout));
}

#[test]
fn clean_only_preview_s_key_never_offers_undeclared_upgrade() {
    let recipe = theseus::recipe::load(Path::new("tests/fixtures/recipes/v2_clean.toml")).unwrap();
    let options = theseus::engine::RunOptions::all(&recipe);
    let mut app = App::preview(recipe, options);
    for _ in 0..4 {
        key(&mut app, KeyCode::Char('s'));
        assert_eq!(
            app.options.as_ref().unwrap().scenarios,
            vec![ScenarioKind::Clean]
        );
        assert!(!screen(&app, 120, 40).contains("upgrade"));
    }
}

#[test]
fn version_two_preview_cycles_in_declaration_order() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("base")).unwrap();
    std::fs::create_dir(temp.path().join("target")).unwrap();
    let path = temp.path().join("theseus.toml");
    std::fs::write(
        &path,
        include_str!("fixtures/recipes/valid.toml").replace(
            "schema_version = 1",
            "schema_version = 2\nscenarios = [\"upgrade\", \"clean\"]",
        ),
    )
    .unwrap();
    let recipe = theseus::recipe::load(&path).unwrap();
    let options = theseus::engine::RunOptions::all(&recipe);
    let mut app = App::preview(recipe, options);
    assert_eq!(
        app.options.as_ref().unwrap().scenarios,
        vec![ScenarioKind::Upgrade, ScenarioKind::Clean]
    );
    for expected in [
        vec![ScenarioKind::Upgrade],
        vec![ScenarioKind::Clean],
        vec![ScenarioKind::Upgrade, ScenarioKind::Clean],
    ] {
        key(&mut app, KeyCode::Char('s'));
        assert_eq!(app.options.as_ref().unwrap().scenarios, expected);
    }
}

#[test]
fn reinstall_preview_explains_repeat_exercise_and_stable_scopes_and_cycles_declarations() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("target")).unwrap();
    let path = temp.path().join("theseus.toml");
    let text = include_str!("fixtures/recipes/v2_reinstall.toml");
    for (declaration, choices) in [
        ("[\"reinstall\"]", vec![vec![ScenarioKind::Reinstall]]),
        (
            "[\"reinstall\", \"clean\", \"upgrade\"]",
            vec![
                vec![
                    ScenarioKind::Reinstall,
                    ScenarioKind::Clean,
                    ScenarioKind::Upgrade,
                ],
                vec![ScenarioKind::Reinstall],
                vec![ScenarioKind::Clean],
                vec![ScenarioKind::Upgrade],
            ],
        ),
    ] {
        std::fs::write(&path, text.replace("[\"reinstall\"]", declaration)).unwrap();
        let recipe = theseus::recipe::load(&path).unwrap();
        let mut app = App::preview(recipe.clone(), theseus::engine::RunOptions::all(&recipe));
        let preview = app.preview_text();
        for expected in [
            "exercise_before_reinstall",
            "exercise_after_reinstall",
            "before_reinstall",
            "Declared stable scopes",
            "/home/theseus/config",
            "sh /inputs/target/install.sh user",
        ] {
            assert!(
                preview.contains(expected),
                "preview missing {expected}: {preview}"
            );
        }
        assert!(!app.start_requested);
        for choice in choices.iter().cycle().take(9) {
            assert_eq!(&app.options.as_ref().unwrap().scenarios, choice);
            key(&mut app, KeyCode::Char('s'));
        }
        key(&mut app, KeyCode::Char('r'));
        // An all-three selection may require missing upgrade inputs, but preview
        // selection itself never starts an execution implicitly.
        if declaration == "[\"reinstall\"]" {
            assert!(app.start_requested);
        }
    }
}
#[test]
fn reinstall_failed_directory_scope_and_comparison_references_are_browsable() {
    let mut report = RunReport::new("reinstall-failed");
    let mut scenario =
        ScenarioReport::new("user-reinstall", ProfileName::User, ScenarioKind::Reinstall);
    let mut stage = StageReport::new("reinstalled");
    stage.outcome = StageOutcome::Failed;
    stage.checks.push(CheckResult {
        command_expectation: None,
        id: "stable-config".into(),
        checkpoint: Checkpoint::Reinstalled,
        outcome: CheckOutcome::Failed,
        message: "declared directory changed".into(),
        path: Some("/home/theseus/config".into()),
        command_ids: vec![],
        backup: None,
        comparison: Some(ComparisonReference::reinstall()),
    });
    scenario.stages.push(stage);
    report.scenarios.push(scenario);
    let mut app = App::offline(report, std::path::PathBuf::new());
    for tab in [Tab::Stages, Tab::Findings, Tab::Files] {
        app.tab = tab;
        let items = app.items().join("\n");
        for expected in ["before_reinstall", "reinstalled", "/home/theseus/config"] {
            assert!(
                items.contains(expected),
                "missing {expected} in {tab:?}: {items}"
            );
        }
    }
    app.tab = Tab::Stages;
    app.focus = Focus::Evidence;
    key(&mut app, KeyCode::Down);
    assert!(screen(&app, 52, 24).contains("declared directory changed"));
}

#[test]
fn interruption_preview_and_offline_proof_remain_explicit_and_narrow() {
    let temp = tempfile::tempdir().unwrap();
    for d in ["base", "target"] {
        std::fs::create_dir(temp.path().join(d)).unwrap();
    }
    let path = temp.path().join("theseus.toml");
    std::fs::write(&path, include_str!("fixtures/recipes/v3_interruption.toml")).unwrap();
    let recipe = theseus::recipe::load(&path).unwrap();
    let options = theseus::engine::RunOptions::all(&recipe);
    let mut app = App::preview(recipe, options);
    let preview = app.preview_text();
    for text in [
        "ContainerSigkill",
        "THESEUS_INTERRUPT_BARRIER",
        "9s",
        "recover-install",
        "before_interrupt",
        "recovered",
    ] {
        assert!(preview.contains(text), "{text}: {preview}");
    }
    assert!(!app.start_requested);
    key(&mut app, KeyCode::Char('s'));
    assert_eq!(
        app.options.as_ref().unwrap().scenarios,
        [ScenarioKind::InterruptedInstall]
    );
    let install_preview = app.preview_text();
    assert!(
        install_preview.contains("before initial installation"),
        "{install_preview}"
    );
    assert!(!install_preview.contains("after first exercise"));
    key(&mut app, KeyCode::Char('s'));
    let upgrade_preview = app.preview_text();
    assert!(upgrade_preview.contains("after first exercise"));
    key(&mut app, KeyCode::Char('r'));
    assert!(app.start_requested);
    let mut report = RunReport::new("proof");
    let mut scenario =
        ScenarioReport::new("x", ProfileName::User, ScenarioKind::InterruptedInstall);
    let mut stage = StageReport::new("interrupt_install");
    let mut command = CommandRecord::new("x", "interrupt_install");
    command.outcome = CommandOutcome::Interrupted;
    command.interruption = Some(InterruptionEvidence {
        mechanism: InterruptionMechanism::ContainerSigkill,
        readiness_verified: true,
        stopped_verified: true,
    });
    stage.commands.push(command);
    scenario.stages.push(stage);
    report.scenarios.push(scenario);
    let mut app = App::offline(report, temp.path().into());
    let items = app.items().join("\n");
    assert!(items.contains("readiness=true"));
    assert!(items.contains("stopped=true"));
    assert!(screen(&app, 52, 24).contains("stopped=true"));
    app.tab = Tab::Commands;
    assert!(app.items().join("\n").contains("ContainerSigkill"));
}

#[test]
fn mixed_schema_three_preview_keeps_stability_scopes_with_their_checkpoint_references() {
    let temp = tempfile::tempdir().unwrap();
    for artifact in ["base", "target"] {
        std::fs::create_dir(temp.path().join(artifact)).unwrap();
    }
    let path = temp.path().join("theseus.toml");
    let interruption = include_str!("fixtures/recipes/v3_interruption.toml").replace(
        "scenarios = [\"interrupted_install\", \"interrupted_upgrade\"]",
        "scenarios = [\"reinstall\", \"interrupted_install\", \"interrupted_upgrade\"]",
    );
    std::fs::write(
        &path,
        format!(
            "{interruption}\n\
             [profiles.user.steps.install_target]\nscript='install-target'\n\
             [profiles.user.steps.reinstall]\nscript='reinstall-target'\n\
             [[profiles.user.checks]]\nid='stable-config'\ncheckpoint='reinstalled'\nkind='unchanged'\npath='/home/theseus/config'\n\
             [[profiles.user.checks]]\nid='stable-partial'\ncheckpoint='interrupted'\nkind='unchanged'\npath='/home/theseus/partial-data'\n"
        ),
    )
    .unwrap();
    let recipe = theseus::recipe::load(&path).unwrap();
    recipe
        .validate_selection(&[ProfileName::User], &recipe.declared_scenarios())
        .unwrap();
    for scenarios in [vec![ScenarioKind::Reinstall], recipe.declared_scenarios()] {
        let includes_interruption = scenarios.len() > 1;
        let options = theseus::engine::RunOptions {
            scenarios,
            ..theseus::engine::RunOptions::all(&recipe)
        };
        let app = App::preview(recipe.clone(), options);
        let preview = app.preview_text();
        let reinstall_scopes = preview
            .lines()
            .find(|line| line.starts_with("Declared stable scopes"))
            .expect("selected reinstall has a stability summary");
        assert_eq!(
            reinstall_scopes,
            "Declared stable scopes (path and descendants): [\"/home/theseus/config\"]",
            "reinstall summary must exclude interrupted/recovered scopes: {preview}"
        );
        assert!(preview.contains("Compare before_reinstall (after first exercise) to reinstalled (before checks and second exercise)."));
        assert_eq!(
            preview.contains(
                "Unchanged compares before_interrupt -> interrupted/recovered (before checks)."
            ),
            includes_interruption
        );
        for (id, checkpoint, scope) in [
            ("stable-config", "reinstalled", "/home/theseus/config"),
            (
                "stable-partial",
                "interrupted",
                "/home/theseus/partial-data",
            ),
            ("data", "recovered", "/home/theseus/data"),
        ] {
            assert!(
                preview.contains(&format!(
                    "check {id} @ {checkpoint}: Unchanged {{ path: \"{scope}\" }}"
                )),
                "declared contract must retain its checkpoint and scope: {preview}"
            );
        }
    }
}

#[test]
fn downgrade_preview_explains_roles_policy_reader_and_reference() {
    let temp = tempfile::tempdir().unwrap();
    for name in ["base", "target"] {
        std::fs::create_dir(temp.path().join(name)).unwrap();
    }
    let path = temp.path().join("theseus.toml");
    std::fs::write(&path, include_str!("fixtures/recipes/v4_downgrade.toml")).unwrap();
    let recipe = theseus::recipe::load(&path).unwrap();
    let options = theseus::engine::RunOptions::all(&recipe);
    let app = App::preview(recipe, options);
    let text = app.preview_text();
    for expected in [
        "target = newer",
        "base = older",
        "compatible",
        "older-data",
        "before_downgrade",
        "exercise-newer",
        "exercise-older",
    ] {
        assert!(text.contains(expected), "missing {expected}");
    }
}
#[test]
fn downgrade_offline_evidence_shows_actual_refusal_and_declared_marker() {
    let mut run = RunReport::new("offline-downgrade");
    let mut s = ScenarioReport::new("user-downgrade", ProfileName::User, ScenarioKind::Downgrade);
    let mut stage = StageReport::new("downgrade");
    stage.step = Some(StepName::Downgrade);
    stage.outcome = StageOutcome::Passed;
    let mut c = CommandRecord::new("attempt", "downgrade");
    c.outcome = CommandOutcome::ExpectedRejection;
    c.exit_status = Some(9);
    c.rejection = Some(RejectionEvidence {
        expected_exit: 9,
        stdout_contains: "newer data".into(),
        stdout_matched: true,
    });
    stage.commands.push(c);
    s.stages.push(stage);
    run.scenarios.push(s);
    let app = App::offline(run, std::path::PathBuf::new());
    let text = app.items().join("\n");
    for expected in ["actual=Some(9)", "expected=9", "matched=true", "newer data"] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
    assert!(screen(&app, 52, 24).contains("expected=9"));
}

#[test]
fn downgrade_completed_evidence_names_the_declared_result() {
    for rejection in [false, true] {
        let mut run = RunReport::new("completed");
        let mut s =
            ScenarioReport::new("user-downgrade", ProfileName::User, ScenarioKind::Downgrade);
        s.outcome = RunOutcome::Passed;
        s.downgrade = Some(if rejection {
            theseus::recipe::DowngradeContract::SafeRejection {
                expected_exit: 9,
                stdout_contains: "newer data".into(),
                binary_checks: vec!["binary".into()],
                data_checks: vec!["data".into()],
            }
        } else {
            theseus::recipe::DowngradeContract::Compatible {
                data_checks: vec!["reader".into()],
            }
        });
        let mut stage = StageReport::new("final");
        stage.outcome = StageOutcome::Passed;
        s.stages.push(stage);
        run.scenarios.push(s);
        let app = App::offline(run, std::path::PathBuf::new());
        assert!(app.items().join("\n").contains(if rejection {
            "Declared rejection checks passed"
        } else {
            "Compatible downgrade passed"
        }));
    }
}

#[test]
fn rollback_preview_lists_runtime_policy_mapping_and_all_derived_proofs() {
    let temp = tempfile::tempdir().unwrap();
    for p in ["base", "target"] {
        std::fs::create_dir(temp.path().join(p)).unwrap();
    }
    let path = temp.path().join("theseus.toml");
    std::fs::write(&path, include_str!("fixtures/recipes/rollback.toml")).unwrap();
    let recipe = theseus::recipe::load(&path).unwrap();
    let mut app = App::preview(recipe.clone(), theseus::engine::RunOptions::all(&recipe));
    let text = app.preview_text();
    for needle in [
        "base = known-good",
        "target = failed candidate",
        "attempt_upgrade exit=9",
        "restore_data",
        "/workspace/data -> /workspace/backup/data",
        "__theseus_rollback_root_absent",
        "__theseus_rollback_source_stable_data",
        "__theseus_rollback_backup_matches_data",
        "__theseus_rollback_backup_intact_data",
        "__theseus_rollback_backup_ready_data",
        "__theseus_rollback_restored_data",
        "__theseus_rollback_backup_retained_data",
        "__theseus_rollback_backup_removed",
        "runtime checks",
    ] {
        assert!(text.contains(needle), "{needle}: {text}");
    }
    key(&mut app, KeyCode::Char('s'));
    assert_eq!(
        app.options.as_ref().unwrap().scenarios,
        vec![ScenarioKind::Rollback]
    );
}

#[test]
fn rollback_offline_narrow_evidence_shows_failed_mutation_and_mapped_restore() {
    let temp = tempfile::tempdir().unwrap();
    let mut report = RunReport::new("rollback-ui");
    let mut scenario =
        ScenarioReport::new("user-rollback", ProfileName::User, ScenarioKind::Rollback);
    let mut stage = StageReport::new("rolled_back");
    stage.outcome = StageOutcome::Failed;
    stage.checks.push(CheckResult {
        command_expectation: None,
        id: "restored-data".into(),
        checkpoint: Checkpoint::RolledBack,
        outcome: CheckOutcome::Failed,
        message: "restore scope differs".into(),
        path: Some("/data".into()),
        command_ids: vec![],
        backup: Some(BackupReference {
            source_path: "/data".into(),
            stored_path: "/backup/data".into(),
        }),
        comparison: Some(ComparisonReference {
            before_stage: "before_upgrade".into(),
            after_stage: "rolled_back".into(),
        }),
    });
    let mut command = CommandRecord::new("attempt", "attempt_upgrade");
    command.outcome = CommandOutcome::ExpectedFailure;
    command.exit_status = Some(9);
    command.expected_failure = Some(ExpectedFailureEvidence {
        expected_exit: 9,
        stdout_contains: "migration failed".into(),
        stdout_matched: true,
    });
    stage.commands.push(command);
    scenario.stages.push(stage);
    report.scenarios.push(scenario);
    let mut app = App::offline(report, temp.path().into());
    let text = app.items().join("\n");
    for needle in [
        "Expected failure: exit=9",
        "migration failed",
        "source /data stored /backup/data",
        "before_upgrade -> rolled_back",
    ] {
        assert!(text.contains(needle), "{needle}: {text}");
    }
    let narrow = screen(&app, 52, 30);
    assert!(narrow.contains("Expected failure"));
    app.focus = Focus::Evidence;
    key(&mut app, KeyCode::End);
    assert!(screen(&app, 52, 30).contains("restore scope differs"));
    assert!(!narrow.contains("Rollback verified"));
}

#[test]
fn interrupted_rollback_preview_describes_real_barrier_and_original_restoration_reference() {
    let temp = tempfile::tempdir().unwrap();
    for name in ["base", "target"] {
        std::fs::create_dir(temp.path().join(name)).unwrap();
    }
    let path = temp.path().join("theseus.toml");
    std::fs::write(
        &path,
        include_str!("fixtures/recipes/interrupted_rollback.toml"),
    )
    .unwrap();
    let recipe = theseus::recipe::load(&path).unwrap();
    let app = App::preview(recipe.clone(), theseus::engine::RunOptions::all(&recipe));
    let text = app.preview_text();
    for expected in [
        "THESEUS_INTERRUPT_BARRIER",
        "ContainerSigkill",
        "same-ID restart",
        "before_interrupt -> interrupted",
        "before_upgrade -> rolled_back",
        "restore_data",
        "/workspace/data -> /workspace/backup/data",
        "Explicit rollback command: rollback",
    ] {
        assert!(text.contains(expected), "{expected}: {text}");
    }
    assert!(!text.contains("Expected failure:"));
}

#[test]
fn actual_pty_default_animation_controls_signals_and_restoration() {
    let output = std::process::Command::new("python3")
        .arg("tests/support/tui_animation.py")
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
