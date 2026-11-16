//! Schema 9 guest requirements: validation, and the `prerequisites` stage with
//! only the container boundary simulated.
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

/// Fails the probes whose phase is listed; records every phase it was asked to run.
struct Guest {
    missing: &'static [&'static str],
    phases: Vec<String>,
}
impl Environment for Guest {
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
        Ok(())
    }
    fn execute(
        &mut self,
        _: &OwnedResource,
        request: CommandRequest,
        _: &ReportStore,
        _: &CancellationToken,
    ) -> Result<ExecutedCommand> {
        self.phases.push(request.phase.clone());
        if request.phase.starts_with("requires:") {
            assert_eq!(
                request.identity.uid, 10001,
                "probes run as the runtime user"
            );
            assert_eq!(request.argv[..2], ["/bin/sh", "-c"]);
            assert!(request.pass_env.is_empty());
        }
        let mut record = CommandRecord::new(&request.id, &request.phase);
        record.identity = request.identity;
        record.argv = request.argv;
        if self.missing.contains(&request.phase.as_str()) {
            record.exit_status = Some(127);
            record.outcome = CommandOutcome::Failed;
        } else {
            record.exit_status = Some(0);
            record.outcome = CommandOutcome::Succeeded;
        }
        Ok(ExecutedCommand {
            record,
            stdout: "2.0".into(),
            stdout_utf8: true,
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
        Ok(roots.iter().map(Snapshot::missing).collect())
    }
    fn stop(&mut self, _: &OwnedResource) -> Result<()> {
        Ok(())
    }
    fn remove(&mut self, _: &OwnedResource) -> Result<()> {
        Ok(())
    }
}

const REQUIRES: &str = "[[requires]]\nid='php-8.3'\nscript='php -v'\ntimeout_seconds=9\n[requires.env]\nLC_ALL='C'\n[[requires]]\nid='node'\nscript='node -v'\n";

fn source(head: &str, requires: &str) -> String {
    let mut text = format!(
        "{head}\nname='fixture'\nimage='ubuntu:24.04'\nobserve=['/home/theseus']\n{requires}[artifacts]\nbase='input'\ntarget='input'\n"
    );
    for step in [
        "install_base",
        "install_target",
        "configure",
        "upgrade",
        "exercise",
        "uninstall",
    ] {
        text += &format!("[profiles.user.steps.{step}]\nscript='true'\n");
    }
    text += "[[profiles.user.checks]]\nid='version'\ncheckpoint='target_installed'\nkind='command'\nscript='echo 2.0'\nexpected_exit=0\nstdout_contains='2.0'\n";
    text += "[[profiles.user.checks]]\nid='gone'\ncheckpoint='uninstalled'\nkind='command'\nscript='true'\nexpected_exit=0\n";
    text
}
fn parse(head: &str, requires: &str) -> Result<(tempfile::TempDir, ValidatedRecipe)> {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("input")).unwrap();
    let path = root.join("theseus.toml");
    std::fs::write(&path, source(head, requires)).unwrap();
    load(&path).map(|recipe| (temp, recipe))
}
const SCHEMA_9: &str = "schema_version=9\nscenarios=['clean','upgrade']";

fn run(missing: &'static [&'static str]) -> (Vec<String>, engine::RunResult) {
    let (temp, recipe) = parse(SCHEMA_9, REQUIRES).unwrap();
    let options = RunOptions {
        output_root: temp.path().join("runs"),
        ..RunOptions::all(&recipe)
    };
    let mut guest = Guest {
        missing,
        phases: vec![],
    };
    let result = engine::run_with(
        &recipe,
        options,
        CancellationToken::default(),
        &mut guest,
        |_| {},
    )
    .unwrap();
    // The saved report is the evidence: it must load back and agree.
    let saved = theseus::report::load(&result.report_path).unwrap();
    assert_eq!(saved.outcome, result.report.outcome);
    (guest.phases, result)
}

#[test]
fn met_requirements_are_probed_once_per_scenario_before_the_first_step() {
    let (phases, result) = run(&[]);
    assert_eq!(result.report.outcome, RunOutcome::Passed);
    assert_eq!(result.report.scenarios.len(), 2);
    for scenario in &result.report.scenarios {
        let names: Vec<_> = scenario.stages.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names[..2], ["baseline", "prerequisites"]);
        let stage = &scenario.stages[1];
        assert_eq!(stage.outcome, StageOutcome::Passed);
        assert!(stage.step.is_none() && stage.checkpoint.is_none());
        let probes: Vec<_> = stage.commands.iter().map(|c| c.phase.as_str()).collect();
        assert_eq!(probes, ["requires:php-8.3", "requires:node"]);
        assert!(!stage.snapshots.is_empty(), "the stage is observed");
        assert!(scenario.unmet_requirements().is_empty());
        assert!(
            scenario
                .stages
                .iter()
                .filter(|s| s.step.is_some())
                .all(|s| s.outcome == StageOutcome::Passed)
        );
    }
    assert_eq!(
        phases[..3],
        ["requires:php-8.3", "requires:node", "install_target"]
    );
    assert_eq!(
        phases.iter().filter(|p| p.starts_with("requires:")).count(),
        4
    );
}

