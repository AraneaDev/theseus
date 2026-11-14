use super::*;
use crate::model::{
    ProfileName, RunOutcome, RunReport, ScenarioKind, ScenarioReport, StageOutcome, StageReport,
    TeardownStatus,
};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{Terminal, backend::TestBackend};

#[test]
fn event_latches_recovery_and_status_even_when_report_cannot_be_loaded() {
    let mut app = App::offline(RunReport::new("old-root-evidence"), PathBuf::from("/old"));
    app.active = true;
    let mut last = None;
    let root = PathBuf::from("/missing-new-root");
    let report_path = root.join("report.json");
    apply_event(
        &mut app,
        WorkEvent::Run(engine::EngineEvent {
            kind: engine::EventKind::StageStarted,
            report_path: report_path.clone(),
            scenario_id: Some("user-clean".into()),
            stage: Some("install".into()),
        }),
        &mut last,
    );
    assert!(
        app.status.contains("StageStarted"),
        "event status must not wait for a successful read"
    );
    assert_eq!(last, Some(report_path.clone()));
    assert_eq!(app.root, root);
    assert!(
        app.report.is_none(),
        "old document must not acquire a different root"
    );
    assert!(complete_run(&mut app, Err(anyhow!("final persistence failed"))).is_err());
    assert!(
        app.terminal_error
            .as_ref()
            .unwrap()
            .contains(&report_path.display().to_string())
    );
}

#[test]
fn cached_worker_error_quit_keeps_diagnostic_when_animation_is_disabled_dumb_or_small() {
    for mode in ["disabled", "dumb", "small"] {
        let mut report = RunReport::new("cached-before-error");
        report.outcome = RunOutcome::Passed;
        let root = PathBuf::from("/tmp/cached-worker-error");
        let mut app = App::offline(report, root.clone());
        app.active = true;
        complete_run(&mut app, Err(anyhow!("final persistence failed"))).unwrap();
        let mut presentation = Presentation::new(mode == "small", Duration::ZERO);
        let size = if mode == "small" { (39, 17) } else { (80, 24) };
        presentation.frame(Duration::ZERO, size.0, size.1);
        handle_key(
            &mut app,
            &mut presentation,
            KeyCode::Char('q').into(),
            &CancellationToken::default(),
            &CancellationToken::default(),
            &mut None,
            Instant::now(),
        );
        let error = quit_result(&app, &mut presentation, Duration::ZERO)
            .unwrap()
            .unwrap_err()
            .to_string();
        for expected in [
            "Engine error: final persistence failed".to_string(),
            format!("Saved report: {}", root.join("report.json").display()),
            format!(
                "Recovery: theseus cleanup {}",
                crate::shell::path_argument(&root)
            ),
        ] {
            assert!(error.contains(&expected), "{mode}: {error}");
        }
    }
}

#[test]
fn active_quit_latches_cancel_deadline_and_outro_bypass_before_ready_completion() {
    for outcome in [
        RunOutcome::Passed,
        RunOutcome::Failed,
        RunOutcome::Cancelled,
        RunOutcome::Incomplete,
    ] {
        let mut app = App::offline(
            RunReport::new("cached-running"),
            PathBuf::from("/tmp/ready-completion"),
        );
        app.active = true;
        let mut presentation = Presentation::new(true, Duration::ZERO);
        assert_eq!(
            presentation.frame(Duration::from_millis(5400), 80, 24),
            None
        );
        let cancel = CancellationToken::default();
        let terminate = CancellationToken::default();
        let now = Instant::now();
        let mut deadline = None;
        let (sender, mut receiver) = oneshot::channel();
        let mut persisted = RunReport::new("persisted-completion");
        persisted.outcome = outcome;
        persisted.cancelled = outcome == RunOutcome::Cancelled;
        assert!(
            sender
                .send(Ok(RunResult {
                    report: persisted,
                    report_path: PathBuf::from("/tmp/ready-completion/report.json")
                }))
                .is_ok()
        );
        handle_key(
            &mut app,
            &mut presentation,
            KeyCode::Char('q').into(),
            &cancel,
            &terminate,
            &mut deadline,
            now,
        );
        assert!(cancel.is_cancelled(), "q must cancel at input time");
        assert_eq!(deadline, Some(now + Duration::from_secs(5)));
        complete_run(&mut app, receiver.try_recv().unwrap()).unwrap();
        assert!(!app.active);
        assert_eq!(
            presentation.frame(Duration::from_millis(5401), 80, 24),
            None
        );
        let result = quit_result(&app, &mut presentation, Duration::from_millis(5401));
        assert!(
            result.is_some(),
            "active q must not start an outro after ready completion"
        );
        assert_eq!(result.unwrap().unwrap(), outcome.exit_code());
        assert_eq!(app.report.as_ref().unwrap().outcome, outcome);
        assert_eq!(
            app.report.as_ref().unwrap().cancelled,
            outcome == RunOutcome::Cancelled
        );
    }
}

