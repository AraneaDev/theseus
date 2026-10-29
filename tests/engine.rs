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

#[test]
fn version_two_clean_defaults_run_without_upgrade_inputs() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("target")).unwrap();
    let path = temp.path().join("theseus.toml");
    std::fs::write(&path, include_str!("fixtures/recipes/v2_clean.toml")).unwrap();
    let recipe = load(&path).unwrap();
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
    assert_eq!(result.report.outcome, RunOutcome::Passed);
    assert_eq!(result.report.selected_scenarios, vec![ScenarioKind::Clean]);
    assert_eq!(result.report.scenarios.len(), 1);
    assert_eq!(result.report.scenarios[0].scenario, ScenarioKind::Clean);
}

/// Only the container boundary is simulated; lifecycle, assertions, persistence,
/// observations, diffs, outcome aggregation and cleanup journals are real.
#[derive(Default)]
struct ReinstallEnvironment {
    fault: &'static str,
    files: BTreeMap<String, BTreeMap<String, theseus::snapshot::FileEntry>>,
    phase: String,
}
fn entry(kind: FileKind, content: &str) -> theseus::snapshot::FileEntry {
    theseus::snapshot::FileEntry {
        kind,
        mode: if kind == FileKind::Directory {
            0o755
        } else {
            0o644
        },
        uid: 10001,
        gid: 10001,
        size: content.len() as u64,
        sha256: (kind == FileKind::File).then(|| content.to_owned()),
        link_target: None,
    }
}
impl Environment for ReinstallEnvironment {
    fn prepare(
        &mut self,
        _: &ValidatedRecipe,
        _: bool,
        _: &CancellationToken,
    ) -> Result<PreparedEnvironment> {
        Ok(PreparedEnvironment {
            image_id: "sha256:reinstall".into(),
            artifacts: vec![],
        })
    }
    fn create(&mut self, run: &str, scenario: &str, _: &ValidatedRecipe) -> Result<OwnedResource> {
        let id = format!("{run}-{scenario}");
        self.files.insert(
            id.clone(),
            BTreeMap::from([("/home/theseus".into(), entry(FileKind::Directory, ""))]),
        );
        Ok(OwnedResource {
            container_id: id,
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
        Ok(())
    }
    fn execute(
        &mut self,
        resource: &OwnedResource,
        request: CommandRequest,
        _: &ReportStore,
        _: &CancellationToken,
    ) -> Result<ExecutedCommand> {
        self.phase = request.phase.clone();
        let files = self
            .files
            .get_mut(&resource.container_id)
            .expect("same owned scenario container");
        let mut record = CommandRecord::new(request.id, &request.phase);
        record.argv = request.argv;
        record.identity = request.identity;
        record.exit_status = Some(0);
        record.outcome = CommandOutcome::Succeeded;
        match request.phase.as_str() {
            "install_target" => {
                files.insert("/home/theseus/bin".into(), entry(FileKind::File, "target"));
            }
            "configure" => {
                files.insert(
                    "/home/theseus/config".into(),
                    entry(FileKind::Directory, ""),
                );
                files.insert(
                    "/home/theseus/config/settings".into(),
                    entry(FileKind::File, "configured"),
                );
                files.insert(
                    "/home/theseus/log".into(),
                    entry(FileKind::File, "configured"),
                );
            }
            "exercise_before_reinstall" => {
                files.insert(
                    "/home/theseus/log".into(),
                    entry(FileKind::File, "first exercise"),
                );
            }
            "reinstall" => match self.fault {
                "reset" => {
                    files.insert(
                        "/home/theseus/config/settings".into(),
                        entry(FileKind::File, "reset"),
                    );
                }
                "duplicate" => {
                    files.insert(
                        "/home/theseus/config/settings".into(),
                        entry(FileKind::File, "configured\nconfigured"),
                    );
                }
                "mode" => {
                    files.get_mut("/home/theseus/config/settings").unwrap().mode = 0o600;
                }
                "add" => {
                    files.insert(
                        "/home/theseus/config/extra".into(),
                        entry(FileKind::File, "new"),
                    );
                }
                "remove" => {
                    files.remove("/home/theseus/config/settings");
                }
                "reject" => {
                    record.outcome = CommandOutcome::Failed;
                    record.exit_status = Some(1);
                }
                "timeout" | "cancel" => {
                    record.exit_status = None;
                    record.timed_out = self.fault == "timeout";
                    record.cancelled = self.fault == "cancel";
                    record.outcome = if record.cancelled {
                        CommandOutcome::Cancelled
                    } else {
                        CommandOutcome::TimedOut
                    };
                }
                _ => {}
            },
            "check:runtime" => {
                files.insert(
                    "/home/theseus/log".into(),
                    entry(FileKind::File, "check side effect"),
                );
            }
            "exercise_after_reinstall" => {
                files.insert(
                    "/home/theseus/log".into(),
                    entry(FileKind::File, "second exercise"),
                );
            }
            "uninstall" => {
                files.remove("/home/theseus/bin");
            }
            _ => {}
        }
        if (self.fault == "first_install" && request.phase == "install_target")
            || (self.fault == "first_exercise" && request.phase == "exercise_before_reinstall")
        {
            record.exit_status = Some(1);
            record.outcome = CommandOutcome::Failed;
        }
        Ok(ExecutedCommand {
            record,
            stdout: String::new(),
            stdout_complete: true,
        })
    }
    fn observe(
        &mut self,
        resource: &OwnedResource,
        _: &[String],
        _: Limits,
        _: &CancellationToken,
    ) -> Result<Vec<Snapshot>> {
        Ok(vec![
            if self.fault == "incomplete" && self.phase == "exercise_before_reinstall" {
                Snapshot::incomplete("/home/theseus", "reference truncated")
            } else {
                Snapshot {
                    root: "/home/theseus".into(),
                    completeness: theseus::snapshot::ObservationStatus::Complete,
                    entries: self.files[&resource.container_id].clone(),
                }
            },
        ])
    }
    fn stop(&mut self, _: &OwnedResource) -> Result<()> {
        Ok(())
    }
    fn remove(&mut self, _: &OwnedResource) -> Result<()> {
        Ok(())
    }
}
fn reinstall_recipe() -> (tempfile::TempDir, ValidatedRecipe) {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("target")).unwrap();
    let user = include_str!("fixtures/recipes/v2_reinstall.toml");
    let system = user[user.find("[profiles.user.steps").unwrap()..]
        .replace("profiles.user", "profiles.system")
        .replace("install.sh user", "install.sh system");
    let path = temp.path().join("theseus.toml");
    std::fs::write(&path, format!("{user}\n{system}")).unwrap();
    (temp, load(&path).expect("reinstall fixture must load"))
}
fn run_reinstall(
    fault: &'static str,
    extra_checks: &str,
) -> (tempfile::TempDir, engine::RunResult) {
    let (temp, mut recipe) = reinstall_recipe();
    if !extra_checks.is_empty() {
        let text = std::fs::read_to_string(&recipe.path).unwrap();
        std::fs::write(&recipe.path, format!("{text}\n{extra_checks}")).unwrap();
        recipe = load(&recipe.path).unwrap();
    }
    let options = RunOptions {
        output_root: temp.path().join("runs"),
        ..RunOptions::all(&recipe)
    };
    let result = engine::run_with(
        &recipe,
        options,
        CancellationToken::default(),
        &mut ReinstallEnvironment {
            fault,
            ..Default::default()
        },
        |_| {},
    )
    .unwrap();
    (temp, result)
}
fn stage<'a>(scenario: &'a ScenarioReport, name: &str) -> &'a StageReport {
    scenario.stages.iter().find(|s| s.name == name).unwrap()
}
#[test]
fn reinstall_exact_order_identities_target_and_distinct_profile_containers() {
    let (_temp, result) = run_reinstall("", "");
    assert_eq!(result.report.outcome, RunOutcome::Passed);
    assert_eq!(result.report.scenarios.len(), 2);
    assert_ne!(
        result.report.scenarios[0].container_id,
        result.report.scenarios[1].container_id
    );
    for scenario in &result.report.scenarios {
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
                "exercise_before_reinstall",
                "before_reinstall",
                "reinstall",
                "reinstalled",
                "reinstalled:after_checks",
                "exercise_after_reinstall",
                "uninstall",
                "uninstalled",
                "uninstalled:after_checks",
                "final",
            ]
        );
        assert_eq!(
            stage(scenario, "install_target").commands[0].argv,
            stage(scenario, "reinstall").commands[0].argv
        );
        assert_eq!(
            stage(scenario, "exercise_before_reinstall").commands[0].argv,
            stage(scenario, "exercise_after_reinstall").commands[0].argv
        );
        for s in &scenario.stages {
            for c in &s.commands {
                let installer =
                    ["install_target", "reinstall", "uninstall"].contains(&c.phase.as_str());
                assert_eq!(
                    c.identity.uid,
                    if installer && scenario.profile == ProfileName::System {
                        0
                    } else {
                        10001
                    }
                );
                assert_eq!(c.identity.gid, c.identity.uid);
            }
        }
        let check = &stage(scenario, "reinstalled").checks[0];
        assert_eq!(check.outcome, CheckOutcome::Passed);
        let json = serde_json::to_value(check).unwrap();
        assert_eq!(json["comparison"]["before_stage"], "before_reinstall");
        assert_eq!(json["comparison"]["after_stage"], "reinstalled");
        assert!(matches!(scenario.teardown, TeardownStatus::Complete));
        assert!(
            scenario
                .findings
                .iter()
                .any(|f| f.path.as_deref() == Some("/home/theseus/config/settings"))
        );
    }
    let journal = theseus::report::load_journal(result.report_path.parent().unwrap()).unwrap();
    assert_eq!(journal.resources.len(), 2);
    assert!(journal.resources.iter().all(|r| r.removed));
    let loaded = theseus::report::load(&result.report_path).unwrap();
    assert_eq!(
        loaded.scenarios[0]
            .stages
            .iter()
            .flat_map(|s| &s.checks)
            .find(|c| c.id == "stable")
            .unwrap()
            .comparison
            .as_ref()
            .unwrap()
            .before_stage,
        "before_reinstall"
    );
}
#[test]
fn reinstall_reference_follows_first_exercise_and_precedes_check_and_second_exercise() {
    let (_temp, result) = run_reinstall(
        "",
        "[[profiles.user.checks]]\nid='runtime'\ncheckpoint='reinstalled'\nkind='command'\nscript='touch log'\nexpected_exit=0\n[[profiles.user.checks]]\nid='stable-log'\ncheckpoint='reinstalled'\nkind='unchanged'\npath='/home/theseus/log'\n",
    );
    assert_eq!(result.report.outcome, RunOutcome::Passed);
    let scenario = &result.report.scenarios[0];
    for name in ["before_reinstall", "reinstalled"] {
        assert_eq!(
            stage(scenario, name).snapshots[0].entries["/home/theseus/log"]
                .sha256
                .as_deref(),
            Some("first exercise")
        );
    }
    assert_eq!(
        stage(scenario, "reinstalled:after_checks").snapshots[0].entries["/home/theseus/log"]
            .sha256
            .as_deref(),
        Some("check side effect")
    );
    assert_eq!(
        stage(scenario, "exercise_after_reinstall").snapshots[0].entries["/home/theseus/log"]
            .sha256
            .as_deref(),
        Some("second exercise")
    );
    assert!(
        stage(scenario, "reinstalled")
            .checks
            .iter()
            .all(|c| c.outcome == CheckOutcome::Passed)
    );
}
#[test]
fn preservation_keeps_configured_reference_separate_from_before_reinstall() {
    let (_temp, result) = run_reinstall(
        "",
        "[[profiles.user.checks]]\nid='retained-log'\ncheckpoint='reinstalled'\nkind='preserved'\npath='/home/theseus/log'\n[[profiles.user.checks]]\nid='uninstalled-log'\ncheckpoint='uninstalled'\nkind='preserved'\npath='/home/theseus/log'\n",
    );
    assert_eq!(result.report.outcome, RunOutcome::Failed);
    let scenario = &result.report.scenarios[0];
    assert_eq!(
        stage(scenario, "reinstalled")
            .checks
            .iter()
            .find(|c| c.id == "retained-log")
            .unwrap()
            .outcome,
        CheckOutcome::Failed
    );
    assert_eq!(
        stage(scenario, "uninstalled").checks[0].outcome,
        CheckOutcome::Failed
    );
}
#[test]
fn repeat_detects_reset_duplicates_permissions_and_directory_additions_removals() {
    for fault in ["reset", "duplicate", "mode", "add", "remove"] {
        let (_temp, result) = run_reinstall(fault, "");
        assert_eq!(result.report.outcome, RunOutcome::Failed, "{fault}");
        for scenario in &result.report.scenarios {
            assert_eq!(
                stage(scenario, "reinstalled").checks[0].outcome,
                CheckOutcome::Failed,
                "{fault}"
            );
            assert!(!stage(scenario, "reinstall").stage_diff.is_empty());
            assert_eq!(
                stage(scenario, "exercise_after_reinstall").outcome,
                StageOutcome::Skipped
            );
            assert_eq!(stage(scenario, "uninstall").commands.len(), 1);
        }
    }
}
#[test]
fn rejected_repeat_skips_second_exercise_but_uninstalls_and_keeps_failure() {
    let (_temp, result) = run_reinstall("reject", "");
    assert_eq!(result.report.outcome, RunOutcome::Failed);
    for scenario in &result.report.scenarios {
        assert_eq!(
            stage(scenario, "reinstalled").checks[0].outcome,
            CheckOutcome::Passed
        );
        assert_eq!(
            stage(scenario, "exercise_after_reinstall").outcome,
            StageOutcome::Skipped
        );
        assert_eq!(stage(scenario, "uninstall").commands.len(), 1);
        assert!(matches!(scenario.teardown, TeardownStatus::Complete));
    }
}
#[test]
fn incomplete_repeat_reference_cannot_pass() {
    let (_temp, result) = run_reinstall("incomplete", "");
    assert_eq!(result.report.outcome, RunOutcome::Incomplete);
    for scenario in &result.report.scenarios {
        assert_eq!(
            stage(scenario, "reinstalled").checks[0].outcome,
            CheckOutcome::Incomplete
        );
    }
}
#[test]
fn repeat_timeout_and_cancel_keep_incomplete_evidence_and_owned_cleanup() {
    for (fault, expected) in [
        ("timeout", RunOutcome::Incomplete),
        ("cancel", RunOutcome::Cancelled),
    ] {
        let (_temp, result) = run_reinstall(fault, "");
        assert_eq!(result.report.outcome, expected);
        let scenario = &result.report.scenarios[0];
        assert_eq!(
            stage(scenario, "reinstalled").checks[0].outcome,
            CheckOutcome::Incomplete
        );
        assert_eq!(stage(scenario, "uninstall").outcome, StageOutcome::Skipped);
        assert!(stage(scenario, "final").snapshots.iter().all(|s| matches!(
            s.completeness,
            theseus::snapshot::ObservationStatus::Incomplete { .. }
        )));
        assert!(matches!(scenario.teardown, TeardownStatus::Complete));
        assert!(
            theseus::report::load_journal(result.report_path.parent().unwrap())
                .unwrap()
                .resources
                .iter()
                .all(|r| r.removed)
        );
    }
}

