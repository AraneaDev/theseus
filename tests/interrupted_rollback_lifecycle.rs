//! Real, cooperative interrupted rollback fixtures; no database snapshot guarantee.
#[path = "support/docker_absence.rs"]
mod docker_absence;
#[path = "support/native.rs"]
mod native;
use std::process::Command;
#[path = "support/interrupted_rollback.rs"]
mod fixture;
use fixture::*;
use theseus::{model::*, recipe};
#[test]
#[ignore = "requires reachable Docker daemon and local ubuntu:24.04"]
fn real_interrupted_rollback_four_way_matrix_restores_runtime_and_reads_actual_data() {
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
            let failed = stage(s, "interrupted");
            let restored = stage(s, "rolled_back");
            assert_eq!(result.report.schema_version, 8);
            assert!(s.rollback.is_none());
            for (phase, id, marker) in [
                ("base_installed", "base-version", "sample-cli 1.0.0"),
                ("known_good", "known-reader", "reader records=2 sum=40"),
                (
                    "rolled_back",
                    "known-good-version-restored",
                    "sample-cli 1.0.0",
                ),
                (
                    "rolled_back",
                    "known-good-reader",
                    "reader records=2 sum=40",
                ),
            ] {
                let c = check(s, phase, id);
                let proof = c.command_expectation.as_ref().unwrap();
                assert_eq!(proof.expected_exit, 0);
                assert_eq!(proof.stdout_contains, marker);
                assert!(proof.stdout_matched);
                assert_eq!(proof.stdout_match.as_deref(), Some(marker));
                for command_id in &c.command_ids {
                    let cmd = stage(s, phase)
                        .commands
                        .iter()
                        .find(|c| &c.id == command_id)
                        .unwrap();
                    let log = cmd.stdout.as_ref().unwrap();
                    assert!(log.bytes > 0 && !log.truncated);
                    assert!(!stdout(&result, cmd).is_empty());
                    assert_eq!((cmd.identity.uid, cmd.identity.gid), (10001, 10001));
                    if cmd.phase.starts_with("check:") {
                        assert!(stdout(&result, cmd).contains(marker));
                        if id.contains("reader") {
                            assert!(stdout(&result, cmd).contains("alpha=11\nbeta=29"));
                        }
                    }
                }
                if id.contains("reader") {
                    assert!(proof.executable.is_none());
                } else {
                    let executable = proof.executable.as_ref().unwrap();
                    assert_eq!(executable.name, "sample-cli");
                    assert_eq!(executable.resolved_path, binary);
                    assert_eq!(executable.args, ["--version"]);
                }
            }
            assert_eq!(
                check(s, "base_installed", "base-version").command_expectation,
                check(s, "rolled_back", "known-good-version-restored").command_expectation
            );
            for (suffix, kind, mode) in [
                ("", FileKind::Directory, 0o750),
                ("/nested", FileKind::Directory, 0o750),
                ("/records", FileKind::File, 0o640),
                ("/nested/child", FileKind::File, 0o640),
                ("/current", FileKind::Symlink, 0o777),
            ] {
                let e = entry(before, &format!("{DATA}{suffix}")).unwrap();
                assert_eq!(
                    (e.kind, e.mode & 0o777, e.uid, e.gid),
                    (kind, mode, 10001, 10001),
                    "{suffix}"
                );
            }
            assert_eq!(
                entry(before, &format!("{DATA}/nested/child"))
                    .unwrap()
                    .sha256
                    .as_deref(),
                Some(hash(b"known child\n").as_str())
            );
            assert_eq!(
                entry(failed, &format!("{DATA}/added"))
                    .unwrap()
                    .sha256
                    .as_deref(),
                Some(hash(b"candidate child\n").as_str())
            );
            assert_eq!(
                entry(failed, &format!("{DATA}/current"))
                    .unwrap()
                    .link_target
                    .as_deref(),
                Some("records")
            );

            assert_eq!(
                tree(before, DATA)
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                ["", "/current", "/nested", "/nested/child", "/records"]
            );
            assert_eq!(tree(before, DATA), tree(stage(s, "before_interrupt"), DATA));
            assert_eq!(
                entry(before, binary),
                entry(stage(s, "before_interrupt"), binary)
            );
            let names = s.stages.iter().map(|s| s.name.as_str()).collect::<Vec<_>>();
            let ordered = [
                "known_good:after_checks",
                "before_upgrade",
                "before_interrupt",
                "interrupt_upgrade",
                "interrupted",
                "interrupted:after_checks",
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
                    hash(
                        &std::fs::read("tests/fixtures/interrupted-rollback/base/sample-cli")
                            .unwrap()
                    )
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
                    hash(
                        &std::fs::read("tests/fixtures/interrupted-rollback/target/sample-cli")
                            .unwrap()
                    )
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
            assert!(stage(s, "interrupt_upgrade").verified_interruption());
            assert!(!serde_json::to_string(s).unwrap().contains("/.theseus-"));
            let stop = stage(s, "interrupt_upgrade");
            let cmd = &stop.commands[0];
            assert!(stop.snapshots.is_empty());
            assert!(!cmd.environment.contains_key("THESEUS_INTERRUPT_BARRIER"));
            assert_eq!(cmd.exit_status, None);
            assert_eq!(cmd.outcome, CommandOutcome::Interrupted);
            assert!(cmd.error.is_none() && !cmd.timed_out && !cmd.cancelled);
            let proof = cmd.interruption.as_ref().unwrap();
            assert!(proof.readiness_verified && proof.stopped_verified);
            let restart = stage(s, "restart_after_interrupt");
            assert!(restart.snapshots.is_empty());
            assert_eq!(
                Some(&restart.restart.as_ref().unwrap().container_id),
                s.container_id.as_ref()
            );
            assert!(restart.restart.as_ref().unwrap().running_verified);
            assert!(entry(stage(s, "interrupted"), "/workspace/continued").is_none());
            assert!(
                entry(stage(s, "interrupted"), "/workspace/writer")
                    .unwrap()
                    .size
                    > 1,
                "background writer really ran before barrier"
            );
            assert_eq!(
                entry(stage(s, "interrupted"), "/workspace/writer"),
                entry(stage(s, "before_rollback"), "/workspace/writer")
            );
            assert_eq!(
                check(s, "interrupted", "candidate-changed-state").comparison,
                Some(ComparisonReference {
                    before_stage: "before_interrupt".into(),
                    after_stage: "interrupted".into()
                })
            );
            assert!(
                stdout(&result, &stage(s, "exercise_rolled_back").commands[0])
                    .contains("reader records=2 sum=40")
            );
            if policy == "restore-data" {
                for phase in [
                    "backup_created",
                    "interrupted",
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
                            | "interrupt_upgrade"
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
fn real_backup_faults_block_restoration_and_preserve_preexisting_root() {
    let mut cases = CaseFilter::from_env();
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
            "interrupted",
            "__theseus_rollback_backup_intact_data",
        ),
        (
            "check-corrupts-backup",
            "before_rollback",
            "__theseus_rollback_backup_ready_data",
        ),
    ] {
        if !cases.enabled(fault) {
            continue;
        }
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
                check(s, "interrupted", "__theseus_rollback_backup_intact_data").outcome,
                CheckOutcome::Passed
            );
            assert_eq!(
                check(s, "interrupted", "corrupt-after-validation").outcome,
                CheckOutcome::Passed
            );
            assert_ne!(
                entry(stage(s, "interrupted"), &format!("{BACKUP}/saved/records")),
                entry(
                    stage(s, "before_rollback"),
                    &format!("{BACKUP}/saved/records")
                )
            );
        }
    }
    for fault in ["partial-backup", "backup-predicate"] {
        if !cases.enabled(fault) {
            continue;
        }
        let (_temp, result) = run("restore-data", fault, vec![ProfileName::User]);
        assert_eq!(result.report.outcome, RunOutcome::Failed, "{fault}");
        let s = &result.report.scenarios[0];
        assert_eq!(
            stage(s, "backup").commands[0].exit_status,
            Some(if fault == "partial-backup" { 7 } else { 1 })
        );
        assert!(!stdout(&result, &stage(s, "backup").commands[0]).contains("backup completed"));
        assert!(stage(s, "interrupt_upgrade").commands.is_empty());
        assert_eq!(stage(s, "cleanup_backup").outcome, StageOutcome::Passed);
    }
    cases.finish();
}
#[test]
#[ignore = "requires Docker"]
fn real_noop_partial_and_metadata_restores_fail_independent_proofs() {
    let mut cases = CaseFilter::from_env();
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
        if !cases.enabled(fault) {
            continue;
        }
        let (_temp, result) = run("restore-data", fault, profiles);
        assert_eq!(
            result.report.outcome,
            RunOutcome::Failed,
            "{fault}: {:#?}",
            result.report
        );
        let s = &result.report.scenarios[0];
        prerequisites(s);
        assert!(stage(s, "interrupt_upgrade").verified_interruption());
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
    for fault in ["restore-predicate", "cleanup-predicate"] {
        if !cases.enabled(fault) {
            continue;
        }
        let (_temp, result) = run("restore-data", fault, vec![ProfileName::User]);
        assert_eq!(result.report.outcome, RunOutcome::Failed, "{fault}");
        let s = &result.report.scenarios[0];
        let name = if fault == "restore-predicate" {
            "rollback"
        } else {
            "cleanup_backup"
        };
        assert_eq!(stage(s, name).commands[0].exit_status, Some(1));
        assert!(
            !stdout(&result, &stage(s, name).commands[0]).contains(if name == "rollback" {
                "rollback completed"
            } else {
                "backup removed"
            })
        );
    }
    cases.finish();
}
#[test]
#[ignore = "requires Docker"]
fn real_incomplete_runtime_coverage_blocks_forward_mutation() {
    let mut cases = CaseFilter::from_env();
    for fault in [
        "incomplete-baseline",
        "incomplete-install",
        "incomplete-exercise",
        "incomplete-backup",
        "incomplete-rollback",
        "incomplete-reference",
        "incomplete-checkpoint",
        "incomplete-after-checks",
    ] {
        if !cases.enabled(fault) {
            continue;
        }
        let (_temp, result) = run("restore-data", fault, vec![ProfileName::User]);
        assert_eq!(result.report.outcome, RunOutcome::Incomplete, "{fault}");
        let s = &result.report.scenarios[0];
        if matches!(fault, "incomplete-baseline" | "incomplete-install") {
            assert!(
                stage(s, "base_installed").commands.is_empty(),
                "unverified prerequisite must block check commands: {fault}"
            );
        }
        assert_eq!(
            !stage(s, "rollback").commands.is_empty(),
            fault == "incomplete-rollback"
        );
        assert!(stage(s, "exercise_rolled_back").commands.is_empty());
        assert!(
            stage(s, "before_rollback")
                .snapshots
                .iter()
                .all(|snapshot| matches!(
                    snapshot.completeness,
                    theseus::snapshot::ObservationStatus::Complete
                )),
            "later complete capture cannot reauthorize {fault}"
        );
        let had_backup = matches!(
            fault,
            "incomplete-backup" | "incomplete-rollback" | "incomplete-after-checks"
        );
        let had_candidate = matches!(fault, "incomplete-rollback" | "incomplete-after-checks");
        assert_eq!(
            !stage(s, "backup").commands.is_empty(),
            had_backup,
            "{fault}"
        );
        assert_eq!(
            !stage(s, "interrupt_upgrade").commands.is_empty(),
            had_candidate,
            "{fault}"
        );
        assert_eq!(
            !stage(s, "cleanup_backup").commands.is_empty(),
            had_backup,
            "{fault}"
        );
        assert_eq!(stage(s, "uninstall").outcome, StageOutcome::Passed);
    }
    cases.finish();
}
#[test]
#[ignore = "requires Docker"]
fn real_timeout_and_cancel_at_every_mutation_skip_forward_steps() {
    let mut cases = CaseFilter::from_env();
    for phase in ["backup", "interrupt", "rollback", "cleanup"] {
        for fault in [format!("timeout-{phase}"), format!("cancel-{phase}")] {
            if !cases.enabled(&fault) {
                continue;
            }
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
                "interrupt" => "interrupt_upgrade",
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
                    "interrupt_upgrade",
                    "rollback",
                    "exercise_rolled_back",
                    "cleanup_backup",
                    "uninstall",
                ],
                "interrupt" => vec![
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
    cases.finish();
}
#[test]
fn interrupted_rollback_pty_current_detail_regressions() {
    let output = Command::new(if cfg!(windows) { "python" } else { "python3" })
        .arg("tests/support/tui_interrupted_rollback_test.py")
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
#[ignore = "requires Docker and Unix PTY"]
fn real_interrupted_rollback_live_offline_narrow_pty_both_policies() {
    let output = Command::new(if cfg!(windows) { "python" } else { "python3" })
        .arg("tests/support/tui_interrupted_rollback.py")
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
fn rollback_invalid_paths_and_scope_overlap_reject_offline_before_resources() {
    let temp = tempfile::tempdir().unwrap();
    let source =
        std::fs::read_to_string("tests/fixtures/interrupted-rollback/restore-data.toml").unwrap();
    for role in ["base", "target"] {
        std::fs::create_dir(temp.path().join(role)).unwrap();
        for file in
            std::fs::read_dir(format!("tests/fixtures/interrupted-rollback/{role}")).unwrap()
        {
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
        if to == "path = \"/home/theseus/.local/share/sample-cli\"" {
            assert_eq!(out.status.code(), Some(2));
            assert_eq!(
                String::from_utf8_lossy(&out.stderr),
                "theseus: binary and data scopes overlap\n"
            );
        }
        assert!(!temp.path().join(".theseus-runs").exists());
    }
    // Host artifact symlink escapes are rejected while loading, before Docker.
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("payload"), "outside").unwrap();
    let artifact = temp.path().join("artifact");
    std::fs::create_dir(&artifact).unwrap();
    native::symlink(outside.path().join("payload"), artifact.join("escape")).unwrap();
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
        entry(stage(s, "before_interrupt"), RECORDS)
    );
    assert_eq!(
        entry(stage(s, "before_interrupt"), RECORDS),
        entry(stage(s, "interrupted"), RECORDS)
    );
    assert_eq!(
        check(s, "interrupted", "candidate-changed-state").outcome,
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

#[test]
#[ignore = "requires Docker"]
fn real_directory_binary_reference_fails_its_kind_check_without_scope_overlap() {
    let (_temp, result) = run(
        "restore-data",
        "directory-binary-promise",
        vec![ProfileName::User],
    );
    assert_eq!(
        result.report.outcome,
        RunOutcome::Failed,
        "{:#?}",
        result.report
    );
    let scenario = &result.report.scenarios[0];
    prerequisites(scenario);
    assert!(stage(scenario, "interrupt_upgrade").verified_interruption());
    assert_eq!(stage(scenario, "rollback").outcome, StageOutcome::Passed);
    let before = stage(scenario, "before_upgrade");
    let after = stage(scenario, "rolled_back");
    let path = "/workspace/binary-promise";
    let directory = entry(before, path).unwrap();
    assert_eq!(directory.kind, FileKind::Directory);
    assert_eq!(entry(after, path), Some(directory));
    // A generic directory Unchanged check would pass: only the binary-role kind
    // restriction can reject this otherwise stable, disjoint reference object.
    assert_eq!(
        theseus::checks::compare_unchanged(path, &before.snapshots, &after.snapshots).outcome,
        CheckOutcome::Passed
    );
    let rejected = check(scenario, "rolled_back", "directory-binary");
    assert_eq!(rejected.path.as_deref(), Some(path));
    assert_eq!(rejected.outcome, CheckOutcome::Failed);
    assert_eq!(
        rejected.message,
        "binary scope must be a file or symlink before upgrade"
    );
    assert!(
        after
            .checks
            .iter()
            .filter(|check| check.id != "directory-binary")
            .all(|check| check.outcome == CheckOutcome::Passed)
    );
    assert!(
        scenario
            .stages
            .iter()
            .flat_map(|stage| &stage.checks)
            .filter(|check| check.outcome == CheckOutcome::Failed)
            .all(|check| check.id == "directory-binary")
    );
    assert!(stage(scenario, "exercise_rolled_back").commands.is_empty());
    assert_eq!(
        stage(scenario, "cleanup_backup").outcome,
        StageOutcome::Passed
    );
    assert_eq!(stage(scenario, "uninstall").outcome, StageOutcome::Passed);
}

#[test]
#[ignore = "requires Docker"]
fn real_zero_return_noop_cannot_prove_original_runtime_or_restored_tree() {
    let (_temp, result) = run("restore-data", "noop-rollback", vec![ProfileName::User]);
    let scenario = &result.report.scenarios[0];
    prerequisites(scenario);
    assert!(stage(scenario, "interrupt_upgrade").verified_interruption());
    assert_eq!(stage(scenario, "rollback").commands[0].exit_status, Some(0));
    for id in [
        "known-good-binary",
        "known-good-version-restored",
        "known-good-reader",
        "__theseus_rollback_restored_data",
    ] {
        assert_eq!(
            check(scenario, "rolled_back", id).outcome,
            CheckOutcome::Failed,
            "{id}"
        );
    }
    assert_eq!(result.report.outcome, RunOutcome::Failed);
}

#[test]
#[ignore = "requires Docker"]
fn real_barrier_stop_restart_and_writer_faults_block_rollback() {
    let mut cases = CaseFilter::from_env();
    for (fault, outcome) in [
        ("early-0", RunOutcome::Failed),
        ("early-9", RunOutcome::Failed),
        ("never-ready", RunOutcome::Incomplete),
        ("stale", RunOutcome::Incomplete),
        ("symlink", RunOutcome::Incomplete),
        ("stop-error", RunOutcome::Incomplete),
        ("restart-error", RunOutcome::Incomplete),
        ("wrong-id", RunOutcome::Incomplete),
        ("unverified-restart", RunOutcome::Incomplete),
        ("no-mutation", RunOutcome::Failed),
        ("changing-writer", RunOutcome::Failed),
    ] {
        if !cases.enabled(fault) {
            continue;
        }
        let (_temp, result) = run("restore-data", fault, vec![ProfileName::User]);
        assert_eq!(
            result.report.outcome, outcome,
            "{fault}: {:#?}",
            result.report
        );
        assert_eq!(
            result.report.outcome.exit_code(),
            if outcome == RunOutcome::Failed { 1 } else { 2 }
        );
        let s = &result.report.scenarios[0];
        prerequisites(s);
        for name in ["rollback", "exercise_rolled_back"] {
            assert!(stage(s, name).commands.is_empty(), "{fault}/{name}");
        }
        if fault.starts_with("early-") {
            let cmd = &stage(s, "interrupt_upgrade").commands[0];
            assert_eq!(cmd.outcome, CommandOutcome::Failed);
            assert_eq!(
                cmd.exit_status,
                Some(if fault == "early-0" { 0 } else { 9 })
            );
            assert!(!stage(s, "interrupt_upgrade").verified_interruption());
        }
        if fault == "no-mutation" {
            assert!(stage(s, "interrupt_upgrade").verified_interruption());
            assert_eq!(
                check(s, "interrupted", "candidate-changed-state").outcome,
                CheckOutcome::Failed
            );
            assert_eq!(
                tree(stage(s, "before_interrupt"), DATA),
                tree(stage(s, "interrupted"), DATA)
            );
        }
        if fault == "changing-writer" {
            assert_eq!(
                check(s, "interrupted", "writer-stopped").outcome,
                CheckOutcome::Failed
            );
            let cmd = stage(s, "interrupted")
                .commands
                .iter()
                .find(|c| c.phase == "check:writer-stopped")
                .unwrap();
            assert_eq!(cmd.exit_status, Some(1));
            assert!(
                !stdout(&result, cmd).contains("stopped"),
                "predicate false pass"
            );
        }
        let cleanup_allowed = outcome == RunOutcome::Failed;
        assert_eq!(
            !stage(s, "cleanup_backup").commands.is_empty(),
            cleanup_allowed,
            "{fault}"
        );
    }
    cases.finish();
}

#[test]
#[ignore = "requires Docker"]
fn real_control_timeouts_and_cancellation_boundaries_are_bounded_and_terminal() {
    let mut cases = CaseFilter::from_env();
    for (fault, outcome) in [
        ("timeout-ready", RunOutcome::Incomplete),
        ("timeout-stop", RunOutcome::Incomplete),
        ("timeout-restart", RunOutcome::Incomplete),
        ("timeout-check", RunOutcome::Incomplete),
        ("cancel-before-barrier", RunOutcome::Cancelled),
        ("cancel-before-stop", RunOutcome::Cancelled),
        ("cancel-stopped", RunOutcome::Cancelled),
        ("cancel-before-restart", RunOutcome::Cancelled),
        ("cancel-during-restart", RunOutcome::Cancelled),
        ("cancel-interrupted", RunOutcome::Cancelled),
        ("cancel-check", RunOutcome::Cancelled),
        ("cancel-failed-cleanup", RunOutcome::Cancelled),
    ] {
        if !cases.enabled(fault) {
            continue;
        }
        let (_temp, result) = run("restore-data", fault, vec![ProfileName::User]);
        assert_eq!(
            result.report.outcome, outcome,
            "{fault}: {:#?}",
            result.report
        );
        assert_eq!(
            result.report.outcome.exit_code(),
            if outcome == RunOutcome::Cancelled {
                130
            } else {
                2
            }
        );
        let s = &result.report.scenarios[0];
        if fault == "cancel-failed-cleanup" {
            let cmd = &stage(s, "cleanup_backup").commands[0];
            assert_eq!(cmd.outcome, CommandOutcome::Failed);
            assert_eq!(cmd.exit_status, Some(1));
            assert!(!stdout(&result, cmd).contains("backup removed"));
            assert!(stage(s, "uninstall").commands.is_empty());
            continue;
        }
        for name in [
            "rollback",
            "exercise_rolled_back",
            "cleanup_backup",
            "uninstall",
        ] {
            assert!(stage(s, name).commands.is_empty(), "{fault}/{name}");
        }
        if matches!(fault, "cancel-before-barrier" | "cancel-before-stop") {
            assert!(stage(s, "interrupt_upgrade").commands.is_empty());
        }
        if matches!(fault, "cancel-stopped" | "cancel-before-restart") {
            assert!(stage(s, "restart_after_interrupt").restart.is_none());
        }
        if fault == "timeout-check" || fault == "cancel-check" {
            let cmd = stage(s, "interrupted")
                .commands
                .iter()
                .find(|c| c.phase == "check:slow-interrupted-check")
                .unwrap();
            assert_eq!(
                cmd.outcome,
                if fault == "timeout-check" {
                    CommandOutcome::TimedOut
                } else {
                    CommandOutcome::Cancelled
                }
            );
            assert!(!stdout(&result, cmd).contains("forbidden-check"));
        }
    }
    cases.finish();
}

#[test]
#[ignore = "requires Docker"]
fn real_mixed_ordinary_and_interrupted_entries_keep_distinct_modes_and_owned_ids() {
    let mut cases = CaseFilter::from_env();
    for policy in ["binary-only", "restore-data"] {
        if !cases.enabled(&format!("mixed-{policy}")) {
            continue;
        }
        let (_temp, result) = run(policy, "mixed", vec![ProfileName::User]);
        assert_eq!(
            result.report.outcome,
            RunOutcome::Passed,
            "{policy}: {:#?}",
            result.report
        );
        assert_eq!(result.report.scenarios.len(), 2);
        let ordinary = &result.report.scenarios[0];
        let interrupted = &result.report.scenarios[1];
        assert_eq!(ordinary.scenario, ScenarioKind::Rollback);
        assert_eq!(interrupted.scenario, ScenarioKind::InterruptedRollback);
        assert_ne!(ordinary.container_id, interrupted.container_id);
        assert!(stage(ordinary, "attempt_upgrade").verified_failure());
        assert!(stage(interrupted, "interrupt_upgrade").verified_interruption());
        assert!(ordinary.interrupted_rollback.is_none() && interrupted.rollback.is_none());
        for s in &result.report.scenarios {
            let restored = if s.scenario == ScenarioKind::InterruptedRollback {
                policy == "restore-data"
            } else {
                policy == "binary-only"
            };
            for name in ["backup", "cleanup_backup"] {
                assert_eq!(
                    s.stages
                        .iter()
                        .find(|stage| stage.name == name)
                        .is_some_and(|stage| !stage.commands.is_empty()),
                    restored,
                    "{policy}/{}/{name}",
                    s.scenario
                );
            }
            assert_eq!(
                s.stages
                    .iter()
                    .flat_map(|s| &s.checks)
                    .any(|c| c.id == "__theseus_rollback_restored_data"
                        && c.outcome == CheckOutcome::Passed),
                restored
            );
            assert_eq!(
                check(s, "rolled_back", "known-good-reader").outcome,
                CheckOutcome::Passed
            );
        }
    }
    cases.finish();
}