#[test]
fn terminal_worker_error_overrides_cached_passed_and_failed_results() {
    for cached in [RunOutcome::Passed, RunOutcome::Failed] {
        let mut report = RunReport::new("cached-before-final-save");
        report.outcome = cached;
        let mut scenario =
            ScenarioReport::new("user-clean", ProfileName::User, ScenarioKind::Clean);
        scenario.outcome = cached;
        scenario.teardown = TeardownStatus::Complete;
        let mut stage = StageReport::new("final");
        stage.outcome = if cached == RunOutcome::Passed {
            StageOutcome::Passed
        } else {
            StageOutcome::Failed
        };
        scenario.stages.push(stage);
        report.scenarios.push(scenario);
        let root = PathBuf::from("/tmp/recovery-run");
        let mut app = App::offline(report, root.clone());
        app.active = true;
        app.filter = "unrelated old filter".into();
        complete_run(&mut app, Err(anyhow!("final persistence failed"))).unwrap();
        assert!(!app.active);
        input(
            &mut app,
            crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char('q')),
        );
        assert!(app.quit_requested);
        assert_eq!(
            app.exit_code(),
            2,
            "cached {cached} must not mask worker failure"
        );
        assert_eq!(
            app.report.as_ref().unwrap().outcome,
            cached,
            "retain the last saved evidence"
        );
        for tab in Tab::ALL {
            app.tab = tab;
            let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
            terminal.draw(|frame| render(frame, &app)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            for expected in [
                // Outcome once, coloured by glyph; run id dimmed in the title.
                "… incomplete  │ ".to_string(),
                " run cached-before-final-save ".to_string(),
                "final persistence failed".to_string(),
                root.join("report.json").display().to_string(),
                format!("theseus cleanup {}", crate::shell::path_argument(&root)),
            ] {
                assert!(text.contains(&expected), "missing {expected}");
            }
        }
    }
}

