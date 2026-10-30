//! Actual daemon acceptance: the tool, reader, receipts and filesystem evidence are real.
#[path = "support/docker_absence.rs"]
mod docker_absence;
use docker_absence::absent;
use std::{path::Path, process::Command};
use theseus::{
    engine::{self, CancellationToken, EventKind, RunOptions},
    model::*,
    recipe,
};

const DATA: &str = "/home/theseus/.local/share/sample-cli";
const RECORDS: &str = "/home/theseus/.local/share/sample-cli/records";
const SETTINGS: &str = "/home/theseus/.config/sample-cli/settings";

fn stage<'a>(scenario: &'a ScenarioReport, name: &str) -> &'a StageReport {
    scenario.stages.iter().find(|s| s.name == name).unwrap()
}
fn entry<'a>(stage: &'a StageReport, path: &str) -> Option<&'a theseus::snapshot::FileEntry> {
    stage.snapshots.iter().find_map(|s| s.entries.get(path))
}
fn stdout(result: &engine::RunResult, command: &CommandRecord) -> String {
    std::fs::read_to_string(
        result
            .report_path
            .parent()
            .unwrap()
            .join(&command.stdout.as_ref().unwrap().path),
    )
    .unwrap()
}
fn hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
fn run(
    policy: &str,
    fault: &str,
    profiles: Vec<ProfileName>,
) -> (tempfile::TempDir, engine::RunResult) {
    let temp = tempfile::tempdir().unwrap();
    let mut recipe = recipe::load(Path::new(&format!(
        "tests/fixtures/downgrade/{policy}.toml"
    )))
    .unwrap();
    for profile in recipe.recipe.profiles.values_mut() {
        let step = profile.steps.get_mut(&StepName::Downgrade).unwrap();
        step.env
            .insert("THESEUS_FIXTURE_FAULT".into(), fault.into());
        if matches!(fault, "timeout" | "cancel") {
            step.timeout_seconds = if fault == "timeout" { 2 } else { 30 };
        }
        if fault == "control-one"
            && let Some(recipe::DowngradeContract::SafeRejection { expected_exit, .. }) =
                &mut profile.downgrade
        {
            *expected_exit = 1;
        }
    }
    let options = RunOptions {
        profiles,
        scenarios: vec![ScenarioKind::Downgrade],
        output_root: temp.path().join("runs"),
        log_limit: 4096,
        ..RunOptions::all(&recipe)
    };
    let cancel = CancellationToken::default();
    let signal = cancel.clone();
    let executable = temp.path().join("docker-proxy");
    let marker = temp.path().join("attempt");
    let reference_marker = temp.path().join("reference");
    std::fs::write(&executable, format!(r#"#!/usr/bin/python3
import pathlib, subprocess, sys
args=sys.argv[1:]
mode={fault:?}
marker=pathlib.Path({marker:?})
reference=pathlib.Path({reference:?})
wrapper='theseus-command' in args
attempt=wrapper and any('/inputs/base/install.sh' in arg for arg in args)
receipt='-c' in args and args[args.index('-c')+1].startswith('test -f')
if mode=='unavailable-reference' and args[0]=='cp' and reference.exists() and args[1].endswith(':/home/theseus'):
    reference.unlink()
    sys.exit(1)
if attempt:
    marker.touch()
    if mode=='control-one':
        print('data format is newer than supported')
        sys.exit(1)
if receipt and marker.exists() and mode=='transport': sys.exit(1)
result=subprocess.run(['docker', *args])
if attempt and mode=='missing-receipt':
    path=args[args.index('theseus-command')+1]
    subprocess.run(['docker','exec',args[args.index('--workdir')+2],'rm','-f',path],check=True)
sys.exit(result.returncode)
"#, marker=marker.to_str().unwrap(), reference=reference_marker.to_str().unwrap())).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut docker = theseus::runner::Docker::new(executable);
    let result = engine::run_with(&recipe, options, cancel, &mut docker, move |event| {
        if event.kind == EventKind::StageFinished
            && event.stage.as_deref() == Some("exercise_before_downgrade")
            && fault == "unavailable-reference"
        {
            std::fs::write(&reference_marker, "reference").unwrap();
        }
        if event.kind == EventKind::StageStarted
            && event.stage.as_deref() == Some("downgrade")
            && fault == "cancel"
        {
            let signal = signal.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(900));
                signal.cancel();
            });
        }
    })
    .unwrap();
    assert!(
        result.report.errors.is_empty(),
        "Docker prerequisites: {:?}",
        result.report.errors
    );
    let journal = theseus::report::load_journal(result.report_path.parent().unwrap()).unwrap();
    assert_eq!(journal.schema_version, 1);
    assert!(journal.cleanup_errors.is_empty());
    assert_eq!(journal.resources.len(), result.report.scenarios.len());
    for resource in &journal.resources {
        assert!(resource.removed);
        assert_eq!(resource.labels["io.theseus.owner"], "v1");
        assert_eq!(resource.labels["io.theseus.run"], result.report.run_id);
        assert!(
            result
                .report
                .scenarios
                .iter()
                .any(|s| s.container_id.as_ref() == Some(&resource.container_id))
        );
        absent(&resource.container_id);
    }
    for scenario in &result.report.scenarios {
        assert!(matches!(scenario.teardown, TeardownStatus::Complete));
    }
    assert_eq!(
        theseus::report::load(&result.report_path).unwrap().outcome,
        result.report.outcome
    );
    if let Ok(destination) = std::env::var("THESEUS_ACCEPTANCE_EVIDENCE") {
        // Copy complete real reports/logs outside temporary test storage for review.
        let destination = Path::new(&destination).join(format!(
            "{policy}-{}-{}",
            if fault.is_empty() { "healthy" } else { fault },
            result.report.run_id
        ));
        std::fs::create_dir_all(&destination).unwrap();
        let copied = Command::new("cp")
            .args(["-a"])
            .arg(result.report_path.parent().unwrap().join("."))
            .arg(&destination)
            .status()
            .unwrap();
        assert!(copied.success());
        println!("Evidence: {}", destination.display());
    }
    (temp, result)
}

