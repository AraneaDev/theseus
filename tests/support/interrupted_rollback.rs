//! Real Docker fixture driver, evidence access and exact owned teardown.
use super::docker_absence::absent;
use std::{path::Path, process::Command};
use theseus::{
    engine::{self, CancellationToken, EventKind, RunOptions},
    model::*,
    recipe,
};
pub const DATA: &str = "/home/theseus/.local/share/sample-cli";
pub const RECORDS: &str = "/home/theseus/.local/share/sample-cli/records";
pub const BACKUP: &str = "/workspace/rollback-backup";
pub fn stage<'a>(s: &'a ScenarioReport, name: &str) -> &'a StageReport {
    s.stages
        .iter()
        .find(|v| v.name == name)
        .unwrap_or_else(|| panic!("missing {name}"))
}
pub fn entry<'a>(s: &'a StageReport, path: &str) -> Option<&'a theseus::snapshot::FileEntry> {
    s.snapshots.iter().find_map(|v| v.entries.get(path))
}
pub fn tree(
    stage: &StageReport,
    root: &str,
) -> std::collections::BTreeMap<String, theseus::snapshot::FileEntry> {
    stage
        .snapshots
        .iter()
        .flat_map(|s| &s.entries)
        .filter(|(path, _)| path.as_str() == root || path.starts_with(&format!("{root}/")))
        .map(|(path, value)| (path[root.len()..].into(), value.clone()))
        .collect()
}
pub fn hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
pub fn stdout(result: &engine::RunResult, c: &CommandRecord) -> String {
    std::fs::read_to_string(
        result
            .report_path
            .parent()
            .unwrap()
            .join(&c.stdout.as_ref().unwrap().path),
    )
    .unwrap()
}
pub fn check<'a>(s: &'a ScenarioReport, name: &str, id: &str) -> &'a CheckResult {
    stage(s, name)
        .checks
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("missing {name}/{id}"))
}
pub fn run(
    policy: &str,
    fault: &str,
    profiles: Vec<ProfileName>,
) -> (tempfile::TempDir, engine::RunResult) {
    let temp = tempfile::tempdir().unwrap();
    let mut recipe = recipe::load(Path::new(&format!(
        "tests/fixtures/interrupted-rollback/{policy}.toml"
    )))
    .unwrap();
    for p in recipe.recipe.profiles.values_mut() {
        if fault == "mixed" {
            let other = recipe::load(Path::new(&format!(
                "tests/fixtures/interrupted-rollback/{}.toml",
                if policy == "binary-only" {
                    "restore-data"
                } else {
                    "binary-only"
                }
            )))
            .unwrap();
            let profile = if p.steps[&StepName::InstallBase].script.ends_with("system") {
                ProfileName::System
            } else {
                ProfileName::User
            };
            let other = &other.recipe.profiles[&profile];
            let mut recovery = other
                .interrupted_rollback
                .as_ref()
                .unwrap()
                .recovery
                .clone();
            recovery.change_checks = vec!["ordinary-changed-state".into()];
            p.rollback = Some(recipe::RollbackContract {
                recovery,
                failure: recipe::ExpectedFailureContract {
                    expected_exit: 9,
                    stdout_contains: "upgrade failed after migration".into(),
                },
            });
            p.checks.push(recipe::Check {
                id: "ordinary-changed-state".into(),
                checkpoint: Checkpoint::UpgradeFailed,
                expectation: recipe::Expectation::Changed { path: DATA.into() },
            });
            for step in [StepName::Backup, StepName::CleanupBackup] {
                if let Some(spec) = other.steps.get(&step) {
                    p.steps.entry(step).or_insert_with(|| spec.clone());
                }
            }
            let dest = if profile == ProfileName::System {
                "/usr/local/bin"
            } else {
                "/home/theseus/.local/bin"
            };
            let mut attempt = p.steps[&StepName::InterruptUpgrade].clone();
            attempt.script = format!(
                "set -eu; cp /inputs/target/sample-cli {dest}/sample-cli; chmod 755 {dest}/sample-cli; printf 'format=2\\nalpha=11\\nbeta=29\\n' > {RECORDS}; printf 'candidate child\\n' > {DATA}/added; echo 'upgrade failed after migration'; exit 9"
            );
            p.steps.insert(StepName::AttemptUpgrade, attempt);
            p.steps.get_mut(&StepName::Rollback).unwrap().script = format!(
                "set -eu; mode=binary-only; if test -d {BACKUP}; then mode=restore-data; fi; sh /inputs/target/rollback.sh {profile} \"$mode\""
            );
        }
        for (name, spec) in &mut p.steps {
            spec.env.insert(
                "THESEUS_FIXTURE_FAULT".into(),
                if fault == "cancel-failed-cleanup" {
                    "cleanup-predicate"
                } else {
                    fault
                }
                .into(),
            );
            if (fault == "never-ready" || fault == "timeout-stop")
                && *name == StepName::InterruptUpgrade
            {
                spec.timeout_seconds = 2;
            }
            if fault
                == format!("timeout-{name}")
                    .replace("interrupt_upgrade", "interrupt")
                    .replace("cleanup_backup", "cleanup")
            {
                spec.timeout_seconds = 2;
            }
        }
        if fault == "directory-binary-promise" {
            p.steps
                .get_mut(&StepName::Configure)
                .unwrap()
                .script
                .push_str(" && mkdir -p /workspace/binary-promise");
            p.interrupted_rollback
                .as_mut()
                .unwrap()
                .recovery
                .binary_checks
                .push("directory-binary".into());
            p.checks.push(recipe::Check {
                id: "directory-binary".into(),
                checkpoint: Checkpoint::RolledBack,
                expectation: recipe::Expectation::Unchanged {
                    path: "/workspace/binary-promise".into(),
                },
            });
        }
        if fault == "preexisting-root" {
            p.steps.get_mut(&StepName::Configure).unwrap().script.push_str(" && mkdir -p /workspace/rollback-backup && printf baseline > /workspace/rollback-backup/owned-before");
        }
        if fault == "no-mutation-after-backup-check" {
            p.checks.push(recipe::Check {
                id: "backup-check-effect".into(), checkpoint: Checkpoint::BackupCreated,
                expectation: recipe::Expectation::Command {
                    script: "printf 'format=2\nalpha=11\nbeta=29\n' > /home/theseus/.local/share/sample-cli/records && echo source-effect".into(),
                    expected_exit: 0, stdout_contains: Some("source-effect".into()),
                    env: Default::default(), pass_env: vec![], timeout_seconds: 15,
                },
            });
        }
        if fault == "known-check-effect" {
            let reader = p
                .checks
                .iter_mut()
                .find(|c| c.id == "known-reader")
                .unwrap();
            if let recipe::Expectation::Command { script, .. } = &mut reader.expectation {
                script.push_str(
                    " && printf known-effect > /home/theseus/.local/share/sample-cli/from-check",
                );
            }
        }
        if matches!(fault, "timeout-check" | "cancel-check") {
            p.checks.push(recipe::Check {
                id: "slow-interrupted-check".into(),
                checkpoint: Checkpoint::Interrupted,
                expectation: recipe::Expectation::Command {
                    script: "set -eu; printf entered > /workspace/slow-check-entered; sleep 600; echo forbidden-check".into(),
                    expected_exit: 0,
                    stdout_contains: Some("forbidden-check".into()),
                    env: Default::default(),
                    pass_env: vec![],
                    timeout_seconds: 2,
                },
            });
        }
        if fault == "check-corrupts-backup" {
            p.checks.push(recipe::Check {id:"corrupt-after-validation".into(),checkpoint:Checkpoint::Interrupted,expectation:recipe::Expectation::Command {script:"printf corrupt > /workspace/rollback-backup/saved/records && echo corruption-completed".into(),expected_exit:0,stdout_contains:Some("corruption-completed".into()), env:Default::default(), pass_env:vec![], timeout_seconds:15}});
        }
    }
    if fault == "mixed" {
        recipe.recipe.scenarios = Some(vec![
            ScenarioKind::Rollback,
            ScenarioKind::InterruptedRollback,
        ]);
    }
    let options = RunOptions {
        profiles,
        scenarios: if fault == "mixed" {
            vec![ScenarioKind::Rollback, ScenarioKind::InterruptedRollback]
        } else {
            vec![ScenarioKind::InterruptedRollback]
        },
        output_root: temp.path().join("runs"),
        log_limit: 4096,
        ..RunOptions::all(&recipe)
    };
    let cancel = CancellationToken::default();
    let signal = cancel.clone();
    let executable = temp.path().join("docker-proxy");
    let marker = temp.path().join("restart-entered");
    let incomplete = temp.path().join("incomplete");
    let trace_path = temp.path().join("calls.jsonl");
    std::fs::write(&executable,format!(r#"#!/usr/bin/python3
import json, pathlib, subprocess, sys, time
args=sys.argv[1:]
mode={fault:?}
marker=pathlib.Path({marker:?})
incomplete=pathlib.Path({incomplete:?})
with open({calls:?}, 'a') as log: log.write(json.dumps(args)+'\n')
if mode.startswith('incomplete-') and incomplete.exists() and args[0]=='cp' and args[1].endswith(':/home/theseus'):
    incomplete.unlink()
    sys.exit(1)
if mode=='cancel-check' and 'theseus-command' in args and any('/workspace/slow-check-entered' in arg for arg in args):
    proc=subprocess.Popen(['docker',*args])
    container=args[args.index('--workdir')+2]
    deadline=time.monotonic()+1.5
    while proc.poll() is None and time.monotonic()<deadline:
        result=subprocess.run(['docker','exec',container,'test','-f','/workspace/slow-check-entered'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=1)
        if result.returncode==0:
            marker.touch()
            break
        time.sleep(0.01)
    sys.exit(proc.wait())
kill=args[:2]==['container','kill']
start=args[:2]==['container','start']
inspect=args[:2]==['container','inspect']
if mode=='stop-error' and kill: sys.exit(1)
if mode=='timeout-stop' and kill: time.sleep(30)
if mode=='restart-error' and start: sys.exit(1)
if mode=='timeout-ready' and '-c' in args and 'mkdir "$1/ready"' in args[args.index('-c')+1]: time.sleep(30)
if mode=='timeout-restart' and start: time.sleep(30)
if start and mode in ['wrong-id','unverified-restart','cancel-during-restart','changing-writer']:
    result=subprocess.run(['docker',*args])
    if result.returncode: sys.exit(result.returncode)
    marker.touch()
    if mode=='cancel-during-restart': time.sleep(30)
    if mode=='changing-writer':
        subprocess.run(['docker','exec','-d',args[-1],'/bin/sh','-c','while :; do printf x >> /workspace/writer; sleep 0.005; done'],check=True)
    sys.exit(0)
if inspect and marker.exists() and mode in ['wrong-id','unverified-restart']:
    result=subprocess.run(['docker',*args],capture_output=True,text=True)
    if result.returncode: sys.stderr.write(result.stderr); sys.exit(result.returncode)
    value=json.loads(result.stdout)
    # Inject only the restart verification; teardown inspects the real owned ID.
    marker.unlink()
    if mode=='wrong-id': value['Id']='0'*64
    else: value['State']['Running']=False
    print(json.dumps(value)); sys.exit(0)
sys.exit(subprocess.run(['docker',*args]).returncode)
"#,marker=marker.to_str().unwrap(),incomplete=incomplete.to_str().unwrap(),calls=trace_path.to_str().unwrap())).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let monitor = if matches!(fault, "cancel-during-restart" | "cancel-check") {
        let signal = signal.clone();
        let monitored_fault = fault.to_owned();
        Some(std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
            while !marker.exists() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(
                marker.exists(),
                "target command never entered: {monitored_fault}"
            );
            signal.cancel();
        }))
    } else {
        None
    };
    let mut docker = theseus::runner::Docker::new(executable);
    let started = std::time::Instant::now();
    let result = engine::run_with(&recipe, options, cancel, &mut docker, move |event| {
        let incomplete_stage = match fault {
            "incomplete-reference" => Some(("known_good", EventKind::StageFinished)),
            "incomplete-checkpoint" => Some(("known_good", EventKind::StageStarted)),
            "incomplete-after-checks" => Some(("interrupted", EventKind::StageFinished)),
            _ => None,
        };
        if incomplete_stage
            .is_some_and(|(name, kind)| event.stage.as_deref() == Some(name) && event.kind == kind)
        {
            std::fs::write(&incomplete, "fail exactly one cp").unwrap();
        }
        let boundary = match fault {
            "cancel-before-barrier" => Some(("before_interrupt", EventKind::StageFinished)),
            "cancel-before-stop" => Some(("interrupt_upgrade", EventKind::StageStarted)),
            "cancel-stopped" => Some(("interrupt_upgrade", EventKind::StageFinished)),
            "cancel-before-restart" => Some(("restart_after_interrupt", EventKind::StageStarted)),
            "cancel-interrupted" => Some(("interrupted", EventKind::StageStarted)),
            "cancel-failed-cleanup" => Some(("cleanup_backup", EventKind::StageFinished)),
            _ => None,
        };
        if boundary
            .is_some_and(|(name, kind)| event.stage.as_deref() == Some(name) && event.kind == kind)
        {
            signal.cancel();
        }
        let delayed = match fault {
            "cancel-backup" => Some("backup"),
            "cancel-interrupt" => Some("interrupt_upgrade"),
            "cancel-rollback" => Some("rollback"),
            "cancel-cleanup" => Some("cleanup_backup"),

            _ => None,
        };
        if delayed.is_some()
            && event.kind == EventKind::StageStarted
            && event.stage.as_deref() == delayed
        {
            let signal = signal.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(900));
                signal.cancel();
            });
        }
    })
    .unwrap();
    if let Some(monitor) = monitor {
        monitor.join().unwrap();
    }
    assert!(
        started.elapsed() < std::time::Duration::from_secs(120),
        "unbounded {fault}: {:?}",
        started.elapsed()
    );
    let calls: Vec<Vec<String>> = std::fs::read_to_string(&trace_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    if fault.is_empty() {
        // Restart uses the same container; setup/input copying/configure occurs only once per scenario.
        assert_eq!(
            calls
                .iter()
                .filter(|a| a.first().is_some_and(|v| v == "create"))
                .count(),
            result.report.scenarios.len()
        );
        assert_eq!(
            calls
                .iter()
                .filter(|a| a.first().is_some_and(|v| v == "container")
                    && a.get(1).is_some_and(|v| v == "start"))
                .count(),
            result.report.scenarios.len()
        );
        assert_eq!(
            calls
                .iter()
                .filter(|a| a.first().is_some_and(|v| v == "cp")
                    && a.last().is_some_and(|v| v.contains(":/inputs/")))
                .count(),
            2 * result.report.scenarios.len()
        );
        assert_eq!(
            calls
                .iter()
                .filter(|a| a.iter().any(|v| v.contains("required tool missing:")))
                .count(),
            result.report.scenarios.len()
        );
        for scenario in &result.report.scenarios {
            assert!(
                !scenario
                    .stages
                    .iter()
                    .flat_map(|s| &s.commands)
                    .any(|c| c.phase.starts_with("recover_"))
            );
            assert_eq!(
                scenario
                    .stages
                    .iter()
                    .flat_map(|s| &s.commands)
                    .filter(|c| c.phase == "configure")
                    .count(),
                1
            );
        }
    }
    assert!(
        result.report.errors.is_empty(),
        "Docker prerequisites: {:?}",
        result.report.errors
    );
    let journal = theseus::report::load_journal(result.report_path.parent().unwrap()).unwrap();
    assert_eq!(journal.schema_version, 1);
    assert!(journal.cleanup_errors.is_empty());
    assert_eq!(journal.resources.len(), result.report.scenarios.len());
    for r in &journal.resources {
        assert!(r.removed);
        assert_eq!(r.labels["io.theseus.owner"], "v1");
        assert_eq!(r.labels["io.theseus.run"], result.report.run_id);
        assert!(
            result
                .report
                .scenarios
                .iter()
                .any(|s| s.container_id.as_ref() == Some(&r.container_id))
        );
        absent(&r.container_id);
    }
    for s in &result.report.scenarios {
        assert_eq!(s.teardown, TeardownStatus::Complete);
    }
    assert_eq!(
        theseus::report::load(&result.report_path).unwrap().outcome,
        result.report.outcome
    );
    if let Ok(dest) = std::env::var("THESEUS_ACCEPTANCE_EVIDENCE") {
        let dest = Path::new(&dest).join(format!(
            "{policy}-{}-{}",
            if fault.is_empty() { "healthy" } else { fault },
            result.report.run_id
        ));
        std::fs::create_dir_all(&dest).unwrap();
        assert!(
            Command::new("cp")
                .arg("-a")
                .arg(result.report_path.parent().unwrap().join("."))
                .arg(&dest)
                .status()
                .unwrap()
                .success()
        );
        std::fs::copy(&trace_path, dest.join("docker-calls.jsonl")).unwrap();
        println!("Evidence: {}", dest.display());
    }
    (temp, result)
}
pub fn prerequisites(s: &ScenarioReport) {
    for name in [
        "install_base",
        "base_installed",
        "configure",
        "configured",
        "exercise_before_upgrade",
        "known_good",
        "before_upgrade",
    ] {
        assert_eq!(stage(s, name).outcome, StageOutcome::Passed, "{name}");
    }
    assert_eq!(
        entry(stage(s, "before_upgrade"), RECORDS)
            .unwrap()
            .sha256
            .as_deref(),
        Some(hash(b"format=1\nalpha=11\nbeta=29\n").as_str())
    );
}

/// Focus a repaired fault row without repeating already-passed Docker rows.
/// Unset in CI: every declared row executes.
pub fn enabled(fault: &str) -> bool {
    std::env::var("THESEUS_ACCEPTANCE_CASES")
        .map_or(true, |cases| cases.split(',').any(|case| case == fault))
}