#[test]
fn reinstall_failed_prerequisites_skip_repeat_contracts_without_losing_known_failure() {
    for fault in ["first_install", "first_exercise"] {
        let (_temp, result) = run_reinstall(fault, "");
        assert_eq!(result.report.outcome, RunOutcome::Failed, "{fault}");
        for scenario in &result.report.scenarios {
            assert_eq!(stage(scenario, "reinstall").outcome, StageOutcome::Skipped);
            assert_eq!(
                stage(scenario, "reinstalled").checks[0].outcome,
                CheckOutcome::Skipped
            );
            assert_eq!(
                stage(scenario, "exercise_after_reinstall").outcome,
                StageOutcome::Skipped
            );
            assert_eq!(stage(scenario, "uninstall").commands.len(), 1);
            assert!(matches!(scenario.teardown, TeardownStatus::Complete));
            assert_eq!(
                stage(scenario, "final").snapshots[0].completeness,
                theseus::snapshot::ObservationStatus::Complete
            );
        }
    }
}

#[test]
fn environment_default_interruption_and_restart_fail_closed() {
    let mut environment = Fake { fault: "" };
    let temporary = tempfile::tempdir().unwrap();
    let store = theseus::report::create(&temporary.path().join("run")).unwrap();
    let resource = OwnedResource {
        container_id: "owned".into(),
        labels: BTreeMap::new(),
        removed: false,
    };
    let cancel = CancellationToken::default();
    assert!(
        environment
            .execute_interrupted(
                &resource,
                CommandRequest {
                    id: "unsupported".into(),
                    phase: "boundary".into(),
                    identity: ExecutionIdentity::runtime(),
                    argv: vec!["/bin/true".into()],
                    env: BTreeMap::new(),
                    pass_env: vec![],
                    timeout_seconds: 10,
                    expected_exit: 0,
                    log_limit: 1024,
                },
                &store,
                &cancel
            )
            .is_err()
    );
    assert!(environment.restart(&resource, &cancel).is_err());
}