#[test]
#[ignore = "requires reachable Docker daemon and local ubuntu:24.04"]
fn real_downgrade_four_way_matrix_reads_newer_records_or_proves_exact_refusal() {
    for policy in ["compatible", "rejection"] {
        let (_temp, result) = run(policy, "", vec![ProfileName::User, ProfileName::System]);
        assert_eq!(
            result.report.outcome,
            RunOutcome::Passed,
            "{policy}: {:#?}",
            result.report
        );
        assert_eq!(result.report.scenarios.len(), 2);
        assert_eq!(result.report.artifacts.len(), 2);
        for role in ["target", "base"] {
            let manifest = result
                .report
                .artifacts
                .iter()
                .find(|a| a.name == role)
                .unwrap();
            let file = manifest
                .files
                .iter()
                .find(|f| f.path == "sample-cli")
                .unwrap();
            let bytes =
                std::fs::read(format!("tests/fixtures/downgrade/{role}/sample-cli")).unwrap();
            assert_eq!(file.sha256, hash(&bytes));
            assert_eq!(file.size, bytes.len() as u64);
        }
        for scenario in &result.report.scenarios {
            let binary = if scenario.profile == ProfileName::User {
                "/home/theseus/.local/bin/sample-cli"
            } else {
                "/usr/local/bin/sample-cli"
            };
            let checkpoint = if policy == "compatible" {
                "downgraded"
            } else {
                "downgrade_rejected"
            };
            let exercise = if policy == "compatible" {
                "exercise_downgraded"
            } else {
                "exercise_after_rejection"
            };
            assert_eq!(
                scenario
                    .stages
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>(),
                [
                    "baseline",
                    "install_target",
                    "target_installed",
                    "target_installed:after_checks",
                    "configure",
                    "configured",
                    "configured:after_checks",
                    "exercise_before_downgrade",
                    "before_downgrade",
                    "downgrade",
                    checkpoint,
                    &format!("{checkpoint}:after_checks"),
                    exercise,
                    "uninstall",
                    "uninstalled",
                    "uninstalled:after_checks",
                    "final"
                ]
            );
            let before = stage(scenario, "before_downgrade");
            let after = stage(scenario, checkpoint);
            let installed = entry(stage(scenario, "target_installed"), binary).unwrap();
            assert_eq!(
                installed.sha256.as_deref(),
                Some(
                    hash(&std::fs::read("tests/fixtures/downgrade/target/sample-cli").unwrap())
                        .as_str()
                )
            );
            assert_eq!(installed.mode & 0o777, 0o755);
            assert_eq!(
                installed.uid,
                if scenario.profile == ProfileName::System {
                    0
                } else {
                    10001
                }
            );
            assert_eq!(installed.gid, installed.uid);
            let role = if policy == "compatible" {
                "base"
            } else {
                "target"
            };
            assert_eq!(
                entry(after, binary).unwrap().sha256.as_deref(),
                Some(
                    hash(
                        &std::fs::read(format!("tests/fixtures/downgrade/{role}/sample-cli"))
                            .unwrap()
                    )
                    .as_str()
                )
            );
            assert_eq!(
                entry(before, RECORDS).unwrap().sha256.as_deref(),
                Some(hash(b"format=2\nalpha=11\nbeta=29\n").as_str())
            );
            assert_eq!(entry(before, RECORDS).unwrap().uid, 10001);
            assert_eq!(entry(before, RECORDS), entry(after, RECORDS));
            assert_eq!(
                entry(before, RECORDS),
                entry(stage(scenario, "uninstalled"), RECORDS)
            );
            assert_eq!(entry(before, SETTINGS), entry(after, SETTINGS));
            assert!(entry(stage(scenario, "uninstalled"), binary).is_none());
            assert!(
                scenario
                    .stages
                    .iter()
                    .flat_map(|s| &s.checks)
                    .all(|c| c.outcome == CheckOutcome::Passed)
            );
            for check in &after.checks {
                if let Some(reference) = &check.comparison {
                    assert_eq!(reference.before_stage, "before_downgrade");
                    assert_eq!(reference.after_stage, checkpoint);
                }
            }
            let attempt = stage(scenario, "downgrade");
            let command = &attempt.commands[0];
            assert!(command.error.is_none() && !command.cancelled && !command.timed_out);
            assert_eq!(
                command.exit_status,
                Some(if policy == "compatible" { 0 } else { 9 })
            );
            assert_eq!(
                command.outcome,
                if policy == "compatible" {
                    CommandOutcome::Succeeded
                } else {
                    CommandOutcome::ExpectedRejection
                }
            );
            assert_eq!(attempt.verified_rejection(), policy == "rejection");
            assert!(
                stdout(&result, &stage(scenario, exercise).commands[0]).contains(
                    if policy == "compatible" {
                        "old-reader records=2 sum=40"
                    } else {
                        "new-reader records=2 sum=40"
                    }
                )
            );
            assert!(
                stdout(
                    &result,
                    &stage(scenario, "exercise_before_downgrade").commands[0]
                )
                .contains("format=2\nalpha=11\nbeta=29\nnew-reader records=2 sum=40")
            );
            for stage in &scenario.stages {
                for command in &stage.commands {
                    let installer = matches!(
                        command.phase.as_str(),
                        "install_target" | "downgrade" | "uninstall"
                    );
                    assert_eq!(
                        command.identity.uid,
                        if installer && scenario.profile == ProfileName::System {
                            0
                        } else {
                            10001
                        }
                    );
                    assert_eq!(command.identity.gid, command.identity.uid);
                    if command.phase.starts_with("exercise") {
                        assert!(stdout(&result, command).contains("runtime uid=10001 gid=10001"));
                    }
                }
            }
        }
    }
}