fn rows(app: &App, width: u16, height: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| render(frame, app)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    (0..height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
        .collect()
}

fn live_report() -> RunReport {
    let mut report = RunReport::new("live");
    let mut done = ScenarioReport::new("user-clean", ProfileName::User, ScenarioKind::Clean);
    let mut stage = StageReport::new("base_installed:after_checks");
    stage.outcome = StageOutcome::Passed;
    done.stages.push(stage);
    let mut running = ScenarioReport::new("user-upgrade", ProfileName::User, ScenarioKind::Upgrade);
    let mut stage = StageReport::new("configure");
    stage.outcome = StageOutcome::Running;
    running.stages.push(stage);
    report.scenarios.extend([done, running]);
    report
}

#[test]
fn live_header_names_the_running_stage_and_elapsed_time_then_freezes() {
    let mut app = App::offline(live_report(), PathBuf::from("/live"));
    app.active = true;
    app.run_started = Some(std::time::Instant::now() - Duration::from_secs(75));
    let screen = rows(&app, 140, 30).join("\n");
    // The running stage is in an unselected scenario; the header still says so.
    assert!(
        screen.contains("user/upgrade · configure  1:15"),
        "{screen}"
    );
    assert!(screen.contains("▸   user/upgrade incomplete"), "{screen}");
    // Narrow terminals give the activity its own header line, kept whole.
    let narrow = rows(&app, 80, 30);
    assert!(
        narrow[2].contains("user/upgrade · configure  1:15"),
        "{}",
        narrow.join("\n")
    );
    assert!(narrow[0].ends_with('┐') && narrow[3].starts_with('└'));
    app.active = false;
    app.run_elapsed = Some(Duration::from_secs(80));
    let screen = rows(&app, 140, 30).join("\n");
    assert!(screen.contains("ran 1:20"), "{screen}");
    assert!(!screen.contains("· configure"), "{screen}");
}

#[test]
fn live_header_falls_back_to_the_latest_engine_event_while_the_report_lags() {
    let mut report = RunReport::new("lagging");
    report.scenarios.push(ScenarioReport::new(
        "system-clean",
        ProfileName::System,
        ScenarioKind::Clean,
    ));
    let mut app = App::offline(report, PathBuf::from("/live"));
    app.active = true;
    app.run_started = Some(std::time::Instant::now());
    let event = |kind, stage: Option<&str>| {
        WorkEvent::Run(engine::EngineEvent {
            kind,
            report_path: PathBuf::from("/live/report.json"),
            scenario_id: Some("system-clean".into()),
            stage: stage.map(Into::into),
        })
    };
    apply_event(
        &mut app,
        event(engine::EventKind::StageStarted, Some("configure\u{1b}")),
        &mut None,
    );
    app.report = Some(RunReport::new("lagging"));
    let screen = rows(&app, 140, 30).join("\n");
    assert!(screen.contains("system-clean · configure�"), "{screen}");
    apply_event(
        &mut app,
        event(engine::EventKind::StageFinished, Some("configure")),
        &mut None,
    );
    app.report = Some(RunReport::new("lagging"));
    assert!(
        rows(&app, 140, 30)
            .join("\n")
            .contains("system-clean · between stages")
    );
}

#[test]
fn selector_column_fits_the_longest_row_within_bounds_and_narrow_stays_stacked() {
    let app = App::offline(live_report(), PathBuf::from("/saved"));
    // Stage rows keep the trailing status word harnesses match.
    let wide = rows(&app, 120, 30);
    let stage = wide
        .iter()
        .find(|row| row.contains("base_installed:after_checks passed"))
        .expect("full stage row is visible");
    assert!(
        stage.starts_with("│✓ > base_installed:after_checks passed"),
        "{stage}"
    );
    // At most 45% of the width, so the detail keeps the rest.
    let border = wide[3].chars().position(|c| c == '┓').expect("border");
    assert!(border < 54, "{}", wide[3]);
    let narrow = rows(&app, 80, 30);
    assert!(
        narrow
            .iter()
            .any(|row| row.contains("Scenarios") && row.contains("Stages")),
        "{}",
        narrow.join("\n")
    );
}

fn attention_report() -> RunReport {
    let stage = |name: &str, outcome| {
        let mut stage = StageReport::new(name);
        stage.outcome = outcome;
        stage
    };
    let mut report = RunReport::new("attention");
    let mut review = ScenarioReport::new("user-clean", ProfileName::User, ScenarioKind::Clean);
    review.stages.push(stage("install", StageOutcome::Passed));
    review.findings.push(crate::model::Finding {
        kind: crate::model::FindingKind::Leftover,
        path: Some("/leftover".into()),
        message: "left behind".into(),
    });
    let mut incomplete =
        ScenarioReport::new("user-upgrade", ProfileName::User, ScenarioKind::Upgrade);
    incomplete
        .stages
        .push(stage("install", StageOutcome::Passed));
    incomplete
        .stages
        .push(stage("upgrade", StageOutcome::Incomplete));
    let mut failed = ScenarioReport::new("system-clean", ProfileName::System, ScenarioKind::Clean);
    failed.stages.push(stage("install", StageOutcome::Passed));
    failed.stages.push(stage("configure", StageOutcome::Passed));
    failed.stages.push(stage("exercise", StageOutcome::Failed));
    review.outcome = RunOutcome::Passed;
    incomplete.outcome = RunOutcome::Incomplete;
    failed.outcome = RunOutcome::Failed;
    report.scenarios.extend([review, incomplete, failed]);
    report
}

#[test]
fn saved_report_opens_on_first_failure_then_incomplete_then_review() {
    let mut report = attention_report();
    let app = App::open_saved(report.clone(), PathBuf::from("/saved"));
    assert_eq!((app.scenario_index, app.stage_index), (2, 2));
    assert!(rows(&app, 120, 30).join("\n").contains("> exercise failed"));
    // A failed scenario without a failed stage selects its first stage.
    report.scenarios[2].stages[2].outcome = StageOutcome::Passed;
    let app = App::open_saved(report.clone(), PathBuf::from("/saved"));
    assert_eq!((app.scenario_index, app.stage_index), (2, 0));
    report.scenarios[2].outcome = RunOutcome::Passed;
    let app = App::open_saved(report.clone(), PathBuf::from("/saved"));
    assert_eq!((app.scenario_index, app.stage_index), (1, 1));
    report.scenarios[1].stages[1].outcome = StageOutcome::Passed;
    report.scenarios[1].outcome = RunOutcome::Passed;
    let app = App::open_saved(report.clone(), PathBuf::from("/saved"));
    assert_eq!((app.scenario_index, app.stage_index), (0, 0));
    report.scenarios[0].findings.clear();
    let app = App::open_saved(report, PathBuf::from("/saved"));
    assert_eq!((app.scenario_index, app.stage_index), (0, 0));
    // The plain constructor (used by embedders and tests) keeps the first row.
    let app = App::offline(attention_report(), PathBuf::from("/saved"));
    assert_eq!((app.scenario_index, app.stage_index), (0, 0));
}

#[test]
fn live_completion_selects_first_failure_unless_the_user_navigated() {
    let finish = |navigate: bool| {
        let mut app = App::offline(live_report(), PathBuf::from("/live"));
        app.active = true;
        if navigate {
            press(&mut app, KeyCode::Down);
        }
        let before = (app.scenario_index, app.stage_index);
        complete_run(
            &mut app,
            Ok(RunResult {
                report: attention_report(),
                report_path: PathBuf::from("/live/report.json"),
            }),
        )
        .unwrap();
        (before, (app.scenario_index, app.stage_index))
    };
    assert_eq!(finish(false), ((0, 0), (2, 2)));
    assert_eq!(finish(true), ((1, 0), (1, 0)), "browsing keeps its place");
}

fn press(app: &mut App, code: KeyCode) {
    input(app, KeyEvent::from(code));
}

#[test]
fn help_overlay_lists_grouped_bindings_and_swallows_other_keys() {
    let mut app = App::offline(live_report(), PathBuf::from("/saved"));
    app.active = true;
    let footer = rows(&app, 160, 30).join("\n");
    assert!(footer.contains("? help"), "{footer}");
    press(&mut app, KeyCode::Char('?'));
    assert!(app.help);
    let screen = rows(&app, 120, 36).join("\n");
    for expected in [
        "Help — ? or Esc closes",
        "Navigate",
        "Evidence",
        "Logs",
        "Run control",
        "Tab/Enter",
        "Home/End",
        "first/last log chunk",
        "Ctrl-C",
    ] {
        assert!(screen.contains(expected), "missing {expected}\n{screen}");
    }
    // Nothing acts while it is open: no quit, cancel, focus or selection.
    for code in [
        KeyCode::Char('q'),
        KeyCode::Char('c'),
        KeyCode::Char('r'),
        KeyCode::Tab,
        KeyCode::Down,
        KeyCode::Char('/'),
    ] {
        press(&mut app, code);
    }
    assert!(!app.quit_requested && !app.cancel_requested && !app.start_requested);
    assert_eq!((app.focus, app.scenario_index), (Focus::Scenario, 0));
    assert!(!app.editing_filter && app.help);
    press(&mut app, KeyCode::Esc);
    assert!(!app.help);
    assert_eq!(app.focus, Focus::Scenario, "closing Esc is not back");
    press(&mut app, KeyCode::Char('?'));
    press(&mut app, KeyCode::Char('?'));
    assert!(!app.help);
    // In the `/` draft `?` is text.
    press(&mut app, KeyCode::Char('/'));
    press(&mut app, KeyCode::Char('?'));
    assert!(!app.help && app.filter_draft == "?");
    // Small terminals still draw a bounded overlay.
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('?'));
    let small = rows(&app, 40, 12);
    assert!(small.iter().any(|row| row.contains("Help")), "{small:?}");
}

