//! Running rows, problem jumps, OSC 52 copy and opt-in mouse mapping.
use super::*;

fn running_report() -> RunReport {
    let mut report = live_report();
    let scenario = &mut report.scenarios[1];
    scenario.stages[0].outcome = StageOutcome::Passed;
    scenario.stages[0].name = "install".into();
    let mut next = StageReport::new("configure");
    next.outcome = StageOutcome::Pending;
    scenario.stages.push(next);
    report
}

#[test]
fn live_rows_show_the_executing_scenario_and_stage_as_running() {
    let mut app = App::offline(running_report(), PathBuf::from("/live"));
    app.scenario_index = 1;
    app.active = true;
    // The saved report lags: the latest engine event names the stage.
    app.live_stage = Some(("user-upgrade".into(), Some("configure".into())));
    let screen = rows(&app, 140, 30).join("\n");
    assert!(screen.contains("▸ > user/upgrade running"), "{screen}");
    assert!(screen.contains("▸   configure running"), "{screen}");
    assert!(screen.contains("install passed"), "{screen}");
    assert!(screen.contains("user/clean incomplete"), "{screen}");
    // A stale event never relabels a finished stage.
    app.live_stage = Some(("user-upgrade".into(), Some("install".into())));
    let screen = rows(&app, 140, 30).join("\n");
    assert!(screen.contains("install passed"), "{screen}");
    assert!(screen.contains("configure pending"), "{screen}");
    assert!(screen.contains("user/upgrade running"), "{screen}");
    // A persisted running stage wins over the event.
    app.report.as_mut().unwrap().scenarios[1].stages[1].outcome = StageOutcome::Running;
    assert!(rows(&app, 140, 30).join("\n").contains("configure running"));
    // Finished runs and offline reports keep their recorded words.
    app.active = false;
    app.report.as_mut().unwrap().scenarios[1].stages[1].outcome = StageOutcome::Pending;
    let screen = rows(&app, 140, 30).join("\n");
    assert!(screen.contains("user/upgrade incomplete"), "{screen}");
    assert!(screen.contains("configure pending"), "{screen}");
    assert!(!screen.contains("running"), "{screen}");
}

#[test]
fn jump_keys_cycle_problems_in_first_failure_precedence() {
    let mut report = attention_report();
    report.scenarios[0].stages[0].outcome = StageOutcome::Failed;
    let mut app = App::offline(report, PathBuf::from("/saved"));
    let at = |app: &App| (app.scenario_index, app.stage_index);
    press(&mut app, KeyCode::Char('f'));
    assert_eq!(at(&app), (2, 2));
    assert!(
        app.status
            .starts_with("Problem 2/2 (failed): system/clean · exercise"),
        "{}",
        app.status
    );
    press(&mut app, KeyCode::Char('f'));
    assert_eq!(at(&app), (0, 0), "wraps to the first problem");
    press(&mut app, KeyCode::Char('F'));
    assert_eq!(at(&app), (2, 2), "previous wraps to the last");
    // Incomplete stages are only targets when nothing failed.
    let mut report = attention_report();
    report.scenarios[2].stages[2].outcome = StageOutcome::Passed;
    report.scenarios[2].outcome = RunOutcome::Passed;
    let mut app = App::offline(report, PathBuf::from("/saved"));
    press(&mut app, KeyCode::Char('f'));
    assert_eq!(at(&app), (1, 1));
    assert!(app.status.contains("(incomplete)"), "{}", app.status);
    // Nothing to visit: selection stays, the status says so.
    let mut app = App::offline(live_report(), PathBuf::from("/saved"));
    app.report.as_mut().unwrap().scenarios.truncate(1);
    app.report.as_mut().unwrap().scenarios[0].outcome = RunOutcome::Passed;
    press(&mut app, KeyCode::Char('f'));
    assert_eq!(at(&app), (0, 0));
    assert_eq!(app.status, "No failed, incomplete or review stages");
    // In the filter draft `f` is text.
    press(&mut app, KeyCode::Char('/'));
    press(&mut app, KeyCode::Char('f'));
    assert_eq!(app.filter_draft, "f");
}

