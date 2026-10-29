use theseus::{
    engine::{self, CancellationToken, EventKind, RunOptions},
    model::*,
    recipe,
};
fn run(
    fault: &str,
    profiles: Vec<ProfileName>,
    scenarios: Vec<ScenarioKind>,
) -> (tempfile::TempDir, engine::RunResult) {
    run_custom(fault, profiles, scenarios, |_| {})
}
fn run_custom(
    fault: &str,
    profiles: Vec<ProfileName>,
    scenarios: Vec<ScenarioKind>,
    change: impl FnOnce(&mut recipe::ValidatedRecipe),
) -> (tempfile::TempDir, engine::RunResult) {
    let temp = tempfile::tempdir().unwrap();
    let mut recipe =
        recipe::load(std::path::Path::new("tests/fixtures/lifecycle/recipe.toml")).unwrap();
    for profile in recipe.recipe.profiles.values_mut() {
        for step in profile.steps.values_mut() {
            step.env
                .insert("THESEUS_FIXTURE_FAULT".into(), fault.into());
            if fault == "timeout_child" {
                step.timeout_seconds = 1;
            }
        }
    }
    change(&mut recipe);
    let options = RunOptions {
        profiles,
        scenarios,
        output_root: temp.path().join("runs"),
        log_limit: if fault == "log_overflow" {
            1024
        } else {
            recipe::DEFAULT_LOG_LIMIT
        },
        ..RunOptions::all(&recipe)
    };
    let cancel = CancellationToken::default();
    let signal = cancel.clone();
    let cancellation = fault == "cancellation";
    let result = engine::run(&recipe, options, cancel, move |event| {
        if cancellation
            && event.kind == EventKind::StageStarted
            && event.stage.as_deref() == Some("install_target")
        {
            let signal = signal.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(500));
                signal.cancel();
            });
        }
    })
    .unwrap();
    assert!(
        result.report.errors.is_empty(),
        "Docker prerequisite/run failure: {:?}",
        result.report.errors
    );
    for scenario in &result.report.scenarios {
        assert!(
            matches!(scenario.teardown, TeardownStatus::Complete),
            "{:?}",
            scenario.teardown
        );
        if let Some(id) = &scenario.container_id {
            assert!(
                !std::process::Command::new("docker")
                    .args(["container", "inspect", id])
                    .output()
                    .unwrap()
                    .status
                    .success(),
                "owned container survived terminal result"
            );
        }
    }
    (temp, result)
}
#[test]
#[ignore = "requires a reachable Docker daemon and local ubuntu:24.04"]
fn healthy_fixture_passes_all_four_real_lifecycles() {
    let (_, result) = run(
        "",
        vec![ProfileName::User, ProfileName::System],
        vec![ScenarioKind::Clean, ScenarioKind::Upgrade],
    );
    assert_eq!(
        result.report.outcome,
        RunOutcome::Passed,
        "{:#?}",
        result.report
    );
    assert_eq!(result.report.scenarios.len(), 4);
    assert_eq!(
        result.report.counts().review_findings,
        0,
        "{:#?}",
        result.report.scenarios
    );
    assert!(result.report.counts().passed_checks >= 20);
    assert_eq!(result.report.artifacts.len(), 2);
    assert!(result.report.artifacts.iter().all(|a| a.files.len() == 3));
    for s in &result.report.scenarios {
        for stage in &s.stages {
            for command in &stage.commands {
                if command.phase.starts_with("check:")
                    || command.phase == "configure"
                    || command.phase == "exercise"
                {
                    assert_eq!(command.identity.uid, 10001);
                }
            }
        }
    }
}
#[test]
#[ignore = "requires Docker"]
fn real_broken_fixtures_reveal_contract_failures_and_review_findings() {
    for (fault, profile, scenario, outcome, finding) in [
        (
            "stale_path",
            ProfileName::User,
            ScenarioKind::Clean,
            RunOutcome::Failed,
            None,
        ),
        (
            "permissions",
            ProfileName::System,
            ScenarioKind::Clean,
            RunOutcome::Failed,
            None,
        ),
        (
            "config_loss",
            ProfileName::User,
            ScenarioKind::Upgrade,
            RunOutcome::Failed,
            None,
        ),
        (
            "lost_data",
            ProfileName::User,
            ScenarioKind::Clean,
            RunOutcome::Failed,
            None,
        ),
        (
            "install",
            ProfileName::User,
            ScenarioKind::Clean,
            RunOutcome::Failed,
            None,
        ),
        (
            "uninstall",
            ProfileName::System,
            ScenarioKind::Clean,
            RunOutcome::Failed,
            None,
        ),
        (
            "leftovers",
            ProfileName::User,
            ScenarioKind::Clean,
            RunOutcome::Passed,
            Some(FindingKind::Leftover),
        ),
        (
            "baseline_deletion",
            ProfileName::User,
            ScenarioKind::Clean,
            RunOutcome::Passed,
            Some(FindingKind::BaselineDeletion),
        ),
    ] {
        let (_, result) = run(fault, vec![profile], vec![scenario]);
        assert_eq!(
            result.report.outcome, outcome,
            "fault {fault}: {:#?}",
            result.report
        );
        if let Some(kind) = finding {
            assert!(
                result.report.scenarios[0]
                    .findings
                    .iter()
                    .any(|f| f.kind == kind),
                "fault {fault}"
            );
        } else {
            assert!(
                result.report.counts().failed_checks > 0
                    || result.report.scenarios[0]
                        .stages
                        .iter()
                        .flat_map(|s| &s.commands)
                        .any(|c| c.outcome == CommandOutcome::Failed),
                "{fault}"
            );
        }
    }
}
#[test]
#[ignore = "requires Docker"]
fn real_timeout_and_cancellation_stop_container_descendants() {
    for (fault, expected) in [
        ("timeout_child", RunOutcome::Incomplete),
        ("cancellation", RunOutcome::Cancelled),
    ] {
        let (_, result) = run(fault, vec![ProfileName::User], vec![ScenarioKind::Clean]);
        assert_eq!(result.report.outcome, expected, "{:#?}", result.report);
        let s = &result.report.scenarios[0];
        assert_eq!(
            s.stages
                .iter()
                .find(|s| s.step == Some(StepName::Uninstall))
                .unwrap()
                .outcome,
            StageOutcome::Skipped
        );
        assert!(
            s.stages
                .iter()
                .flat_map(|s| &s.commands)
                .any(|c| c.timed_out || c.cancelled)
        );
    }
}
#[test]
#[ignore = "requires Docker"]
fn real_logs_are_drained_and_capped_independently() {
    let (temp, result) = run(
        "log_overflow",
        vec![ProfileName::User],
        vec![ScenarioKind::Clean],
    );
    assert_eq!(result.report.outcome, RunOutcome::Passed);
    let command = &result.report.scenarios[0]
        .stages
        .iter()
        .find(|s| s.step == Some(StepName::InstallTarget))
        .unwrap()
        .commands[0];
    for log in [
        command.stdout.as_ref().unwrap(),
        command.stderr.as_ref().unwrap(),
    ] {
        assert!(log.truncated);
        assert_eq!(log.bytes, 1024);
        assert_eq!(
            std::fs::metadata(result.report_path.parent().unwrap().join(&log.path))
                .unwrap()
                .len(),
            1024
        );
    }
    assert!(temp.path().exists());
}

