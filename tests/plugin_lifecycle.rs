//! Synthetic replay acceptance of the public Docker engine, not Claude compatibility.
#[path = "support/docker_absence.rs"]
mod docker_absence;
#[path = "support/plugin.rs"]
mod fixture;
use std::fs;
use theseus::{
    engine::{self, CancellationToken, EventKind, RunOptions},
    model::*,
    recipe::{self, Expectation},
};

fn run(scope: &str, fault: &str, version: Option<&str>, cancel_update: bool) -> engine::RunResult {
    let input = tempfile::tempdir().unwrap();
    let client = input.path().join("dummy-ELF-never-executed");
    fs::write(&client, b"\x7fELFsynthetic-preparation-only").unwrap();
    let staged = fixture::stage_plugin(&client, "2.1.312 (Claude Code)", scope);
    let root = staged.path().join("prepared");
    fixture::replace_with_replay(&root);
    let mut recipe = recipe::load(&root.join("theseus.toml")).unwrap();
    recipe.recipe.name = format!("SYNTHETIC replay scope={scope} fault={fault}");
    recipe.recipe.image =
        "sha256:534baea6a22c03a63003dbc8dbe78fe34bc0d7e595d9a9dc9834884ff530eb55".into();
    for profile in recipe.recipe.profiles.values_mut() {
        for step in profile.steps.values_mut() {
            step.env.insert("THESEUS_REPLAY_FAULT".into(), fault.into());
            if fault == "hang-update" || cancel_update {
                step.timeout_seconds = 2;
            }
        }
        for check in &mut profile.checks {
            match &mut check.expectation {
                Expectation::JsonCommand { env, .. } | Expectation::Command { env, .. } => {
                    env.insert("THESEUS_REPLAY_FAULT".into(), fault.into());
                    if let Some(v) = version {
                        env.insert("THESEUS_REPLAY_VERSION".into(), v.into());
                    }
                }
                _ => unreachable!(),
            }
        }
    }
    let evidence =
        std::path::Path::new(".superpowers/sdd/2026-10-30-plugin-lifecycle/task-3-replay-runs");
    fs::create_dir_all(evidence).unwrap();
    let attempt = tempfile::Builder::new()
        .prefix(&format!("synthetic-{scope}-{fault}-"))
        .tempdir_in(evidence)
        .unwrap()
        .keep();
    fs::copy(
        root.join("input-manifest.json"),
        attempt.join("synthetic-input-manifest.json"),
    )
    .unwrap();
    let options = RunOptions {
        output_root: attempt,
        log_limit: 16384,
        ..RunOptions::all(&recipe)
    };
    let token = CancellationToken::default();
    let signal = token.clone();
    let result = engine::run(&recipe, options, token, move |event| {
        if cancel_update
            && event.kind == EventKind::StageStarted
            && event.stage.as_deref() == Some("upgrade")
        {
            let signal = signal.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(500));
                signal.cancel();
            });
        }
    })
    .unwrap();
    println!(
        "SYNTHETIC scope={scope} fault={fault}: {:?} {}",
        result.report.outcome,
        result.report_path.display()
    );
    assert!(
        result.report.errors.is_empty(),
        "Docker prerequisite/run failure: {:?}",
        result.report.errors
    );
    for scenario in &result.report.scenarios {
        assert_eq!(scenario.teardown, TeardownStatus::Complete);
        docker_absence::absent(scenario.container_id.as_ref().expect("owned container"));
        for command in scenario.stages.iter().flat_map(|s| &s.commands) {
            assert_eq!(command.identity.uid, 10001);
            assert!(command.forwarded_environment.is_empty());
        }
    }
    let saved = theseus::report::load(result.report_path.parent().unwrap()).unwrap();
    assert_eq!(saved.outcome, result.report.outcome);
    result
}

#[test]
#[ignore = "requires Docker; synthetic shell replay, not native client acceptance"]
fn synthetic_healthy_all_scopes_pass_five_checkpoints() {
    for scope in ["user", "project", "local"] {
        let result = run(scope, "", None, false);
        assert_eq!(
            result.report.outcome,
            RunOutcome::Passed,
            "{:#?}",
            result.report
        );
        assert_eq!(
            result.report.scenarios[0]
                .stages
                .iter()
                .filter(|s| s.checkpoint.is_some())
                .count(),
            5
        );
    }
}

