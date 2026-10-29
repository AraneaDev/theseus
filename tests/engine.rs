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
            CheckOutcome::Skipped
        );
        // The unverified reference now blocks the repeated mutation itself.
        assert!(stage(scenario, "reinstall").commands.is_empty());
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

#[derive(Default)]
struct InterruptionEnvironment {
    inner: ReinstallEnvironment,
    fault: &'static str,
    stopped: bool,
    setups: usize,
    restarts: usize,
    calls: Vec<(String, String)>,
}
impl Environment for InterruptionEnvironment {
    fn prepare(
        &mut self,
        r: &ValidatedRecipe,
        p: bool,
        c: &CancellationToken,
    ) -> Result<PreparedEnvironment> {
        self.inner.prepare(r, p, c)
    }
    fn create(&mut self, run: &str, sid: &str, r: &ValidatedRecipe) -> Result<OwnedResource> {
        let resource = self.inner.create(run, sid, r)?;
        self.inner
            .files
            .get_mut(&resource.container_id)
            .unwrap()
            .insert(
                "/home/theseus/data".into(),
                entry(FileKind::File, "baseline-data"),
            );
        Ok(resource)
    }
    fn setup(
        &mut self,
        _: &OwnedResource,
        _: &ValidatedRecipe,
        _: &CancellationToken,
    ) -> Result<()> {
        self.setups += 1;
        Ok(())
    }
    fn execute(
        &mut self,
        r: &OwnedResource,
        q: CommandRequest,
        _: &ReportStore,
        cancel: &CancellationToken,
    ) -> Result<ExecutedCommand> {
        assert!(!cancel.is_cancelled(), "no command after cancellation");
        assert!(!self.stopped, "execute requires running container");
        self.calls.push((r.container_id.clone(), q.phase.clone()));
        let f = self.inner.files.get_mut(&r.container_id).unwrap();
        let mut record = CommandRecord::new(q.id, &q.phase);
        record.identity = q.identity;
        record.argv = q.argv;
        record.outcome = CommandOutcome::Succeeded;
        record.exit_status = Some(0);
        match q.phase.as_str() {
            "install_base" => {
                f.insert("/home/theseus/bin".into(), entry(FileKind::File, "base"));
            }
            "configure" => {
                f.insert(
                    "/home/theseus/data".into(),
                    entry(FileKind::File, "configured"),
                );
            }
            "exercise_before_interrupt" => {
                f.insert(
                    "/home/theseus/data".into(),
                    entry(FileKind::File, "exercised"),
                );
            }
            "recover_install" | "recover_upgrade" => {
                if self.fault == "recovery" {
                    record.outcome = CommandOutcome::Failed;
                    record.exit_status = Some(7);
                } else {
                    f.remove("/home/theseus/partial");
                    f.insert("/home/theseus/bin".into(), entry(FileKind::File, "target"));
                }
            }
            "check:partial-writer" => {
                f.insert(
                    "/home/theseus/data".into(),
                    entry(FileKind::File, "partial-check"),
                );
            }
            "uninstall" => {
                f.remove("/home/theseus/bin");
                f.remove("/home/theseus/partial");
            }
            _ => {}
        }
        let stdout = if q.phase.starts_with("resolve:") {
            "/home/theseus/bin"
        } else {
            "2.0"
        };
        Ok(ExecutedCommand {
            record,
            stdout: stdout.into(),
            stdout_complete: true,
        })
    }
    fn execute_interrupted(
        &mut self,
        r: &OwnedResource,
        q: CommandRequest,
        _: &ReportStore,
        c: &CancellationToken,
    ) -> Result<ExecutedCommand> {
        assert!(!c.is_cancelled(), "no interruption after cancellation");
        assert!(!self.stopped);
        self.calls.push((r.container_id.clone(), q.phase.clone()));
        if self.fault != "partial" {
            self.inner.files.get_mut(&r.container_id).unwrap().insert(
                "/home/theseus/partial".into(),
                entry(FileKind::File, "partial-only"),
            );
        }
        let mut record = CommandRecord::new(q.id, &q.phase);
        record.identity = q.identity;
        record.argv = q.argv;
        record.outcome = CommandOutcome::Interrupted;
        record.interruption = Some(InterruptionEvidence {
            mechanism: InterruptionMechanism::ContainerSigkill,
            readiness_verified: true,
            stopped_verified: true,
        });
        self.stopped = true;
        if self.fault == "early" {
            self.stopped = false;
            record.outcome = CommandOutcome::Failed;
            record.exit_status = Some(0);
            record.interruption.as_mut().unwrap().stopped_verified = false;
            record.interruption.as_mut().unwrap().readiness_verified = false;
        }
        if self.fault == "proof" {
            record.interruption = None;
        }
        if self.fault == "cancel-stop" {
            c.cancel();
        }
        Ok(ExecutedCommand {
            record,
            stdout: String::new(),
            stdout_complete: false,
        })
    }
    fn restart(&mut self, r: &OwnedResource, c: &CancellationToken) -> Result<RestartEvidence> {
        assert!(self.stopped);
        assert!(!c.is_cancelled());
        self.restarts += 1;
        if self.fault == "restart" {
            anyhow::bail!("restart unavailable")
        }
        self.stopped = false;
        if self.fault == "cancel-restart" {
            c.cancel();
        }
        Ok(RestartEvidence {
            container_id: if self.fault == "wrong-id" {
                "other".into()
            } else {
                r.container_id.clone()
            },
            running_verified: true,
        })
    }
    fn observe(
        &mut self,
        r: &OwnedResource,
        _: &[String],
        _: Limits,
        _: &CancellationToken,
    ) -> Result<Vec<Snapshot>> {
        assert!(!self.stopped, "never observe stopped container");
        if self.fault == "incomplete" && self.restarts > 0 {
            return Ok(vec![Snapshot::incomplete(
                "/home/theseus",
                "partial observation unavailable",
            )]);
        }
        Ok(vec![Snapshot {
            root: "/home/theseus".into(),
            completeness: theseus::snapshot::ObservationStatus::Complete,
            entries: self.inner.files[&r.container_id].clone(),
        }])
    }
    fn stop(&mut self, _: &OwnedResource) -> Result<()> {
        self.stopped = false;
        Ok(())
    }
    fn remove(&mut self, _: &OwnedResource) -> Result<()> {
        self.stopped = false;
        Ok(())
    }
}
fn interruption_recipe() -> (tempfile::TempDir, ValidatedRecipe) {
    let temp = tempfile::tempdir().unwrap();
    for d in ["base", "target"] {
        std::fs::create_dir(temp.path().join(d)).unwrap();
    }
    let user = include_str!("fixtures/recipes/v3_interruption.toml");
    let system = user[user.find("[profiles.user.steps").unwrap()..]
        .replace("profiles.user", "profiles.system");
    let path = temp.path().join("theseus.toml");
    std::fs::write(&path, format!("{user}\n{system}")).unwrap();
    (temp, load(&path).unwrap())
}
fn run_interruption(
    fault: &'static str,
) -> (
    tempfile::TempDir,
    engine::RunResult,
    InterruptionEnvironment,
) {
    let (temp, recipe) = interruption_recipe();
    let mut env = InterruptionEnvironment {
        fault,
        ..Default::default()
    };
    let options = RunOptions {
        output_root: temp.path().join("runs"),
        ..RunOptions::all(&recipe)
    };
    let result = engine::run_with(
        &recipe,
        options,
        CancellationToken::default(),
        &mut env,
        |_| {},
    )
    .unwrap();
    (temp, result, env)
}
#[test]
fn interruption_real_orchestration_order_references_identity_and_same_container() {
    let (_temp, result, env) = run_interruption("");
    assert_eq!(result.report.outcome, RunOutcome::Passed);
    assert_eq!(env.setups, 4);
    assert_eq!(env.restarts, 4);
    let install = [
        "baseline",
        "before_interrupt",
        "interrupt_install",
        "restart_after_interrupt",
        "interrupted",
        "interrupted:after_checks",
        "recover_install",
        "recovered",
        "recovered:after_checks",
        "configure",
        "configured",
        "configured:after_checks",
        "exercise_after_recovery",
        "uninstall",
        "uninstalled",
        "uninstalled:after_checks",
        "final",
    ];
    let upgrade = [
        "baseline",
        "install_base",
        "base_installed",
        "base_installed:after_checks",
        "configure",
        "configured",
        "configured:after_checks",
        "exercise_before_interrupt",
        "before_interrupt",
        "interrupt_upgrade",
        "restart_after_interrupt",
        "interrupted",
        "interrupted:after_checks",
        "recover_upgrade",
        "recovered",
        "recovered:after_checks",
        "exercise_after_recovery",
        "uninstall",
        "uninstalled",
        "uninstalled:after_checks",
        "final",
    ];
    let ids: std::collections::BTreeSet<_> = result
        .report
        .scenarios
        .iter()
        .map(|s| s.container_id.clone())
        .collect();
    assert_eq!(ids.len(), 4);
    for s in &result.report.scenarios {
        let is_install = s.scenario.to_string() == "interrupted_install";
        assert_eq!(
            s.stages.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            if is_install {
                install.to_vec()
            } else {
                upgrade.to_vec()
            }
        );
        let interrupted = stage(
            s,
            if is_install {
                "interrupt_install"
            } else {
                "interrupt_upgrade"
            },
        );
        assert!(interrupted.snapshots.is_empty());
        assert!(stage(s, "restart_after_interrupt").snapshots.is_empty());
        assert_eq!(
            stage(s, "restart_after_interrupt")
                .restart
                .as_ref()
                .unwrap()
                .container_id,
            s.container_id.clone().unwrap()
        );
        assert!(
            stage(s, "interrupted").snapshots[0]
                .entries
                .contains_key("/home/theseus/partial")
        );
        assert!(
            !stage(s, "recovered").snapshots[0]
                .entries
                .contains_key("/home/theseus/partial")
        );
        let data = stage(s, "recovered")
            .checks
            .iter()
            .find(|c| c.id == "data")
            .unwrap();
        assert_eq!(
            data.comparison.as_ref().unwrap().before_stage,
            "before_interrupt"
        );
        for c in s.stages.iter().flat_map(|s| &s.commands) {
            let root = s.profile == ProfileName::System
                && [
                    "install_base",
                    "interrupt_install",
                    "interrupt_upgrade",
                    "recover_install",
                    "recover_upgrade",
                    "uninstall",
                ]
                .contains(&c.phase.as_str());
            assert_eq!(c.identity.uid, if root { 0 } else { 10001 });
        }
        assert!(
            env.calls
                .iter()
                .filter(|(id, _)| Some(id) == s.container_id.as_ref())
                .any(|(_, p)| p == "uninstall")
        );
    }
    assert_eq!(
        theseus::report::load(&result.report_path).unwrap().outcome,
        RunOutcome::Passed
    );
}
#[test]
fn interruption_faults_gate_recovery_restart_and_uninstall() {
    for (fault, outcome, restart, recover, uninstall) in [
        ("partial", RunOutcome::Failed, true, false, true),
        ("recovery", RunOutcome::Failed, true, true, true),
        ("early", RunOutcome::Failed, false, false, true),
        ("proof", RunOutcome::Incomplete, false, false, false),
        ("restart", RunOutcome::Incomplete, true, false, false),
        ("wrong-id", RunOutcome::Incomplete, true, false, false),
        ("cancel-stop", RunOutcome::Cancelled, false, false, false),
        ("cancel-restart", RunOutcome::Cancelled, true, false, false),
        ("incomplete", RunOutcome::Incomplete, true, false, true),
    ] {
        let (_temp, result, env) = run_interruption(fault);
        assert_eq!(result.report.outcome, outcome, "{fault}");
        assert_eq!(env.restarts > 0, restart, "{fault}");
        assert_eq!(
            env.calls.iter().any(|(_, p)| p.starts_with("recover_")),
            recover,
            "{fault}"
        );
        assert_eq!(
            env.calls.iter().any(|(_, p)| p == "uninstall"),
            uninstall,
            "{fault}"
        );
        assert!(
            result
                .report
                .scenarios
                .iter()
                .all(|s| matches!(s.teardown, TeardownStatus::Complete))
        );
    }
}
#[test]
fn interruption_cancellation_at_event_boundaries_never_mutates_after_cancel() {
    // Exact stage names are independently asserted in the lifecycle-order test.
    let (_reference_temp, reference, _) = run_interruption("");
    for scenario in &reference.report.scenarios[..2] {
        for kind in [
            engine::EventKind::StageStarted,
            engine::EventKind::StageFinished,
        ] {
            for name in scenario.stages.iter().map(|stage| stage.name.as_str()) {
                let (temp, recipe) = interruption_recipe();
                let mut env = InterruptionEnvironment::default();
                let cancel = CancellationToken::default();
                let trigger = cancel.clone();
                let options = RunOptions {
                    profiles: vec![ProfileName::User],
                    scenarios: vec![scenario.scenario],
                    output_root: temp.path().join("runs"),
                    ..RunOptions::all(&recipe)
                };
                let result = engine::run_with(&recipe, options, cancel, &mut env, |event| {
                    if event.kind == kind && event.stage.as_deref() == Some(name) {
                        trigger.cancel()
                    }
                })
                .unwrap();
                assert_eq!(
                    result.report.outcome,
                    RunOutcome::Cancelled,
                    "{kind:?} {name}"
                );
                assert!(matches!(
                    result.report.scenarios[0].teardown,
                    TeardownStatus::Complete
                ));
            }
        }
    }
}
#[test]
fn interruption_checks_and_exercise_do_not_rebase_comparison_or_configuration() {
    for preserved in [false, true] {
        let (temp, mut recipe) = interruption_recipe();
        let profile = recipe.recipe.profiles.get_mut(&ProfileName::User).unwrap();
        profile.checks.push(Check {
            id: "partial-writer".into(),
            checkpoint: Checkpoint::Interrupted,
            expectation: Expectation::Command {
                script: "mutate data".into(),
                timeout_seconds: 10,
                env: BTreeMap::new(),
                pass_env: vec![],
                expected_exit: 0,
                stdout_contains: None,
            },
        });
        if preserved {
            profile.checks.push(Check {
                id: "configured-data".into(),
                checkpoint: Checkpoint::Interrupted,
                expectation: Expectation::Preserved {
                    path: "/home/theseus/data".into(),
                },
            });
        }
        let options = RunOptions {
            profiles: vec![ProfileName::User],
            scenarios: vec![ScenarioKind::InterruptedUpgrade],
            output_root: temp.path().join("runs"),
            ..RunOptions::all(&recipe)
        };
        let result = engine::run_with(
            &recipe,
            options,
            CancellationToken::default(),
            &mut InterruptionEnvironment::default(),
            |_| {},
        )
        .unwrap();
        assert_eq!(result.report.outcome, RunOutcome::Failed);
        let s = &result.report.scenarios[0];
        let hash = |name: &str| {
            stage(s, name).snapshots[0].entries["/home/theseus/data"]
                .sha256
                .clone()
        };
        assert_eq!(hash("configured"), Some("configured".into()));
        assert_eq!(hash("before_interrupt"), Some("exercised".into()));
        assert_eq!(hash("interrupted"), Some("exercised".into()));
        assert_eq!(
            hash("interrupted:after_checks"),
            Some("partial-check".into())
        );
        if preserved {
            assert_eq!(
                stage(s, "interrupted")
                    .checks
                    .iter()
                    .find(|c| c.id == "configured-data")
                    .unwrap()
                    .outcome,
                CheckOutcome::Failed
            );
            assert!(stage(s, "recover_upgrade").commands.is_empty());
        } else {
            let check = stage(s, "recovered")
                .checks
                .iter()
                .find(|c| c.id == "data")
                .unwrap();
            assert_eq!(check.outcome, CheckOutcome::Failed);
            assert_eq!(
                check.comparison,
                ComparisonReference::for_checkpoint(Checkpoint::Recovered)
            );
        }
    }
}