#[test]
#[ignore = "requires Docker"]
fn actual_archive_overflow_and_symlink_ancestors_are_incomplete() {
    let (_, result) = run_custom(
        "",
        vec![ProfileName::User],
        vec![ScenarioKind::Clean],
        |recipe| {
            recipe.recipe.limits.max_entries = 2;
        },
    );
    assert_eq!(result.report.outcome, RunOutcome::Incomplete);
    assert!(result.report.scenarios[0].stages.iter().flat_map(|s| &s.snapshots).any(|s| matches!(&s.completeness, theseus::snapshot::ObservationStatus::Incomplete { reason } if reason.contains("limit"))));
    let (_, result) = run_custom(
        "",
        vec![ProfileName::User],
        vec![ScenarioKind::Clean],
        |recipe| {
            recipe.recipe.observe = vec![
                "/home/theseus/.config/sample-cli".into(),
                "/home/theseus/.local".into(),
            ];
            recipe
                .recipe
                .profiles
                .get_mut(&ProfileName::User)
                .unwrap()
                .steps
                .get_mut(&StepName::Configure)
                .unwrap()
                .script += "; mv /home/theseus/.config /workspace/config; ln -s /workspace/config /home/theseus/.config";
        },
    );
    assert_eq!(result.report.outcome, RunOutcome::Incomplete);
    assert!(result.report.scenarios[0].stages.iter().flat_map(|s| &s.snapshots).any(|s| matches!(&s.completeness, theseus::snapshot::ObservationStatus::Incomplete { reason } if reason.contains("symlink ancestor"))));
}