#[test]
#[ignore = "requires Docker; synthetic shell replay"]
fn synthetic_inventory_preservation_components_and_mutation_failures_stay_failed() {
    for fault in [
        "duplicate-target",
        "wrong-scope",
        "stale-version",
        "remove-companion",
        "delete-setting",
        "missing-component",
        "wrong-hook",
        "fail-install",
        "fail-update",
        "fail-disable",
        "fail-enable",
        "fail-uninstall",
        "malformed-list",
        "duplicate-key-list",
        "invalid-utf8-list",
    ] {
        let result = run("project", fault, None, false);
        assert_eq!(
            result.report.outcome,
            RunOutcome::Failed,
            "{fault}: {:#?}",
            result.report
        );
        let scenario = &result.report.scenarios[0];
        let checks: Vec<_> = scenario.stages.iter().flat_map(|s| &s.checks).collect();
        let commands: Vec<_> = scenario.stages.iter().flat_map(|s| &s.commands).collect();
        if fault.starts_with("fail-") {
            let phase = match fault {
                "fail-install" => "install_base",
                "fail-update" => "upgrade",
                "fail-disable" => "disable_plugin",
                "fail-enable" => "enable_plugin",
                "fail-uninstall" => "uninstall",
                _ => unreachable!(),
            };
            let command = commands.iter().find(|c| c.phase == phase).unwrap();
            assert_eq!(command.exit_status, Some(42), "{fault}");
            assert_eq!(command.outcome, CommandOutcome::Failed, "{fault}");
        } else {
            assert!(
                checks
                    .iter()
                    .filter(|c| c.checkpoint == Checkpoint::BaseInstalled)
                    .all(|c| c.outcome == CheckOutcome::Passed),
                "base must be healthy before {fault}"
            );
            let id = match fault {
                "delete-setting" => "target-scope-settings",
                "missing-component" | "wrong-hook" => "target-components",
                _ => "target-inventory",
            };
            let check = checks.iter().find(|c| c.id == id).unwrap();
            assert_eq!(
                check.outcome,
                CheckOutcome::Failed,
                "intended proof {fault}"
            );
            assert_eq!(
                commands
                    .iter()
                    .find(|c| c.phase == "upgrade")
                    .unwrap()
                    .exit_status,
                Some(0),
                "fault injection must follow successful update: {fault}"
            );
            if fault == "duplicate-target" {
                assert!(matches!(
                    check.json_expectation.as_ref().unwrap().observations[0],
                    JsonAssertionObservation::ArrayMatch {
                        matched_count: 2,
                        ..
                    }
                ));
            }
        }
    }
}

#[test]
#[ignore = "requires Docker; synthetic shell replay"]
fn synthetic_truncated_inventory_timeout_and_cancellation_are_incomplete_or_cancelled() {
    for (fault, cancel, outcome) in [
        ("truncate-list", false, RunOutcome::Incomplete),
        ("hang-update", false, RunOutcome::Incomplete),
        ("hang-update", true, RunOutcome::Cancelled),
    ] {
        let result = run("local", fault, None, cancel);
        assert_eq!(
            result.report.outcome, outcome,
            "{fault}: {:#?}",
            result.report
        );
        let scenario = &result.report.scenarios[0];
        assert!(
            scenario
                .stages
                .iter()
                .flat_map(|s| &s.checks)
                .filter(|c| c.checkpoint == Checkpoint::BaseInstalled)
                .all(|c| c.outcome == CheckOutcome::Passed)
        );
        if fault == "truncate-list" {
            let check = scenario
                .stages
                .iter()
                .flat_map(|s| &s.checks)
                .find(|c| c.id == "target-inventory")
                .unwrap();
            assert_eq!(check.outcome, CheckOutcome::Incomplete);
            assert!(!check.json_expectation.as_ref().unwrap().complete);
        } else {
            let command = scenario
                .stages
                .iter()
                .flat_map(|s| &s.commands)
                .find(|c| c.phase == "upgrade")
                .unwrap();
            assert_eq!(command.cancelled, cancel);
            assert_eq!(command.timed_out, !cancel);
        }
        assert!(
            !result.report.scenarios[0]
                .stages
                .iter()
                .flat_map(|s| &s.commands)
                .any(|c| c.phase == "enable_plugin")
        );
    }
}

#[test]
#[ignore = "requires Docker; synthetic version-output boundary"]
fn synthetic_version_adapter_rejects_unknown_output_instead_of_sanitizing() {
    for version in [
        "",
        "bad\"version",
        "first\nsecond",
        "trailing\n\n",
        "control\r",
        "\\escaped",
    ] {
        let result = run("user", "", Some(version), false);
        assert_eq!(result.report.outcome, RunOutcome::Failed, "{version:?}");
        assert_version_capture_rejected(&result);
    }
}

fn assert_version_capture_rejected(result: &engine::RunResult) {
    let command = result.report.scenarios[0]
        .stages
        .iter()
        .flat_map(|s| &s.commands)
        .find(|c| c.phase == "check:client-version")
        .unwrap();
    assert_ne!(
        command.exit_status,
        Some(0),
        "unrecognized bytes must be rejected, never sanitized into JSON"
    );
}

#[test]
#[ignore = "requires Docker; synthetic raw-byte version boundary"]
fn synthetic_version_adapter_rejects_nul_before_command_substitution() {
    let result = run("user", "", Some("__synthetic_version_with_nul__"), false);
    assert_version_capture_rejected(&result);
}
