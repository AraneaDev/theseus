use std::{path::Path, process::Command};
use theseus::{
    engine::{self, CancellationToken, EventKind, RunOptions},
    model::*,
    recipe,
};

fn absent(id: &str) {
    let output = Command::new("docker")
        .args(["inspect", id])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr)
        .trim()
        .to_lowercase();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        stderr == format!("error: no such object: {id}")
            || stderr == format!("error response from daemon: no such container: {id}"),
        "{output:?}"
    );
}

fn run(
    fault: &str,
    profiles: Vec<ProfileName>,
    scenarios: Vec<ScenarioKind>,
    cancellation: Option<(&str, EventKind)>,
) -> (tempfile::TempDir, engine::RunResult) {
    let temp = tempfile::tempdir().unwrap();
    let mut recipe = recipe::load(Path::new("tests/fixtures/interruption/recipe.toml")).unwrap();
    for profile in recipe.recipe.profiles.values_mut() {
        for (name, step) in &mut profile.steps {
            if matches!(
                name,
                StepName::InterruptInstall
                    | StepName::InterruptUpgrade
                    | StepName::RecoverInstall
                    | StepName::RecoverUpgrade
            ) {
                step.env
                    .insert("THESEUS_FIXTURE_FAULT".into(), fault.into());
            }
            if matches!(
                name,
                StepName::InterruptInstall | StepName::InterruptUpgrade
            ) && fault == "never-ready"
            {
                step.timeout_seconds = 2;
            }
        }
    }
    let options = RunOptions {
        profiles,
        scenarios,
        output_root: temp.path().join("runs"),
        ..RunOptions::all(&recipe)
    };
    let cancel = CancellationToken::default();
    let signal = cancel.clone();
    let executable = temp.path().join("docker-proxy");
    let marker = temp.path().join("restart-entered");
    std::fs::write(&executable, format!(r#"#!/usr/bin/python3
import pathlib, subprocess, sys, time
args=sys.argv[1:]
mode={fault:?}
if mode=='restart-error' and args[:2]==['container','start']: sys.exit(1)
if mode=='control-error' and '-c' in args and 'iflag=nofollow,nonblock' in args[args.index('-c')+1]: sys.exit(1)
if mode=='cancel-restart' and args[:2]==['container','start']:
    result=subprocess.run(['docker', *args])
    if result.returncode: sys.exit(result.returncode)
    pathlib.Path({marker:?}).touch()
    time.sleep(5)
    sys.exit(0)
result=subprocess.run(['docker', *args])
sys.exit(result.returncode)
"#, marker = marker.to_str().unwrap())).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let monitor = if fault == "cancel-restart" {
        let signal = signal.clone();
        Some(std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while !marker.exists() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(marker.exists(), "restart not entered");
            signal.cancel();
        }))
    } else {
        None
    };
    let mut docker = if matches!(fault, "restart-error" | "control-error" | "cancel-restart") {
        theseus::runner::Docker::new(executable)
    } else {
        theseus::runner::Docker::default()
    };
    let result = engine::run_with(&recipe, options, cancel, &mut docker, move |event| {
        if let Some((stage, kind)) = cancellation
            && event.stage.as_deref() == Some(stage)
            && event.kind == kind
        {
            if fault == "cancel-before-ready" {
                let signal = signal.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    signal.cancel();
                });
            } else {
                signal.cancel();
            }
        }
    })
    .unwrap();
    if let Some(monitor) = monitor {
        monitor.join().unwrap();
    }
    assert!(
        result.report.errors.is_empty(),
        "Docker prerequisites: {:?}",
        result.report.errors
    );
    let journal = theseus::report::load_journal(result.report_path.parent().unwrap()).unwrap();
    assert!(journal.resources.iter().all(|resource| resource.removed));
    for scenario in &result.report.scenarios {
        assert!(matches!(scenario.teardown, TeardownStatus::Complete));
        if let Some(id) = &scenario.container_id {
            absent(id);
        }
    }
    assert_eq!(
        theseus::report::load(&result.report_path).unwrap().outcome,
        result.report.outcome
    );
    (temp, result)
}