#[test]
#[ignore = "requires Docker"]
fn real_compatible_faults_fail_the_actual_old_reader_or_exact_stable_scope() {
    for (fault, check_id, path) in [
        ("broken-reader", "older-data", None),
        ("silent-reader-drop", "older-data", None),
        ("silent-drop", "data-stable", Some(DATA)),
        ("reset-config", "settings-stable", Some(SETTINGS)),
    ] {
        let (_temp, result) = run("compatible", fault, vec![ProfileName::User]);
        assert_eq!(
            result.report.outcome,
            RunOutcome::Failed,
            "{fault}: {:#?}",
            result.report
        );
        let scenario = &result.report.scenarios[0];
        prerequisites(scenario);
        let attempt = &stage(scenario, "downgrade").commands[0];
        assert_eq!(attempt.exit_status, Some(0));
        assert_eq!(attempt.outcome, CommandOutcome::Succeeded);
        let checkpoint = stage(scenario, "downgraded");
        let check = checkpoint.checks.iter().find(|c| c.id == check_id).unwrap();
        assert_eq!(check.outcome, CheckOutcome::Failed, "{fault}: {check:?}");
        assert_eq!(check.path.as_deref(), path);
        if matches!(fault, "broken-reader" | "silent-reader-drop") {
            let command = checkpoint
                .commands
                .iter()
                .find(|c| c.phase == "check:older-data")
                .unwrap();
            assert_eq!(
                command.exit_status,
                Some(if fault == "broken-reader" { 8 } else { 0 })
            );
            if fault == "silent-reader-drop" {
                assert!(stdout(&result, command).contains("old-reader records=1 sum=11"));
                assert_eq!(
                    entry(stage(scenario, "before_downgrade"), RECORDS),
                    entry(checkpoint, RECORDS)
                );
            }
            assert!(command.error.is_none());
            assert!(!stdout(&result, command).contains("old-reader records=2 sum=40"));
        } else {
            assert_eq!(
                check.comparison,
                Some(ComparisonReference {
                    before_stage: "before_downgrade".into(),
                    after_stage: "downgraded".into()
                })
            );
            let changed = if fault == "silent-drop" {
                RECORDS
            } else {
                SETTINGS
            };
            assert_ne!(
                entry(stage(scenario, "before_downgrade"), changed),
                entry(checkpoint, changed)
            );
        }
        assert!(stage(scenario, "exercise_downgraded").commands.is_empty());
        assert_eq!(stage(scenario, "uninstall").outcome, StageOutcome::Passed);
    }
}
fn prerequisites(scenario: &ScenarioReport) {
    for name in [
        "install_target",
        "target_installed",
        "configure",
        "configured",
        "exercise_before_downgrade",
    ] {
        assert_eq!(
            stage(scenario, name).outcome,
            StageOutcome::Passed,
            "{name}"
        );
    }
    assert_eq!(
        entry(stage(scenario, "before_downgrade"), RECORDS)
            .unwrap()
            .sha256
            .as_deref(),
        Some(hash(b"format=2\nalpha=11\nbeta=29\n").as_str())
    );
}

