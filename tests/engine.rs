use anyhow::Result;
use std::collections::BTreeMap;
use theseus::{
    engine::{self, CancellationToken, RunOptions},
    model::*,
    recipe::*,
    report::ReportStore,
    runner::{CommandRequest, Environment, ExecutedCommand, PreparedEnvironment},
    snapshot::Snapshot,
};

struct Fake {
    fault: &'static str,
}
impl Environment for Fake {
    fn prepare(
        &mut self,
        _: &ValidatedRecipe,
        _: bool,
        _: &CancellationToken,
    ) -> Result<PreparedEnvironment> {
        Ok(PreparedEnvironment {
            image_id: "sha256:fixture".into(),
            artifacts: vec![],
        })
    }
    fn create(&mut self, run: &str, scenario: &str, _: &ValidatedRecipe) -> Result<OwnedResource> {
        Ok(OwnedResource {
            container_id: format!("{run}-{scenario}"),
            labels: BTreeMap::new(),
            removed: false,
        })
    }
    fn setup(
        &mut self,
        _: &OwnedResource,
        _: &ValidatedRecipe,
        _: &CancellationToken,
    ) -> Result<()> {
        if self.fault == "setup" {
            anyhow::bail!("unsupported fixture image");
        }
        Ok(())
    }
    fn execute(
        &mut self,
        _: &OwnedResource,
        request: CommandRequest,
        _: &ReportStore,
        _: &CancellationToken,
    ) -> Result<ExecutedCommand> {
        let mut record = CommandRecord::new(&request.id, &request.phase);
        record.identity = request.identity;
        record.argv = request.argv;
        record.exit_status = Some(0);
        record.outcome = CommandOutcome::Succeeded;
        if self.fault == "install" && request.phase.starts_with("install")
            || self.fault == "uninstall" && request.phase == "uninstall"
        {
            record.exit_status = Some(1);
            record.outcome = CommandOutcome::Failed;
        }
        if self.fault == "check" && request.phase == "check:version" {
            record.exit_status = Some(1);
            record.outcome = CommandOutcome::Failed;
        }
        if ["timeout", "cancel"].contains(&self.fault) && request.phase.starts_with("install") {
            record.exit_status = None;
            record.timed_out = self.fault == "timeout";
            record.cancelled = self.fault == "cancel";
            record.outcome = if record.cancelled {
                CommandOutcome::Cancelled
            } else {
                CommandOutcome::TimedOut
            };
        }
        Ok(ExecutedCommand {
            record,
            stdout: "2.0".into(),
            stdout_complete: true,
        })
    }
    fn observe(
        &mut self,
        _: &OwnedResource,
        roots: &[String],
        _: Limits,
        _: &CancellationToken,
    ) -> Result<Vec<Snapshot>> {
        Ok(roots
            .iter()
            .map(|r| {
                if self.fault == "overflow" {
                    Snapshot::incomplete(r, "entry limit exceeded")
                } else {
                    Snapshot::missing(r)
                }
            })
            .collect())
    }
    fn stop(&mut self, _: &OwnedResource) -> Result<()> {
        Ok(())
    }
    fn remove(&mut self, _: &OwnedResource) -> Result<()> {
        if self.fault == "cleanup" {
            anyhow::bail!("daemon unavailable during cleanup");
        }
        Ok(())
    }
}
fn recipe() -> (tempfile::TempDir, ValidatedRecipe) {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("input")).unwrap();
    let mut text = "schema_version=1\nname='fixture'\nimage='ubuntu:24.04'\nobserve=['/home/theseus']\n[artifacts]\nbase='input'\ntarget='input'\n".to_string();
    for profile in ["user", "system"] {
        for step in [
            "install_base",
            "install_target",
            "configure",
            "upgrade",
            "exercise",
            "uninstall",
        ] {
            text += &format!("[profiles.{profile}.steps.{step}]\nscript='true'\n");
        }
        text += &format!(
            "[[profiles.{profile}.checks]]\nid='version'\ncheckpoint='target_installed'\nkind='command'\nscript='echo 2.0'\nexpected_exit=0\nstdout_contains='2.0'\n"
        );
    }
    let path = temp.path().join("theseus.toml");
    std::fs::write(&path, text).unwrap();
    (temp, load(&path).unwrap())
}
fn run(fault: &'static str) -> (tempfile::TempDir, theseus::engine::RunResult) {
    let (temp, recipe) = recipe();
    let options = RunOptions {
        output_root: temp.path().join("runs"),
        ..RunOptions::all(&recipe)
    };
    let result = engine::run_with(
        &recipe,
        options,
        CancellationToken::default(),
        &mut Fake { fault },
        |_| {},
    )
    .unwrap();
    (temp, result)
}
#[test]
fn complete_matrix_has_real_order_final_evidence_and_cleanup() {
    let (_temp, result) = run("");
    assert_eq!(result.report.outcome, RunOutcome::Passed);
    assert_eq!(result.report.scenarios.len(), 4);
    for s in &result.report.scenarios {
        let stages: Vec<_> = s.stages.iter().filter_map(|s| s.step).collect();
        assert_eq!(
            stages,
            if s.scenario == ScenarioKind::Clean {
                vec![
                    StepName::InstallTarget,
                    StepName::Configure,
                    StepName::Exercise,
                    StepName::Uninstall,
                ]
            } else {
                vec![
                    StepName::InstallBase,
                    StepName::Configure,
                    StepName::Upgrade,
                    StepName::Exercise,
                    StepName::Uninstall,
                ]
            }
        );
        assert!(matches!(s.teardown, TeardownStatus::Complete));
        assert_eq!(s.stages.last().unwrap().name, "final");
        assert!(!s.stages.last().unwrap().snapshots.is_empty());
        let install = s
            .stages
            .iter()
            .find(|s| {
                matches!(
                    s.step,
                    Some(StepName::InstallBase | StepName::InstallTarget)
                )
            })
            .unwrap();
        assert_eq!(
            install.commands[0].identity.uid,
            if s.profile == ProfileName::System {
                0
            } else {
                10001
            }
        );
    }
    assert_eq!(
        theseus::report::load(&result.report_path).unwrap().outcome,
        RunOutcome::Passed
    );
}
#[test]
fn nonzero_install_skips_dependents_but_uninstalls_and_observes() {
    let (_, result) = run("install");
    assert_eq!(result.report.outcome, RunOutcome::Failed);
    for s in result.report.scenarios {
        assert_eq!(
            s.stages
                .iter()
                .find(|s| s.step == Some(StepName::Configure))
                .unwrap()
                .outcome,
            StageOutcome::Skipped
        );
        assert!(
            !s.stages
                .iter()
                .find(|s| s.step == Some(StepName::Uninstall))
                .unwrap()
                .commands
                .is_empty()
        );
        assert_eq!(s.stages.last().unwrap().name, "final");
        assert!(matches!(s.teardown, TeardownStatus::Complete));
    }
}
#[test]
fn failed_check_stops_forward_mutation_but_keeps_final_observations() {
    let (_, result) = run("check");
    assert_eq!(result.report.outcome, RunOutcome::Failed);
    for s in result.report.scenarios {
        assert_eq!(
            s.stages
                .iter()
                .find(|s| s.step == Some(StepName::Exercise))
                .unwrap()
                .outcome,
            StageOutcome::Skipped
        );
        assert!(!s.stages.last().unwrap().snapshots.is_empty());
    }
}
#[test]
fn uninstall_failure_is_a_contract_failure_with_final_state() {
    let (_, result) = run("uninstall");
    assert_eq!(result.report.outcome, RunOutcome::Failed);
    assert!(
        result
            .report
            .scenarios
            .iter()
            .all(|s| !s.stages.last().unwrap().snapshots.is_empty()
                && matches!(s.teardown, TeardownStatus::Complete))
    );
}
#[test]
fn timeout_is_incomplete_and_cannot_claim_uninstall_observation() {
    let (_, result) = run("timeout");
    assert_eq!(result.report.outcome, RunOutcome::Incomplete);
    for s in result.report.scenarios {
        assert_eq!(
            s.stages
                .iter()
                .find(|s| s.step == Some(StepName::Uninstall))
                .unwrap()
                .outcome,
            StageOutcome::Skipped
        );
        assert!(s.stages.last().unwrap().snapshots.iter().all(|s| matches!(
            s.completeness,
            theseus::snapshot::ObservationStatus::Incomplete { .. }
        )));
        assert!(matches!(s.teardown, TeardownStatus::Complete));
    }
}
#[test]
fn cancellation_has_priority_and_preserves_cleanup() {
    let (_, result) = run("cancel");
    assert_eq!(result.report.outcome, RunOutcome::Cancelled);
    assert!(
        result
            .report
            .scenarios
            .iter()
            .all(|s| matches!(s.teardown, TeardownStatus::Complete))
    );
}
#[test]
fn observation_overflow_cannot_pass() {
    let (_, result) = run("overflow");
    assert_eq!(result.report.outcome, RunOutcome::Incomplete);
    assert!(result.report.scenarios.iter().all(|s| {
        s.findings
            .iter()
            .any(|f| f.kind == FindingKind::ObservationIncomplete)
    }));
}