#[test]
#[ignore = "requires Docker"]
fn cleanup_refuses_mismatching_ownership_and_is_idempotent_for_missing_resources() {
    use std::collections::BTreeMap;
    let output = std::process::Command::new("docker")
        .args([
            "create",
            "--label",
            "io.theseus.owner=someone-else",
            "ubuntu:24.04",
            "/bin/true",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Docker prerequisite: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let id = String::from_utf8(output.stdout).unwrap().trim().to_string();
    struct Guard(String);
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = std::process::Command::new("docker")
                .args(["rm", "-f", &self.0])
                .output();
        }
    }
    let _guard = Guard(id.clone());
    let temp = tempfile::tempdir().unwrap();
    let store = theseus::report::create(&temp.path().join("run")).unwrap();
    store
        .save_journal(&RunJournal {
            schema_version: 1,
            run_id: "recovery".into(),
            resources: vec![OwnedResource {
                container_id: id.clone(),
                labels: BTreeMap::from([
                    ("io.theseus.owner".into(), "v1".into()),
                    ("io.theseus.run".into(), "recovery".into()),
                    ("io.theseus.scenario".into(), "user-clean".into()),
                ]),
                removed: false,
            }],
            cleanup_errors: vec![],
        })
        .unwrap();
    let error = theseus::runner::Docker::default()
        .cleanup(store.root())
        .unwrap_err();
    assert!(format!("{error:#}").contains("ownership label mismatch"));
    assert!(
        std::process::Command::new("docker")
            .args(["inspect", &id])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(
        !theseus::report::load_journal(store.root())
            .unwrap()
            .cleanup_errors
            .is_empty()
    );
    assert!(
        std::process::Command::new("docker")
            .args(["rm", "-f", &id])
            .output()
            .unwrap()
            .status
            .success()
    );
    theseus::runner::Docker::default()
        .cleanup(store.root())
        .unwrap();
    theseus::runner::Docker::default()
        .cleanup(store.root())
        .unwrap();
    assert!(
        theseus::report::load_journal(store.root())
            .unwrap()
            .resources[0]
            .removed
    );
}

#[test]
#[ignore = "requires Docker"]
fn real_application_exit_one_and_docker_control_exit_one_are_distinct() {
    use std::collections::BTreeMap;
    use theseus::runner::{CommandRequest, Docker, Environment};
    let recipe =
        recipe::load(std::path::Path::new("tests/fixtures/lifecycle/recipe.toml")).unwrap();
    let cancel = CancellationToken::default();
    let mut docker = Docker::default();
    docker.prepare(&recipe, false, &cancel).unwrap();
    let resource = docker
        .create("completion-proof", "user-clean", &recipe)
        .unwrap();
    struct Cleanup(OwnedResource);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            Docker::default().remove(&self.0).unwrap();
        }
    }
    let _cleanup = Cleanup(resource.clone());
    docker.setup(&resource, &recipe, &cancel).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let store = theseus::report::create(&temp.path().join("run")).unwrap();
    let request = |id: &str, expected_exit| CommandRequest {
        id: id.into(),
        phase: "check:negative".into(),
        identity: ExecutionIdentity::runtime(),
        argv: vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf 'real stdout'; printf 'real stderr' >&2; exit 1".into(),
        ],
        env: BTreeMap::new(),
        pass_env: vec![],
        timeout_seconds: 10,
        expected_exit,
        log_limit: 1024,
    };
    let check = recipe::Check {
        id: "negative".into(),
        checkpoint: Checkpoint::Configured,
        expectation: recipe::Expectation::Command {
            script: "exit 1".into(),
            timeout_seconds: 10,
            env: BTreeMap::new(),
            pass_env: vec![],
            expected_exit: 1,
            stdout_contains: Some("real stdout".into()),
        },
    };
    let executed = docker
        .execute(&resource, request("real-negative", 1), &store, &cancel)
        .unwrap();
    assert_eq!(executed.record.outcome, CommandOutcome::Succeeded);
    assert_eq!(executed.record.exit_status, Some(1));
    assert_eq!(executed.stdout, "real stdout");
    assert_eq!(
        std::fs::read_to_string(store.root().join(executed.record.stderr.unwrap().path)).unwrap(),
        "real stderr"
    );
    let evaluate = |executed: &theseus::runner::ExecutedCommand| {
        theseus::checks::evaluate_command(
            &check,
            &theseus::checks::CommandObservation {
                exit_status: executed.record.exit_status,
                stdout: executed.stdout.clone(),
                complete: executed.stdout_complete,
                error: executed.record.error.clone(),
            },
        )
        .outcome
    };
    // A real negative exit passes the production checker.
    let executed = docker
        .execute(&resource, request("real-checker", 1), &store, &cancel)
        .unwrap();
    assert_eq!(evaluate(&executed), CheckOutcome::Passed);
    let mut missing = resource.clone();
    missing.container_id = "0".repeat(64);
    let executed = docker
        .execute(&missing, request("missing-control", 1), &store, &cancel)
        .unwrap();
    assert_eq!(evaluate(&executed), CheckOutcome::Incomplete);
    assert_eq!(executed.record.exit_status, None);
    assert!(executed.record.error.unwrap().contains("Some(1)"));
    // Exercise both shell checks and resolved executable/version checks through
    // the shared engine as well as the direct production adapter boundary.
    let (_, result) = run_custom(
        "",
        vec![ProfileName::User],
        vec![ScenarioKind::Clean],
        |recipe| {
            let checks = &mut recipe
                .recipe
                .profiles
                .get_mut(&ProfileName::User)
                .unwrap()
                .checks;
            checks.push(recipe::Check {
                id: "intentional-command-one".into(),
                checkpoint: Checkpoint::Configured,
                expectation: recipe::Expectation::Command {
                    script: "printf negative-one; exit 1".into(),
                    timeout_seconds: 10,
                    env: BTreeMap::new(),
                    pass_env: vec![],
                    expected_exit: 1,
                    stdout_contains: Some("negative-one".into()),
                },
            });
            checks.push(recipe::Check {
                id: "intentional-version-one".into(),
                checkpoint: Checkpoint::Configured,
                expectation: recipe::Expectation::Executable {
                    name: "sh".into(),
                    resolved_path: "/usr/bin/sh".into(),
                    args: vec!["-c".into(), "printf version-one; exit 1".into()],
                    timeout_seconds: 10,
                    env: BTreeMap::new(),
                    pass_env: vec![],
                    expected_exit: 1,
                    stdout_contains: Some("version-one".into()),
                },
            });
        },
    );
    assert_eq!(
        result.report.outcome,
        RunOutcome::Passed,
        "{:#?}",
        result.report
    );
    for id in ["intentional-command-one", "intentional-version-one"] {
        let check = result.report.scenarios[0]
            .stages
            .iter()
            .flat_map(|s| &s.checks)
            .find(|c| c.id == id)
            .unwrap();
        assert_eq!(check.outcome, CheckOutcome::Passed);
    }
}