#[test]
#[ignore = "requires Docker"]
fn real_rejection_mutations_fail_exact_binary_data_or_configuration_paths() {
    for (fault, check_id, scope, changed) in [
        (
            "binary-change",
            "binary-stable",
            "/home/theseus/.local/bin/sample-cli",
            "/home/theseus/.local/bin/sample-cli",
        ),
        (
            "mode-change",
            "binary-stable",
            "/home/theseus/.local/bin/sample-cli",
            "/home/theseus/.local/bin/sample-cli",
        ),
        (
            "link-change",
            "binary-stable",
            "/home/theseus/.local/bin/sample-cli",
            "/home/theseus/.local/bin/sample-cli",
        ),
        (
            "add-child",
            "data-stable",
            DATA,
            "/home/theseus/.local/share/sample-cli/extra",
        ),
        ("remove-child", "data-stable", DATA, RECORDS),
        ("reset-config", "settings-stable", SETTINGS, SETTINGS),
    ] {
        let (_temp, result) = run("rejection", fault, vec![ProfileName::User]);
        assert_eq!(
            result.report.outcome,
            RunOutcome::Failed,
            "{fault}: {:#?}",
            result.report
        );
        let scenario = &result.report.scenarios[0];
        prerequisites(scenario);
        let attempt = stage(scenario, "downgrade");
        assert!(
            attempt.verified_rejection(),
            "the real status/marker matches but preservation fails"
        );
        assert_eq!(attempt.commands[0].exit_status, Some(9));
        let checkpoint = stage(scenario, "downgrade_rejected");
        let check = checkpoint.checks.iter().find(|c| c.id == check_id).unwrap();
        assert_eq!(check.outcome, CheckOutcome::Failed, "{fault}: {check:?}");
        assert_eq!(check.path.as_deref(), Some(scope));
        assert_eq!(
            check.comparison,
            Some(ComparisonReference {
                before_stage: "before_downgrade".into(),
                after_stage: "downgrade_rejected".into()
            })
        );
        assert_ne!(
            entry(stage(scenario, "before_downgrade"), changed),
            entry(checkpoint, changed),
            "{fault}/{changed}"
        );
        match fault {
            "mode-change" => assert_eq!(entry(checkpoint, changed).unwrap().mode & 0o777, 0o700),
            "link-change" => {
                assert_eq!(entry(checkpoint, changed).unwrap().kind, FileKind::Symlink);
                assert_eq!(
                    entry(checkpoint, changed).unwrap().link_target.as_deref(),
                    Some("/inputs/target/sample-cli")
                );
            }
            "add-child" => assert!(entry(checkpoint, changed).is_some()),
            "remove-child" => assert!(entry(checkpoint, changed).is_none()),
            _ => {}
        }
        assert!(
            stage(scenario, "exercise_after_rejection")
                .commands
                .is_empty()
        );
        assert_eq!(stage(scenario, "uninstall").outcome, StageOutcome::Passed);
    }
}