#[test]
#[ignore = "requires reachable Docker daemon and local ubuntu:24.04"]
fn real_interruption_four_way_matrix_retains_partial_state_then_recovers() {
    let (_, result) = run(
        "",
        vec![ProfileName::User, ProfileName::System],
        vec![
            ScenarioKind::InterruptedInstall,
            ScenarioKind::InterruptedUpgrade,
        ],
        None,
    );
    assert_eq!(
        result.report.outcome,
        RunOutcome::Passed,
        "{:#?}",
        result.report
    );
    assert_eq!(result.report.scenarios.len(), 4);
    for scenario in &result.report.scenarios {
        let interrupt = scenario
            .stages
            .iter()
            .find(|stage| stage.verified_interruption())
            .unwrap();
        assert!(interrupt.snapshots.is_empty());
        let metadata = serde_json::to_string(&result.report).unwrap();
        assert!(!metadata.contains("/.theseus-"));
        assert!(
            !interrupt.commands[0]
                .environment
                .contains_key("THESEUS_INTERRUPT_BARRIER")
        );
        let command = &interrupt.commands[0];
        assert_eq!(command.outcome, CommandOutcome::Interrupted);
        assert_eq!(command.exit_status, None);
        assert_eq!(
            command.identity.uid,
            if scenario.profile == ProfileName::System {
                0
            } else {
                10001
            }
        );
        let restart = scenario
            .stages
            .iter()
            .find_map(|stage| stage.restart.as_ref())
            .unwrap();
        assert_eq!(Some(&restart.container_id), scenario.container_id.as_ref());
        assert!(restart.running_verified);
        let partial = scenario
            .stages
            .iter()
            .find(|stage| stage.name == "interrupted")
            .unwrap();
        let recovered = scenario
            .stages
            .iter()
            .find(|stage| stage.name == "recovered")
            .unwrap();
        let path = if scenario.profile == ProfileName::User {
            "/home/theseus/.local/bin/sample-cli.partial"
        } else {
            "/usr/local/bin/sample-cli.partial"
        };
        let file = partial
            .snapshots
            .iter()
            .find_map(|snapshot| snapshot.entries.get(path))
            .unwrap();
        assert_eq!(file.uid, command.identity.uid as u64);
        assert_eq!(file.mode & 0o777, 0o640);
        assert!(file.size > 100);
        use sha2::{Digest, Sha256};
        let expected = format!(
            "{:x}",
            Sha256::digest(std::fs::read("tests/fixtures/interruption/target/sample-cli").unwrap())
        );
        assert_eq!(file.sha256.as_deref(), Some(expected.as_str()));
        let installed_path = path.trim_end_matches(".partial");
        let prior = partial
            .snapshots
            .iter()
            .find_map(|snapshot| snapshot.entries.get(installed_path));
        if scenario.scenario == ScenarioKind::InterruptedInstall {
            assert!(
                prior.is_none(),
                "fresh interrupted install must not already have a complete binary"
            );
        } else {
            let base_hash = format!(
                "{:x}",
                Sha256::digest(
                    std::fs::read("tests/fixtures/interruption/base/sample-cli").unwrap()
                )
            );
            assert_eq!(prior.unwrap().sha256.as_deref(), Some(base_hash.as_str()));
        }
        let writer = partial
            .snapshots
            .iter()
            .find_map(|snapshot| snapshot.entries.get("/workspace/writer"))
            .unwrap();
        assert!(
            writer.size > 0,
            "writer must actually have run before interruption"
        );
        let recovered_path = path.trim_end_matches(".partial");
        let recovered_binary = recovered
            .snapshots
            .iter()
            .find_map(|snapshot| snapshot.entries.get(recovered_path))
            .unwrap();
        assert_eq!(recovered_binary.sha256, file.sha256);
        assert_eq!(recovered_binary.uid, file.uid);
        assert_eq!(recovered_binary.mode & 0o777, 0o755);
        if scenario.scenario == ScenarioKind::InterruptedUpgrade {
            let notes = "/home/theseus/.local/share/sample-cli/notes";
            let before = scenario
                .stages
                .iter()
                .find(|stage| stage.name == "before_interrupt")
                .unwrap()
                .snapshots
                .iter()
                .find_map(|snapshot| snapshot.entries.get(notes))
                .unwrap();
            let after = recovered
                .snapshots
                .iter()
                .find_map(|snapshot| snapshot.entries.get(notes))
                .unwrap();
            assert_eq!(before, after);
        }
        assert!(
            recovered
                .snapshots
                .iter()
                .all(|snapshot| !snapshot.entries.contains_key(path))
        );
        assert!(
            partial
                .checks
                .iter()
                .all(|check| check.outcome == CheckOutcome::Passed)
        );
        assert!(
            recovered
                .checks
                .iter()
                .all(|check| check.outcome == CheckOutcome::Passed)
        );
        for command in scenario.stages.iter().flat_map(|stage| &stage.commands) {
            if command.phase.starts_with("check:")
                || command.phase == "configure"
                || command.phase == "exercise"
            {
                assert_eq!(command.identity.uid, 10001);
            }
        }
    }
}

