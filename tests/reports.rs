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
fn new_reports_write_schema_two_while_journals_stay_schema_one() {
    let temp = tempfile::tempdir().unwrap();
    let store = report::create(&temp.path().join("run")).unwrap();
    store.save(&RunReport::new("new-run")).unwrap();
    let serialized: serde_json::Value =
        serde_json::from_slice(&fs::read(store.report_path()).unwrap()).unwrap();
    assert_eq!(serialized["schema_version"], 2);
    assert_eq!(report::load(store.root()).unwrap().schema_version, 2);
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
    for version in [1, 2] {
        let mut run = RunReport::new("run");
        run.schema_version = version;
        store.save(&run).unwrap();
        assert_eq!(report::load(store.root()).unwrap().schema_version, version);
    }
    for version in [0, 999] {
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
