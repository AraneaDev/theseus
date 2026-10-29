use anyhow::Result;
use std::collections::BTreeMap;
use theseus::{
    engine::{self, CancellationToken, EventKind, RunOptions},
    model::*,
    recipe::{self, Limits, ValidatedRecipe},
    report::ReportStore,
    runner::{CommandRequest, Environment, ExecutedCommand, PreparedEnvironment},
    snapshot::{FileEntry, ObservationStatus, Snapshot},
};

fn entry(kind: FileKind, content: &str) -> FileEntry {
    FileEntry {
        kind,
        mode: 0o755,
        uid: 10001,
        gid: 10001,
        size: content.len() as u64,
        sha256: (kind == FileKind::File).then(|| content.into()),
        link_target: (kind == FileKind::Symlink).then(|| content.into()),
    }
}
struct Evidence {
    files: BTreeMap<String, FileEntry>,
    fault: String,
    calls: Vec<CommandRequest>,
    observations: usize,
    stopped: bool,
    setups: usize,
    restarts: usize,
    creates: usize,
    resource_id: String,
}
impl Evidence {
    fn new(fault: &str) -> Self {
        Self {
            files: BTreeMap::from([("/workspace".into(), entry(FileKind::Directory, ""))]),
            fault: fault.into(),
            calls: vec![],
            observations: 0,
            stopped: false,
            setups: 0,
            restarts: 0,
            creates: 0,
            resource_id: "same-container".into(),
        }
    }
    fn put(&mut self, path: &str, content: &str) {
        self.files
            .insert(path.into(), entry(FileKind::File, content));
    }
    fn copy(&mut self, source: &str, target: &str) {
        let entries: Vec<_> = self
            .files
            .iter()
            .filter(|(p, _)| *p == source || p.starts_with(&format!("{source}/")))
            .map(|(p, e)| (format!("{target}{}", &p[source.len()..]), e.clone()))
            .collect();
        self.files
            .retain(|p, _| p != target && !p.starts_with(&format!("{target}/")));
        self.files.extend(entries);
    }
}
impl Environment for Evidence {
    fn prepare(
        &mut self,
        _: &ValidatedRecipe,
        _: bool,
        _: &CancellationToken,
    ) -> Result<PreparedEnvironment> {
        Ok(PreparedEnvironment {
            image_id: "same-image".into(),
            artifacts: vec![],
        })
    }
    fn create(&mut self, _: &str, _: &str, _: &ValidatedRecipe) -> Result<OwnedResource> {
        self.creates += 1;
        self.resource_id = if self.creates == 1 {
            "same-container".into()
        } else {
            "second-container".into()
        };
        self.files = BTreeMap::from([("/workspace".into(), entry(FileKind::Directory, ""))]);
        Ok(OwnedResource {
            container_id: self.resource_id.clone(),
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
        self.setups += 1;
        Ok(())
    }
    fn execute(
        &mut self,
        resource: &OwnedResource,
        request: CommandRequest,
        store: &ReportStore,
        cancel: &CancellationToken,
    ) -> Result<ExecutedCommand> {
        assert!(!self.stopped, "no command while stopped");
        assert_eq!(resource.container_id, self.resource_id);
        assert!(!cancel.is_cancelled());
        self.calls.push(request.clone());
        let phase = request.phase.as_str();
        let mut stdout = "readable".to_string();
        let mut status = 0;
        match phase {
            "install_base" => {
                self.put("/workspace/tool", "good");
                self.files
                    .insert("/workspace/data".into(), entry(FileKind::Directory, ""));
                self.put("/workspace/data/row", "configured");
                if self.fault == "preexisting" {
                    self.put("/workspace/backup", "old owned data");
                }
                if self.fault == "binary-directory" {
                    self.files
                        .insert("/workspace/tool".into(), entry(FileKind::Directory, ""));
                }
                if self.fault == "binary-link" {
                    self.files.insert(
                        "/workspace/tool".into(),
                        entry(FileKind::Symlink, "old-target"),
                    );
                }
            }
            "check:expected-nonzero" => status = 7,
            "configure" => self.put("/workspace/data/row", "configured"),
            "check:known" => self.put("/workspace/data/row", "known-good-after-reader"),
            "backup" => {
                self.copy("/workspace/data", "/workspace/backup/data");
                self.files
                    .insert("/workspace/backup".into(), entry(FileKind::Directory, ""));
                if self.fault == "destructive-backup" {
                    self.put("/workspace/data/row", "destroyed");
                }
                if self.fault == "corrupt-backup" {
                    self.put("/workspace/backup/data/row", "bad");
                }
                if self.fault == "partial-backup" {
                    self.files.remove("/workspace/backup/data/row");
                    status = 1;
                }
            }
            "check:backup-effect" => self.put("/workspace/data/row", "backup-check-effect"),
            "attempt_upgrade" => {
                status = 9;
                stdout = "migration failed".into();
                if self.fault != "no-mutation" && self.fault != "backup-check-effect" {
                    self.put("/workspace/tool", "candidate");
                    self.put("/workspace/data/row", "candidate");
                }
                if self.fault == "success" {
                    status = 0;
                }
                if self.fault == "wrong-status" {
                    status = 8;
                }
                if self.fault == "wrong-marker" {
                    stdout = "wrong".into();
                }
                if self.fault == "attempt-corrupts-backup" {
                    self.put("/workspace/backup/data/row", "corrupt");
                }
            }
            "check:failure-effect" => {
                self.put("/workspace/backup/data/row", "after-check-corruption")
            }
            "rollback" => {
                if self.fault != "noop-rollback" {
                    if self.fault == "binary-link" {
                        self.files.insert(
                            "/workspace/tool".into(),
                            entry(FileKind::Symlink, "old-target"),
                        );
                    } else {
                        self.put("/workspace/tool", "good");
                    }
                    if self.files.contains_key("/workspace/backup/data") {
                        self.copy("/workspace/backup/data", "/workspace/data");
                    }
                }
                if self.fault == "partial-restore" {
                    self.files.remove("/workspace/data/row");
                }
            }
            "cleanup_backup" => {
                if self.fault == "cleanup-fails" {
                    status = 1;
                } else {
                    self.files
                        .retain(|p, _| !p.starts_with("/workspace/backup"));
                }
            }
            "check:reader" if self.fault == "separate-reader" => {
                stdout = "compatible retained data".into()
            }
            "check:reader" if self.fault == "reader-fails" => {
                status = 1;
                stdout = "unreadable".into();
            }
            "resolve:base" | "resolve:runtime" => stdout = "/workspace/tool".into(),
            "check:base" | "check:runtime" => {
                stdout = if self.fault == "wrong-runtime" && phase == "check:runtime" {
                    "candidate".into()
                } else {
                    "known good".into()
                }
            }
            _ => {}
        }
        let mut record = CommandRecord::new(&request.id, phase);
        record.identity = request.identity;
        record.argv = request.argv;
        record.exit_status = Some(status);
        record.outcome = if status == request.expected_exit {
            CommandOutcome::Succeeded
        } else {
            CommandOutcome::Failed
        };
        if self.fault == format!("cancel:{phase}") {
            record.cancelled = true;
            record.outcome = CommandOutcome::Cancelled;
        }
        if self.fault == format!("timeout:{phase}") {
            record.timed_out = true;
            record.outcome = CommandOutcome::TimedOut;
        }
        if self.fault == "unverified" && phase == "attempt_upgrade" {
            record.exit_status = None;
            record.error = Some("no normal completion receipt".into());
        }
        let log = format!("logs/{}.stdout", record.id);
        std::io::Write::write_all(&mut store.create_log(&log).unwrap(), stdout.as_bytes()).unwrap();
        record.stdout = Some(LogReference {
            path: log,
            bytes: stdout.len() as u64,
            truncated: false,
        });
        Ok(ExecutedCommand {
            record,
            stdout,
            stdout_complete: !(self.fault == "truncated" && phase == "attempt_upgrade"),
        })
    }
    fn execute_interrupted(
        &mut self,
        resource: &OwnedResource,
        request: CommandRequest,
        _: &ReportStore,
        cancel: &CancellationToken,
    ) -> Result<ExecutedCommand> {
        assert!(!self.stopped);
        assert!(!cancel.is_cancelled());
        assert_eq!(resource.container_id, self.resource_id);
        self.calls.push(request.clone());
        if self.fault != "no-mutation" && self.fault != "backup-check-effect" {
            self.put("/workspace/tool", "candidate");
            self.put("/workspace/data/row", "candidate");
        }
        if self.fault == "attempt-corrupts-backup" {
            self.put("/workspace/backup/data/row", "corrupt");
        }
        let mut record = CommandRecord::new(request.id, request.phase);
        record.identity = request.identity;
        record.argv = request.argv;
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
            let proof = record.interruption.as_mut().unwrap();
            proof.readiness_verified = false;
            proof.stopped_verified = false;
        }
        if self.fault == "contradiction" {
            record.expected_failure = Some(ExpectedFailureEvidence {
                expected_exit: 9,
                stdout_contains: "ordinary failure".into(),
                stdout_matched: true,
            });
        }
        if self.fault == "malformed-interrupt" {
            record.argv = vec!["/bin/true".into()];
        }
        if self.fault == "unverified" {
            record.interruption = None;
        }
        if self.fault == "cancel-stop" {
            cancel.cancel();
        }
        Ok(ExecutedCommand {
            record,
            stdout: String::new(),
            stdout_complete: false,
        })
    }
    fn restart(
        &mut self,
        resource: &OwnedResource,
        cancel: &CancellationToken,
    ) -> Result<RestartEvidence> {
        assert!(self.stopped);
        assert!(!cancel.is_cancelled());
        self.restarts += 1;
        if self.fault == "restart-error" {
            anyhow::bail!("restart unavailable");
        }
        self.stopped = false;
        if self.fault == "cancel-restart" {
            cancel.cancel();
        }
        Ok(RestartEvidence {
            container_id: if self.fault == "wrong-id" {
                "other".into()
            } else {
                resource.container_id.clone()
            },
            running_verified: self.fault != "unverified-restart",
        })
    }
    fn observe(
        &mut self,
        _: &OwnedResource,
        roots: &[String],
        _: Limits,
        _: &CancellationToken,
    ) -> Result<Vec<Snapshot>> {
        assert!(!self.stopped, "no observations while stopped");
        self.observations += 1;
        if self.fault == format!("incomplete:{}", self.observations) {
            return Ok(roots
                .iter()
                .map(|r| Snapshot::incomplete(r, "uncertain"))
                .collect());
        }
        Ok(roots
            .iter()
            .map(|r| Snapshot {
                root: r.clone(),
                completeness: ObservationStatus::Complete,
                entries: self.files.clone(),
            })
            .collect())
    }
    fn stop(&mut self, _: &OwnedResource) -> Result<()> {
        Ok(())
    }
    fn remove(&mut self, _: &OwnedResource) -> Result<()> {
        Ok(())
    }
}
fn run(
    fault: &str,
    binary: bool,
    profile: ProfileName,
    boundary: Option<(EventKind, &str)>,
) -> (tempfile::TempDir, engine::RunResult, Evidence) {
    let temp = tempfile::tempdir().unwrap();
    for p in ["base", "target"] {
        std::fs::create_dir(temp.path().join(p)).unwrap();
    }
    let mut text = include_str!("fixtures/recipes/rollback.toml").to_string();
    text = text.replace("schema_version = 5", "schema_version = 6")
        .replace("scenarios = ['rollback']", "scenarios = ['interrupted_rollback']")
        .replace("[profiles.user.rollback.failure]\nexpected_exit = 9\nstdout_contains = 'migration failed'\n", "")
        .replace("profiles.user.rollback", "profiles.user.interrupted_rollback")
        .replace("steps.attempt_upgrade", "steps.interrupt_upgrade")
        .replace("checkpoint = 'upgrade_failed'", "checkpoint = 'interrupted'");
    if binary {
        text=text.replace("mode = 'restore_data'\nbackup_root = '/workspace/backup'\n[[profiles.user.interrupted_rollback.restoration.scopes]]\nid = 'data'\nsource = '/workspace/data'\nstored = 'data'","mode = 'binary_only'").replace("[profiles.user.steps.backup]\nscript = 'backup'\n", "").replace("[profiles.user.steps.cleanup_backup]\nscript = 'cleanup backup'\n","");
    }
    if fault == "separate-reader" {
        text = text.replace("id = 'reader'\ncheckpoint = 'rolled_back'\nkind = 'command'\nscript = 'tool read'\nexpected_exit = 0\nstdout_contains = 'readable'", "id = 'reader'\ncheckpoint = 'rolled_back'\nkind = 'command'\nscript = 'tool inspect --compatible'\nexpected_exit = 0\nstdout_contains = 'compatible retained data'");
    }
    if fault == "nonzero-check" {
        text += "\n[[profiles.user.checks]]\nid='expected-nonzero'\ncheckpoint='configured'\nkind='command'\nscript='normal nonzero check'\nexpected_exit=7\nstdout_contains='readable'\n";
    }
    if fault == "preserved-reference" {
        text += "\n[[profiles.user.checks]]\nid='configured-retention'\ncheckpoint='uninstalled'\nkind='preserved'\npath='/workspace/data/row'\n";
    }
    if fault == "backup-check-effect" {
        text += "\n[[profiles.user.checks]]\nid='backup-effect'\ncheckpoint='backup_created'\nkind='command'\nscript='mutate backup check'\nexpected_exit=0\nstdout_contains='readable'\n";
    }
    if fault == "failure-check-corrupts" {
        text += "\n[[profiles.user.checks]]\nid='failure-effect'\ncheckpoint='interrupted'\nkind='command'\nscript='corrupt backup'\nexpected_exit=0\nstdout_contains='readable'\n";
    }
    if fault.starts_with("mixed") {
        text = text.replace(
            "scenarios = ['interrupted_rollback']",
            "scenarios = ['rollback', 'interrupted_rollback']",
        );
        text += "\n[profiles.user.rollback]\nchange_checks=['ordinary-changed']\nbinary_checks=['binary']\nruntime_checks=['runtime']\ndata_checks=['reader']\n[profiles.user.rollback.failure]\nexpected_exit=9\nstdout_contains='migration failed'\n[profiles.user.rollback.restoration]\nmode='restore_data'\nbackup_root='/workspace/backup'\n[[profiles.user.rollback.restoration.scopes]]\nid='data'\nsource='/workspace/data'\nstored='data'\n[profiles.user.steps.attempt_upgrade]\nscript='attempt upgrade'\n[[profiles.user.checks]]\nid='ordinary-changed'\ncheckpoint='upgrade_failed'\nkind='changed'\npath='/workspace/data'\n";
        if fault == "mixed-ordinary-binary" {
            text = text.replace("[profiles.user.rollback.restoration]\nmode='restore_data'\nbackup_root='/workspace/backup'\n[[profiles.user.rollback.restoration.scopes]]\nid='data'\nsource='/workspace/data'\nstored='data'", "[profiles.user.rollback.restoration]\nmode='binary_only'");
        }
        if fault == "mixed-interrupted-binary" {
            text = text.replace("mode = 'restore_data'\nbackup_root = '/workspace/backup'\n[[profiles.user.interrupted_rollback.restoration.scopes]]\nid = 'data'\nsource = '/workspace/data'\nstored = 'data'", "mode = 'binary_only'");
            text = text.replace(
                "scenarios = ['rollback', 'interrupted_rollback']",
                "scenarios = ['interrupted_rollback', 'rollback']",
            );
        }
        if fault != "mixed" {
            text += "\n[[profiles.user.checks]]\nid='backup-contract'\ncheckpoint='backup_created'\nkind='present'\npath='/workspace/backup/data'\n[[profiles.user.checks]]\nid='backup-removal-contract'\ncheckpoint='backup_removed'\nkind='absent'\npath='/workspace/backup'\n";
        }
    }
    if profile == ProfileName::System {
        text = text.replace("profiles.user", "profiles.system");
    }
    let path = temp.path().join("theseus.toml");
    std::fs::write(&path, text).unwrap();
    let recipe = recipe::load(&path).unwrap();
    let cancel = CancellationToken::default();
    let callback_cancel = cancel.clone();
    let mut env = Evidence::new(fault);
    let result = engine::run_with(
        &recipe,
        RunOptions {
            output_root: temp.path().join("runs"),
            ..RunOptions::all(&recipe)
        },
        cancel,
        &mut env,
        |e| {
            if boundary
                .is_some_and(|(kind, stage)| kind == e.kind && e.stage.as_deref() == Some(stage))
            {
                callback_cancel.cancel();
            }
        },
    )
    .unwrap();
    (temp, result, env)
}
#[test]
fn both_profiles_and_policies_verify_interruption_before_restoring_original() {
    for profile in [ProfileName::User, ProfileName::System] {
        for binary in [false, true] {
            let (_temp, result, env) = run("", binary, profile, None);
            assert_eq!(
                result.report.outcome,
                RunOutcome::Passed,
                "{:#?}",
                result.report.scenarios[0]
            );
            let stages = &result.report.scenarios[0].stages;
            let start = stages
                .iter()
                .position(|s| s.name == "before_interrupt")
                .unwrap();
            assert_eq!(
                stages[start..start + 10]
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>(),
                [
                    "before_interrupt",
                    "interrupt_upgrade",
                    "restart_after_interrupt",
                    "interrupted",
                    "interrupted:after_checks",
                    "before_rollback",
                    "rollback",
                    "rolled_back",
                    "rolled_back:after_checks",
                    "exercise_rolled_back"
                ]
            );
            assert!(stages[start + 1].verified_interruption());
            assert!(stages[start + 1].snapshots.is_empty());
            assert!(stages[start + 2].snapshots.is_empty());
            assert_eq!(
                stages[start + 2].restart.as_ref().unwrap().container_id,
                "same-container"
            );
            assert_eq!(env.setups, 1);
            assert_eq!(env.restarts, 1);
            assert_eq!(
                env.calls.iter().filter(|r| r.phase == "configure").count(),
                1
            );
            assert!(
                !env.calls
                    .iter()
                    .any(|r| r.phase == "recover_upgrade" || r.phase == "attempt_upgrade")
            );
            for q in &env.calls {
                let runtime = q.phase.starts_with("check:")
                    || q.phase.starts_with("resolve:")
                    || q.phase.starts_with("exercise")
                    || q.phase == "configure";
                assert_eq!(
                    q.identity.uid,
                    if runtime || profile == ProfileName::User {
                        10001
                    } else {
                        0
                    }
                );
                assert_eq!(q.expected_exit, 0);
            }
            assert_eq!(
                theseus::report::load(&result.report_path).unwrap().outcome,
                RunOutcome::Passed
            );
        }
    }
}
#[test]
fn blocked_evidence_never_authorizes_rollback_and_cleanup_obeys_availability() {
    for (fault, expected, cleanup) in [
        ("no-mutation", RunOutcome::Failed, true),
        ("incomplete:16", RunOutcome::Incomplete, true),
        ("early", RunOutcome::Failed, true),
        ("unverified", RunOutcome::Incomplete, false),
        ("contradiction", RunOutcome::Incomplete, false),
        ("malformed-interrupt", RunOutcome::Incomplete, false),
        ("restart-error", RunOutcome::Incomplete, false),
        ("wrong-id", RunOutcome::Incomplete, false),
        ("unverified-restart", RunOutcome::Incomplete, false),
        ("cancel-stop", RunOutcome::Cancelled, false),
        ("cancel-restart", RunOutcome::Cancelled, false),
        ("attempt-corrupts-backup", RunOutcome::Failed, true),
        ("failure-check-corrupts", RunOutcome::Failed, true),
        ("backup-check-effect", RunOutcome::Failed, true),
        ("preexisting", RunOutcome::Failed, false),
    ] {
        let (_temp, result, env) = run(fault, false, ProfileName::User, None);
        assert_eq!(result.report.outcome, expected, "{fault}");
        assert!(
            !env.calls
                .iter()
                .any(|q| q.phase == "rollback" || q.phase == "exercise_rolled_back"),
            "{fault}"
        );
        assert_eq!(
            env.calls.iter().any(|q| q.phase == "cleanup_backup"),
            cleanup,
            "{fault}"
        );
        assert_eq!(
            theseus::report::load(&result.report_path).unwrap().outcome,
            expected,
            "{fault}"
        );
    }
}

#[test]
fn serialized_success_requires_each_stage_check_command_and_role_proof() {
    let (_temp, valid, _) = run("", false, ProfileName::System, None);
    assert_eq!(valid.report.outcome, RunOutcome::Passed);
    let original = serde_json::to_value(&valid.report).unwrap();
    let assert_blocked = |value: serde_json::Value, label: &str| {
        std::fs::write(
            &valid.report_path,
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
        let loaded = theseus::report::load(&valid.report_path).unwrap();
        assert!(
            matches!(loaded.outcome, RunOutcome::Incomplete | RunOutcome::Failed),
            "{label}: {}",
            loaded.outcome
        );
    };
    let stages = original["scenarios"][0]["stages"].as_array().unwrap();
    for (i, stage) in stages.iter().enumerate() {
        let mut value = original.clone();
        value["scenarios"][0]["stages"]
            .as_array_mut()
            .unwrap()
            .remove(i);
        assert_blocked(value, &format!("stage {i}"));
        for (field, list) in [
            ("checks", stage["checks"].as_array().unwrap()),
            ("commands", stage["commands"].as_array().unwrap()),
        ] {
            for j in 0..list.len() {
                let mut value = original.clone();
                value["scenarios"][0]["stages"][i][field]
                    .as_array_mut()
                    .unwrap()
                    .remove(j);
                assert_blocked(value, &format!("remove {field} {i}/{j}"));
            }
        }
    }
    for (stage_name, phase) in [
        ("known_good", "check:known"),
        ("base_installed", "check:base"),
        ("rolled_back", "check:runtime"),
        ("rolled_back", "check:reader"),
        ("interrupt_upgrade", "interrupt_upgrade"),
        ("rollback", "rollback"),
    ] {
        let i = stages.iter().position(|s| s["name"] == stage_name).unwrap();
        let commands = stages[i]["commands"].as_array().unwrap();
        let j = commands.iter().position(|c| c["phase"] == phase).unwrap();
        for (field, replacement) in [
            ("argv", serde_json::json!(["/bin/true"])),
            ("phase", serde_json::json!("unrelated")),
            (
                "identity",
                serde_json::json!({"uid":42,"gid":42,"home":"/wrong"}),
            ),
            (
                "expected_failure",
                serde_json::json!({"expected_exit":9,"stdout_contains":"failure","stdout_matched":true}),
            ),
            (
                "rejection",
                serde_json::json!({"expected_exit":9,"stdout_contains":"rejected","stdout_matched":true}),
            ),
        ] {
            let mut value = original.clone();
            value["scenarios"][0]["stages"][i]["commands"][j][field] = replacement;
            assert_blocked(value, &format!("{phase}/{field}"));
        }
    }
    for (stage_name, id) in [
        ("known_good", "known"),
        ("base_installed", "base"),
        ("rolled_back", "runtime"),
        ("rolled_back", "reader"),
    ] {
        let i = stages.iter().position(|s| s["name"] == stage_name).unwrap();
        let j = stages[i]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .position(|c| c["id"] == id)
            .unwrap();
        assert!(
            stages[i]["checks"][j]["command_expectation"].is_object(),
            "{id}: missing typed role proof"
        );
        for field in [
            "command_expectation",
            "stdout_contains",
            "stdout_matched",
            "stdout_match",
            "expected_exit",
        ] {
            let mut value = original.clone();
            if field == "command_expectation" {
                value["scenarios"][0]["stages"][i]["checks"][j]
                    .as_object_mut()
                    .unwrap()
                    .remove(field);
            } else {
                value["scenarios"][0]["stages"][i]["checks"][j]["command_expectation"][field] =
                    match field {
                        "stdout_contains" => serde_json::json!("wrong marker"),
                        "stdout_matched" => serde_json::json!(false),
                        "stdout_match" => serde_json::Value::Null,
                        _ => serde_json::json!(9),
                    };
            }
            assert_blocked(value, &format!("{id}/{field}"));
        }
        let command_id = stages[i]["checks"][j]["command_ids"]
            .as_array()
            .unwrap()
            .last()
            .unwrap();
        let ci = stages[i]["commands"]
            .as_array()
            .unwrap()
            .iter()
            .position(|c| &c["id"] == command_id)
            .unwrap();
        let mut value = original.clone();
        value["scenarios"][0]["stages"][i]["commands"][ci]["stdout"] = serde_json::Value::Null;
        assert_blocked(value, &format!("{id}/stdout"));
        for bytes in [0, 1] {
            let mut value = original.clone();
            value["scenarios"][0]["stages"][i]["commands"][ci]["stdout"]["bytes"] = bytes.into();
            assert_blocked(value, &format!("{id}/short stdout {bytes}"));
        }
        let mut value = original.clone();
        value["scenarios"][0]["stages"][i]["commands"][ci]["stdout"]["truncated"] = true.into();
        assert_blocked(value, &format!("{id}/truncated stdout"));
        if id == "runtime" || id == "base" {
            for (field, replacement) in [
                ("name", serde_json::json!("unrelated")),
                ("resolved_path", serde_json::json!("/workspace/unrelated")),
                ("args", serde_json::json!(["wrong"])),
            ] {
                let mut value = original.clone();
                value["scenarios"][0]["stages"][i]["checks"][j]["command_expectation"]["executable"]
                    [field] = replacement;
                assert_blocked(value, &format!("{id}/executable/{field}"));
            }
        }
    }
    for (stage_name, field, replacement) in [
        (
            "interrupt_upgrade",
            "readiness_verified",
            serde_json::json!(false),
        ),
        (
            "interrupt_upgrade",
            "stopped_verified",
            serde_json::json!(false),
        ),
        (
            "restart_after_interrupt",
            "running_verified",
            serde_json::json!(false),
        ),
        (
            "restart_after_interrupt",
            "container_id",
            serde_json::json!("other"),
        ),
    ] {
        let i = stages.iter().position(|s| s["name"] == stage_name).unwrap();
        let mut value = original.clone();
        if stage_name == "interrupt_upgrade" {
            value["scenarios"][0]["stages"][i]["commands"][0]["interruption"][field] = replacement;
        } else {
            value["scenarios"][0]["stages"][i]["restart"][field] = replacement;
        }
        assert_blocked(value, &format!("{stage_name}/{field}"));
    }
    for (stage_name, field) in [
        ("interrupted", "comparison"),
        ("before_rollback", "backup"),
        ("rolled_back", "backup"),
    ] {
        let i = stages.iter().position(|s| s["name"] == stage_name).unwrap();
        let j = stages[i]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .position(|c| c[field].is_object() && (field != "comparison" || c["id"] == "changed"))
            .unwrap();
        let mut value = original.clone();
        if field == "comparison" {
            value["scenarios"][0]["stages"][i]["checks"][j][field]["before_stage"] =
                "before_upgrade".into();
        } else {
            value["scenarios"][0]["stages"][i]["checks"][j][field]["stored_path"] =
                "/workspace/wrong".into();
        }
        assert_blocked(value, &format!("{stage_name}/{field}"));
    }
    let mut value = original.clone();
    value["scenarios"][0]["interrupted_rollback"]["restoration"]["scopes"][0]["stored"] =
        "wrong".into();
    assert_blocked(value, "scope mapping");
    for stage_name in ["interrupt_upgrade", "restart_after_interrupt"] {
        let i = stages.iter().position(|s| s["name"] == stage_name).unwrap();
        let mut value = original.clone();
        value["scenarios"][0]["stages"][i]["snapshots"] = stages[0]["snapshots"].clone();
        assert_blocked(value, &format!("{stage_name}/invented snapshots"));
    }
    let i = stages
        .iter()
        .position(|s| s["name"] == "before_interrupt")
        .unwrap();
    let configure = stages.iter().find(|s| s["name"] == "configure").unwrap();
    let mut value = original.clone();
    value["scenarios"][0]["stages"][i]["step"] = "configure".into();
    let mut command = configure["commands"][0].clone();
    command["phase"] = "before_interrupt".into();
    value["scenarios"][0]["stages"][i]["commands"] = serde_json::json!([command]);
    assert_blocked(value, "reference stage with forged step role");
    for stage_name in [
        "before_upgrade",
        "before_interrupt",
        "interrupted",
        "before_rollback",
        "rolled_back",
    ] {
        let i = stages.iter().position(|s| s["name"] == stage_name).unwrap();
        let mut value = original.clone();
        value["scenarios"][0]["stages"][i]["snapshots"][0]["completeness"] =
            serde_json::json!({"status":"incomplete","reason":"missing"});
        assert_blocked(value, &format!("{stage_name}/incomplete"));
    }
}

#[test]
fn cancellation_at_every_interrupted_rollback_boundary_stops_forward_commands() {
    let (_temp, valid, _) = run("", false, ProfileName::User, None);
    for stage in &valid.report.scenarios[0].stages {
        for kind in [EventKind::StageStarted, EventKind::StageFinished] {
            let (_temp, result, _) = run("", false, ProfileName::User, Some((kind, &stage.name)));
            assert_eq!(
                result.report.outcome,
                RunOutcome::Cancelled,
                "{kind:?} {}",
                stage.name
            );
            assert_eq!(
                theseus::report::load(&result.report_path).unwrap().outcome,
                RunOutcome::Cancelled
            );
        }
    }
}
#[test]
fn live_and_offline_interruption_views_require_complete_proof_for_verified_banner() {
    let (_temp, valid, _) = run("", false, ProfileName::User, None);
    let mut app = theseus::tui::App::offline(
        valid.report.clone(),
        valid.report_path.parent().unwrap().into(),
    );
    app.stage_index = valid.report.scenarios[0]
        .stages
        .iter()
        .position(|s| s.name == "interrupt_upgrade")
        .unwrap();
    let text = app.items().join("\n");
    assert!(text.contains("Interruption ContainerSigkill: readiness=true stopped=true"));
    assert!(text.contains("Interrupted rollback verified"));
    app.stage_index += 1;
    assert!(
        app.items()
            .join("\n")
            .contains("Restart: same-container running=true")
    );
    app.stage_index += 1;
    assert!(
        app.items()
            .join("\n")
            .contains("before_interrupt -> interrupted")
    );
    let mut stripped = valid.report.clone();
    let stop = stripped.scenarios[0]
        .stages
        .iter_mut()
        .find(|s| s.name == "interrupt_upgrade")
        .unwrap();
    stop.commands[0].interruption = None;
    app.active = true;
    app.set_report(stripped.clone(), valid.report_path.parent().unwrap().into());
    assert!(
        !app.items()
            .join("\n")
            .contains("Interrupted rollback verified")
    );
    std::fs::write(&valid.report_path, serde_json::to_vec(&stripped).unwrap()).unwrap();
    let report = theseus::report::load(&valid.report_path).unwrap();
    assert_eq!(report.outcome, RunOutcome::Incomplete);
    app.set_report(report, valid.report_path.parent().unwrap().into());
    assert!(
        !app.items()
            .join("\n")
            .contains("Interrupted rollback verified")
    );
}

#[test]
fn mixed_ordinary_and_interrupted_rollback_use_independent_policies_and_owned_containers() {
    let (_temp, result, env) = run("mixed", false, ProfileName::User, None);
    assert_eq!(
        result.report.outcome,
        RunOutcome::Passed,
        "{:#?}",
        result.report
    );
    assert_eq!(result.report.scenarios.len(), 2);
    let ordinary = &result.report.scenarios[0];
    let interrupted = &result.report.scenarios[1];
    assert_eq!(ordinary.container_id.as_deref(), Some("same-container"));
    assert_eq!(
        interrupted.container_id.as_deref(),
        Some("second-container")
    );
    assert!(ordinary.rollback.is_some() && ordinary.interrupted_rollback.is_none());
    assert!(interrupted.rollback.is_none() && interrupted.interrupted_rollback.is_some());
    assert_eq!(env.setups, 2);
    assert_eq!(env.restarts, 1);
    for (scenario, id, other) in [
        (ordinary, "ordinary-changed", "changed"),
        (interrupted, "changed", "ordinary-changed"),
    ] {
        assert!(
            scenario
                .stages
                .iter()
                .flat_map(|s| &s.checks)
                .any(|c| c.id == id && c.outcome == CheckOutcome::Passed)
        );
        assert!(
            !scenario
                .stages
                .iter()
                .flat_map(|s| &s.checks)
                .any(|c| c.id == other)
        );
    }
    assert_eq!(
        theseus::report::load(&result.report_path).unwrap().outcome,
        RunOutcome::Passed
    );
}

#[test]
fn unrelated_nonzero_command_checks_keep_their_declared_exit_expectation() {
    let (_temp, result, _) = run("nonzero-check", false, ProfileName::User, None);
    assert_eq!(result.report.outcome, RunOutcome::Passed);
    assert_eq!(
        theseus::report::load(&result.report_path).unwrap().outcome,
        RunOutcome::Passed
    );
}

#[test]
fn declared_restored_reader_can_use_a_distinct_script_and_marker() {
    let (_temp, result, _) = run("separate-reader", true, ProfileName::User, None);
    assert_eq!(result.report.outcome, RunOutcome::Passed);
    assert_eq!(
        theseus::report::load(&result.report_path).unwrap().outcome,
        RunOutcome::Passed
    );
}

#[test]
fn mixed_modes_keep_backup_steps_checks_and_authorization_with_restore_data_entry_only() {
    for (fault, binary_kind, restore_kind) in [
        (
            "mixed-ordinary-binary",
            ScenarioKind::Rollback,
            ScenarioKind::InterruptedRollback,
        ),
        (
            "mixed-interrupted-binary",
            ScenarioKind::InterruptedRollback,
            ScenarioKind::Rollback,
        ),
    ] {
        let (_temp, result, env) = run(fault, false, ProfileName::User, None);
        assert_eq!(
            result.report.outcome,
            RunOutcome::Passed,
            "{fault}: {:#?}",
            result.report
        );
        let binary = result
            .report
            .scenarios
            .iter()
            .find(|s| s.scenario == binary_kind)
            .unwrap();
        let restore = result
            .report
            .scenarios
            .iter()
            .find(|s| s.scenario == restore_kind)
            .unwrap();
        assert_ne!(binary.container_id, restore.container_id);
        assert!(!binary.stages.iter().any(|s| matches!(
            s.step,
            Some(StepName::Backup | StepName::CleanupBackup)
        ) || matches!(
            s.checkpoint,
            Some(Checkpoint::BackupCreated | Checkpoint::BackupRemoved)
        )));
        assert!(
            !binary
                .stages
                .iter()
                .flat_map(|s| &s.checks)
                .any(|c| c.backup.is_some()
                    || matches!(
                        c.checkpoint,
                        Checkpoint::BackupCreated | Checkpoint::BackupRemoved
                    ))
        );
        assert!(
            restore
                .stages
                .iter()
                .flat_map(|s| &s.checks)
                .any(|c| c.id == "backup-contract" && c.outcome == CheckOutcome::Passed)
        );
        assert!(
            restore
                .stages
                .iter()
                .flat_map(|s| &s.checks)
                .any(|c| c.id == "backup-removal-contract" && c.outcome == CheckOutcome::Passed)
        );
        assert_eq!(env.calls.iter().filter(|q| q.phase == "backup").count(), 1);
        assert_eq!(
            env.calls
                .iter()
                .filter(|q| q.phase == "cleanup_backup")
                .count(),
            1
        );
        assert_eq!(
            theseus::report::load(&result.report_path).unwrap().outcome,
            RunOutcome::Passed
        );
        let (_temp, cancelled, env) = run(
            fault,
            false,
            ProfileName::User,
            Some((EventKind::StageStarted, "baseline")),
        );
        assert_eq!(cancelled.report.outcome, RunOutcome::Cancelled);
        assert_eq!(cancelled.report.scenarios[0].scenario, binary_kind);
        assert!(
            !cancelled.report.scenarios[0]
                .stages
                .iter()
                .flat_map(|s| &s.checks)
                .any(|c| matches!(
                    c.checkpoint,
                    Checkpoint::BackupCreated | Checkpoint::BackupRemoved
                ))
        );
        assert!(
            !env.calls
                .iter()
                .any(|q| q.phase == "backup" || q.phase == "cleanup_backup")
        );
    }
}