#[derive(Default)]
struct DowngradeEnvironment {
    inner: ReinstallEnvironment,
    rejection: bool,
    fault: &'static str,
    calls: Vec<(String, String, i32)>,
    removed: usize,
}
impl Environment for DowngradeEnvironment {
    fn prepare(
        &mut self,
        r: &ValidatedRecipe,
        p: bool,
        c: &CancellationToken,
    ) -> Result<PreparedEnvironment> {
        assert!(r.base_artifact.is_some());
        self.inner.prepare(r, p, c)
    }
    fn create(&mut self, run: &str, sid: &str, r: &ValidatedRecipe) -> Result<OwnedResource> {
        self.inner.create(run, sid, r)
    }
    fn setup(
        &mut self,
        r: &OwnedResource,
        v: &ValidatedRecipe,
        c: &CancellationToken,
    ) -> Result<()> {
        self.inner.setup(r, v, c)
    }
    fn execute(
        &mut self,
        r: &OwnedResource,
        q: CommandRequest,
        _: &ReportStore,
        c: &CancellationToken,
    ) -> Result<ExecutedCommand> {
        assert!(!c.is_cancelled());
        self.inner.phase = q.phase.clone();
        self.calls
            .push((r.container_id.clone(), q.phase.clone(), q.expected_exit));
        let files = self.inner.files.get_mut(&r.container_id).unwrap();
        let mut record = CommandRecord::new(q.id, &q.phase);
        record.identity = q.identity;
        record.argv = q.argv;
        record.exit_status = Some(0);
        record.outcome = CommandOutcome::Succeeded;
        let mut stdout = "2.0".to_string();
        if q.phase.starts_with("resolve:") {
            stdout = "/home/theseus/bin".into();
        }
        match q.phase.as_str() {
            "install_target" => {
                files.insert(
                    "/home/theseus/bin".into(),
                    entry(
                        if self.fault == "directory-binary" {
                            FileKind::Directory
                        } else {
                            FileKind::File
                        },
                        "newer",
                    ),
                );
            }
            "configure" => {
                files.insert("/home/theseus/data".into(), entry(FileKind::Directory, ""));
                files.insert(
                    "/home/theseus/data/record".into(),
                    entry(FileKind::File, "configured"),
                );
            }
            "exercise_before_downgrade" => {
                files.insert(
                    "/home/theseus/data/record".into(),
                    entry(FileKind::File, "exercised"),
                );
            }
            "downgrade" => {
                if self.rejection {
                    record.exit_status = Some(9);
                    stdout = "newer data refusal".into();
                    match self.fault {
                        "binary" => {
                            files.insert(
                                "/home/theseus/bin".into(),
                                entry(FileKind::File, "changed"),
                            );
                        }
                        "add" => {
                            files.insert(
                                "/home/theseus/data/extra".into(),
                                entry(FileKind::File, "extra"),
                            );
                        }
                        "remove" => {
                            files.remove("/home/theseus/data/record");
                        }
                        "wrong-status" => record.exit_status = Some(8),
                        "success" => record.exit_status = Some(0),
                        "marker" => stdout = "different".into(),
                        "receipt" => record.exit_status = None,
                        "transport" => record.error = Some("lost control".into()),
                        "cancel" => {
                            record.cancelled = true;
                            record.outcome = CommandOutcome::Cancelled;
                            c.cancel();
                        }
                        "timeout" => {
                            record.timed_out = true;
                            record.outcome = CommandOutcome::TimedOut;
                        }
                        _ => {}
                    }
                } else {
                    files.insert("/home/theseus/bin".into(), entry(FileKind::File, "older"));
                    if self.fault == "wrong-status" {
                        record.exit_status = Some(9);
                    }
                }
                if !record.cancelled {
                    record.outcome = if record.exit_status == Some(q.expected_exit) {
                        CommandOutcome::Succeeded
                    } else {
                        CommandOutcome::Failed
                    };
                }
            }
            "check:older" => stdout = "1.0".into(),
            "check:older-data" => {
                stdout = "record survives".into();
                if self.fault == "reader" {
                    record.exit_status = Some(1);
                    record.outcome = CommandOutcome::Failed;
                }
            }
            "check:writer" => {
                files.insert(
                    "/home/theseus/data/record".into(),
                    entry(FileKind::File, "check-side-effect"),
                );
            }
            "uninstall" => {
                files.remove("/home/theseus/bin");
            }
            _ => {}
        }
        if (self.fault == "initial" && q.phase == "exercise_before_downgrade")
            || (self.fault == "install" && q.phase == "install_target")
            || (self.fault == "configure" && q.phase == "configure")
            || (self.fault == "newer-check" && q.phase == "check:newer")
        {
            record.exit_status = Some(1);
            record.outcome = CommandOutcome::Failed;
        }
        Ok(ExecutedCommand {
            record,
            stdout,
            stdout_complete: self.fault != "truncated" || q.phase != "downgrade",
        })
    }
    fn observe(
        &mut self,
        r: &OwnedResource,
        _: &[String],
        _: Limits,
        _: &CancellationToken,
    ) -> Result<Vec<Snapshot>> {
        if self.fault == "incomplete" && self.inner.phase == "downgrade" {
            return Ok(vec![Snapshot::incomplete(
                "/home/theseus",
                "post-attempt unavailable",
            )]);
        }
        Ok(vec![Snapshot {
            root: "/home/theseus".into(),
            completeness: theseus::snapshot::ObservationStatus::Complete,
            entries: self.inner.files[&r.container_id].clone(),
        }])
    }
    fn stop(&mut self, _: &OwnedResource) -> Result<()> {
        Ok(())
    }
    fn remove(&mut self, _: &OwnedResource) -> Result<()> {
        self.removed += 1;
        Ok(())
    }
}
fn downgrade_recipe(rejection: bool) -> (tempfile::TempDir, ValidatedRecipe) {
    let temp = tempfile::tempdir().unwrap();
    for name in ["base", "target"] {
        std::fs::create_dir(temp.path().join(name)).unwrap();
    }
    let mut text = include_str!("fixtures/recipes/v4_downgrade.toml").to_string();
    if rejection {
        text = text.replace("expect = \"compatible\"\ndata_checks = [\"older-data\"]", "expect = \"safe_rejection\"\nexpected_exit = 9\nstdout_contains = \"newer data refusal\"\nbinary_checks = [\"binary-stable\"]\ndata_checks = [\"data-stable\"]");
        text.truncate(
            text.find("[[profiles.user.checks]]\nid = \"older\"")
                .unwrap(),
        );
        text.push_str("\n[[profiles.user.checks]]\nid = \"still-newer\"\ncheckpoint = \"downgrade_rejected\"\nkind = \"executable\"\nname = \"sample-cli\"\nresolved_path = \"/home/theseus/bin\"\nexpected_exit = 0\nstdout_contains = \"2.0\"\n[[profiles.user.checks]]\nid = \"binary-stable\"\ncheckpoint = \"downgrade_rejected\"\nkind = \"unchanged\"\npath = \"/home/theseus/bin\"\n[[profiles.user.checks]]\nid = \"data-stable\"\ncheckpoint = \"downgrade_rejected\"\nkind = \"unchanged\"\npath = \"/home/theseus/data\"\n");
    }
    let suffix = text[text.find("[profiles.user.downgrade]").unwrap()..]
        .replace("profiles.user", "profiles.system");
    let path = temp.path().join("theseus.toml");
    std::fs::write(&path, format!("{text}\n{suffix}")).unwrap();
    (temp, load(&path).expect("downgrade recipe loads"))
}
fn run_downgrade(
    rejection: bool,
    fault: &'static str,
) -> (tempfile::TempDir, engine::RunResult, DowngradeEnvironment) {
    let (temp, recipe) = downgrade_recipe(rejection);
    let mut env = DowngradeEnvironment {
        rejection,
        fault,
        ..Default::default()
    };
    let options = RunOptions {
        output_root: temp.path().join("runs"),
        ..RunOptions::all(&recipe)
    };
    let result = engine::run_with(
        &recipe,
        options,
        CancellationToken::default(),
        &mut env,
        |_| {},
    )
    .unwrap();
    (temp, result, env)
}
#[test]
fn downgrade_actual_flow_uses_declared_policy_same_container_and_persists_refusal() {
    for rejection in [false, true] {
        let (_temp, result, env) = run_downgrade(rejection, "");
        assert_eq!(result.report.outcome, RunOutcome::Passed);
        assert_eq!(env.removed, 2);
        for s in &result.report.scenarios {
            let mut expected = vec![
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
            ];
            expected.extend(if rejection {
                vec![
                    "downgrade_rejected",
                    "downgrade_rejected:after_checks",
                    "exercise_after_rejection",
                ]
            } else {
                vec![
                    "downgraded",
                    "downgraded:after_checks",
                    "exercise_downgraded",
                ]
            });
            expected.extend([
                "uninstall",
                "uninstalled",
                "uninstalled:after_checks",
                "final",
            ]);
            assert_eq!(
                s.stages.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
                expected
            );
            assert_eq!(
                stage(s, "before_downgrade").snapshots[0].entries["/home/theseus/data/record"]
                    .sha256
                    .as_deref(),
                Some("exercised")
            );
            for cmd in s.stages.iter().flat_map(|s| &s.commands) {
                let root = s.profile == ProfileName::System
                    && ["install_target", "downgrade", "uninstall"].contains(&cmd.phase.as_str());
                assert_eq!(cmd.identity.uid, if root { 0 } else { 10001 });
                assert_eq!(cmd.identity.gid, cmd.identity.uid);
                if cmd.phase == "downgrade" {
                    assert!(cmd.argv.last().unwrap().contains("/inputs/base/"));
                    assert_eq!(cmd.exit_status, Some(if rejection { 9 } else { 0 }));
                    assert_eq!(
                        cmd.outcome.to_string(),
                        if rejection {
                            "expected_rejection"
                        } else {
                            "succeeded"
                        }
                    );
                }
            }
            assert!(
                env.calls
                    .iter()
                    .filter(|(id, _, _)| Some(id) == s.container_id.as_ref())
                    .all(|(_, phase, expected)| *expected
                        == if phase == "downgrade" && rejection {
                            9
                        } else {
                            0
                        })
            );
        }
        let loaded = theseus::report::load(&result.report_path).unwrap();
        assert_eq!(loaded.schema_version, 6);
        assert_eq!(loaded.outcome, RunOutcome::Passed);
        if rejection {
            assert_eq!(
                stage(&loaded.scenarios[0], "downgrade").commands[0]
                    .outcome
                    .to_string(),
                "expected_rejection"
            );
        }
    }
}
#[test]
fn downgrade_actual_failures_preserve_selected_branch_evidence_and_cleanup() {
    for (rejection, fault, outcome) in [
        (true, "binary", RunOutcome::Failed),
        (true, "add", RunOutcome::Failed),
        (true, "remove", RunOutcome::Failed),
        (true, "directory-binary", RunOutcome::Failed),
        (true, "wrong-status", RunOutcome::Failed),
        (true, "success", RunOutcome::Failed),
        (true, "marker", RunOutcome::Failed),
        (true, "timeout", RunOutcome::Incomplete),
        (true, "incomplete", RunOutcome::Incomplete),
        (true, "receipt", RunOutcome::Incomplete),
        (true, "transport", RunOutcome::Incomplete),
        (true, "truncated", RunOutcome::Incomplete),
        (true, "cancel", RunOutcome::Cancelled),
        (false, "install", RunOutcome::Failed),
        (false, "configure", RunOutcome::Failed),
        (false, "newer-check", RunOutcome::Failed),
        (false, "reader", RunOutcome::Failed),
        (false, "wrong-status", RunOutcome::Failed),
        (false, "initial", RunOutcome::Failed),
    ] {
        let (_temp, result, env) = run_downgrade(rejection, fault);
        assert_eq!(result.report.outcome, outcome, "{rejection}/{fault}");
        assert_eq!(env.removed, if fault == "cancel" { 1 } else { 2 });
        for s in &result.report.scenarios {
            if s.container_id.is_none() {
                continue;
            }
            let cp = if rejection {
                "downgrade_rejected"
            } else {
                "downgraded"
            };
            assert!(s.stages.iter().any(|s| s.name == cp));
            assert!(!s.stages.iter().any(|s| s.name
                == if rejection {
                    "downgraded"
                } else {
                    "downgrade_rejected"
                }));
            assert!(
                stage(
                    s,
                    if rejection {
                        "exercise_after_rejection"
                    } else {
                        "exercise_downgraded"
                    }
                )
                .commands
                .is_empty(),
                "{fault}"
            );
            if ["initial", "install", "configure", "newer-check"].contains(&fault) {
                assert!(stage(s, "downgrade").commands.is_empty());
            }
            if !["cancel", "timeout"].contains(&fault) {
                assert_eq!(stage(s, "uninstall").commands.len(), 1);
            }
        }
    }
}