#[test]
fn deliberately_skipped_configuration_does_not_turn_known_failure_into_incomplete() {
    let (temp, mut recipe) = recipe();
    for profile in recipe.recipe.profiles.values_mut() {
        profile.checks.push(Check {
            id: "retention".into(),
            checkpoint: Checkpoint::Uninstalled,
            expectation: Expectation::Preserved {
                path: "/home/theseus/config".into(),
            },
        });
    }
    let options = RunOptions {
        output_root: temp.path().join("runs"),
        ..RunOptions::all(&recipe)
    };
    let result = engine::run_with(
        &recipe,
        options,
        CancellationToken::default(),
        &mut Fake { fault: "install" },
        |_| {},
    )
    .unwrap();
    assert_eq!(result.report.outcome, RunOutcome::Failed);
    for scenario in &result.report.scenarios {
        let check = scenario
            .stages
            .iter()
            .flat_map(|s| &s.checks)
            .find(|c| c.id == "retention")
            .unwrap();
        assert_eq!(check.outcome, CheckOutcome::Skipped);
        assert!(check.message.contains("configuration"));
    }
}

#[test]
fn setup_failure_journals_cleanup_and_marks_unperformed_checks_incomplete() {
    let (_temp, result) = run("setup");
    assert_eq!(result.report.outcome, RunOutcome::Incomplete);
    assert_eq!(result.report.counts().incomplete_checks, 4);
    let journal = theseus::report::load_journal(result.report_path.parent().unwrap()).unwrap();
    assert_eq!(journal.resources.len(), 4);
    assert!(journal.resources.iter().all(|r| r.removed));
    assert!(result.report.scenarios.iter().all(|s| {
        s.errors
            .iter()
            .any(|e| e.contains("unsupported fixture image"))
    }));
}
#[test]
fn cleanup_failure_preserves_recovery_journal_and_does_not_claim_success() {
    let (_temp, result) = run("cleanup");
    assert_eq!(result.report.outcome, RunOutcome::Incomplete);
    assert!(
        result
            .report
            .scenarios
            .iter()
            .all(|s| matches!(s.teardown, TeardownStatus::Failed { .. }))
    );
    let journal = theseus::report::load_journal(result.report_path.parent().unwrap()).unwrap();
    assert_eq!(journal.cleanup_errors.len(), 4);
    assert!(journal.resources.iter().all(|r| !r.removed));
}
#[test]
fn pre_cancelled_run_records_remaining_checks_without_creating_resources() {
    let (temp, recipe) = recipe();
    let options = RunOptions {
        output_root: temp.path().join("runs"),
        ..RunOptions::all(&recipe)
    };
    let cancel = CancellationToken::default();
    cancel.cancel();
    let result =
        engine::run_with(&recipe, options, cancel, &mut Fake { fault: "" }, |_| {}).unwrap();
    assert_eq!(result.report.outcome, RunOutcome::Cancelled);
    assert_eq!(result.report.counts().incomplete_checks, 4);
    assert!(
        theseus::report::load_journal(result.report_path.parent().unwrap())
            .unwrap()
            .resources
            .is_empty()
    );
}

#[test]
fn missing_pass_env_is_preflight_incomplete_without_script_or_container() {
    let (temp, mut recipe) = recipe();
    let missing = format!("THESEUS_MISSING_TEST_{}_FORWARD", std::process::id());
    assert!(std::env::var_os(&missing).is_none());
    recipe
        .recipe
        .profiles
        .get_mut(&ProfileName::User)
        .unwrap()
        .steps
        .get_mut(&StepName::InstallTarget)
        .unwrap()
        .pass_env
        .push(missing.clone());
    let options = RunOptions {
        output_root: temp.path().join("runs"),
        ..RunOptions::all(&recipe)
    };
    let result = engine::run_with(
        &recipe,
        options,
        CancellationToken::default(),
        &mut Fake { fault: "" },
        |_| {},
    )
    .unwrap();
    assert_eq!(result.report.outcome, RunOutcome::Incomplete);
    assert!(result.report.errors.iter().any(|e| e.contains(&missing)));
    assert!(result.report.scenarios.is_empty());
    assert!(
        theseus::report::load_journal(result.report_path.parent().unwrap())
            .unwrap()
            .resources
            .is_empty()
    );
}
