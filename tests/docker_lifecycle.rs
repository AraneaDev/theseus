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