#[test]
#[ignore = "requires Docker"]
fn real_interruption_faults_keep_literal_failure_and_no_false_recovery() {
    for (fault, expected, recovered) in [
        ("early-0", RunOutcome::Failed, false),
        ("early-9", RunOutcome::Failed, false),
        ("never-ready", RunOutcome::Incomplete, false),
        ("stale", RunOutcome::Incomplete, false),
        ("symlink", RunOutcome::Incomplete, false),
        ("recovery-9", RunOutcome::Failed, true),
        ("leftover-partial", RunOutcome::Failed, true),
        ("corrupt-data", RunOutcome::Failed, true),
        ("restart-error", RunOutcome::Incomplete, false),
        ("control-error", RunOutcome::Incomplete, false),
    ] {
        let (_, result) = run(
            fault,
            vec![ProfileName::User],
            vec![ScenarioKind::InterruptedUpgrade],
            None,
        );
        assert_eq!(
            result.report.outcome, expected,
            "{fault}: {:#?}",
            result.report
        );
        assert_eq!(
            result.report.outcome.exit_code(),
            if expected == RunOutcome::Failed { 1 } else { 2 }
        );
        let scenario = &result.report.scenarios[0];
        // Healthy installation/configuration/exercise must precede the intended fault.
        for name in ["install_base", "configure", "exercise_before_interrupt"] {
            assert_eq!(
                scenario
                    .stages
                    .iter()
                    .find(|stage| stage.name == name)
                    .unwrap()
                    .outcome,
                StageOutcome::Passed,
                "{fault}/{name}"
            );
        }
        let recovery = scenario
            .stages
            .iter()
            .find(|stage| stage.name == "recover_upgrade")
            .unwrap();
        assert_eq!(!recovery.commands.is_empty(), recovered, "{fault}");
        if fault.starts_with("early-") {
            let command = &scenario
                .stages
                .iter()
                .find(|stage| stage.name == "interrupt_upgrade")
                .unwrap()
                .commands[0];
            assert_eq!(command.outcome, CommandOutcome::Failed);
            assert_eq!(
                command.exit_status,
                Some(if fault == "early-0" { 0 } else { 9 })
            );
            assert!(command.error.is_none() && !command.timed_out && !command.cancelled);
        }
        let uninstall = scenario
            .stages
            .iter()
            .find(|stage| stage.name == "uninstall")
            .unwrap();
        assert_eq!(
            uninstall.outcome,
            if expected == RunOutcome::Failed {
                StageOutcome::Passed
            } else {
                StageOutcome::Skipped
            },
            "{fault}"
        );
    }
}