#[test]
fn an_unmet_requirement_fails_its_stage_and_skips_every_step_including_uninstall() {
    let (phases, result) = run(&["requires:php-8.3"]);
    assert_eq!(result.report.outcome, RunOutcome::Failed);
    // Every probe still runs, so one run names everything the image lacks.
    assert_eq!(
        phases,
        [
            "requires:php-8.3",
            "requires:node",
            "requires:php-8.3",
            "requires:node"
        ],
        "no installer, check or uninstall command reaches the guest"
    );
    for scenario in &result.report.scenarios {
        assert_eq!(scenario.unmet_requirements(), ["php-8.3"]);
        let stage = &scenario.stages[1];
        assert_eq!(stage.name, "prerequisites");
        assert_eq!(stage.outcome, StageOutcome::Failed);
        assert_eq!(stage.commands[0].exit_status, Some(127));
        assert_eq!(stage.commands[1].outcome, CommandOutcome::Succeeded);
        let steps: Vec<_> = scenario
            .stages
            .iter()
            .filter(|s| s.step.is_some())
            .collect();
        assert!(steps.iter().any(|s| s.step == Some(StepName::Uninstall)));
        for step in steps {
            assert_eq!(step.outcome, StageOutcome::Skipped, "{}", step.name);
            assert_eq!(
                step.skip_reason.as_deref(),
                Some("guest requirement not met: php-8.3")
            );
            assert!(step.commands.is_empty());
        }
        let checks: Vec<_> = scenario.stages.iter().flat_map(|s| &s.checks).collect();
        assert!(!checks.is_empty());
        assert!(checks.iter().all(|c| c.outcome == CheckOutcome::Skipped));
        // Only the prerequisites stage carries the failure.
        assert!(
            scenario
                .stages
                .iter()
                .filter(|s| s.name != "prerequisites")
                .all(|s| s.outcome != StageOutcome::Failed)
        );
        assert!(matches!(scenario.teardown, TeardownStatus::Complete));
        assert_eq!(scenario.stages.last().unwrap().name, "final");
    }
}

#[test]
fn several_unmet_requirements_are_all_named() {
    let (_, result) = run(&["requires:php-8.3", "requires:node"]);
    let scenario = &result.report.scenarios[0];
    assert_eq!(scenario.unmet_requirements(), ["php-8.3", "node"]);
    let install = scenario.stages.iter().find(|s| s.step.is_some()).unwrap();
    assert_eq!(
        install.skip_reason.as_deref(),
        Some("guest requirement not met: php-8.3, node")
    );
}

#[test]
fn a_recipe_without_requirements_has_no_prerequisites_stage() {
    let (temp, recipe) = parse(SCHEMA_9, "").unwrap();
    assert!(recipe.recipe.requires.is_empty());
    let options = RunOptions {
        output_root: temp.path().join("runs"),
        ..RunOptions::all(&recipe)
    };
    let mut guest = Guest {
        missing: &[],
        phases: vec![],
    };
    let result = engine::run_with(
        &recipe,
        options,
        CancellationToken::default(),
        &mut guest,
        |_| {},
    )
    .unwrap();
    assert_eq!(result.report.outcome, RunOutcome::Passed);
    assert!(
        result
            .report
            .scenarios
            .iter()
            .flat_map(|s| &s.stages)
            .all(|s| s.name != "prerequisites")
    );
}

#[test]
fn requirements_parse_with_defaults_and_validate() {
    let (_temp, recipe) = parse(SCHEMA_9, REQUIRES).unwrap();
    let requires = &recipe.recipe.requires;
    assert_eq!(requires.len(), 2);
    assert_eq!(requires[0].phase(), "requires:php-8.3");
    assert_eq!(requires[0].timeout_seconds, 9);
    assert_eq!(requires[0].env["LC_ALL"], "C");
    assert_eq!(requires[1].timeout_seconds, DEFAULT_TIMEOUT_SECONDS);
    let error = |head: &str, requires: &str| format!("{:#}", parse(head, requires).unwrap_err());
    for older in [
        "schema_version=1".to_string(),
        "schema_version=2\nscenarios=['clean']".to_string(),
        "schema_version=8\nscenarios=['clean']".to_string(),
    ] {
        assert!(
            error(&older, REQUIRES).contains("requires needs recipe schema version 9"),
            "{older}"
        );
    }
    assert!(
        error(
            SCHEMA_9,
            "[[requires]]\nid='a'\nscript='true'\n[[requires]]\nid='a'\nscript='true'\n"
        )
        .contains("duplicate requirement ID a")
    );
    for id in ["", "has space", "a/b", &"x".repeat(65)] {
        assert!(
            error(
                SCHEMA_9,
                &format!("[[requires]]\nid='{id}'\nscript='true'\n")
            )
            .contains("requirement ID must be"),
            "{id:?}"
        );
    }
    assert!(
        error(SCHEMA_9, "[[requires]]\nid='a'\nscript=' '\n")
            .contains("requirement a: script cannot be empty")
    );
    assert!(
        error(
            SCHEMA_9,
            "[[requires]]\nid='a'\nscript='true'\ntimeout_seconds=0\n"
        )
        .contains("requirement a: timeout must be between")
    );
    assert!(
        error(
            SCHEMA_9,
            "[[requires]]\nid='a'\nscript='true'\n[requires.env]\n'1BAD'='x'\n"
        )
        .contains("invalid environment name 1BAD")
    );
    assert!(
        error(
            SCHEMA_9,
            "[[requires]]\nid='a'\nscript='true'\npass_env=['HOME']\n"
        )
        .contains("unknown field `pass_env`")
    );
    let many: String = (0..=MAX_REQUIREMENTS)
        .map(|i| format!("[[requires]]\nid='r{i}'\nscript='true'\n"))
        .collect();
    assert!(error(SCHEMA_9, &many).contains("at most 32 requirements"));
    assert!(
        error("schema_version=11\nscenarios=['clean']", "")
            .contains("unsupported recipe schema version 11")
    );
}
