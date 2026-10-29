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

fn container_is_absent(id: &str, output: &std::process::Output) -> bool {
    let error = String::from_utf8_lossy(&output.stderr)
        .trim()
        .to_ascii_lowercase();
    output.status.code() == Some(1)
        && [
            format!("error: no such object: {id}"),
            format!("error response from daemon: no such container: {id}"),
        ]
        .contains(&error)
}

#[test]
fn docker_absence_requires_missing_target_and_expected_exit() {
    use std::os::unix::process::ExitStatusExt;
    for (status, error, absent) in [
        (1, "error: no such object: owned-id\n", true),
        (1, "Error: No such object: owned-id\n", true),
        (
            1,
            "Error response from daemon: No such container: owned-id\n",
            true,
        ),
        (
            1,
            "Cannot connect to the Docker daemon at unix:///missing.sock",
            false,
        ),
        (
            1,
            "permission denied while trying to connect to the docker API",
            false,
        ),
        (1, "error: no such object: another-id", false),
        (
            1,
            "error: no such object: owned-id\ntransport disconnected",
            false,
        ),
        (0, "error: no such object: owned-id", false),
        (125, "error: no such object: owned-id", false),
        (1, "", false),
    ] {
        let output = std::process::Output {
            status: std::process::ExitStatus::from_raw(status << 8),
            stdout: b"[]\n".to_vec(),
            stderr: error.as_bytes().to_vec(),
        };
        assert_eq!(
            container_is_absent("owned-id", &output),
            absent,
            "{status}: {error}"
        );
    }
    let signalled = std::process::Output {
        status: std::process::ExitStatus::from_raw(9),
        stdout: vec![],
        stderr: b"error: no such object: owned-id".to_vec(),
    };
    assert!(!container_is_absent("owned-id", &signalled));
}

