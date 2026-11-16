//! Schema 10 private Docker daemon: validation, companion ownership with only
//! the container boundary simulated, and one real run that needs Docker.
use anyhow::Result;
use std::collections::BTreeMap;
use theseus::{
    engine::{self, CancellationToken, RunOptions},
    model::*,
    recipe::*,
    report::ReportStore,
    runner::{
        CommandRequest, Environment, ExecutedCommand, PreparedEnvironment, ROLE_LABEL,
        SCENARIO_LABEL,
    },
    snapshot::Snapshot,
};

/// Records the order in which the engine asks for owned resources.
#[derive(Default)]
struct Boundary {
    guest_creation_fails: bool,
    daemon_removal_fails: bool,
    events: Vec<String>,
}
impl Boundary {
    fn resource(kind: &str, run: &str, scenario: &str) -> OwnedResource {
        let mut labels = BTreeMap::from([(SCENARIO_LABEL.to_string(), scenario.to_string())]);
        if kind == "daemon" {
            labels.insert(ROLE_LABEL.into(), "docker-daemon".into());
        }
        OwnedResource {
            container_id: format!("{kind}-{run}-{scenario}"),
            labels,
            removed: false,
        }
    }
}
impl Environment for Boundary {
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
    fn create_companion(
        &mut self,
        run: &str,
        scenario: &str,
        recipe: &ValidatedRecipe,
    ) -> Result<Option<OwnedResource>> {
        if recipe.recipe.docker.is_none() {
            return Ok(None);
        }
        self.events.push(format!("create daemon {scenario}"));
        Ok(Some(Self::resource("daemon", run, scenario)))
    }
    fn create(&mut self, run: &str, scenario: &str, _: &ValidatedRecipe) -> Result<OwnedResource> {
        if self.guest_creation_fails {
            anyhow::bail!("guest creation refused");
        }
        self.events.push(format!("create guest {scenario}"));
        Ok(Self::resource("guest", run, scenario))
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
        let mut record = CommandRecord::new(&request.id, &request.phase);
        record.identity = request.identity;
        record.argv = request.argv;
        record.exit_status = Some(0);
        record.outcome = CommandOutcome::Succeeded;
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
    fn remove(&mut self, resource: &OwnedResource) -> Result<()> {
        let kind = resource.container_id.split('-').next().unwrap();
        if kind == "daemon" && self.daemon_removal_fails {
            anyhow::bail!("daemon removal refused");
        }
        self.events
            .push(format!("remove {kind} {}", resource.labels[SCENARIO_LABEL]));
        Ok(())
    }
}

const DOCKER: &str = "[docker]\nimage='docker:28-dind'\nload=['ubuntu:24.04']\n";

fn source(head: &str, docker: &str) -> String {
    let mut text = format!(
        "{head}\nname='fixture'\nimage='ubuntu:24.04'\nobserve=['/home/theseus']\n{docker}[artifacts]\nbase='input'\ntarget='input'\n"
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
    text
}
fn parse(head: &str, docker: &str) -> Result<(tempfile::TempDir, ValidatedRecipe)> {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("input")).unwrap();
    let path = root.join("theseus.toml");
    std::fs::write(&path, source(head, docker)).unwrap();
    load(&path).map(|recipe| (temp, recipe))
}
const SCHEMA_10: &str = "schema_version=10\nscenarios=['clean','upgrade']";

fn run(docker: &str, mut boundary: Boundary) -> (Boundary, engine::RunResult, RunJournal) {
    let (temp, recipe) = parse(SCHEMA_10, docker).unwrap();
    let options = RunOptions {
        output_root: temp.path().join("runs"),
        ..RunOptions::all(&recipe)
    };
    let result = engine::run_with(
        &recipe,
        options,
        CancellationToken::default(),
        &mut boundary,
        |_| {},
    )
    .unwrap();
    let journal = theseus::report::load_journal(result.report_path.parent().unwrap()).unwrap();
    (boundary, result, journal)
}

#[test]
fn the_daemon_is_journaled_before_its_guest_and_removed_after_it() {
    let (boundary, result, journal) = run(DOCKER, Boundary::default());
    assert_eq!(result.report.outcome, RunOutcome::Passed);
    assert_eq!(
        boundary.events,
        [
            "create daemon user-clean",
            "create guest user-clean",
            "remove guest user-clean",
            "remove daemon user-clean",
            "create daemon user-upgrade",
            "create guest user-upgrade",
            "remove guest user-upgrade",
            "remove daemon user-upgrade",
        ]
    );
    // Two owned resources per matrix entry, the daemon first, all removed.
    assert_eq!(journal.resources.len(), 4);
    assert!(journal.resources[0].labels.contains_key(ROLE_LABEL));
    assert!(!journal.resources[1].labels.contains_key(ROLE_LABEL));
    assert!(journal.resources.iter().all(|resource| resource.removed));
    assert!(journal.cleanup_errors.is_empty());
    for scenario in &result.report.scenarios {
        assert!(matches!(scenario.teardown, TeardownStatus::Complete));
        // The report's container is the guest, never the daemon.
        assert!(
            scenario
                .container_id
                .as_ref()
                .unwrap()
                .starts_with("guest-")
        );
    }
}

#[test]
fn a_recipe_without_a_daemon_owns_only_its_guest() {
    let (boundary, result, journal) = run("", Boundary::default());
    assert_eq!(result.report.outcome, RunOutcome::Passed);
    assert!(
        boundary
            .events
            .iter()
            .all(|event| !event.contains("daemon"))
    );
    assert_eq!(journal.resources.len(), 2);
}

#[test]
fn a_daemon_whose_guest_was_never_created_is_still_removed() {
    let (boundary, result, journal) = run(
        DOCKER,
        Boundary {
            guest_creation_fails: true,
            ..Boundary::default()
        },
    );
    assert_ne!(result.report.outcome, RunOutcome::Passed);
    assert_eq!(
        boundary.events,
        [
            "create daemon user-clean",
            "remove daemon user-clean",
            "create daemon user-upgrade",
            "remove daemon user-upgrade",
        ]
    );
    assert_eq!(journal.resources.len(), 2);
    assert!(journal.resources.iter().all(|resource| resource.removed));
}

#[test]
fn a_daemon_that_cannot_be_removed_fails_teardown_and_stays_journaled() {
    let (_, result, journal) = run(
        DOCKER,
        Boundary {
            daemon_removal_fails: true,
            ..Boundary::default()
        },
    );
    for scenario in &result.report.scenarios {
        assert!(
            matches!(&scenario.teardown, TeardownStatus::Failed { reason } if reason.contains("daemon removal refused")),
            "{:?}",
            scenario.teardown
        );
    }
    let daemons: Vec<_> = journal
        .resources
        .iter()
        .filter(|resource| resource.labels.contains_key(ROLE_LABEL))
        .collect();
    assert_eq!(daemons.len(), 2);
    // Left for `theseus cleanup`, which removes every journaled resource.
    assert!(daemons.iter().all(|resource| !resource.removed));
    assert_eq!(journal.cleanup_errors.len(), 2);
}

#[test]
fn the_docker_block_parses_with_defaults_and_validates() {
    let (_temp, recipe) = parse(SCHEMA_10, DOCKER).unwrap();
    let docker = recipe.recipe.docker.as_ref().unwrap();
    assert_eq!(docker.image, "docker:28-dind");
    assert_eq!(docker.load, ["ubuntu:24.04"]);
    assert_eq!(docker.share, ["/home/theseus", "/workspace"]);
    assert_eq!(GUEST_DOCKER_HOST, "tcp://127.0.0.1:2375");
    let error = |head: &str, docker: &str| format!("{:#}", parse(head, docker).unwrap_err());
    for older in ["schema_version=1", "schema_version=9\nscenarios=['clean']"] {
        assert!(
            error(older, DOCKER).contains("docker needs recipe schema version 10"),
            "{older}"
        );
    }
    assert!(error(SCHEMA_10, "[docker]\nload=[]\n").contains("missing field `image`"));
    for image in ["", "--privileged", "has space"] {
        assert!(
            error(SCHEMA_10, &format!("[docker]\nimage='{image}'\n"))
                .contains("docker.image must be an image reference"),
            "{image:?}"
        );
    }
    assert!(
        error(SCHEMA_10, "[docker]\nimage='d'\nload=['a','a']\n")
            .contains("duplicate docker.load image a")
    );
    assert!(
        error(SCHEMA_10, "[docker]\nimage='d'\nload=['-x']\n")
            .contains("docker.load must be an image reference")
    );
    let many: Vec<String> = (0..=MAX_LOADED_IMAGES).map(|i| format!("'i{i}'")).collect();
    assert!(
        error(
            SCHEMA_10,
            &format!("[docker]\nimage='d'\nload=[{}]\n", many.join(","))
        )
        .contains("at most 16 images")
    );
    for (share, message) in [
        ("[]", "docker.share lists 1 to 8 guest paths"),
        ("['/']", "sharing the entire filesystem is forbidden"),
        ("['/proc/x']", "unsupported docker.share path"),
        ("['/inputs']", "unsupported docker.share path"),
        ("['/a','/a/b']", "overlapping docker.share paths"),
        ("['relative']", ""),
    ] {
        let text = error(SCHEMA_10, &format!("[docker]\nimage='d'\nshare={share}\n"));
        assert!(text.contains(message), "{share}: {text}");
    }
    assert!(
        error(SCHEMA_10, "[docker]\nimage='d'\nprivileged=false\n")
            .contains("unknown field `privileged`")
    );
    assert!(
        error("schema_version=11\nscenarios=['clean']", "")
            .contains("unsupported recipe schema version 11")
    );
}

/// The example recipe against a real daemon: a tool in the guest starts
/// containers on its private daemon, an inner container writes through a bind
/// mount of a guest path, and nothing is left on the host.
#[test]
#[ignore = "requires Docker with privileged containers, and docker:28-cli, docker:28-dind and ubuntu:24.04 locally"]
fn real_private_daemon_runs_containers_for_the_guest_and_leaves_nothing() {
    use std::process::Command;
    let recipe = load(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("examples/docker-sidecar/theseus.toml"),
    )
    .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let options = RunOptions {
        output_root: temp.path().join("runs"),
        ..RunOptions::all(&recipe)
    };
    let result = engine::run(&recipe, options, CancellationToken::default(), |_| {}).unwrap();
    assert_eq!(
        result.report.outcome,
        RunOutcome::Passed,
        "{:#?}",
        result.report
    );
    assert_eq!(result.report.counts().review_findings, 0);
    let scenario = &result.report.scenarios[0];
    assert!(scenario.unmet_requirements().is_empty());
    let exercise = scenario
        .stages
        .iter()
        .find(|stage| stage.step == Some(StepName::Exercise))
        .unwrap();
    assert_eq!(
        exercise.commands[0].environment["DOCKER_HOST"],
        GUEST_DOCKER_HOST
    );
    let journal = theseus::report::load_journal(result.report_path.parent().unwrap()).unwrap();
    assert_eq!(journal.resources.len(), 2);
    assert!(journal.resources.iter().all(|resource| resource.removed));
    let left = Command::new("docker")
        .args([
            "ps",
            "--all",
            "--quiet",
            "--filter",
            &format!("label=io.theseus.run={}", result.report.run_id),
        ])
        .output()
        .unwrap();
    assert!(left.status.success());
    assert_eq!(String::from_utf8_lossy(&left.stdout).trim(), "");
}