#[test]
fn downgrade_comparison_precedes_checkpoint_side_effects_and_keeps_configured_reference() {
    for preserved in [false, true] {
        let (temp, mut recipe) = downgrade_recipe(true);
        let p = recipe.recipe.profiles.get_mut(&ProfileName::User).unwrap();
        // Put the writer before stability checks to prove comparisons use the
        // initial checkpoint observation, not a later command-mutated snapshot.
        p.checks.insert(
            0,
            Check {
                id: "writer".into(),
                checkpoint: Checkpoint::DowngradeRejected,
                expectation: Expectation::Command {
                    script: "mutate".into(),
                    timeout_seconds: 10,
                    env: BTreeMap::new(),
                    pass_env: vec![],
                    expected_exit: 0,
                    stdout_contains: None,
                },
            },
        );
        if preserved {
            p.checks.push(Check {
                id: "configured-data".into(),
                checkpoint: Checkpoint::DowngradeRejected,
                expectation: Expectation::Preserved {
                    path: "/home/theseus/data/record".into(),
                },
            });
        }
        let options = RunOptions {
            profiles: vec![ProfileName::User],
            output_root: temp.path().join("runs"),
            ..RunOptions::all(&recipe)
        };
        let result = engine::run_with(
            &recipe,
            options,
            CancellationToken::default(),
            &mut DowngradeEnvironment {
                rejection: true,
                ..Default::default()
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(
            result.report.outcome,
            if preserved {
                RunOutcome::Failed
            } else {
                RunOutcome::Passed
            }
        );
        let s = &result.report.scenarios[0];
        let hash = |name: &str| {
            stage(s, name).snapshots[0].entries["/home/theseus/data/record"]
                .sha256
                .as_deref()
                .unwrap()
        };
        assert_eq!(hash("configured"), "configured");
        assert_eq!(hash("before_downgrade"), "exercised");
        assert_eq!(hash("downgrade_rejected"), "exercised");
        assert_eq!(hash("downgrade_rejected:after_checks"), "check-side-effect");
        let stable = stage(s, "downgrade_rejected")
            .checks
            .iter()
            .find(|c| c.id == "data-stable")
            .unwrap();
        assert_eq!(stable.outcome, CheckOutcome::Passed);
        assert_eq!(
            stable.comparison.as_ref().unwrap().before_stage,
            "before_downgrade"
        );
    }
}
#[test]
fn downgrade_cancellation_at_every_event_boundary_prevents_later_mutation() {
    for rejection in [false, true] {
        let (_reference_temp, reference, _) = run_downgrade(rejection, "");
        for kind in [
            engine::EventKind::StageStarted,
            engine::EventKind::StageFinished,
        ] {
            for name in reference.report.scenarios[0]
                .stages
                .iter()
                .map(|s| s.name.as_str())
            {
                let (temp, recipe) = downgrade_recipe(rejection);
                let token = CancellationToken::default();
                let trigger = token.clone();
                let mut env = DowngradeEnvironment {
                    rejection,
                    ..Default::default()
                };
                let options = RunOptions {
                    profiles: vec![ProfileName::User],
                    output_root: temp.path().join("runs"),
                    ..RunOptions::all(&recipe)
                };
                let result = engine::run_with(&recipe, options, token, &mut env, |e| {
                    if e.kind == kind && e.stage.as_deref() == Some(name) {
                        trigger.cancel();
                    }
                })
                .unwrap();
                assert_eq!(
                    result.report.outcome,
                    RunOutcome::Cancelled,
                    "{rejection}/{kind:?}/{name}"
                );
                assert_eq!(env.removed, 1);
            }
        }
    }
}

#[test]
fn downgrade_unperformed_evidence_excludes_the_other_policy_checkpoint() {
    for rejection in [false, true] {
        let (temp, mut recipe) = downgrade_recipe(rejection);
        let other = if rejection {
            Checkpoint::Downgraded
        } else {
            Checkpoint::DowngradeRejected
        };
        recipe
            .recipe
            .profiles
            .get_mut(&ProfileName::User)
            .unwrap()
            .checks
            .push(Check {
                id: "other-policy-only".into(),
                checkpoint: other,
                expectation: Expectation::Present {
                    path: "/home/theseus/unused".into(),
                    expected_kind: None,
                },
            });
        let options = RunOptions {
            profiles: vec![ProfileName::User],
            output_root: temp.path().join("runs"),
            ..RunOptions::all(&recipe)
        };
        let token = CancellationToken::default();
        token.cancel();
        let result = engine::run_with(
            &recipe,
            options,
            token,
            &mut DowngradeEnvironment {
                rejection,
                ..Default::default()
            },
            |_| {},
        )
        .unwrap();
        let checks = result.report.scenarios[0]
            .stages
            .iter()
            .flat_map(|s| &s.checks)
            .collect::<Vec<_>>();
        assert!(!checks.is_empty());
        assert!(
            checks
                .iter()
                .all(|c| c.checkpoint != other && c.id != "other-policy-only")
        );
        assert_eq!(result.report.outcome, RunOutcome::Cancelled);
    }
}