#[test]
fn python_docker_absence_requires_missing_target_and_expected_exit() {
    let output = std::process::Command::new("python3")
        .arg("tests/support/docker_absence_test.py")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
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
            let output = std::process::Command::new("docker")
                .args(["inspect", id])
                .output()
                .unwrap();
            assert!(
                container_is_absent(id, &output),
                "Docker inspect did not prove {id} absent: status={:?}, stdout={:?}, stderr={:?}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout)
                    .chars()
                    .take(1024)
                    .collect::<String>(),
                String::from_utf8_lossy(&output.stderr)
                    .chars()
                    .take(1024)
                    .collect::<String>(),
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

struct InterruptionBoundary {
    docker: theseus::runner::Docker,
    resource: OwnedResource,
    store: theseus::report::ReportStore,
    _temporary: tempfile::TempDir,
}
impl InterruptionBoundary {
    fn new() -> Self {
        use theseus::runner::Environment;
        let recipe =
            recipe::load(std::path::Path::new("tests/fixtures/lifecycle/recipe.toml")).unwrap();
        let cancel = CancellationToken::default();
        let mut docker = theseus::runner::Docker::default();
        docker.prepare(&recipe, false, &cancel).unwrap();
        let resource = docker
            .create("interruption-boundary", "boundary", &recipe)
            .unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let boundary = Self {
            docker,
            resource,
            store: theseus::report::create(&temporary.path().join("run")).unwrap(),
            _temporary: temporary,
        };
        let mut boundary = boundary;
        boundary
            .docker
            .setup(&boundary.resource, &recipe, &cancel)
            .unwrap();
        boundary
    }
    fn request(
        id: &str,
        script: &str,
        identity: ExecutionIdentity,
    ) -> theseus::runner::CommandRequest {
        theseus::runner::CommandRequest {
            id: id.into(),
            phase: "boundary".into(),
            identity,
            argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
            env: Default::default(),
            pass_env: vec![],
            timeout_seconds: 10,
            expected_exit: 0,
            log_limit: 2048,
        }
    }
    fn ordinary(&mut self, id: &str, script: &str, identity: ExecutionIdentity) -> String {
        use theseus::runner::Environment;
        let result = self
            .docker
            .execute(
                &self.resource,
                Self::request(id, script, identity),
                &self.store,
                &CancellationToken::default(),
            )
            .unwrap();
        assert_eq!(
            result.record.outcome,
            CommandOutcome::Succeeded,
            "{:?}",
            result.record
        );
        result.stdout
    }
    fn assert_absent(&self) {
        let output = std::process::Command::new("docker")
            .args(["inspect", &self.resource.container_id])
            .output()
            .unwrap();
        assert!(
            container_is_absent(&self.resource.container_id, &output),
            "{output:?}"
        );
    }
}
impl Drop for InterruptionBoundary {
    fn drop(&mut self) {
        use theseus::runner::Environment;
        let result = theseus::runner::Docker::default().remove(&self.resource);
        if !std::thread::panicking() {
            result.unwrap();
            self.assert_absent();
        }
    }
}

#[test]
#[ignore = "requires Docker"]
fn real_interruption_retains_partial_state_and_restarts_without_setup_or_copy() {
    use theseus::runner::Environment;
    for identity in [ExecutionIdentity::runtime(), ExecutionIdentity::root()] {
        let mut boundary = InterruptionBoundary::new();
        // Mutations that setup/chown/input-recopy would undo are independent
        // evidence that restart only starts the original idle entrypoint.
        boundary.ordinary(
            "mutate-inputs",
            "printf retained > /inputs/target/retained; chmod 711 /workspace; chown 0:0 /workspace",
            ExecutionIdentity::root(),
        );
        let request = InterruptionBoundary::request(
            "interrupt",
            r#"set -eu
helper=$THESEUS_INTERRUPT_BARRIER
parent=${helper%/*}
test "$(stat -c '%u:%g:%a' "$helper")" = '0:0:644'
test "$(stat -c '%u:%g:%a' "$parent")" = '0:0:755'
test ! -L "$helper"
test "$(stat -c '%u:%g:%a' "$parent/ready")" = "$(id -u):$(id -g):700"
if test "$(id -u)" != 0; then
    if (printf corrupt >> "$helper") 2>/dev/null; then exit 41; fi
    if (mv "$parent/ready" "$parent/other") 2>/dev/null; then exit 42; fi
fi
printf partial > "$HOME/partial"
chmod 640 "$HOME/partial"
(while :; do printf x >> "$HOME/writer"; sleep 0.02; done) &
/bin/sh "$helper"
printf forbidden > "$HOME/continued"
"#,
            identity.clone(),
        );
        let executed = boundary
            .docker
            .execute_interrupted(
                &boundary.resource,
                request,
                &boundary.store,
                &CancellationToken::default(),
            )
            .unwrap();
        assert_eq!(
            executed.record.outcome,
            CommandOutcome::Interrupted,
            "{:?}",
            executed.record
        );
        assert_eq!(executed.record.exit_status, None);
        assert!(!executed.record.timed_out && !executed.record.cancelled);
        assert!(executed.record.error.is_none());
        let proof = executed.record.interruption.as_ref().unwrap();
        assert_eq!(proof.mechanism, InterruptionMechanism::ContainerSigkill);
        assert!(proof.readiness_verified && proof.stopped_verified);
        assert!(
            !executed
                .record
                .environment
                .contains_key("THESEUS_INTERRUPT_BARRIER")
        );
        assert!(executed.record.forwarded_environment.is_empty());
        let serialized = serde_json::to_string(&executed.record).unwrap();
        assert!(!serialized.contains("/.theseus-"));
        let stopped = std::process::Command::new("docker")
            .args([
                "inspect",
                "--format",
                "{{.State.Running}}",
                &boundary.resource.container_id,
            ])
            .output()
            .unwrap();
        assert!(stopped.status.success());
        assert_eq!(stopped.stdout, b"false\n");
        let restart = boundary
            .docker
            .restart(&boundary.resource, &CancellationToken::default())
            .unwrap();
        assert_eq!(restart.container_id, boundary.resource.container_id);
        assert!(restart.running_verified);
        let evidence = boundary.ordinary(
            "partial-after-restart",
            r#"set -eu
cat "$HOME/partial"
printf '\n'
stat -c '%u:%g:%a' "$HOME/partial"
test ! -e "$HOME/continued"
a=$(wc -c < "$HOME/writer"); sleep 0.15; b=$(wc -c < "$HOME/writer"); test "$a" = "$b"
cat /inputs/target/retained
printf '\n'
stat -c '%u:%g:%a' /workspace
"#,
            identity.clone(),
        );
        assert_eq!(
            evidence,
            format!(
                "partial\n{}:{}:640\nretained\n0:0:711\n",
                identity.uid, identity.gid
            )
        );
    }
}

#[test]
#[ignore = "requires Docker"]
fn real_interruption_early_exit_is_failed_contract_with_real_status() {
    use theseus::runner::Environment;
    let mut boundary = InterruptionBoundary::new();
    for status in [0, 7, 137] {
        let mut request = InterruptionBoundary::request(
            &format!("early-{status}"),
            &format!("exit {status}"),
            ExecutionIdentity::runtime(),
        );
        request.expected_exit = status;
        let result = boundary
            .docker
            .execute_interrupted(
                &boundary.resource,
                request,
                &boundary.store,
                &CancellationToken::default(),
            )
            .unwrap();
        assert_eq!(
            result.record.outcome,
            CommandOutcome::Failed,
            "{:?}",
            result.record
        );
        assert_eq!(result.record.exit_status, Some(status));
        assert!(result.record.error.is_none());
        assert!(!result.record.timed_out && !result.record.cancelled);
        let proof = result.record.interruption.unwrap();
        assert!(!proof.readiness_verified && !proof.stopped_verified);
    }
    assert_eq!(
        boundary.ordinary(
            "still-running",
            "printf alive",
            ExecutionIdentity::runtime()
        ),
        "alive"
    );
}

#[test]
#[ignore = "requires Docker"]
fn real_interruption_never_ready_or_invalid_receipt_removes_owned_container() {
    use theseus::runner::Environment;
    for mode in ["never", "stale", "symlink", "oversized", "control-word"] {
        let mut boundary = InterruptionBoundary::new();
        let script = match mode {
            "never" => "sleep 600",
            "stale" => {
                "printf 'stale-nonce\\n' > \"${THESEUS_INTERRUPT_BARRIER%/*}/ready/receipt\"; sleep 600"
            }
            "control-word" => {
                "printf waiting > \"${THESEUS_INTERRUPT_BARRIER%/*}/ready/receipt\"; sleep 600"
            }
            "symlink" => {
                "ln -s /etc/passwd \"${THESEUS_INTERRUPT_BARRIER%/*}/ready/receipt\"; sleep 600"
            }
            _ => {
                "dd if=/dev/zero of=\"${THESEUS_INTERRUPT_BARRIER%/*}/ready/receipt\" bs=1024 count=1 2>/dev/null; sleep 600"
            }
        };
        let mut request = InterruptionBoundary::request(mode, script, ExecutionIdentity::runtime());
        request.timeout_seconds = 2;
        let result = boundary
            .docker
            .execute_interrupted(
                &boundary.resource,
                request,
                &boundary.store,
                &CancellationToken::default(),
            )
            .unwrap();
        assert_ne!(result.record.outcome, CommandOutcome::Interrupted, "{mode}");
        assert!(
            result.record.timed_out || result.record.error.is_some(),
            "{mode}: {:?}",
            result.record
        );
        if mode != "never" {
            assert_eq!(result.record.outcome, CommandOutcome::Error, "{mode}");
            assert!(
                !result.record.timed_out,
                "malformed readiness must fail immediately: {mode}"
            );
        }
        assert!(!result.record.interruption.unwrap().stopped_verified);
        boundary.assert_absent();
    }
}

#[test]
#[ignore = "requires Docker"]
fn real_interruption_ownership_mismatch_cannot_stop_or_restart_container() {
    use theseus::runner::Environment;
    let mut boundary = InterruptionBoundary::new();
    let mut impostor = boundary.resource.clone();
    impostor
        .labels
        .insert("io.theseus.run".into(), "someone-else".into());
    let result = boundary
        .docker
        .execute_interrupted(
            &impostor,
            InterruptionBoundary::request(
                "mismatch",
                "/bin/sh \"$THESEUS_INTERRUPT_BARRIER\"",
                ExecutionIdentity::runtime(),
            ),
            &boundary.store,
            &CancellationToken::default(),
        )
        .unwrap();
    assert_eq!(result.record.outcome, CommandOutcome::Error);
    assert!(!result.record.interruption.unwrap().stopped_verified);
    let untouched = std::process::Command::new("docker")
        .args([
            "exec",
            &boundary.resource.container_id,
            "/bin/sh",
            "-c",
            "for p in /.theseus-*; do test ! -e \"$p\" || exit 1; done",
        ])
        .output()
        .unwrap();
    assert!(
        untouched.status.success(),
        "ownership mismatch mutated container control state"
    );
    assert!(
        boundary
            .docker
            .restart(&impostor, &CancellationToken::default())
            .is_err()
    );
    assert_eq!(
        boundary.ordinary("owner-alive", "printf alive", ExecutionIdentity::runtime()),
        "alive"
    );
}

impl InterruptionBoundary {
    /// Fault injection sits at the Docker transport boundary. Every mutation and
    /// successful inspect still goes through the real daemon and owned resource.
    fn proxy(&mut self, mode: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        use theseus::runner::Environment;
        let executable = self._temporary.path().join("docker-proxy");
        let marker = self._temporary.path().join("transition");
        let script = r#"#!/usr/bin/python3
import json,pathlib,subprocess,sys,time
args=sys.argv[1:]
mode=MODE
marker=pathlib.Path(MARKER)
is_inspect=args[:2]==['container','inspect']
is_kill=args[:2]==['container','kill']
is_start=args[:2]==['container','start']
is_poll='-c' in args and 'iflag=nofollow,nonblock' in args[args.index('-c')+1]
if mode=='kill-failed' and is_kill: sys.exit(1)
if mode=='restart-failed' and is_start: sys.exit(1)
if mode=='restart-error-cleanup' and is_start:
    marker.touch()
    sys.exit(1)
if mode=='restart-error-cleanup' and args[0]=='rm' and marker.exists(): sys.exit(1)
result=subprocess.run(['docker',*args],capture_output=True)
if is_kill and result.returncode==0: marker.with_suffix('.killed').touch()
ready=is_poll and result.returncode==0 and result.stdout not in [b'waiting',b'completed']
if ready and mode=='transport': sys.exit(1)
if ready and mode=='invalid-control': result.stdout=b'not-a-ready-receipt\n'
if ready and mode=='blocked-control': time.sleep(5)
if mode=='blocked-completion' and '-c' in args and args[args.index('-c')+1].startswith('test -f'): time.sleep(5)
if mode=='stop-proof' and is_inspect and marker.with_suffix('.killed').exists() and not marker.exists():
    marker.touch()
    sys.exit(1)
if mode=='restart-proof' and is_inspect and marker.with_suffix('.started').exists() and not marker.exists():
    marker.touch()
    sys.exit(1)
if mode=='cancel-ready' and ready or mode=='cancel-stop' and is_kill or mode=='cancel-restart' and is_start:
    marker.touch()
    time.sleep(5)
if mode=='cancel-stopped' and is_inspect and marker.with_suffix('.killed').exists() and not marker.exists():
    marker.touch()
    time.sleep(5)
if mode=='restart-proof' and is_start: marker.with_suffix('.started').touch()
sys.stdout.buffer.write(result.stdout)
sys.stderr.buffer.write(result.stderr)
sys.exit(result.returncode)
"#.replace("MODE", &serde_json::to_string(mode).unwrap()).replace("MARKER", &serde_json::to_string(&marker).unwrap());
        std::fs::write(&executable, script).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        self.docker = theseus::runner::Docker::new(executable);
        let recipe =
            recipe::load(std::path::Path::new("tests/fixtures/lifecycle/recipe.toml")).unwrap();
        self.docker
            .prepare(&recipe, false, &CancellationToken::default())
            .unwrap();
        marker
    }
    fn interrupt(&mut self, cancel: &CancellationToken) -> theseus::runner::ExecutedCommand {
        use theseus::runner::Environment;
        self.docker
            .execute_interrupted(
                &self.resource,
                Self::request(
                    "interrupt",
                    "/bin/sh \"$THESEUS_INTERRUPT_BARRIER\"",
                    ExecutionIdentity::runtime(),
                ),
                &self.store,
                cancel,
            )
            .unwrap()
    }
}

#[test]
#[ignore = "requires Docker"]
fn real_interruption_unverified_transport_and_stop_never_mint_planned_success() {
    for mode in [
        "transport",
        "invalid-control",
        "blocked-control",
        "kill-failed",
        "stop-proof",
    ] {
        let mut boundary = InterruptionBoundary::new();
        boundary.proxy(mode);
        let started = std::time::Instant::now();
        let result = boundary.interrupt(&CancellationToken::default());
        assert_eq!(
            result.record.outcome,
            CommandOutcome::Error,
            "{mode}: {:?}",
            result.record
        );
        assert!(result.record.error.is_some());
        assert_eq!(result.record.exit_status, None);
        assert!(
            !result.record.timed_out && !result.record.cancelled,
            "{mode}"
        );
        assert!(
            !result.record.interruption.unwrap().stopped_verified,
            "{mode}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(4),
            "{mode}"
        );
        boundary.assert_absent();
    }
}

fn cancel_on_marker(
    marker: std::path::PathBuf,
    cancel: CancellationToken,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !marker.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(marker.exists(), "transition marker was not reached");
        cancel.cancel();
    })
}

#[test]
#[ignore = "requires Docker"]
fn real_interruption_cancellation_at_ready_kill_and_stopped_removes_container() {
    for mode in [
        "cancel-ready",
        "cancel-stop",
        "cancel-stopped",
        "cancel-before",
    ] {
        let mut boundary = InterruptionBoundary::new();
        let marker = boundary.proxy(mode);
        let cancel = CancellationToken::default();
        let signal = if mode == "cancel-before" {
            cancel.cancel();
            None
        } else {
            Some(cancel_on_marker(marker, cancel.clone()))
        };
        let result = boundary.interrupt(&cancel);
        if let Some(signal) = signal {
            signal.join().unwrap();
        }
        assert_eq!(
            result.record.outcome,
            CommandOutcome::Cancelled,
            "{mode}: {:?}",
            result.record
        );
        assert!(result.record.cancelled);
        assert_eq!(result.record.exit_status, None);
        boundary.assert_absent();
    }
}

#[test]
#[ignore = "requires Docker"]
fn real_interruption_restart_errors_or_cancellation_remove_without_forward_mutation() {
    use theseus::runner::Environment;
    for mode in [
        "restart-failed",
        "restart-proof",
        "cancel-restart",
        "cancel-before-restart",
    ] {
        let mut boundary = InterruptionBoundary::new();
        let marker = boundary.proxy(mode);
        let stopped = boundary.interrupt(&CancellationToken::default());
        assert_eq!(stopped.record.outcome, CommandOutcome::Interrupted);
        let cancel = CancellationToken::default();
        let signal = if mode == "cancel-before-restart" {
            cancel.cancel();
            None
        } else if mode == "cancel-restart" {
            Some(cancel_on_marker(marker, cancel.clone()))
        } else {
            None
        };
        assert!(
            boundary
                .docker
                .restart(&boundary.resource, &cancel)
                .is_err(),
            "{mode}"
        );
        if let Some(signal) = signal {
            signal.join().unwrap();
        }
        boundary.assert_absent();
    }
}

#[test]
#[ignore = "requires Docker"]
fn real_interruption_early_completion_control_is_bounded_independently_of_command() {
    use theseus::runner::Environment;
    let mut boundary = InterruptionBoundary::new();
    boundary.proxy("blocked-completion");
    let started = std::time::Instant::now();
    let result = boundary
        .docker
        .execute_interrupted(
            &boundary.resource,
            InterruptionBoundary::request("early", "exit 0", ExecutionIdentity::runtime()),
            &boundary.store,
            &CancellationToken::default(),
        )
        .unwrap();
    assert_eq!(result.record.outcome, CommandOutcome::Error);
    assert!(!result.record.timed_out);
    // Includes preparation, ownership inspections, and owned removal under
    // concurrent daemon load. The injected receipt read alone stalls for 5s.
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    boundary.assert_absent();
}

#[test]
#[ignore = "requires Docker"]
fn real_interruption_restart_retains_primary_and_cleanup_errors() {
    use theseus::runner::Environment;
    let mut boundary = InterruptionBoundary::new();
    boundary.proxy("restart-error-cleanup");
    assert_eq!(
        boundary
            .interrupt(&CancellationToken::default())
            .record
            .outcome,
        CommandOutcome::Interrupted
    );
    let error = boundary
        .docker
        .restart(&boundary.resource, &CancellationToken::default())
        .unwrap_err();
    let text = format!("{error:#}");
    assert!(
        text.contains("container transition failed"),
        "lost primary restart failure: {text}"
    );
    assert!(
        text.contains("owned restart cleanup failed"),
        "lost cleanup failure: {text}"
    );
    // Drop uses the real Docker adapter, so the injected removal error cannot
    // leak the stopped resource after this boundary test.
}