fn command_report() -> RunReport {
    let mut report = RunReport::new("copy");
    let mut scenario = ScenarioReport::new("user-clean", ProfileName::User, ScenarioKind::Clean);
    let mut stage = StageReport::new("install");
    stage.outcome = StageOutcome::Passed;
    let mut command = crate::model::CommandRecord::new("user-clean-1", "install");
    command.argv = vec!["/bin/sh".into(), "-c".into(), "echo 'hi' \u{1b}[2J".into()];
    command.stdout = Some(crate::model::LogReference {
        path: "logs/user-clean-1.stdout".into(),
        bytes: 3,
        truncated: false,
    });
    stage.commands.push(command);
    scenario.stages.push(stage);
    report.scenarios.push(scenario);
    report
}

fn copied(app: &mut App) -> String {
    press(app, KeyCode::Char('y'));
    let sequence = app.clipboard.take().expect("an OSC 52 sequence");
    let body = sequence
        .strip_prefix("\x1b]52;c;")
        .and_then(|s| s.strip_suffix('\x07'))
        .expect("OSC 52 framing");
    assert!(!body.bytes().any(|b| b.is_ascii_control()));
    body.to_string()
}

#[test]
fn copy_sends_the_selected_record_per_tab_through_osc52() {
    let mut app = App::offline(command_report(), PathBuf::from("/saved"));
    app.tab = Tab::Commands;
    let encoded = copied(&mut app);
    assert_eq!(
        encoded,
        clipboard::base64("/bin/sh -c 'echo '\"'\"'hi'\"'\"' \u{1b}[2J'".as_bytes())
    );
    assert!(
        app.status.starts_with("Copied command line ("),
        "{}",
        app.status
    );
    app.tab = Tab::Logs;
    let encoded = copied(&mut app);
    let path = PathBuf::from("/saved").join("logs/user-clean-1.stdout");
    assert_eq!(
        encoded,
        clipboard::base64(path.display().to_string().as_bytes())
    );
    assert!(app.status.starts_with("Copied log path"), "{}", app.status);
    app.tab = Tab::Stages;
    copied(&mut app);
    assert!(
        app.status.starts_with("Copied record text"),
        "{}",
        app.status
    );
    // Oversized text is refused, not truncated.
    let mut report = command_report();
    report.scenarios[0].stages[0].commands[0].argv = vec!["x".repeat(70 * 1024)];
    let mut app = App::offline(report, PathBuf::from("/saved"));
    app.tab = Tab::Commands;
    press(&mut app, KeyCode::Char('y'));
    assert!(app.clipboard.is_none());
    assert!(app.status.contains("64 KiB"), "{}", app.status);
    // No record, no copy; and in the filter draft `y` is text.
    let mut app = App::offline(RunReport::new("empty"), PathBuf::from("/saved"));
    press(&mut app, KeyCode::Char('y'));
    assert!(app.clipboard.is_none() && app.status.starts_with("Nothing to copy"));
    press(&mut app, KeyCode::Char('/'));
    press(&mut app, KeyCode::Char('y'));
    assert!(app.clipboard.is_none() && app.filter_draft == "y");
    // A native preview has nothing to copy and `y` does nothing there.
    let mut preview = App::native_preview("preview".into());
    press(&mut preview, KeyCode::Char('y'));
    assert!(preview.clipboard.is_none());
}

fn pointer(app: &mut App, kind: crossterm::event::MouseEventKind, column: u16, row: u16) {
    mouse::input(
        app,
        crossterm::event::MouseEvent {
            kind,
            column,
            row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        },
    );
}
fn find(screen: &[String], needle: &str) -> (u16, u16) {
    screen
        .iter()
        .enumerate()
        .find_map(|(y, row)| {
            row.find(needle)
                .map(|x| (row[..x].chars().count() as u16, y as u16))
        })
        .unwrap_or_else(|| panic!("{needle} not on screen\n{}", screen.join("\n")))
}