#[test]
#[ignore = "requires Docker and a Unix PTY"]
fn real_pty_active_signals_cleanup_and_cancel_browsing() {
    let output = std::process::Command::new("python3")
        .arg("tests/support/tui_signals.py")
        .arg(env!("CARGO_BIN_EXE_theseus"))
        .arg("--docker")
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

fn run_reinstall(fault: &str) -> (tempfile::TempDir, engine::RunResult) {
    let temp = tempfile::tempdir().unwrap();
    let mut recipe =
        recipe::load(std::path::Path::new("tests/fixtures/reinstall/recipe.toml")).unwrap();
    for profile in recipe.recipe.profiles.values_mut() {
        let repeat = profile.steps.get_mut(&StepName::Reinstall).unwrap();
        repeat
            .env
            .insert("THESEUS_FIXTURE_FAULT".into(), fault.into());
        if fault == "timeout_child" {
            repeat.timeout_seconds = 1;
        }
    }
    let cancel = CancellationToken::default();
    let signal = cancel.clone();
    let cancellation = fault == "cancellation";
    let result = engine::run(
        &recipe,
        RunOptions {
            output_root: temp.path().join("runs"),
            ..RunOptions::all(&recipe)
        },
        cancel,
        move |event| {
            if cancellation
                && event.kind == EventKind::StageStarted
                && event.stage.as_deref() == Some("reinstall")
            {
                let signal = signal.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    signal.cancel();
                });
            }
        },
    )
    .unwrap();
    assert!(result.report.errors.is_empty(), "{:#?}", result.report);
    let journal = theseus::report::load_journal(result.report_path.parent().unwrap()).unwrap();
    assert!(!journal.resources.is_empty());
    assert!(journal.resources.iter().all(|r| r.removed));
    for s in &result.report.scenarios {
        assert_eq!(s.teardown, TeardownStatus::Complete);
        if let Some(id) = &s.container_id {
            assert!(
                !std::process::Command::new("docker")
                    .args(["inspect", id])
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        }
    }
    (temp, result)
}