#[test]
#[ignore = "requires Docker"]
fn real_interruption_cancellation_before_ready_stopped_and_restart_is_130() {
    for (fault, stage, kind) in [
        (
            "cancel-before-ready",
            "interrupt_install",
            EventKind::StageStarted,
        ),
        (
            "cancel-stopped",
            "interrupt_install",
            EventKind::StageFinished,
        ),
        (
            "cancel-at-restart",
            "restart_after_interrupt",
            EventKind::StageStarted,
        ),
        ("cancel-restart", "unused", EventKind::StageStarted),
    ] {
        let (_, result) = run(
            fault,
            vec![ProfileName::User],
            vec![ScenarioKind::InterruptedInstall],
            Some((stage, kind)),
        );
        assert_eq!(
            result.report.outcome,
            RunOutcome::Cancelled,
            "{fault}: {:#?}",
            result.report
        );
        assert_eq!(result.report.outcome.exit_code(), 130);
        let scenario = &result.report.scenarios[0];
        assert!(
            scenario
                .stages
                .iter()
                .filter(|stage| stage.name == "recover_install"
                    || stage.name == "configure"
                    || stage.name == "exercise_after_recovery")
                .all(|stage| stage.commands.is_empty()),
            "{fault}"
        );
    }
}

#[test]
#[ignore = "requires Docker and Unix PTY"]
fn real_public_example_headless_and_live_offline_pty() {
    let output = Command::new("python3")
        .arg("tests/support/tui_interruption.py")
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
#[ignore = "requires Docker"]
fn real_declared_writer_checks_reject_a_changing_writer() {
    use theseus::runner::{CommandRequest, Docker, Environment};
    struct Container(OwnedResource);
    impl Drop for Container {
        fn drop(&mut self) {
            let removed = Docker::default().remove(&self.0);
            if !std::thread::panicking() {
                removed.unwrap();
                absent(&self.0.container_id);
            }
        }
    }
    let fixture = recipe::load(Path::new("tests/fixtures/interruption/recipe.toml")).unwrap();
    let cancel = CancellationToken::default();
    let mut docker = Docker::default();
    docker.prepare(&fixture, false, &cancel).unwrap();
    let container = Container(
        docker
            .create("writer-negative", "writer-negative", &fixture)
            .unwrap(),
    );
    docker.setup(&container.0, &fixture, &cancel).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let store = theseus::report::create(&temp.path().join("run")).unwrap();
    for path in [
        "tests/fixtures/interruption/recipe.toml",
        "examples/interruption/theseus.toml",
    ] {
        let declared = recipe::load(Path::new(path)).unwrap();
        for (profile, spec) in &declared.recipe.profiles {
            let check = spec
                .checks
                .iter()
                .find(|check| check.id == "writer-stopped")
                .unwrap();
            let recipe::Expectation::Command {
                script,
                expected_exit,
                ..
            } = &check.expectation
            else {
                panic!("writer check must be an executable command")
            };
            let result = docker
                .execute(
                    &container.0,
                    CommandRequest {
                        id: format!(
                            "writer-{profile}-{}",
                            if path.starts_with("tests") {
                                "fixture"
                            } else {
                                "example"
                            }
                        ),
                        phase: format!("check:{}", check.id),
                        identity: ExecutionIdentity::runtime(),
                        // The declaration itself is executed unchanged in its own /bin/sh -c,
                        // exactly as in the engine. Only the outer test driver owns the writer.
                        argv: vec![
                            "/bin/sh".into(),
                            "-c".into(),
                            r#"
set -u
printf x > /workspace/writer
(while :; do printf x >> /workspace/writer; sleep 0.005; done) &
writer=$!
trap 'kill "$writer"; wait "$writer" 2>/dev/null || :' EXIT
before=$(wc -c < /workspace/writer)
/bin/sh -c "$1"
status=$?
after=$(wc -c < /workspace/writer)
test "$after" -gt "$before" || exit 92
exit "$status"
"#
                            .into(),
                            "writer-test".into(),
                            script.clone(),
                        ],
                        env: Default::default(),
                        pass_env: vec![],
                        timeout_seconds: 10,
                        expected_exit: *expected_exit,
                        log_limit: 2048,
                    },
                    &store,
                    &cancel,
                )
                .unwrap();
            assert_eq!(
                result.record.exit_status,
                Some(1),
                "{path}/{profile}: {:?}; stdout={:?}",
                result.record,
                result.stdout
            );
            assert_eq!(result.record.outcome, CommandOutcome::Failed);
            assert!(
                !result.stdout.contains("stopped"),
                "false success marker: {path}/{profile}: {:?}",
                result.stdout
            );
        }
    }
}