#[test]
fn mouse_clicks_and_wheel_map_through_the_rendered_layout() {
    use crossterm::event::{MouseButton, MouseEventKind::*};
    let click = Down(MouseButton::Left);
    let mut app = App::offline(running_report(), PathBuf::from("/saved"));
    for (width, height) in [(140, 30), (80, 30)] {
        app.scenario_index = 0;
        app.stage_index = 0;
        app.focus = Focus::Scenario;
        app.tab = Tab::Stages;
        let screen = rows(&app, width, height);
        let (x, y) = find(&screen, "user/upgrade");
        pointer(&mut app, click, x, y);
        assert_eq!((app.scenario_index, app.focus), (1, Focus::Scenario));
        let screen = rows(&app, width, height);
        let (x, y) = find(&screen, "configure pending");
        pointer(&mut app, click, x, y);
        assert_eq!((app.stage_index, app.focus), (1, Focus::Stage));
        // Wheel over the stage list moves its selection without refocusing.
        app.focus = Focus::Scenario;
        let screen = rows(&app, width, height);
        let (x, y) = find(&screen, "install passed");
        pointer(&mut app, ScrollUp, x, y);
        assert_eq!((app.stage_index, app.focus), (0, Focus::Scenario));
        // Tabs switch on click and focus evidence.
        let screen = rows(&app, width, height);
        let (x, y) = find(&screen, "Commands");
        pointer(&mut app, click, x + 1, y);
        assert_eq!((app.tab, app.focus), (Tab::Commands, Focus::Evidence));
        // Wheel over the detail scrolls it.
        app.tab = Tab::Stages;
        let screen = rows(&app, width, height);
        let (x, y) = find(&screen, "Detail — record");
        pointer(&mut app, ScrollDown, x, y + 1);
        assert_eq!(app.scroll, 3);
        pointer(&mut app, ScrollUp, x, y + 1);
        assert_eq!(app.scroll, 0);
    }
    // Records: a click selects the clicked row and focuses evidence.
    let mut report = running_report();
    let mut stage = StageReport::new("checked");
    stage.errors = vec!["first-error".into(), "second-error".into()];
    report.scenarios[0].stages[0] = stage;
    let mut app = App::offline(report, PathBuf::from("/saved"));
    let screen = rows(&app, 140, 30);
    let (x, y) = find(&screen, "second-error");
    pointer(&mut app, click, x, y);
    assert_eq!(app.focus, Focus::Evidence);
    assert!(app.evidence_index > 0);
    let screen = rows(&app, 140, 30);
    assert!(
        screen.iter().any(|row| row.contains("> second-error")),
        "{}",
        screen.join("\n")
    );
    // Every recorded tab rectangle covers that tab's drawn title.
    for (rect, tab) in app.hits.borrow().tabs.clone() {
        let text: String = screen[rect.y as usize]
            .chars()
            .skip(rect.x as usize)
            .take(rect.width as usize)
            .collect();
        assert!(text.contains(&format!("{tab:?}")), "{tab:?}: {text:?}");
    }
    // Help: a click closes it and does nothing else.
    app.help = true;
    let before = (app.scenario_index, app.stage_index, app.tab, app.focus);
    let (x, y) = find(&screen, "user/upgrade");
    pointer(&mut app, click, x, y);
    assert!(!app.help);
    assert_eq!(
        (app.scenario_index, app.stage_index, app.tab, app.focus),
        before
    );
    // The filter draft ignores the pointer.
    app.editing_filter = true;
    pointer(&mut app, click, x, y);
    assert_eq!(app.scenario_index, before.0);
}