#[test]
#[ignore = "requires Docker"]
fn healthy_reinstall_passes_both_profiles_with_same_container_and_verified_completions() {
    let (_temp, result) = run_reinstall("");
    assert_eq!(
        result.report.outcome,
        RunOutcome::Passed,
        "{:#?}",
        result.report
    );
    assert_eq!(result.report.scenarios.len(), 2);
    assert_eq!(result.report.artifacts.len(), 1);
    assert_eq!(result.report.counts().review_findings, 0);
    assert_eq!(result.report.counts().passed_checks, 18);
    for scenario in &result.report.scenarios {
        assert_eq!(scenario.scenario, ScenarioKind::Reinstall);
        for (phase, marker) in [
            ("install_target", "installation=1"),
            ("reinstall", "installation=2"),
            ("exercise_before_reinstall", "healthy"),
            ("exercise_after_reinstall", "healthy"),
        ] {
            let command = scenario
                .stages
                .iter()
                .flat_map(|s| &s.commands)
                .find(|c| c.phase == phase)
                .unwrap();
            assert_eq!(command.outcome, CommandOutcome::Succeeded);
            assert_eq!(
                command.exit_status,
                Some(0),
                "completion receipt missing for {phase}"
            );
            assert_eq!(
                command.identity.uid,
                if matches!(phase, "install_target" | "reinstall")
                    && scenario.profile == ProfileName::System
                {
                    0
                } else {
                    10001
                }
            );
            let stdout = command.stdout.as_ref().unwrap();
            let text =
                std::fs::read_to_string(result.report_path.parent().unwrap().join(&stdout.path))
                    .unwrap();
            assert!(text.contains(marker), "{phase}: {text}");
        }
        for check in scenario
            .stages
            .iter()
            .flat_map(|s| &s.checks)
            .filter(|c| c.id.ends_with("-stable"))
        {
            assert_eq!(check.outcome, CheckOutcome::Passed);
            assert_eq!(check.checkpoint, Checkpoint::Reinstalled);
            assert_eq!(check.comparison, Some(ComparisonReference::reinstall()));
        }
        assert_eq!(
            scenario
                .stages
                .iter()
                .find(|s| s.step == Some(StepName::Uninstall))
                .unwrap()
                .outcome,
            StageOutcome::Passed
        );
    }
}