#[test]
#[ignore = "requires Docker"]
fn real_refusal_wrong_status_marker_success_and_failed_retained_exercise_are_failed() {
    for (fault, status, marker, exercise) in [
        ("wrong-status", 8, true, false),
        ("wrong-marker", 9, false, false),
        ("missing-marker", 9, false, false),
        ("unexpected-zero", 0, true, false),
        ("exercise-fails", 9, true, true),
    ] {
        let (_temp, result) = run("rejection", fault, vec![ProfileName::User]);
        assert_eq!(
            result.report.outcome,
            RunOutcome::Failed,
            "{fault}: {:#?}",
            result.report
        );
        let scenario = &result.report.scenarios[0];
        prerequisites(scenario);
        let attempt = stage(scenario, "downgrade");
        let command = &attempt.commands[0];
        assert_eq!(command.exit_status, Some(status));
        assert!(command.error.is_none() && !command.timed_out && !command.cancelled);
        assert_eq!(
            stdout(&result, command).contains("data format is newer than supported"),
            marker
        );
        assert_eq!(
            command.outcome,
            if exercise {
                CommandOutcome::ExpectedRejection
            } else {
                CommandOutcome::Failed
            }
        );
        assert_eq!(attempt.verified_rejection(), exercise);
        let checkpoint = stage(scenario, "downgrade_rejected");
        assert!(
            checkpoint
                .checks
                .iter()
                .all(|c| c.outcome == CheckOutcome::Passed)
        );
        let after = stage(scenario, "exercise_after_rejection");
        assert_eq!(!after.commands.is_empty(), exercise);
        if exercise {
            assert_eq!(after.outcome, StageOutcome::Failed);
            assert_eq!(after.commands[0].exit_status, Some(1));
        }
        assert_eq!(stage(scenario, "uninstall").outcome, StageOutcome::Passed);
    }
}

#[test]
#[ignore = "requires Docker"]
fn real_missing_truncated_transport_control_one_and_unavailable_reference_are_incomplete() {
    for fault in [
        "missing-receipt",
        "truncated",
        "transport",
        "control-one",
        "unavailable-reference",
    ] {
        let (_temp, result) = run("rejection", fault, vec![ProfileName::User]);
        assert_eq!(
            result.report.outcome,
            RunOutcome::Incomplete,
            "{fault}: {:#?}",
            result.report
        );
        let scenario = &result.report.scenarios[0];
        if fault != "unavailable-reference" {
            prerequisites(scenario);
        }
        let attempt = stage(scenario, "downgrade");
        if fault == "unavailable-reference" {
            for name in [
                "install_target",
                "target_installed",
                "configure",
                "configured",
                "exercise_before_downgrade",
            ] {
                assert_eq!(
                    stage(scenario, name).outcome,
                    StageOutcome::Passed,
                    "{fault}/{name}: {scenario:#?}"
                );
            }
            let reference = stage(scenario, "before_downgrade");
            assert_eq!(reference.outcome, StageOutcome::Incomplete, "{fault}");
            assert_eq!(
                reference
                    .snapshots
                    .iter()
                    .map(|snapshot| snapshot.root.as_str())
                    .collect::<Vec<_>>(),
                ["/home/theseus", "/usr/local/bin", "/workspace"],
                "{fault}: {reference:#?}"
            );
            assert!(matches!(
                &reference.snapshots[0].completeness,
                theseus::snapshot::ObservationStatus::Incomplete { reason } if !reason.is_empty()
            ));
            assert!(reference.snapshots[1..].iter().all(|snapshot| matches!(
                snapshot.completeness,
                theseus::snapshot::ObservationStatus::Complete
            )));
            assert_eq!(attempt.outcome, StageOutcome::Skipped, "{fault}");
            assert_eq!(attempt.skip_reason.as_deref(), Some("failed prerequisite"));
            assert!(attempt.commands.is_empty(), "{fault}: {attempt:#?}");
            assert!(!attempt.verified_rejection());

            // A later complete capture cannot rehabilitate the lost reference.
            for name in ["downgrade_rejected", "downgrade_rejected:after_checks"] {
                let checkpoint = stage(scenario, name);
                assert_eq!(checkpoint.snapshots.len(), 3, "{fault}/{name}");
                assert!(checkpoint.snapshots.iter().all(|snapshot| matches!(
                    snapshot.completeness,
                    theseus::snapshot::ObservationStatus::Complete
                )));
                assert!(checkpoint.commands.is_empty(), "{fault}: {checkpoint:#?}");
            }
            let checkpoint = stage(scenario, "downgrade_rejected");
            assert_eq!(checkpoint.checks.len(), 4, "{fault}: {checkpoint:#?}");
            for (id, path) in [
                ("retained-version", None),
                ("binary-stable", Some("/home/theseus/.local/bin/sample-cli")),
                ("data-stable", Some(DATA)),
                ("settings-stable", Some(SETTINGS)),
            ] {
                let check = checkpoint
                    .checks
                    .iter()
                    .find(|check| check.id == id)
                    .unwrap_or_else(|| panic!("{fault}: missing {id}: {checkpoint:#?}"));
                assert_eq!(check.outcome, CheckOutcome::Skipped, "{fault}/{id}");
                assert_eq!(check.path.as_deref(), path, "{fault}/{id}");
                assert!(check.command_ids.is_empty(), "{fault}/{id}");
                if path.is_some() {
                    assert_eq!(
                        check.comparison,
                        Some(ComparisonReference {
                            before_stage: "before_downgrade".into(),
                            after_stage: "downgrade_rejected".into()
                        }),
                        "{fault}/{id}"
                    );
                }
            }
            let after = stage(scenario, "exercise_after_rejection");
            assert_eq!(after.outcome, StageOutcome::Skipped, "{fault}");
            assert_eq!(after.skip_reason.as_deref(), Some("failed prerequisite"));
            let uninstall = stage(scenario, "uninstall");
            assert_eq!(uninstall.outcome, StageOutcome::Passed, "{fault}");
            assert_eq!(uninstall.commands.len(), 1, "{fault}");
        } else {
            assert_eq!(attempt.commands.len(), 1, "{fault}: {attempt:#?}");
            let command = &attempt.commands[0];
            assert!(!attempt.verified_rejection());
            assert_eq!(command.outcome, CommandOutcome::Error);
            assert!(command.rejection.is_none());
            if fault == "truncated" {
                assert_eq!(command.exit_status, Some(9));
                assert!(command.stdout.as_ref().unwrap().truncated);
                assert!(stdout(&result, command).contains("data format is newer than supported"));
            } else {
                assert_eq!(command.exit_status, None);
                assert!(
                    command
                        .error
                        .as_deref()
                        .unwrap()
                        .contains("completion unverified")
                );
            }
        }
        assert!(
            stage(scenario, "exercise_after_rejection")
                .commands
                .is_empty()
        );
    }
}

