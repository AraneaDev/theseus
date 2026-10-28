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
    run.schema_version = 2;
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