#[test]
fn filter_highlights_matches_in_visible_rows_and_detail() {
    let mut report = RunReport::new("highlight");
    let mut scenario = ScenarioReport::new("user-clean", ProfileName::User, ScenarioKind::Clean);
    let mut stage = StageReport::new("Café-alpha");
    stage.outcome = StageOutcome::Passed;
    stage.errors.push("ALPHA\u{1b}beta CAFÉ".into());
    scenario.stages.push(stage);
    report.scenarios.push(scenario);
    let mut app = App::offline(report, PathBuf::from("/saved"));
    app.filter = "café".into();
    app.sync_view();
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let mut highlighted = String::from("|");
    for y in 0..30 {
        for x in 0..120 {
            let cell = &buffer[(x, y)];
            if cell.modifier.contains(ratatui::style::Modifier::REVERSED) {
                highlighted.push_str(cell.symbol());
            } else if !highlighted.ends_with('|') {
                highlighted.push('|');
            }
        }
    }
    let parts: Vec<_> = highlighted.split('|').filter(|s| !s.is_empty()).collect();
    assert!(
        parts.iter().all(|p| p.to_lowercase() == "café"),
        "{parts:?}"
    );
    assert!(parts.len() >= 3, "row and detail matches: {parts:?}");
    let screen = rows(&app, 120, 30).join("\n");
    assert!(
        screen.contains("ALPHA�beta CAFÉ"),
        "escaping unchanged: {screen}"
    );
}

#[test]
fn active_run_header_says_running_not_a_provisional_outcome() {
    let draw = |app: &App| {
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    };
    let mut app = App::offline(RunReport::new("live-run"), PathBuf::from("/live"));
    app.active = true;
    assert_eq!(live_outcome_word(&app), "running");
    let header = draw(&app);
    assert!(header.contains("running"), "{header}");
    app.active = false;
    assert_eq!(live_outcome_word(&app), app.outcome().to_string());
    assert!(!draw(&app).contains("▸ running"));
}