#[test]
#[ignore = "requires Docker"]
fn real_downgrade_timeout_and_cancellation_stop_without_post_attempt_runtime() {
    for policy in ["compatible", "rejection"] {
        for (fault, outcome, command_outcome) in [
            ("timeout", RunOutcome::Incomplete, CommandOutcome::TimedOut),
            ("cancel", RunOutcome::Cancelled, CommandOutcome::Cancelled),
        ] {
            let (_temp, result) = run(policy, fault, vec![ProfileName::User]);
            assert_eq!(
                result.report.outcome, outcome,
                "{policy}/{fault}: {:#?}",
                result.report
            );
            assert_eq!(
                result.report.outcome.exit_code(),
                if fault == "cancel" { 130 } else { 2 }
            );
            let scenario = &result.report.scenarios[0];
            prerequisites(scenario);
            let command = &stage(scenario, "downgrade").commands[0];
            assert_eq!(command.outcome, command_outcome);
            assert_eq!(command.exit_status, None);
            assert!(command.rejection.is_none());
            assert_eq!(command.timed_out, fault == "timeout");
            assert_eq!(command.cancelled, fault == "cancel");
            let checkpoint = stage(
                scenario,
                if policy == "compatible" {
                    "downgraded"
                } else {
                    "downgrade_rejected"
                },
            );
            assert!(checkpoint.snapshots.iter().all(|s| matches!(
                s.completeness,
                theseus::snapshot::ObservationStatus::Incomplete { .. }
            )));
            assert!(
                stage(
                    scenario,
                    if policy == "compatible" {
                        "exercise_downgraded"
                    } else {
                        "exercise_after_rejection"
                    }
                )
                .commands
                .is_empty()
            );
            assert!(stage(scenario, "uninstall").commands.is_empty());
        }
    }
}

#[test]
#[ignore = "requires Docker and Unix PTY"]
fn real_downgrade_live_and_offline_narrow_pty_both_policies() {
    let output = Command::new("python3")
        .arg("tests/support/tui_downgrade.py")
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
