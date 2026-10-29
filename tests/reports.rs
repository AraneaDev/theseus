use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};
use theseus::{model::*, report};

#[test]
fn incomplete_evidence_never_earns_a_success_exit() {
    assert_eq!(RunOutcome::Passed.exit_code(), 0);
    assert_eq!(RunOutcome::Failed.exit_code(), 1);
    assert_eq!(RunOutcome::Incomplete.exit_code(), 2);
    assert_eq!(RunOutcome::Cancelled.exit_code(), 130);
}
#[test]
fn failed_contract_evidence_survives_cleanup_error_and_cancellation_wins() {
    let mut run = RunReport::new("test-run");
    let mut scenario = ScenarioReport::new("user-clean", ProfileName::User, ScenarioKind::Clean);
    let mut stage = StageReport::new("uninstalled");
    stage.checks.push(CheckResult {
        id: "binary-removed".into(),
        checkpoint: Checkpoint::Uninstalled,
        outcome: CheckOutcome::Failed,
        message: "binary remains".into(),
        path: Some("/usr/local/bin/tool".into()),
        command_ids: vec![],
        backup: None,
        comparison: None,
    });
    stage.outcome = StageOutcome::Failed;
    scenario.stages.push(stage);
    scenario.teardown = TeardownStatus::Failed {
        reason: "Docker unavailable".into(),
    };
    run.scenarios.push(scenario);
    run.recompute_outcome();
    assert_eq!(run.outcome, RunOutcome::Incomplete);
    assert_eq!(run.counts().failed_checks, 1);
    assert_eq!(
        run.scenarios[0].stages[0].checks[0].message,
        "binary remains"
    );
    run.cancelled = true;
    run.recompute_outcome();
    assert_eq!(run.outcome, RunOutcome::Cancelled);
}
#[test]
fn pending_or_incomplete_observations_prevent_success_but_review_only_does_not_fail() {
    let mut run = RunReport::new("run");
    run.recompute_outcome();
    assert_eq!(run.outcome, RunOutcome::Incomplete);
    let mut scenario = ScenarioReport::new("user-clean", ProfileName::User, ScenarioKind::Clean);
    scenario.teardown = TeardownStatus::Complete;
    let mut stage = StageReport::new("baseline");
    stage.outcome = StageOutcome::Passed;
    stage
        .snapshots
        .push(theseus::snapshot::Snapshot::incomplete(
            "/usr/local/bin",
            "denied",
        ));
    scenario.stages.push(stage);
    run.scenarios.push(scenario);
    run.recompute_outcome();
    assert_eq!(run.outcome, RunOutcome::Incomplete);
    run.scenarios[0].stages[0].snapshots.clear();
    run.scenarios[0].findings.push(Finding {
        kind: FindingKind::Leftover,
        path: Some("/usr/local/bin/extra".into()),
        message: "review".into(),
    });
    run.recompute_outcome();
    assert_eq!(run.outcome, RunOutcome::Passed);
    assert_eq!(run.counts().review_findings, 1);
}
#[test]
fn stores_atomically_in_private_files_and_refuses_run_collisions() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("runs/run-1");
    let store = report::create(&root).unwrap();
    assert!(report::create(&root).is_err());
    let mut run = RunReport::new("run-1");
    store.save(&run).unwrap();
    run.errors.push("later event".into());
    store.save(&run).unwrap();
    let loaded = report::load(&root).unwrap();
    assert_eq!(loaded.errors, vec!["later event"]);
    assert_eq!(
        fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(store.report_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::read_dir(&root).unwrap().count(),
        1,
        "atomic temporary files must be removed"
    );
    let journal = RunJournal {
        schema_version: 1,
        run_id: "run-1".into(),
        resources: vec![OwnedResource {
            container_id: "abc123".into(),
            labels: [("dev.theseus.run".into(), "run-1".into())].into(),
            removed: false,
        }],
        cleanup_errors: vec![],
    };
    store.save_journal(&journal).unwrap();
    assert_eq!(
        report::load_journal(&root).unwrap().resources[0].container_id,
        "abc123"
    );
    assert_eq!(
        fs::metadata(root.join("journal.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}
#[test]
fn rejects_report_schema_and_unsafe_log_references() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("run");
    let store = report::create(&root).unwrap();
    let mut run = RunReport::new("run");
    run.schema_version = 999;
    assert!(store.save(&run).is_err());
    fs::write(store.report_path(), serde_json::to_vec(&run).unwrap()).unwrap();
    assert!(report::load(&root).is_err());
    for reference in [
        "../secret",
        "/etc/passwd",
        "logs/../../secret",
        "",
        "report.json",
        "journal.json",
        "./stdout",
    ] {
        assert!(
            store.write_log(reference, b"secret", 16).is_err(),
            "accepted {reference:?}"
        );
        assert!(store.read_log(reference, 16).is_err());
    }
}
#[test]
fn log_reads_and_writes_are_bounded_and_symlinks_are_never_followed() {
    let dir = tempfile::tempdir().unwrap();
    let store = report::create(&dir.path().join("run")).unwrap();
    let log = store
        .write_log("logs/command.stdout", b"12345678", 5)
        .unwrap();
    assert_eq!(log.bytes, 5);
    assert!(log.truncated);
    assert_eq!(
        fs::read(store.root().join("logs/command.stdout")).unwrap(),
        b"12345"
    );
    let read = store.read_log(&log.path, 3).unwrap();
    assert_eq!(read.bytes, b"123");
    assert!(read.truncated);
    assert_eq!(
        fs::metadata(store.root().join("logs"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(store.root().join(&log.path))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    fs::write(dir.path().join("secret"), "outside").unwrap();
    symlink(dir.path().join("secret"), store.root().join("link")).unwrap();
    symlink(dir.path(), store.root().join("dir-link")).unwrap();
    assert!(store.read_log("link", 20).is_err());
    assert!(store.read_log("dir-link/secret", 20).is_err());
    assert!(
        store
            .write_log("dir-link/secret", b"overwritten", 20)
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("secret")).unwrap(),
        "outside"
    );
}
#[test]
fn save_and_load_reject_embedded_traversing_log_paths() {
    let dir = tempfile::tempdir().unwrap();
    let store = report::create(&dir.path().join("run")).unwrap();
    let mut run = RunReport::new("run");
    let mut scenario = ScenarioReport::new("user-clean", ProfileName::User, ScenarioKind::Clean);
    let mut stage = StageReport::new("install");
    let mut command = CommandRecord::new("install-1", "install");
    command.stdout = Some(LogReference {
        path: "../secret".into(),
        bytes: 5,
        truncated: false,
    });
    stage.commands.push(command);
    scenario.stages.push(stage);
    run.scenarios.push(scenario);
    assert!(store.save(&run).is_err());
    fs::write(store.report_path(), serde_json::to_vec(&run).unwrap()).unwrap();
    assert!(report::load(&store.report_path()).is_err());
}

#[test]
fn selected_matrix_entries_cannot_disappear_from_a_success_report() {
    let mut run = RunReport::new("run");
    run.selected_profiles = vec![ProfileName::User, ProfileName::System];
    run.selected_scenarios = vec![ScenarioKind::Clean, ScenarioKind::Upgrade];
    let mut scenario = ScenarioReport::new("user-clean", ProfileName::User, ScenarioKind::Clean);
    scenario.teardown = TeardownStatus::Complete;
    let mut stage = StageReport::new("done");
    stage.outcome = StageOutcome::Passed;
    scenario.stages.push(stage);
    run.scenarios.push(scenario);
    run.recompute_outcome();
    assert_eq!(run.outcome, RunOutcome::Incomplete);
}

fn load_metadata_from_real_current_directory(test_name: &str, journal: bool) {
    use std::{ffi::OsStr, path::Path, process::Command};
    // Changing the harness cwd would race unrelated parallel tests. A child runs
    // this same test in the real run directory, with only this exact test selected.
    if std::env::var_os("THESEUS_METADATA_CWD_TEST").as_deref() == Some(OsStr::new(test_name)) {
        if journal {
            assert_eq!(
                report::load_journal(Path::new("journal.json"))
                    .unwrap()
                    .run_id,
                "cwd-fixture"
            );
        } else {
            assert_eq!(
                report::load(Path::new("report.json")).unwrap().run_id,
                "cwd-fixture"
            );
        }
        return;
    }
    let parent = tempfile::tempdir().unwrap();
    let store = report::create(&parent.path().join("run")).unwrap();
    store.save(&RunReport::new("cwd-fixture")).unwrap();
    store
        .save_journal(&RunJournal {
            schema_version: 1,
            run_id: "cwd-fixture".into(),
            resources: vec![],
            cleanup_errors: vec![],
        })
        .unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test_name, "--nocapture"])
        .env("THESEUS_METADATA_CWD_TEST", test_name)
        .current_dir(store.root())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn loads_bare_relative_report_filename() {
    load_metadata_from_real_current_directory("loads_bare_relative_report_filename", false);
}

#[test]
fn loads_bare_relative_journal_filename() {
    load_metadata_from_real_current_directory("loads_bare_relative_journal_filename", true);
}

#[test]
fn reopening_a_store_for_cleanup_preserves_existing_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let original = theseus::report::create(&temp.path().join("run")).unwrap();
    original
        .save(&theseus::model::RunReport::new("recovery"))
        .unwrap();
    let reopened = theseus::report::open_existing(original.root()).unwrap();
    reopened
        .save_journal(&theseus::model::RunJournal {
            schema_version: 1,
            run_id: "recovery".into(),
            resources: vec![],
            cleanup_errors: vec![],
        })
        .unwrap();
    assert_eq!(
        theseus::report::load(original.root()).unwrap().run_id,
        "recovery"
    );
    let alias = temp.path().join("alias");
    std::os::unix::fs::symlink(original.root(), &alias).unwrap();
    assert!(theseus::report::open_existing(&alias).is_err());
}

#[test]
fn frozen_schema_one_report_and_journal_remain_readable_and_writable() {
    let temp = tempfile::tempdir().unwrap();
    let store = report::create(&temp.path().join("run")).unwrap();
    fs::write(
        store.report_path(),
        include_bytes!("fixtures/reports/schema1-report.json"),
    )
    .unwrap();
    fs::write(
        store.root().join("journal.json"),
        include_bytes!("fixtures/reports/schema1-journal.json"),
    )
    .unwrap();
    let run = report::load(store.root()).unwrap();
    assert_eq!(run.schema_version, 1);
    assert_eq!(run.outcome, RunOutcome::Passed);
    assert_eq!(run.scenarios[0].stages[0].checks[0].id, "binary-removed");
    assert!(
        run.scenarios[0].stages[0].commands[0]
            .forwarded_environment
            .is_empty()
    );
    store.save(&run).unwrap();
    let journal = report::load_journal(store.root()).unwrap();
    assert_eq!(journal.schema_version, 1);
    assert_eq!(journal.resources[0].labels["io.theseus.owner"], "v1");
    store.save_journal(&journal).unwrap();
    assert_eq!(
        report::load_journal(store.root()).unwrap().run_id,
        "schema1-fixture"
    );
}

#[test]
fn new_reports_write_schema_five_while_journals_stay_schema_one() {
    let temp = tempfile::tempdir().unwrap();
    let store = report::create(&temp.path().join("run")).unwrap();
    store.save(&RunReport::new("new-run")).unwrap();
    let serialized: serde_json::Value =
        serde_json::from_slice(&fs::read(store.report_path()).unwrap()).unwrap();
    assert_eq!(serialized["schema_version"], 5);
    assert_eq!(report::load(store.root()).unwrap().schema_version, 5);
    let journal = RunJournal {
        schema_version: 1,
        run_id: "new-run".into(),
        resources: vec![],
        cleanup_errors: vec![],
    };
    store.save_journal(&journal).unwrap();
    let serialized: serde_json::Value =
        serde_json::from_slice(&fs::read(store.root().join("journal.json")).unwrap()).unwrap();
    assert_eq!(serialized["schema_version"], 1);
    assert_eq!(
        report::load_journal(store.root()).unwrap().schema_version,
        1
    );
}

#[test]
fn report_versions_and_journal_versions_are_validated_independently() {
    let temp = tempfile::tempdir().unwrap();
    let store = report::create(&temp.path().join("run")).unwrap();
    for version in [1, 2, 3, 4, 5] {
        let mut run = RunReport::new("run");
        run.schema_version = version;
        store.save(&run).unwrap();
        assert_eq!(report::load(store.root()).unwrap().schema_version, version);
    }
    for version in [0, 6, 999] {
        let mut run = RunReport::new("run");
        run.schema_version = version;
        assert!(store.save(&run).is_err());
        fs::write(store.report_path(), serde_json::to_vec(&run).unwrap()).unwrap();
        assert!(report::load(store.root()).is_err());
    }
    for version in [0, 2, 999] {
        let journal = RunJournal {
            schema_version: version,
            run_id: "run".into(),
            resources: vec![],
            cleanup_errors: vec![],
        };
        assert!(store.save_journal(&journal).is_err());
        fs::write(
            store.root().join("journal.json"),
            serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
        assert!(report::load_journal(store.root()).is_err());
    }
}

#[test]
fn schema_one_cannot_claim_new_reinstall_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let store = report::create(&temp.path().join("run")).unwrap();
    let old: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/reports/schema1-report.json")).unwrap();
    for (pointer, value) in [
        ("/selected_scenarios/0", serde_json::json!("reinstall")),
        ("/scenarios/0/scenario", serde_json::json!("reinstall")),
        ("/scenarios/0/stages/0/step", serde_json::json!("reinstall")),
        (
            "/scenarios/0/stages/0/checkpoint",
            serde_json::json!("reinstalled"),
        ),
        (
            "/scenarios/0/stages/0/checks/0/checkpoint",
            serde_json::json!("reinstalled"),
        ),
    ] {
        let mut json = old.clone();
        *json.pointer_mut(pointer).unwrap() = value;
        fs::write(store.report_path(), serde_json::to_vec(&json).unwrap()).unwrap();
        let error = report::load(store.root()).expect_err(pointer);
        assert!(error.to_string().contains("schema version 2"), "{error:#}");
        // The writer enforces the same rule as the reader.
        assert!(store.save(&serde_json::from_value(json).unwrap()).is_err());
    }
    let mut json = old;
    json["scenarios"][0]["stages"][0]["checks"][0]["comparison"] =
        serde_json::json!({"before_stage":"before_reinstall", "after_stage":"reinstalled"});
    fs::write(store.report_path(), serde_json::to_vec(&json).unwrap()).unwrap();
    assert!(
        report::load(store.root())
            .unwrap_err()
            .to_string()
            .contains("schema version 2")
    );
}
#[test]
fn schema_two_reinstall_comparisons_round_trip_and_old_checks_omit_reference() {
    let temp = tempfile::tempdir().unwrap();
    let store = report::create(&temp.path().join("run")).unwrap();
    let mut json: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/reports/schema1-report.json")).unwrap();
    let old: RunReport = serde_json::from_value(json.clone()).unwrap();
    assert!(old.scenarios[0].stages[0].checks[0].comparison.is_none());
    assert!(
        serde_json::to_value(old).unwrap()["scenarios"][0]["stages"][0]["checks"][0]
            .get("comparison")
            .is_none()
    );
    json["schema_version"] = 2.into();
    json["selected_scenarios"] = serde_json::json!(["reinstall"]);
    json["scenarios"][0]["scenario"] = "reinstall".into();
    json["scenarios"][0]["stages"][0]["step"] = "reinstall".into();
    json["scenarios"][0]["stages"][0]["checkpoint"] = "reinstalled".into();
    json["scenarios"][0]["stages"][0]["checks"][0]["checkpoint"] = "reinstalled".into();
    json["scenarios"][0]["stages"][0]["checks"][0]["comparison"] =
        serde_json::json!({"before_stage":"before_reinstall", "after_stage":"reinstalled"});
    store.save(&serde_json::from_value(json).unwrap()).unwrap();
    let loaded = report::load(store.root()).unwrap();
    assert_eq!(loaded.scenarios[0].scenario, ScenarioKind::Reinstall);
    assert_eq!(
        loaded.scenarios[0].stages[0].checks[0].comparison,
        Some(ComparisonReference {
            before_stage: "before_reinstall".into(),
            after_stage: "reinstalled".into()
        })
    );
}

#[test]
fn report_three_retains_interruption_proofs_but_cannot_pass_an_unmatched_stage() {
    let temp = tempfile::tempdir().unwrap();
    let store = report::create(&temp.path().join("run")).unwrap();
    let mut json: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/reports/schema1-report.json")).unwrap();
    json["schema_version"] = 3.into();
    json["scenarios"][0]["stages"][0]["restart"] =
        serde_json::json!({"container_id": "owned-id", "running_verified": true});
    let command = &mut json["scenarios"][0]["stages"][0]["commands"][0];
    command["outcome"] = "interrupted".into();
    command["exit_status"] = serde_json::Value::Null;
    command["interruption"] = serde_json::json!({
        "mechanism": "container_sigkill", "readiness_verified": true, "stopped_verified": true
    });
    let run: RunReport =
        serde_json::from_value(json.clone()).expect("report3 interruption decodes");
    store.save(&run).unwrap();
    let loaded = report::load(store.root()).unwrap();
    assert_eq!(serde_json::to_value(&loaded).unwrap(), json);
    assert_eq!(
        loaded.scenarios[0].stages[0].aggregate_outcome(),
        RunOutcome::Incomplete
    );
}

#[test]
fn lower_report_versions_reject_each_interruption_feature() {
    let temp = tempfile::tempdir().unwrap();
    let store = report::create(&temp.path().join("run")).unwrap();
    for version in [1, 2] {
        for feature in ["outcome", "interruption", "restart"] {
            let mut json: serde_json::Value =
                serde_json::from_str(include_str!("fixtures/reports/schema1-report.json")).unwrap();
            json["schema_version"] = version.into();
            let stage = &mut json["scenarios"][0]["stages"][0];
            match feature {
                "outcome" => stage["commands"][0]["outcome"] = "interrupted".into(),
                "interruption" => {
                    stage["commands"][0]["interruption"] = serde_json::json!({
                        "mechanism":"container_sigkill", "readiness_verified":false, "stopped_verified":false
                    })
                }
                _ => {
                    stage["restart"] =
                        serde_json::json!({"container_id":"id", "running_verified":false})
                }
            }
            fs::write(store.report_path(), serde_json::to_vec(&json).unwrap()).unwrap();
            assert!(
                report::load(store.root()).is_err(),
                "schema{version} accepted {feature}"
            );
            if let Ok(run) = serde_json::from_value::<RunReport>(json) {
                assert!(store.save(&run).is_err(), "writer accepted {feature}");
            }
        }
    }
}

#[test]
fn old_reports_omit_new_optional_proofs() {
    let run: RunReport =
        serde_json::from_str(include_str!("fixtures/reports/schema1-report.json")).unwrap();
    let json = serde_json::to_value(run).unwrap();
    assert!(json["scenarios"][0]["stages"][0].get("restart").is_none());
    assert!(
        json["scenarios"][0]["stages"][0]["commands"][0]
            .get("interruption")
            .is_none()
    );
}

#[test]
fn unverified_restart_evidence_prevents_a_passing_stage() {
    let mut stage = StageReport::new("restart_after_interrupt");
    stage.outcome = StageOutcome::Passed;
    stage.restart = Some(RestartEvidence {
        container_id: "owned".into(),
        running_verified: false,
    });
    assert_eq!(stage.aggregate_outcome(), RunOutcome::Incomplete);
}

#[test]
fn frozen_schema_two_report_retains_reinstall_evidence_without_interruption_fields() {
    let temporary = tempfile::tempdir().unwrap();
    let store = report::create(&temporary.path().join("run")).unwrap();
    fs::write(
        store.report_path(),
        include_bytes!("fixtures/reports/schema2-report.json"),
    )
    .unwrap();
    let report = report::load(store.root()).unwrap();
    assert_eq!(report.schema_version, 2);
    assert_eq!(report.scenarios[0].scenario, ScenarioKind::Reinstall);
    let stage = &report.scenarios[0].stages[0];
    assert_eq!(stage.checkpoint, Some(Checkpoint::Reinstalled));
    assert_eq!(
        stage.checks[0].comparison,
        Some(ComparisonReference::reinstall())
    );
    assert!(stage.restart.is_none());
    assert!(stage.commands[0].interruption.is_none());
    store.save(&report).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(store.report_path()).unwrap())
            .unwrap(),
        serde_json::from_str::<serde_json::Value>(include_str!(
            "fixtures/reports/schema2-report.json"
        ))
        .unwrap()
    );
}

#[test]
fn interruption_variants_are_schema_three_only_and_proof_is_scoped() {
    use theseus::model::*;
    let temp = tempfile::tempdir().unwrap();
    let store = theseus::report::create(&temp.path().join("run")).unwrap();
    for version in [1, 2] {
        let mut report = RunReport::new("old");
        report.schema_version = version;
        report.selected_scenarios = vec![ScenarioKind::InterruptedInstall];
        assert!(
            store.save(&report).is_err(),
            "schema {version} accepted new selection"
        );
        report.selected_scenarios.clear();
        let mut scenario = ScenarioReport::new("x", ProfileName::User, ScenarioKind::Clean);
        let mut stage = StageReport::new("x");
        stage.checkpoint = Some(Checkpoint::Recovered);
        scenario.stages.push(stage);
        report.scenarios.push(scenario);
        assert!(
            store.save(&report).is_err(),
            "schema {version} accepted new checkpoint"
        );
    }
    let mut stage = StageReport::new("interrupt_install");
    stage.step = Some(StepName::InterruptInstall);
    stage.outcome = StageOutcome::Passed;
    let mut command = CommandRecord::new("x", "interrupt_install");
    command.outcome = CommandOutcome::Interrupted;
    command.interruption = Some(InterruptionEvidence {
        mechanism: InterruptionMechanism::ContainerSigkill,
        readiness_verified: true,
        stopped_verified: true,
    });
    stage.commands.push(command);
    assert_eq!(stage.aggregate_outcome(), RunOutcome::Passed);
    for mutation in [
        "phase", "exit", "step", "proof", "error", "timeout", "cancel",
    ] {
        let mut invalid = stage.clone();
        match mutation {
            "phase" => invalid.commands[0].phase = "other".into(),
            "exit" => invalid.commands[0].exit_status = Some(137),
            "step" => invalid.step = Some(StepName::InstallTarget),
            "proof" => invalid.commands[0].interruption = None,
            "error" => invalid.commands[0].error = Some("uncertain".into()),
            "timeout" => invalid.commands[0].timed_out = true,
            "cancel" => invalid.commands[0].cancelled = true,
            _ => unreachable!(),
        }
        assert_ne!(
            invalid.aggregate_outcome(),
            RunOutcome::Passed,
            "{mutation}"
        );
    }
    let mut scenario =
        ScenarioReport::new("x", ProfileName::User, ScenarioKind::InterruptedInstall);
    scenario.container_id = Some("owned".into());
    scenario.teardown = TeardownStatus::Complete;
    let mut restart = StageReport::new("restart_after_interrupt");
    restart.outcome = StageOutcome::Passed;
    restart.restart = Some(RestartEvidence {
        container_id: "owned".into(),
        running_verified: true,
    });
    scenario.stages.push(restart);
    scenario.recompute_outcome();
    assert_eq!(
        scenario.outcome,
        RunOutcome::Incomplete,
        "restart without matching verified stop"
    );
}

fn rejected_downgrade_report() -> RunReport {
    let mut run = RunReport::new("refusal");
    let mut scenario =
        ScenarioReport::new("user-downgrade", ProfileName::User, ScenarioKind::Downgrade);
    scenario.downgrade = Some(theseus::recipe::DowngradeContract::SafeRejection {
        expected_exit: 9,
        stdout_contains: "newer data".into(),
        binary_checks: vec!["binary".into()],
        data_checks: vec!["data".into()],
    });
    scenario.teardown = TeardownStatus::Complete;
    let mut stage = StageReport::new("downgrade");
    stage.step = Some(StepName::Downgrade);
    stage.outcome = StageOutcome::Passed;
    let mut command = CommandRecord::new("attempt", "downgrade");
    command.outcome = CommandOutcome::ExpectedRejection;
    command.exit_status = Some(9);
    command.rejection = Some(RejectionEvidence {
        expected_exit: 9,
        stdout_contains: "newer data".into(),
        stdout_matched: true,
    });
    stage.commands.push(command);
    scenario.stages.push(stage);
    let mut checkpoint = StageReport::new("downgrade_rejected");
    checkpoint.checkpoint = Some(Checkpoint::DowngradeRejected);
    checkpoint.outcome = StageOutcome::Passed;
    for id in ["binary", "data"] {
        checkpoint.checks.push(CheckResult {
            id: id.into(),
            checkpoint: Checkpoint::DowngradeRejected,
            outcome: CheckOutcome::Passed,
            message: "stable".into(),
            path: Some(format!("/home/theseus/{id}")),
            command_ids: vec![],
            backup: None,
            comparison: ComparisonReference::for_checkpoint(Checkpoint::DowngradeRejected),
        });
    }
    scenario.stages.push(checkpoint);
    run.scenarios.push(scenario);
    run.recompute_outcome();
    assert_eq!(run.outcome, RunOutcome::Passed);
    run
}
#[test]
fn downgrade_refusal_proof_and_policy_context_are_required_for_aggregation() {
    let valid = rejected_downgrade_report();
    assert_eq!(
        valid.scenarios[0].stages[0].aggregate_outcome(),
        RunOutcome::Passed
    );
    for mutation in [
        "step",
        "phase",
        "name",
        "status",
        "missing-proof",
        "empty-marker",
        "unmatched",
        "policy-status",
        "policy-marker",
        "policy-compatible",
        "scenario",
        "checkpoint",
        "missing-data",
        "skipped-data",
        "stray-proof",
        "skipped-checkpoint",
        "skipped-extra-check",
        "missing-attempt",
    ] {
        let mut run = valid.clone();
        let s = &mut run.scenarios[0];
        match mutation {
            "step" => s.stages[0].step = Some(StepName::InstallTarget),
            "phase" => s.stages[0].commands[0].phase = "install_target".into(),
            "name" => s.stages[0].name = "other".into(),
            "status" => s.stages[0].commands[0].exit_status = Some(0),
            "missing-proof" => s.stages[0].commands[0].rejection = None,
            "empty-marker" => s.stages[0].commands[0]
                .rejection
                .as_mut()
                .unwrap()
                .stdout_contains
                .clear(),
            "unmatched" => {
                s.stages[0].commands[0]
                    .rejection
                    .as_mut()
                    .unwrap()
                    .stdout_matched = false
            }
            "policy-status" => {
                if let Some(theseus::recipe::DowngradeContract::SafeRejection {
                    expected_exit,
                    ..
                }) = &mut s.downgrade
                {
                    *expected_exit = 8;
                }
            }
            "policy-marker" => {
                if let Some(theseus::recipe::DowngradeContract::SafeRejection {
                    stdout_contains,
                    ..
                }) = &mut s.downgrade
                {
                    *stdout_contains = "other".into();
                }
            }
            "policy-compatible" => {
                s.downgrade = Some(theseus::recipe::DowngradeContract::Compatible {
                    data_checks: vec!["data".into()],
                })
            }
            "scenario" => s.scenario = ScenarioKind::Clean,
            "checkpoint" => {
                s.stages.pop();
            }
            "missing-data" => {
                s.stages[1].checks.pop();
            }
            "skipped-data" => s.stages[1].checks[1].outcome = CheckOutcome::Skipped,
            "stray-proof" => s.stages[0].commands[0].outcome = CommandOutcome::Succeeded,
            "skipped-checkpoint" => s.stages[1].outcome = StageOutcome::Skipped,
            "skipped-extra-check" => {
                let mut extra = s.stages[1].checks[0].clone();
                extra.id = "extra".into();
                extra.outcome = CheckOutcome::Skipped;
                s.stages[1].checks.push(extra);
            }
            "missing-attempt" => {
                s.stages.remove(0);
            }
            _ => unreachable!(),
        }
        run.recompute_outcome();
        assert_eq!(run.outcome, RunOutcome::Incomplete, "{mutation}");
    }
}
#[test]
fn downgrade_schema_four_reader_recomputes_forged_pass_and_preserves_older_readers() {
    let temp = tempfile::tempdir().unwrap();
    let store = report::create(&temp.path().join("run")).unwrap();
    let mut run = rejected_downgrade_report();
    run.schema_version = 4;
    run.scenarios[0].stages[1].checks.clear();
    // Simulate hand-edited persisted metadata that falsely claims Passed.
    fs::write(store.report_path(), serde_json::to_vec(&run).unwrap()).unwrap();
    assert_eq!(
        report::load(store.root()).unwrap().outcome,
        RunOutcome::Incomplete
    );
    store.save(&run).unwrap();
    let saved: RunReport = serde_json::from_slice(&fs::read(store.report_path()).unwrap()).unwrap();
    assert_eq!(saved.outcome, RunOutcome::Incomplete);
    for version in [1, 2, 3] {
        let mut old = RunReport::new("old");
        old.schema_version = version;
        store.save(&old).unwrap();
        assert_eq!(report::load(store.root()).unwrap().schema_version, version);
        old.scenarios = rejected_downgrade_report().scenarios;
        assert!(
            store.save(&old).is_err(),
            "old schema {version} accepted downgrade"
        );
        fs::write(store.report_path(), serde_json::to_vec(&old).unwrap()).unwrap();
        assert!(report::load(store.root()).is_err());
    }
}

#[test]
fn downgrade_each_feature_has_schema_four_floor_and_optional_fields_omit_none() {
    let temp = tempfile::tempdir().unwrap();
    let store = report::create(&temp.path().join("run")).unwrap();
    for version in [1, 2, 3] {
        for feature in [
            "selection",
            "scenario",
            "metadata",
            "step",
            "exercise",
            "checkpoint",
            "check",
            "comparison",
            "proof",
            "outcome",
        ] {
            let mut run = RunReport::new("floor");
            run.schema_version = version;
            let mut s = ScenarioReport::new("user-clean", ProfileName::User, ScenarioKind::Clean);
            let mut stage = StageReport::new("test");
            let mut cmd = CommandRecord::new("test", "test");
            match feature {
                "selection" => run.selected_scenarios.push(ScenarioKind::Downgrade),
                "scenario" => s.scenario = ScenarioKind::Downgrade,
                "metadata" => {
                    s.downgrade = rejected_downgrade_report().scenarios[0].downgrade.clone()
                }
                "step" => stage.step = Some(StepName::Downgrade),
                "exercise" => stage.step = Some(StepName::ExerciseDowngraded),
                "checkpoint" => stage.checkpoint = Some(Checkpoint::Downgraded),
                "check" | "comparison" => stage.checks.push(CheckResult {
                    id: "x".into(),
                    checkpoint: if feature == "check" {
                        Checkpoint::DowngradeRejected
                    } else {
                        Checkpoint::Uninstalled
                    },
                    outcome: CheckOutcome::Passed,
                    message: "x".into(),
                    path: None,
                    command_ids: vec![],
                    backup: None,
                    comparison: if feature == "comparison" {
                        ComparisonReference::for_checkpoint(Checkpoint::Downgraded)
                    } else {
                        None
                    },
                }),
                "proof" => {
                    cmd.rejection = Some(RejectionEvidence {
                        expected_exit: 9,
                        stdout_contains: "refusal".into(),
                        stdout_matched: true,
                    })
                }
                "outcome" => cmd.outcome = CommandOutcome::ExpectedRejection,
                _ => unreachable!(),
            }
            stage.commands.push(cmd);
            s.stages.push(stage);
            run.scenarios.push(s);
            assert!(store.save(&run).is_err(), "{version}/{feature}");
            fs::write(store.report_path(), serde_json::to_vec(&run).unwrap()).unwrap();
            assert!(report::load(store.root()).is_err(), "{version}/{feature}");
        }
    }
    let run = RunReport::new("optional");
    let s = ScenarioReport::new("x", ProfileName::User, ScenarioKind::Clean);
    let c = CommandRecord::new("x", "x");
    assert!(
        serde_json::to_value(&run)
            .unwrap()
            .get("downgrade")
            .is_none()
    );
    assert!(serde_json::to_value(&s).unwrap().get("downgrade").is_none());
    assert!(serde_json::to_value(&c).unwrap().get("rejection").is_none());
}
#[test]
fn downgrade_incomplete_proof_cannot_pass_a_stage_and_cancellation_wins() {
    let valid = rejected_downgrade_report().scenarios[0].stages[0].clone();
    for mutation in [
        "truncated",
        "timeout",
        "cancel",
        "error",
        "receipt",
        "invalid-status",
        "proof",
        "extra-command",
    ] {
        let mut stage = valid.clone();
        let c = &mut stage.commands[0];
        match mutation {
            "truncated" => {
                c.stdout = Some(LogReference {
                    path: "logs/out".into(),
                    bytes: 1,
                    truncated: true,
                })
            }
            "timeout" => c.timed_out = true,
            "cancel" => c.cancelled = true,
            "error" => c.error = Some("control error".into()),
            "receipt" => c.exit_status = None,
            "invalid-status" => {
                c.exit_status = Some(126);
                c.rejection.as_mut().unwrap().expected_exit = 126;
            }
            "proof" => c.rejection = None,
            "extra-command" => stage.commands.push(valid.commands[0].clone()),
            _ => unreachable!(),
        }
        assert_eq!(
            stage.aggregate_outcome(),
            if mutation == "cancel" {
                RunOutcome::Cancelled
            } else {
                RunOutcome::Incomplete
            },
            "{mutation}"
        );
    }
}

#[test]
fn stray_expected_failure_proof_prevents_passing_even_on_successful_commands() {
    let mut stage = StageReport::new("install_target");
    stage.outcome = StageOutcome::Passed;
    let mut command = CommandRecord::new("cmd", "install_target");
    command.outcome = CommandOutcome::Succeeded;
    command.exit_status = Some(0);
    let mut json = serde_json::to_value(command).unwrap();
    json["expected_failure"] =
        serde_json::json!({"expected_exit":9,"stdout_contains":"failed","stdout_matched":true});
    stage.commands.push(serde_json::from_value(json).unwrap());
    assert_eq!(stage.aggregate_outcome(), RunOutcome::Incomplete);
}

fn rollback_policy_json() -> serde_json::Value {
    serde_json::json!({
        "failure":{"expected_exit":9,"stdout_contains":"upgrade failed"},
        "change_checks":["changed"],"binary_checks":["binary"],"data_checks":["reader"],
        "restoration":{"mode":"restore_data","backup_root":"/workspace/backup","scopes":[{"id":"data","source":"/home/theseus/data","stored":"data"}]}
    })
}

#[test]
fn report_five_roundtrips_rollback_metadata_without_authorizing_unmatched_flow() {
    let temp = tempfile::tempdir().unwrap();
    let store = report::create(&temp.path().join("run")).unwrap();
    let mut value: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/reports/schema2-report.json")).unwrap();
    value["schema_version"] = 5.into();
    value["scenarios"][0]["rollback"] = rollback_policy_json();
    let stage = &mut value["scenarios"][0]["stages"][0];
    stage["checks"][0]["backup"] = serde_json::json!({"source_path":"/home/theseus/data","stored_path":"/workspace/backup/data"});
    stage["commands"][0]["expected_failure"] = serde_json::json!({"expected_exit":9,"stdout_contains":"upgrade failed","stdout_matched":true});
    stage["commands"][0]["outcome"] = "expected_failure".into();
    stage["commands"][0]["exit_status"] = 9.into();
    let run: RunReport = serde_json::from_value(value.clone()).unwrap();
    store.save(&run).unwrap();
    let loaded = report::load(store.root()).unwrap();
    assert_eq!(loaded.outcome, RunOutcome::Incomplete);
    let saved = serde_json::to_value(loaded).unwrap();
    assert_eq!(
        saved["scenarios"][0]["rollback"],
        value["scenarios"][0]["rollback"]
    );
    assert_eq!(
        saved["scenarios"][0]["stages"][0]["commands"][0]["expected_failure"],
        value["scenarios"][0]["stages"][0]["commands"][0]["expected_failure"]
    );
    assert_eq!(
        saved["scenarios"][0]["stages"][0]["checks"][0]["backup"],
        value["scenarios"][0]["stages"][0]["checks"][0]["backup"]
    );
}

#[test]
fn rollback_metadata_and_every_reference_have_report_five_floor() {
    let temp = tempfile::tempdir().unwrap();
    let store = report::create(&temp.path().join("run")).unwrap();
    for version in 1..=4 {
        for feature in [
            "policy",
            "proof",
            "backup",
            "outcome",
            "before_upgrade",
            "before_attempt",
            "before_rollback",
            "known_good",
            "backup_created",
            "upgrade_failed",
            "rolled_back",
            "backup_removed",
        ] {
            for side in ["before_stage", "after_stage"] {
                let mut value: serde_json::Value =
                    serde_json::from_str(include_str!("fixtures/reports/schema2-report.json"))
                        .unwrap();
                value["schema_version"] = version.into();
                // Remove preexisting newer evidence so the targeted feature is the only reason for rejection.
                value["selected_scenarios"] = serde_json::json!(["clean"]);
                let scenario = &mut value["scenarios"][0];
                scenario["scenario"] = "clean".into();
                let stage = &mut scenario["stages"][0];
                stage["step"] = "install_target".into();
                stage["checkpoint"] = "target_installed".into();
                stage["checks"][0]["checkpoint"] = "target_installed".into();
                stage["checks"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("comparison");
                match feature {
                    "policy" => scenario["rollback"] = rollback_policy_json(),
                    "proof" => {
                        stage["commands"][0]["expected_failure"] = serde_json::json!({"expected_exit":9,"stdout_contains":"failed","stdout_matched":true})
                    }
                    "backup" => {
                        stage["checks"][0]["backup"] =
                            serde_json::json!({"source_path":"/data","stored_path":"/backup/data"})
                    }
                    "outcome" => stage["commands"][0]["outcome"] = "expected_failure".into(),
                    _ => {
                        stage["checks"][0]["comparison"] = serde_json::json!({"before_stage":"configured","after_stage":"target_installed"});
                        stage["checks"][0]["comparison"][side] = feature.into();
                    }
                }
                fs::write(store.report_path(), serde_json::to_vec(&value).unwrap()).unwrap();
                assert!(
                    report::load(store.root()).is_err(),
                    "schema{version}/{feature}/{side}"
                );
                if let Ok(run) = serde_json::from_value::<RunReport>(value) {
                    assert!(
                        store.save(&run).is_err(),
                        "writer schema{version}/{feature}/{side}"
                    );
                }
            }
        }
    }
}

#[test]
fn failure_proof_is_strict_and_never_passes_without_rollback_context() {
    let mut stage = StageReport::new("attempt_upgrade");
    stage.outcome = StageOutcome::Passed;
    let mut command =
        serde_json::to_value(CommandRecord::new("attempt", "attempt_upgrade")).unwrap();
    command["outcome"] = "expected_failure".into();
    command["exit_status"] = 9.into();
    command["expected_failure"] =
        serde_json::json!({"expected_exit":9,"stdout_contains":"failed","stdout_matched":true});
    for mutation in [
        "valid",
        "missing-proof",
        "invalid-status",
        "empty-marker",
        "unmatched",
        "missing-receipt",
        "truncated",
        "timeout",
        "error",
        "stray-proof",
        "cancel",
    ] {
        let mut value = command.clone();
        match mutation {
            "missing-proof" => {
                value.as_object_mut().unwrap().remove("expected_failure");
            }
            "invalid-status" => value["expected_failure"]["expected_exit"] = 126.into(),
            "empty-marker" => value["expected_failure"]["stdout_contains"] = "".into(),
            "unmatched" => value["expected_failure"]["stdout_matched"] = false.into(),
            "missing-receipt" => value["exit_status"] = serde_json::Value::Null,
            "truncated" => {
                value["stdout"] = serde_json::json!({"path":"logs/out","bytes":20,"truncated":true})
            }
            "timeout" => value["timed_out"] = true.into(),
            "error" => value["error"] = "transport".into(),
            "stray-proof" => value["outcome"] = "succeeded".into(),
            "cancel" => value["cancelled"] = true.into(),
            _ => {}
        }
        stage.commands = vec![serde_json::from_value(value).unwrap()];
        assert_eq!(
            stage.aggregate_outcome(),
            if mutation == "cancel" {
                RunOutcome::Cancelled
            } else {
                RunOutcome::Incomplete
            },
            "{mutation}"
        );
    }
    for malformed in [
        serde_json::json!({"expected_exit":9,"stdout_contains":"failed"}),
        serde_json::json!({"expected_exit":"9","stdout_contains":"failed","stdout_matched":true}),
        serde_json::json!({"expected_exit":9,"stdout_contains":"failed","stdout_matched":true,"extra":true}),
    ] {
        let mut value = command.clone();
        value["expected_failure"] = malformed;
        assert!(serde_json::from_value::<CommandRecord>(value).is_err());
    }
}

#[test]
fn old_reports_omit_rollback_fields_and_schema_four_still_normalizes() {
    let mut run: RunReport =
        serde_json::from_str(include_str!("fixtures/reports/schema2-report.json")).unwrap();
    let value = serde_json::to_value(&run).unwrap();
    assert!(value["scenarios"][0].get("rollback").is_none());
    assert!(
        value["scenarios"][0]["stages"][0]["commands"][0]
            .get("expected_failure")
            .is_none()
    );
    assert!(
        value["scenarios"][0]["stages"][0]["checks"][0]
            .get("backup")
            .is_none()
    );
    run.schema_version = 4;
    run.scenarios[0].stages[0].commands[0].error = Some("incomplete".into());
    let temp = tempfile::tempdir().unwrap();
    let store = report::create(&temp.path().join("run")).unwrap();
    store.save(&run).unwrap();
    assert_eq!(
        report::load(store.root()).unwrap().outcome,
        RunOutcome::Incomplete
    );
}