#[test]
#[ignore = "requires Docker"]
fn reinstall_only_faults_reveal_changed_children_and_continue_uninstall() {
    for (fault, child) in [
        ("reset_settings", "settings"),
        ("duplicate_settings", "settings"),
        ("change_mode", "settings"),
        ("add_child", "unexpected"),
        ("remove_child", "settings"),
    ] {
        let (_temp, result) = run_reinstall(fault);
        assert_eq!(
            result.report.outcome,
            RunOutcome::Failed,
            "{fault}: {:#?}",
            result.report
        );
        for s in &result.report.scenarios {
            let first = s
                .stages
                .iter()
                .find(|s| s.step == Some(StepName::InstallTarget))
                .unwrap();
            assert_eq!(first.outcome, StageOutcome::Passed);
            assert_eq!(
                first.commands[0]
                    .environment
                    .get("THESEUS_FIXTURE_FAULT")
                    .unwrap(),
                ""
            );
            let exercise = s
                .stages
                .iter()
                .find(|s| s.name == "exercise_before_reinstall")
                .unwrap();
            assert_eq!(exercise.outcome, StageOutcome::Passed);
            let repeat = s
                .stages
                .iter()
                .find(|s| s.step == Some(StepName::Reinstall))
                .unwrap();
            assert_eq!(
                repeat.commands[0]
                    .environment
                    .get("THESEUS_FIXTURE_FAULT")
                    .unwrap(),
                fault
            );
            assert_eq!(repeat.commands[0].exit_status, Some(0));
            let checkpoint = s.stages.iter().find(|s| s.name == "reinstalled").unwrap();
            let stable = checkpoint
                .checks
                .iter()
                .find(|c| c.id == "configuration-stable")
                .unwrap();
            assert_eq!(stable.outcome, CheckOutcome::Failed, "{fault}");
            assert_eq!(stable.checkpoint, Checkpoint::Reinstalled);
            assert_eq!(stable.comparison, Some(ComparisonReference::reinstall()));
            let path = format!("/home/theseus/.config/sample-cli/{child}");
            assert!(
                repeat.stage_diff.iter().any(|c| c.path == path),
                "{fault}: {:?}",
                repeat.stage_diff
            );
            assert_eq!(
                s.stages
                    .iter()
                    .find(|s| s.step == Some(StepName::Uninstall))
                    .unwrap()
                    .outcome,
                StageOutcome::Passed
            );
        }
    }
    let (_temp, result) = run_reinstall("reject_existing");
    assert_eq!(
        result.report.outcome,
        RunOutcome::Failed,
        "{:#?}",
        result.report
    );
    for s in &result.report.scenarios {
        let repeat = s
            .stages
            .iter()
            .find(|s| s.step == Some(StepName::Reinstall))
            .unwrap();
        assert_eq!(repeat.commands[0].outcome, CommandOutcome::Failed);
        assert_eq!(repeat.commands[0].exit_status, Some(9));
        assert!(
            repeat.commands[0].error.is_none(),
            "an application rejection has a valid receipt"
        );
        assert_eq!(
            s.stages
                .iter()
                .find(|s| s.step == Some(StepName::Uninstall))
                .unwrap()
                .outcome,
            StageOutcome::Passed
        );
    }
}

#[test]
#[ignore = "requires Docker"]
fn real_reinstall_timeout_and_cancellation_remove_journaled_container() {
    for (fault, outcome) in [
        ("timeout_child", RunOutcome::Incomplete),
        ("cancellation", RunOutcome::Cancelled),
    ] {
        let (_temp, result) = run_reinstall(fault);
        assert_eq!(
            result.report.outcome, outcome,
            "{fault}: {:#?}",
            result.report
        );
        let s = &result.report.scenarios[0];
        assert_eq!(
            s.stages
                .iter()
                .find(|s| s.name == "exercise_before_reinstall")
                .unwrap()
                .outcome,
            StageOutcome::Passed
        );
        let repeat = s
            .stages
            .iter()
            .find(|s| s.step == Some(StepName::Reinstall))
            .unwrap();
        assert!(repeat.commands.iter().any(|c| c.timed_out || c.cancelled));
        assert_eq!(
            s.stages
                .iter()
                .find(|s| s.step == Some(StepName::Uninstall))
                .unwrap()
                .outcome,
            StageOutcome::Skipped
        );
    }
}

#[test]
#[ignore = "requires Docker and a Unix PTY"]
fn real_pty_reinstall_selection_and_new_report_comparison_visibility() {
    let output = std::process::Command::new("python3")
        .arg("tests/support/tui_reinstall.py")
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
