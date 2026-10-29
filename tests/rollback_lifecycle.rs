//! Real, quiesced synchronous rollback fixtures; no database snapshot guarantee.
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
const BACKUP: &str = "/workspace/rollback-backup";
fn stage<'a>(s: &'a ScenarioReport, name: &str) -> &'a StageReport {
    s.stages
        .iter()
        .find(|v| v.name == name)
        .unwrap_or_else(|| panic!("missing {name}"))
}
fn entry<'a>(s: &'a StageReport, path: &str) -> Option<&'a theseus::snapshot::FileEntry> {
    s.snapshots.iter().find_map(|v| v.entries.get(path))
}
fn tree(
    stage: &StageReport,
    root: &str,
) -> std::collections::BTreeMap<String, theseus::snapshot::FileEntry> {
    stage
        .snapshots
        .iter()
        .flat_map(|s| &s.entries)
        .filter(|(path, _)| path.as_str() == root || path.starts_with(&format!("{root}/")))
        .map(|(path, value)| (path[root.len()..].into(), value.clone()))
        .collect()
}
fn hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
fn stdout(result: &engine::RunResult, c: &CommandRecord) -> String {
    std::fs::read_to_string(
        result
            .report_path
            .parent()
            .unwrap()
            .join(&c.stdout.as_ref().unwrap().path),
    )
    .unwrap()
}
fn check<'a>(s: &'a ScenarioReport, name: &str, id: &str) -> &'a CheckResult {
    stage(s, name)
        .checks
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("missing {name}/{id}"))
}
fn run(
    policy: &str,
    fault: &str,
    profiles: Vec<ProfileName>,
) -> (tempfile::TempDir, engine::RunResult) {
    let temp = tempfile::tempdir().unwrap();
    let mut recipe =
        recipe::load(Path::new(&format!("tests/fixtures/rollback/{policy}.toml"))).unwrap();
    for p in recipe.recipe.profiles.values_mut() {
        for (name, spec) in &mut p.steps {
            spec.env
                .insert("THESEUS_FIXTURE_FAULT".into(), fault.into());
            if fault
                == format!("timeout-{name}")
                    .replace("attempt_upgrade", "attempt")
                    .replace("cleanup_backup", "cleanup")
            {
                spec.timeout_seconds = 2;
            }
        }
        if fault == "preexisting-root" {
            p.steps.get_mut(&StepName::Configure).unwrap().script.push_str(" && mkdir -p /workspace/rollback-backup && printf baseline > /workspace/rollback-backup/owned-before");
        }
        if fault == "no-mutation-after-backup-check" {
            p.checks.push(recipe::Check {
                id: "backup-check-effect".into(), checkpoint: Checkpoint::BackupCreated,
                expectation: recipe::Expectation::Command {
                    script: "printf 'format=2\nalpha=11\nbeta=29\n' > /home/theseus/.local/share/sample-cli/records && echo source-effect".into(),
                    expected_exit: 0, stdout_contains: Some("source-effect".into()),
                    env: Default::default(), pass_env: vec![], timeout_seconds: 15,
                },
            });
        }
        if fault == "known-check-effect" {
            let reader = p
                .checks
                .iter_mut()
                .find(|c| c.id == "known-reader")
                .unwrap();
            if let recipe::Expectation::Command { script, .. } = &mut reader.expectation {
                script.push_str(
                    " && printf known-effect > /home/theseus/.local/share/sample-cli/from-check",
                );
            }
        }
        if fault == "check-corrupts-backup" {
            p.checks.push(recipe::Check {id:"corrupt-after-validation".into(),checkpoint:Checkpoint::UpgradeFailed,expectation:recipe::Expectation::Command {script:"printf corrupt > /workspace/rollback-backup/saved/records && echo corruption-completed".into(),expected_exit:0,stdout_contains:Some("corruption-completed".into()), env:Default::default(), pass_env:vec![], timeout_seconds:15}});
        }
    }
    let options = RunOptions {
        profiles,
        scenarios: vec![ScenarioKind::Rollback],
        output_root: temp.path().join("runs"),
        log_limit: 4096,
        ..RunOptions::all(&recipe)
    };
    let cancel = CancellationToken::default();
    let signal = cancel.clone();
    let executable = temp.path().join("docker-proxy");
    let marker = temp.path().join("attempt");
    let incomplete = temp.path().join("incomplete");
    std::fs::write(&executable,format!(r#"#!/usr/bin/python3
import pathlib, subprocess, sys
args=sys.argv[1:]
mode={fault:?}
marker=pathlib.Path({marker:?})
incomplete=pathlib.Path({incomplete:?})
attempt='theseus-command' in args and any('/inputs/target/attempt.sh' in arg for arg in args)
receipt='-c' in args and args[args.index('-c')+1].startswith('test -f')
if mode=='incomplete-reference' and incomplete.exists() and args[0]=='cp' and args[1].endswith(':/home/theseus'):
    sys.exit(1)
if attempt: marker.touch()
if mode=='transport' and receipt and marker.exists(): sys.exit(1)
result=subprocess.run(['docker',*args])
if attempt and mode=='missing-receipt':
    path=args[args.index('theseus-command')+1]
    subprocess.run(['docker','exec',args[args.index('--workdir')+2],'rm','-f',path],check=True)
sys.exit(result.returncode)
"#,marker=marker.to_str().unwrap(),incomplete=incomplete.to_str().unwrap())).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut docker = theseus::runner::Docker::new(executable);
    let result = engine::run_with(&recipe, options, cancel, &mut docker, move |event| {
        if fault == "incomplete-reference"
            && event.kind == EventKind::StageFinished
            && event.stage.as_deref() == Some("known_good:after_checks")
        {
            std::fs::write(&incomplete, "fail cp").unwrap();
        }
        if fault.starts_with("cancel-")
            && event.kind == EventKind::StageStarted
            && event.stage.as_deref()
                == Some(match fault {
                    "cancel-attempt" => "attempt_upgrade",
                    "cancel-cleanup" => "cleanup_backup",
                    "cancel-backup" => "backup",
                    _ => "rollback",
                })
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
    for r in &journal.resources {
        assert!(r.removed);
        assert_eq!(r.labels["io.theseus.owner"], "v1");
        assert_eq!(r.labels["io.theseus.run"], result.report.run_id);
        assert!(
            result
                .report
                .scenarios
                .iter()
                .any(|s| s.container_id.as_ref() == Some(&r.container_id))
        );
        absent(&r.container_id);
    }
    for s in &result.report.scenarios {
        assert_eq!(s.teardown, TeardownStatus::Complete);
    }
    assert_eq!(
        theseus::report::load(&result.report_path).unwrap().outcome,
        result.report.outcome
    );
    if let Ok(dest) = std::env::var("THESEUS_ACCEPTANCE_EVIDENCE") {
        let dest = Path::new(&dest).join(format!(
            "{policy}-{}-{}",
            if fault.is_empty() { "healthy" } else { fault },
            result.report.run_id
        ));
        std::fs::create_dir_all(&dest).unwrap();
        assert!(
            Command::new("cp")
                .arg("-a")
                .arg(result.report_path.parent().unwrap().join("."))
                .arg(&dest)
                .status()
                .unwrap()
                .success()
        );
        println!("Evidence: {}", dest.display());
    }
    (temp, result)
}
fn prerequisites(s: &ScenarioReport) {
    for name in [
        "install_base",
        "base_installed",
        "configure",
        "configured",
        "exercise_before_upgrade",
        "known_good",
        "before_upgrade",
    ] {
        assert_eq!(stage(s, name).outcome, StageOutcome::Passed, "{name}");
    }
    assert_eq!(
        entry(stage(s, "before_upgrade"), RECORDS)
            .unwrap()
            .sha256
            .as_deref(),
        Some(hash(b"format=1\nalpha=11\nbeta=29\n").as_str())
    );
}
#[test]
#[ignore = "requires reachable Docker daemon and local ubuntu:24.04"]
fn real_rollback_four_way_matrix_restores_runtime_and_reads_actual_data() {
    for policy in ["binary-only", "restore-data"] {
        let (_temp, result) = run(policy, "", vec![ProfileName::User, ProfileName::System]);
        assert_eq!(
            result.report.outcome,
            RunOutcome::Passed,
            "{policy}: {:#?}",
            result.report
        );
        for s in &result.report.scenarios {
            prerequisites(s);
            let binary = if s.profile == ProfileName::User {
                "/home/theseus/.local/bin/sample-cli"
            } else {
                "/usr/local/bin/sample-cli"
            };
            let before = stage(s, "before_upgrade");
            let failed = stage(s, "upgrade_failed");
            let restored = stage(s, "rolled_back");
            assert_eq!(
                tree(before, DATA)
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                ["", "/current", "/nested", "/nested/child", "/records"]
            );
            assert_eq!(tree(before, DATA), tree(stage(s, "before_attempt"), DATA));
            assert_eq!(
                entry(before, binary),
                entry(stage(s, "before_attempt"), binary)
            );
            let names = s.stages.iter().map(|s| s.name.as_str()).collect::<Vec<_>>();
            let ordered = [
                "known_good:after_checks",
                "before_upgrade",
                "before_attempt",
                "attempt_upgrade",
                "upgrade_failed",
                "upgrade_failed:after_checks",
                "before_rollback",
                "rollback",
                "rolled_back",
                "rolled_back:after_checks",
                "exercise_rolled_back",
                "uninstall",
                "final",
            ];
            assert!(
                ordered
                    .windows(2)
                    .all(|pair| names.iter().position(|n| n == &pair[0]).unwrap()
                        < names.iter().position(|n| n == &pair[1]).unwrap())
            );
            let bin = entry(before, binary).unwrap();
            assert_eq!(
                bin.sha256.as_deref(),
                Some(
                    hash(&std::fs::read("tests/fixtures/rollback/base/sample-cli").unwrap())
                        .as_str()
                )
            );
            assert_eq!(bin.mode & 0o777, 0o755);
            assert_eq!(
                bin.uid,
                if s.profile == ProfileName::System {
                    0
                } else {
                    10001
                }
            );
            assert_eq!(bin.gid, bin.uid);
            assert_eq!(entry(restored, binary), Some(bin));
            assert_eq!(
                entry(failed, binary).unwrap().sha256.as_deref(),
                Some(
                    hash(&std::fs::read("tests/fixtures/rollback/target/sample-cli").unwrap())
                        .as_str()
                )
            );
            assert_eq!(
                entry(failed, RECORDS).unwrap().sha256.as_deref(),
                Some(hash(b"format=2\nalpha=11\nbeta=29\n").as_str())
            );
            assert_eq!(
                tree(failed, DATA)
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                [
                    "",
                    "/added",
                    "/current",
                    "/nested",
                    "/nested/child",
                    "/records"
                ]
            );
            assert_eq!(entry(failed, RECORDS).unwrap().uid, 10001);
            assert_eq!(entry(failed, RECORDS).unwrap().gid, 10001);
            assert_eq!(entry(failed, RECORDS).unwrap().mode & 0o777, 0o640);
            assert_eq!(
                entry(before, &format!("{DATA}/current"))
                    .unwrap()
                    .link_target
                    .as_deref(),
                Some("records")
            );
            assert_eq!(entry(before, RECORDS).unwrap().uid, 10001);
            assert_eq!(entry(before, RECORDS).unwrap().gid, 10001);
            assert_eq!(entry(before, RECORDS).unwrap().mode & 0o777, 0o640);
            assert!(stage(s, "attempt_upgrade").verified_failure());
            let cmd = &stage(s, "attempt_upgrade").commands[0];
            assert_eq!(cmd.exit_status, Some(9));
            assert_eq!(cmd.outcome, CommandOutcome::ExpectedFailure);
            assert!(cmd.error.is_none() && !cmd.timed_out && !cmd.cancelled);
            assert!(!cmd.stdout.as_ref().unwrap().truncated);
            assert!(cmd.expected_failure.as_ref().unwrap().stdout_matched);
            assert!(stdout(&result, cmd).contains("upgrade failed after migration"));
            assert_eq!(
                check(s, "upgrade_failed", "candidate-changed-state").comparison,
                Some(ComparisonReference {
                    before_stage: "before_attempt".into(),
                    after_stage: "upgrade_failed".into()
                })
            );
            assert!(
                stdout(&result, &stage(s, "exercise_rolled_back").commands[0])
                    .contains("reader records=2 sum=40")
            );
            if policy == "restore-data" {
                for phase in [
                    "backup_created",
                    "upgrade_failed",
                    "before_rollback",
                    "rolled_back",
                ] {
                    let snapshot = stage(s, phase);
                    assert_eq!(
                        tree(before, DATA),
                        tree(snapshot, &format!("{BACKUP}/saved")),
                        "mapped child sets and all metadata at {phase}"
                    );
                    for path in [
                        DATA,
                        RECORDS,
                        &format!("{DATA}/current"),
                        &format!("{DATA}/nested"),
                        &format!("{DATA}/nested/child"),
                    ] {
                        let mapped = path.replacen(DATA, &format!("{BACKUP}/saved"), 1);
                        let saved = entry(snapshot, &mapped).unwrap().clone();
                        assert_eq!(Some(&saved), entry(before, path), "{phase}: {path}");
                    }
                }
                assert_eq!(tree(before, DATA), tree(restored, DATA));
                for path in [
                    DATA,
                    RECORDS,
                    &format!("{DATA}/current"),
                    &format!("{DATA}/nested/child"),
                ] {
                    assert_eq!(entry(before, path), entry(restored, path));
                }
                assert!(entry(restored, &format!("{DATA}/added")).is_none());
                assert!(
                    check(s, "before_rollback", "__theseus_rollback_backup_ready_data")
                        .backup
                        .is_some()
                );
                assert_eq!(
                    check(s, "before_rollback", "__theseus_rollback_backup_ready_data")
                        .comparison
                        .as_ref()
                        .unwrap()
                        .after_stage,
                    "before_rollback"
                );
                assert!(entry(stage(s, "backup_removed"), BACKUP).is_none());
            } else {
                assert_eq!(entry(failed, RECORDS), entry(restored, RECORDS));
                assert!(entry(restored, &format!("{DATA}/added")).is_some());
                assert!(
                    !s.stages
                        .iter()
                        .flat_map(|s| &s.checks)
                        .any(|c| c.id.starts_with("__theseus_rollback_"))
                );
            }
            assert!(entry(stage(s, "uninstalled"), binary).is_none());
            assert!(
                s.stages
                    .iter()
                    .flat_map(|s| &s.checks)
                    .all(|c| c.outcome == CheckOutcome::Passed)
            );
            for phase in &s.stages {
                for cmd in &phase.commands {
                    let mutation = matches!(
                        cmd.phase.as_str(),
                        "install_base"
                            | "backup"
                            | "attempt_upgrade"
                            | "rollback"
                            | "cleanup_backup"
                            | "uninstall"
                    );
                    assert_eq!(
                        cmd.identity.uid,
                        if mutation && s.profile == ProfileName::System {
                            0
                        } else {
                            10001
                        }
                    );
                    assert_eq!(cmd.identity.gid, cmd.identity.uid);
                }
            }
        }
    }
}
#[test]
#[ignore = "requires Docker"]
fn real_failed_attempt_requires_mutation_status_marker_and_verified_receipt() {
    for (fault, outcome) in [
        ("no-mutation", RunOutcome::Failed),
        ("unexpected-zero", RunOutcome::Failed),
        ("wrong-status", RunOutcome::Failed),
        ("wrong-marker", RunOutcome::Failed),
        ("missing-receipt", RunOutcome::Incomplete),
        ("transport", RunOutcome::Incomplete),
        ("truncated", RunOutcome::Incomplete),
    ] {
        let (_temp, result) = run("restore-data", fault, vec![ProfileName::User]);
        assert_eq!(
            result.report.outcome, outcome,
            "{fault}: {:#?}",
            result.report
        );
        let s = &result.report.scenarios[0];
        prerequisites(s);
        assert!(stage(s, "rollback").commands.is_empty());
        assert!(stage(s, "exercise_rolled_back").commands.is_empty());
        if fault == "no-mutation" {
            assert!(stage(s, "attempt_upgrade").verified_failure());
            assert_eq!(
                check(s, "upgrade_failed", "candidate-changed-state").outcome,
                CheckOutcome::Failed
            );
            assert_eq!(
                entry(stage(s, "before_attempt"), RECORDS),
                entry(stage(s, "upgrade_failed"), RECORDS)
            );
        } else {
            assert!(!stage(s, "attempt_upgrade").verified_failure());
            assert!(
                stage(s, "attempt_upgrade").commands[0]
                    .expected_failure
                    .is_none()
            );
        }
        if matches!(fault, "missing-receipt" | "transport") {
            assert_eq!(stage(s, "attempt_upgrade").commands[0].exit_status, None);
        }
        assert!(!stage(s, "cleanup_backup").commands.is_empty());
    }
}
#[test]
#[ignore = "requires Docker"]
fn real_backup_faults_block_restoration_and_preserve_preexisting_root() {
    for (fault, phase, id) in [
        (
            "preexisting-root",
            "backup",
            "__theseus_rollback_root_absent",
        ),
        (
            "destructive-backup",
            "backup_created",
            "__theseus_rollback_source_stable_data",
        ),
        (
            "bad-backup-copy",
            "backup_created",
            "__theseus_rollback_backup_matches_data",
        ),
        (
            "corrupt-backup",
            "upgrade_failed",
            "__theseus_rollback_backup_intact_data",
        ),
        (
            "check-corrupts-backup",
            "before_rollback",
            "__theseus_rollback_backup_ready_data",
        ),
    ] {
        let (_temp, result) = run("restore-data", fault, vec![ProfileName::User]);
        assert_eq!(
            result.report.outcome,
            RunOutcome::Failed,
            "{fault}: {:#?}",
            result.report
        );
        let s = &result.report.scenarios[0];
        prerequisites(s);
        assert_eq!(check(s, phase, id).outcome, CheckOutcome::Failed);
        assert!(stage(s, "rollback").commands.is_empty());
        if fault == "preexisting-root" {
            assert!(stage(s, "backup").commands.is_empty());
            assert!(stage(s, "cleanup_backup").commands.is_empty());
            assert_eq!(
                entry(stage(s, "final"), &format!("{BACKUP}/owned-before"))
                    .unwrap()
                    .sha256
                    .as_deref(),
                Some(hash(b"baseline").as_str())
            );
        } else {
            assert_eq!(stage(s, "cleanup_backup").outcome, StageOutcome::Passed);
            assert!(entry(stage(s, "backup_removed"), BACKUP).is_none());
        }
        if fault == "check-corrupts-backup" {
            assert_eq!(
                check(s, "upgrade_failed", "__theseus_rollback_backup_intact_data").outcome,
                CheckOutcome::Passed
            );
            assert_eq!(
                check(s, "upgrade_failed", "corrupt-after-validation").outcome,
                CheckOutcome::Passed
            );
            assert_ne!(
                entry(
                    stage(s, "upgrade_failed"),
                    &format!("{BACKUP}/saved/records")
                ),
                entry(
                    stage(s, "before_rollback"),
                    &format!("{BACKUP}/saved/records")
                )
            );
        }
    }
    for fault in ["partial-backup", "backup-predicate"] {
        let (_temp, result) = run("restore-data", fault, vec![ProfileName::User]);
        assert_eq!(result.report.outcome, RunOutcome::Failed, "{fault}");
        let s = &result.report.scenarios[0];
        assert_eq!(
            stage(s, "backup").commands[0].exit_status,
            Some(if fault == "partial-backup" { 7 } else { 1 })
        );
        assert!(!stdout(&result, &stage(s, "backup").commands[0]).contains("backup completed"));
        assert!(stage(s, "attempt_upgrade").commands.is_empty());
        assert_eq!(stage(s, "cleanup_backup").outcome, StageOutcome::Passed);
    }
}
#[test]
#[ignore = "requires Docker"]
fn real_noop_partial_and_metadata_restores_fail_independent_proofs() {
    for (fault, id, path) in [
        (
            "noop-rollback",
            "known-good-binary",
            "/home/theseus/.local/bin/sample-cli",
        ),
        ("missing-child", "__theseus_rollback_restored_data", DATA),
        ("added-child", "__theseus_rollback_restored_data", DATA),
        ("wrong-mode", "__theseus_rollback_restored_data", DATA),
        ("wrong-owner", "__theseus_rollback_restored_data", DATA),
        ("wrong-link", "__theseus_rollback_restored_data", DATA),
        ("wrong-reader", "known-good-reader", ""),
        ("wrong-runtime", "known-good-version-restored", ""),
    ] {
        let profiles = if fault == "wrong-owner" {
            vec![ProfileName::System]
        } else {
            vec![ProfileName::User]
        };
        let (_temp, result) = run("restore-data", fault, profiles);
        assert_eq!(
            result.report.outcome,
            RunOutcome::Failed,
            "{fault}: {:#?}",
            result.report
        );
        let s = &result.report.scenarios[0];
        prerequisites(s);
        assert!(stage(s, "attempt_upgrade").verified_failure());
        assert_eq!(stage(s, "rollback").commands[0].exit_status, Some(0));
        let c = check(s, "rolled_back", id);
        assert_eq!(c.outcome, CheckOutcome::Failed, "{fault}: {c:?}");
        if !path.is_empty() {
            assert_eq!(c.path.as_deref(), Some(path));
        }
        if matches!(fault, "wrong-reader" | "wrong-runtime") {
            assert_eq!(
                check(s, "rolled_back", "known-good-binary").outcome,
                CheckOutcome::Passed
            );
            assert_eq!(
                check(s, "rolled_back", "__theseus_rollback_restored_data").outcome,
                CheckOutcome::Passed
            );
            let cmd = stage(s, "rolled_back")
                .commands
                .iter()
                .find(|c| c.phase == format!("check:{id}"))
                .unwrap();
            assert_eq!(cmd.exit_status, Some(0));
        }
        assert!(stage(s, "exercise_rolled_back").commands.is_empty());
        assert_eq!(stage(s, "cleanup_backup").outcome, StageOutcome::Passed);
    }
    for fault in ["restore-predicate", "cleanup-fails"] {
        let (_temp, result) = run("restore-data", fault, vec![ProfileName::User]);
        assert_eq!(result.report.outcome, RunOutcome::Failed, "{fault}");
        let s = &result.report.scenarios[0];
        let name = if fault == "restore-predicate" {
            "rollback"
        } else {
            "cleanup_backup"
        };
        assert_eq!(
            stage(s, name).commands[0].exit_status,
            Some(if fault == "restore-predicate" { 1 } else { 7 })
        );
        assert!(!stdout(&result, &stage(s, name).commands[0]).contains("rollback completed"));
    }
}
#[test]
#[ignore = "requires Docker"]
fn real_incomplete_runtime_coverage_blocks_forward_mutation() {
    let (_temp, result) = run(
        "restore-data",
        "incomplete-reference",
        vec![ProfileName::User],
    );
    assert_eq!(
        result.report.outcome,
        RunOutcome::Incomplete,
        "{:#?}",
        result.report
    );
    let s = &result.report.scenarios[0];
    assert!(stage(s, "backup").commands.is_empty());
    assert!(stage(s, "attempt_upgrade").commands.is_empty());
    assert!(stage(s, "rollback").commands.is_empty());
    assert!(stage(s, "cleanup_backup").commands.is_empty());
}
#[test]
#[ignore = "requires Docker"]
fn real_timeout_and_cancel_at_every_mutation_skip_forward_steps() {
    for phase in ["backup", "attempt", "rollback", "cleanup"] {
        for fault in [format!("timeout-{phase}"), format!("cancel-{phase}")] {
            let (_temp, result) = run("restore-data", &fault, vec![ProfileName::User]);
            let cancelled = fault.starts_with("cancel");
            assert_eq!(
                result.report.outcome,
                if cancelled {
                    RunOutcome::Cancelled
                } else {
                    RunOutcome::Incomplete
                },
                "{fault}: {:#?}",
                result.report
            );
            assert_eq!(
                result.report.outcome.exit_code(),
                if cancelled { 130 } else { 2 }
            );
            let s = &result.report.scenarios[0];
            let name = match phase {
                "attempt" => "attempt_upgrade",
                "cleanup" => "cleanup_backup",
                v => v,
            };
            let cmd = &stage(s, name).commands[0];
            assert_eq!(
                cmd.outcome,
                if cancelled {
                    CommandOutcome::Cancelled
                } else {
                    CommandOutcome::TimedOut
                }
            );
            assert_eq!(cmd.exit_status, None);
            assert_eq!(cmd.cancelled, cancelled);
            assert_eq!(cmd.timed_out, !cancelled);
            for later in match phase {
                "backup" => vec![
                    "attempt_upgrade",
                    "rollback",
                    "exercise_rolled_back",
                    "cleanup_backup",
                    "uninstall",
                ],
                "attempt" => vec![
                    "rollback",
                    "exercise_rolled_back",
                    "cleanup_backup",
                    "uninstall",
                ],
                "rollback" => vec!["exercise_rolled_back", "cleanup_backup", "uninstall"],
                _ => vec!["uninstall"],
            } {
                assert!(stage(s, later).commands.is_empty(), "{fault}: {later}");
            }
        }
    }
}
#[test]
#[ignore = "requires Docker and Unix PTY"]
fn real_rollback_live_offline_narrow_pty_both_policies() {
    let output = Command::new("python3")
        .arg("tests/support/tui_rollback.py")
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
fn rollback_invalid_paths_and_binary_promises_reject_offline_before_resources() {
    let temp = tempfile::tempdir().unwrap();
    let source = std::fs::read_to_string("tests/fixtures/rollback/restore-data.toml").unwrap();
    for role in ["base", "target"] {
        std::fs::create_dir(temp.path().join(role)).unwrap();
        for file in std::fs::read_dir(format!("tests/fixtures/rollback/{role}")).unwrap() {
            let file = file.unwrap();
            std::fs::copy(file.path(), temp.path().join(role).join(file.file_name())).unwrap();
        }
    }
    let healthy = temp.path().join("healthy.toml");
    std::fs::write(&healthy, &source).unwrap();
    recipe::load(&healthy).unwrap();
    for (from, to) in [
        (
            "backup_root = \"/workspace/rollback-backup\"",
            "backup_root = \"/\"",
        ),
        (
            "backup_root = \"/workspace/rollback-backup\"",
            "backup_root = \"/inputs/backup\"",
        ),
        ("stored = \"saved\"", "stored = \"../escape\""),
        (
            "source = \"/home/theseus/.local/share/sample-cli\"",
            "source = \"/home/theseus/.local/share/sample-cli/../elsewhere\"",
        ),
        (
            "backup_root = \"/workspace/rollback-backup\"",
            "backup_root = \"/home/theseus/.local/share/sample-cli/backup\"",
        ),
        (
            "path = \"/home/theseus/.local/bin/sample-cli\"",
            "path = \"/home/theseus/.local/share/sample-cli\"",
        ),
    ] {
        let path = temp.path().join("invalid.toml");
        std::fs::write(&path, source.replace(from, to)).unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_theseus"))
            .args(["validate", "--recipe"])
            .arg(&path)
            .env("DOCKER_HOST", "unix:///nonexistent-rollback-validate.sock")
            .current_dir(temp.path())
            .output()
            .unwrap();
        assert!(!out.status.success(), "accepted {to}");
        assert!(!temp.path().join(".theseus-runs").exists());
    }
    // Host artifact symlink escapes are rejected while loading, before Docker.
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("payload"), "outside").unwrap();
    let artifact = temp.path().join("artifact");
    std::fs::create_dir(&artifact).unwrap();
    std::os::unix::fs::symlink(outside.path().join("payload"), artifact.join("escape")).unwrap();
    let source = source.replace("base = \"base\"", "base = \"artifact\"");
    let path = temp.path().join("symlink.toml");
    std::fs::write(&path, source).unwrap();
    let error = recipe::load(&path).unwrap_err().to_string();
    assert!(error.contains("symlink"), "{error}");
    assert!(!temp.path().join(".theseus-runs").exists());
}

#[test]
#[ignore = "requires Docker"]
fn real_reference_boundaries_capture_check_effects_before_forward_mutation() {
    let (_temp, result) = run(
        "restore-data",
        "no-mutation-after-backup-check",
        vec![ProfileName::User],
    );
    assert_eq!(result.report.outcome, RunOutcome::Failed);
    let s = &result.report.scenarios[0];
    assert_eq!(
        check(s, "backup_created", "__theseus_rollback_source_stable_data").outcome,
        CheckOutcome::Passed
    );
    assert_eq!(
        check(s, "backup_created", "backup-check-effect").outcome,
        CheckOutcome::Passed
    );
    assert_ne!(
        entry(stage(s, "before_upgrade"), RECORDS),
        entry(stage(s, "before_attempt"), RECORDS)
    );
    assert_eq!(
        entry(stage(s, "before_attempt"), RECORDS),
        entry(stage(s, "upgrade_failed"), RECORDS)
    );
    assert_eq!(
        check(s, "upgrade_failed", "candidate-changed-state").outcome,
        CheckOutcome::Failed
    );
    assert!(stage(s, "rollback").commands.is_empty());
    let (_temp, result) = run(
        "restore-data",
        "known-check-effect",
        vec![ProfileName::User],
    );
    assert_eq!(
        result.report.outcome,
        RunOutcome::Passed,
        "{:#?}",
        result.report
    );
    let s = &result.report.scenarios[0];
    let path = format!("{DATA}/from-check");
    assert!(entry(stage(s, "known_good"), &path).is_none());
    assert_eq!(
        entry(stage(s, "known_good:after_checks"), &path),
        entry(stage(s, "before_upgrade"), &path)
    );
    assert_eq!(
        entry(stage(s, "before_upgrade"), &path),
        entry(stage(s, "rolled_back"), &path)
    );
    assert_eq!(
        entry(stage(s, "before_upgrade"), &path)
            .unwrap()
            .sha256
            .as_deref(),
        Some(hash(b"known-effect").as_str())
    );
}
