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
}
impl Evidence {
    fn new(fault: &str) -> Self {
        Self {
            files: BTreeMap::from([("/workspace".into(), entry(FileKind::Directory, ""))]),
            fault: fault.into(),
            calls: vec![],
            observations: 0,
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
        Ok(OwnedResource {
            container_id: "same-container".into(),
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
        cancel: &CancellationToken,
    ) -> Result<ExecutedCommand> {
        assert_eq!(resource.container_id, "same-container");
        assert!(!cancel.is_cancelled());
        self.calls.push(request.clone());
        let phase = request.phase.as_str();
        let mut stdout = "readable".to_string();
        let mut status = 0;
        match phase {
            "install_base" => {
                if self.fault == "install-fails" {
                    status = 1;
                }
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
        Ok(ExecutedCommand {
            record,
            stdout,
            stdout_complete: !(self.fault == "truncated" && phase == "attempt_upgrade"),
        })
    }
    fn observe(
        &mut self,
        _: &OwnedResource,
        roots: &[String],
        _: Limits,
        _: &CancellationToken,
    ) -> Result<Vec<Snapshot>> {
        self.observations += 1;
        if self.fault == format!("empty:{}", self.observations) {
            return Ok(vec![]);
        }
        if self.fault == format!("error:{}", self.observations) {
            anyhow::bail!("capture transport failed");
        }
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
    if binary {
        text=text.replace("mode = 'restore_data'\nbackup_root = '/workspace/backup'\n[[profiles.user.rollback.restoration.scopes]]\nid = 'data'\nsource = '/workspace/data'\nstored = 'data'","mode = 'binary_only'").replace("[profiles.user.steps.backup]\nscript = 'backup'\n", "").replace("[profiles.user.steps.cleanup_backup]\nscript = 'cleanup backup'\n","");
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
        text += "\n[[profiles.user.checks]]\nid='failure-effect'\ncheckpoint='upgrade_failed'\nkind='command'\nscript='corrupt backup'\nexpected_exit=0\nstdout_contains='readable'\n";
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
fn both_profiles_and_policies_prove_expected_failure_readers_and_restore_references() {
    for profile in [ProfileName::User, ProfileName::System] {
        for binary in [false, true] {
            let (_temp, result, env) = run("", binary, profile, None);
            assert_eq!(
                result.report.outcome,
                RunOutcome::Passed,
                "{:#?}",
                result.report.scenarios[0]
            );
            let s = &result.report.scenarios[0];
            let mut expected = vec![
                "baseline",
                "install_base",
                "base_installed",
                "base_installed:after_checks",
                "configure",
                "configured",
                "configured:after_checks",
                "exercise_before_upgrade",
                "known_good",
                "known_good:after_checks",
                "before_upgrade",
            ];
            if !binary {
                expected.extend(["backup", "backup_created", "backup_created:after_checks"]);
            }
            expected.extend([
                "before_attempt",
                "attempt_upgrade",
                "upgrade_failed",
                "upgrade_failed:after_checks",
                "before_rollback",
                "rollback",
                "rolled_back",
                "rolled_back:after_checks",
                "exercise_rolled_back",
            ]);
            if !binary {
                expected.extend([
                    "cleanup_backup",
                    "backup_removed",
                    "backup_removed:after_checks",
                ]);
            }
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
            let before = s
                .stages
                .iter()
                .find(|s| s.name == "before_upgrade")
                .unwrap();
            assert_eq!(
                before.snapshots[0].entries["/workspace/data/row"]
                    .sha256
                    .as_deref(),
                Some("known-good-after-reader")
            );
            let attempt = s
                .stages
                .iter()
                .find(|s| s.name == "attempt_upgrade")
                .unwrap();
            assert!(attempt.verified_failure());
            assert_eq!(attempt.outcome, StageOutcome::Passed);
            assert_eq!(
                env.calls.iter().filter(|r| r.phase == "configure").count(),
                1
            );
            assert_eq!(
                env.calls
                    .iter()
                    .filter(|r| r.expected_exit != 0)
                    .map(|r| r.phase.as_str())
                    .collect::<Vec<_>>(),
                vec!["attempt_upgrade"]
            );
            for request in &env.calls {
                let runtime = request.phase.starts_with("check:")
                    || request.phase.starts_with("resolve:")
                    || request.phase.starts_with("exercise")
                    || request.phase == "configure";
                assert_eq!(
                    request.identity.uid,
                    if runtime || profile == ProfileName::User {
                        10001
                    } else {
                        0
                    }
                );
            }
            let saved = theseus::report::load(&result.report_path).unwrap();
            assert_eq!(saved.outcome, RunOutcome::Passed);
            if !binary {
                assert_eq!(
                    env.files["/workspace/data/row"].sha256.as_deref(),
                    Some("known-good-after-reader")
                );
            }
        }
    }
}
#[test]
fn invalid_attempt_and_backup_never_authorize_restore_but_owned_backup_is_cleaned() {
    for fault in [
        "no-mutation",
        "success",
        "wrong-status",
        "wrong-marker",
        "unverified",
        "truncated",
        "destructive-backup",
        "corrupt-backup",
        "partial-backup",
        "attempt-corrupts-backup",
        "failure-check-corrupts",
        "backup-check-effect",
        "preexisting",
    ] {
        let (_temp, result, env) = run(fault, false, ProfileName::User, None);
        assert_ne!(result.report.outcome, RunOutcome::Passed, "{fault}");
        assert!(!env.calls.iter().any(|r| r.phase == "rollback"), "{fault}");
        assert_eq!(
            env.calls.iter().any(|r| r.phase == "cleanup_backup"),
            fault != "preexisting",
            "{fault}"
        );
        if fault == "preexisting" {
            assert!(!env.calls.iter().any(|r| r.phase == "backup"));
            assert!(env.files.contains_key("/workspace/backup"));
        }
        if fault == "backup-check-effect" {
            let c = result.report.scenarios[0]
                .stages
                .iter()
                .flat_map(|s| &s.checks)
                .find(|c| c.id == "changed")
                .unwrap();
            assert_eq!(c.outcome, CheckOutcome::Failed);
        }
    }
}
#[test]
fn failed_restore_reader_cleanup_and_directory_binary_do_not_pass() {
    for binary in [false, true] {
        for fault in [
            "noop-rollback",
            "reader-fails",
            "binary-directory",
            "wrong-runtime",
        ] {
            let (_temp, result, _) = run(fault, binary, ProfileName::User, None);
            assert_eq!(
                result.report.outcome,
                RunOutcome::Failed,
                "{fault} {binary}"
            );
        }
    }
    for fault in ["partial-restore", "cleanup-fails"] {
        let (_temp, result, _) = run(fault, false, ProfileName::User, None);
        assert_eq!(result.report.outcome, RunOutcome::Failed, "{fault}");
    }
    let (_temp, result, _) = run("binary-link", false, ProfileName::User, None);
    assert_eq!(result.report.outcome, RunOutcome::Passed);
}
#[test]
fn incomplete_new_reference_stops_forward_mutation() {
    // Literal observe positions include the command snapshots and after-check snapshots.
    for (position, blocked) in [(11, "backup"), (15, "attempt_upgrade"), (19, "rollback")] {
        let (_temp, result, env) = run(
            &format!("incomplete:{position}"),
            false,
            ProfileName::User,
            None,
        );
        assert_eq!(result.report.outcome, RunOutcome::Incomplete);
        assert!(
            !env.calls.iter().any(|r| r.phase == blocked),
            "observation {position}: {blocked}"
        );
    }
}
#[test]
fn cancellation_at_every_event_boundary_prevents_further_user_mutation() {
    let (_temp, passed, _) = run("", false, ProfileName::User, None);
    for stage in &passed.report.scenarios[0].stages {
        for kind in [EventKind::StageStarted, EventKind::StageFinished] {
            let (_temp, result, _) = run("", false, ProfileName::User, Some((kind, &stage.name)));
            assert_eq!(
                result.report.outcome,
                RunOutcome::Cancelled,
                "{kind:?} {}",
                stage.name
            );
        }
    }
    for phase in ["backup", "attempt_upgrade", "rollback", "cleanup_backup"] {
        for fault in [format!("cancel:{phase}"), format!("timeout:{phase}")] {
            let (_temp, result, env) = run(&fault, false, ProfileName::User, None);
            assert_ne!(result.report.outcome, RunOutcome::Passed);
            let index = env.calls.iter().position(|r| r.phase == phase).unwrap();
            assert_eq!(index + 1, env.calls.len(), "{fault}");
        }
    }
}

#[test]
fn completed_report_normalization_requires_commands_policy_and_every_selected_proof() {
    let (_temp, valid, _) = run("", false, ProfileName::User, None);
    let stage_count = valid.report.scenarios[0].stages.len();
    for index in 0..stage_count {
        let mut report = valid.report.clone();
        report.scenarios[0].stages.remove(index);
        report.recompute_outcome();
        assert_eq!(
            report.outcome,
            RunOutcome::Incomplete,
            "missing stage {index}"
        );
        for check_index in 0..valid.report.scenarios[0].stages[index].checks.len() {
            let mut report = valid.report.clone();
            report.scenarios[0].stages[index].checks.remove(check_index);
            report.recompute_outcome();
            assert_eq!(
                report.outcome,
                RunOutcome::Incomplete,
                "missing check {index}/{check_index}"
            );
        }
        if valid.report.scenarios[0].stages[index].step.is_some() {
            for mode in 0..3 {
                let mut report = valid.report.clone();
                let stage = &mut report.scenarios[0].stages[index];
                match mode {
                    0 => stage.commands.clear(),
                    1 => stage.commands[0].phase = "wrong-phase".into(),
                    _ => stage.commands[0].exit_status = Some(99),
                }
                report.recompute_outcome();
                assert_eq!(
                    report.outcome,
                    RunOutcome::Incomplete,
                    "command {index}/{mode}"
                );
            }
        }
    }
    for mode in 0..8 {
        let mut report = valid.report.clone();
        let s = &mut report.scenarios[0];
        match mode {
            0 => s.rollback = None,
            1 => s.rollback.as_mut().unwrap().failure.expected_exit = 8,
            2 => {
                s.stages
                    .iter_mut()
                    .find(|s| s.name == "before_rollback")
                    .unwrap()
                    .checks[0]
                    .comparison
                    .as_mut()
                    .unwrap()
                    .after_stage = "upgrade_failed".into()
            }
            3 => {
                s.stages
                    .iter_mut()
                    .find(|s| s.name == "rolled_back")
                    .unwrap()
                    .checks
                    .iter_mut()
                    .find(|c| c.id == "reader")
                    .unwrap()
                    .command_ids = vec!["missing-command".into()]
            }
            4 => s
                .stages
                .iter_mut()
                .find(|s| s.name == "before_upgrade")
                .unwrap()
                .snapshots
                .clear(),
            6 => {
                let stage = s
                    .stages
                    .iter_mut()
                    .find(|s| s.name == "rolled_back")
                    .unwrap();
                stage
                    .commands
                    .iter_mut()
                    .find(|c| c.phase == "check:runtime")
                    .unwrap()
                    .argv[0] = "/workspace/unrelated".into();
            }
            7 => {
                let stage = s
                    .stages
                    .iter_mut()
                    .find(|s| s.name == "rolled_back")
                    .unwrap();
                let runtime = stage.checks.iter_mut().find(|c| c.id == "runtime").unwrap();
                runtime.command_ids.remove(0);
            }
            _ => {
                s.stages
                    .iter_mut()
                    .find(|s| s.name == "rolled_back")
                    .unwrap()
                    .checks[0]
                    .backup
                    .as_mut()
                    .unwrap()
                    .stored_path = "/workspace/elsewhere".into()
            }
        }
        report.recompute_outcome();
        assert_eq!(report.outcome, RunOutcome::Incomplete, "context {mode}");
    }
}

#[test]
fn rollback_preserves_normal_nonzero_checks_and_configured_retention_reference() {
    let (_temp, result, _) = run("nonzero-check", false, ProfileName::User, None);
    assert_eq!(result.report.outcome, RunOutcome::Passed);
    let (_temp, result, _) = run("preserved-reference", false, ProfileName::User, None);
    assert_eq!(result.report.outcome, RunOutcome::Failed);
    let retention = result.report.scenarios[0]
        .stages
        .iter()
        .flat_map(|s| &s.checks)
        .find(|c| c.id == "configured-retention")
        .unwrap();
    assert_eq!(retention.outcome, CheckOutcome::Failed);
}

#[test]
fn required_rollback_commands_reject_the_twelve_reviewed_malformed_records() {
    let (_temp, valid, _) = run("", false, ProfileName::User, None);
    assert_eq!(valid.report.outcome, RunOutcome::Passed);
    let mut accepted = vec![];
    for (stage_name, phase) in [
        ("known_good", "check:known"),
        ("rolled_back", "check:reader"),
        ("rollback", "rollback"),
        ("attempt_upgrade", "attempt_upgrade"),
    ] {
        for argv in [vec![], vec!["/bin/true"], vec!["/bin/sh", "-c", ""]] {
            let mut report = valid.report.clone();
            let command = report.scenarios[0]
                .stages
                .iter_mut()
                .find(|s| s.name == stage_name)
                .unwrap()
                .commands
                .iter_mut()
                .find(|c| c.phase == phase)
                .unwrap();
            command.argv = argv.iter().map(|s| (*s).into()).collect();
            report.recompute_outcome();
            if report.outcome != RunOutcome::Incomplete {
                accepted.push(format!("{stage_name}/{phase} {argv:?}: {}", report.outcome));
            }
        }
    }
    assert!(
        accepted.is_empty(),
        "malformed required records accepted: {accepted:#?}"
    );
}

#[test]
fn required_shell_roles_reject_invalid_script_shapes_and_accept_valid_author_scripts() {
    for binary in [false, true] {
        let (_temp, valid, _) = run("", binary, ProfileName::User, None);
        assert_eq!(valid.report.outcome, RunOutcome::Passed);
        let mut roles = vec![
            ("known_good", "check:known"),
            ("rolled_back", "check:reader"),
            ("install_base", "install_base"),
            ("configure", "configure"),
            ("exercise_before_upgrade", "exercise_before_upgrade"),
            ("attempt_upgrade", "attempt_upgrade"),
            ("rollback", "rollback"),
            ("exercise_rolled_back", "exercise_rolled_back"),
            ("uninstall", "uninstall"),
        ];
        if !binary {
            roles.extend([("backup", "backup"), ("cleanup_backup", "cleanup_backup")]);
        }
        for (stage_name, phase) in roles {
            for argv in [
                vec![],
                vec!["/bin/true"],
                vec!["/bin/sh", "-c", ""],
                vec!["/bin/sh", "-c", " \t\n"],
                vec!["/bin/sh", "-c", "echo ok\0ignored"],
                vec!["/bin/sh", "-c", "true", "extra"],
            ] {
                let mut report = valid.report.clone();
                report.scenarios[0]
                    .stages
                    .iter_mut()
                    .find(|s| s.name == stage_name)
                    .unwrap()
                    .commands
                    .iter_mut()
                    .find(|c| c.phase == phase)
                    .unwrap()
                    .argv = argv.iter().map(|s| (*s).into()).collect();
                report.recompute_outcome();
                assert_eq!(
                    report.outcome,
                    RunOutcome::Incomplete,
                    "{binary}/{stage_name} {argv:?}"
                );
            }
            for script in [
                "printf '%s\\n' readable",
                "# author script\n: \"$variable\"\nexit 0",
                " true\n",
            ] {
                let mut report = valid.report.clone();
                report.scenarios[0]
                    .stages
                    .iter_mut()
                    .find(|s| s.name == stage_name)
                    .unwrap()
                    .commands
                    .iter_mut()
                    .find(|c| c.phase == phase)
                    .unwrap()
                    .argv = vec!["/bin/sh".into(), "-c".into(), script.into()];
                report.recompute_outcome();
                assert_eq!(
                    report.outcome,
                    RunOutcome::Passed,
                    "{binary}/{stage_name} {script:?}"
                );
            }
        }
    }
}

#[test]
fn required_runtime_records_reject_impossible_resolver_and_execution_arguments() {
    let (_temp, valid, _) = run("", false, ProfileName::User, None);
    assert_eq!(valid.report.outcome, RunOutcome::Passed);
    for (role, argv) in [
        ("resolve", vec![]),
        ("resolve", vec!["/bin/true"]),
        (
            "resolve",
            vec!["/bin/sh", "-c", "command -v \"$1\"", "theseus-resolve", ""],
        ),
        (
            "resolve",
            vec![
                "/bin/sh",
                "-c",
                "command -v \"$1\"",
                "theseus-resolve",
                "tool\0ignored",
            ],
        ),
        (
            "resolve",
            vec![
                "/bin/sh",
                "-c",
                "command -v \"$1\"",
                "theseus-resolve",
                "tool name",
            ],
        ),
        (
            "resolve",
            vec![
                "/bin/sh",
                "-c",
                "command -v \"$1\"",
                "theseus-resolve",
                "/workspace/tool",
            ],
        ),
        ("check", vec![]),
        ("check", vec!["/workspace/tool", "bad\0argument"]),
    ] {
        let mut report = valid.report.clone();
        // Mutate both signatures so matching base/restored argv cannot mask the malformed shape.
        for (stage, id) in [("base_installed", "base"), ("rolled_back", "runtime")] {
            report.scenarios[0]
                .stages
                .iter_mut()
                .find(|s| s.name == stage)
                .unwrap()
                .commands
                .iter_mut()
                .find(|c| c.phase == format!("{role}:{id}"))
                .unwrap()
                .argv = argv.iter().map(|s| (*s).into()).collect();
        }
        report.recompute_outcome();
        assert_eq!(report.outcome, RunOutcome::Incomplete, "{role} {argv:?}");
    }
}

#[test]
fn incomplete_checkpoint_observations_revoke_forward_authorization_permanently() {
    // Literal capture positions include command, checkpoint and after-check snapshots.
    // Each row independently states which forward commands were already authorized.
    let mut violations = vec![];
    for (capture, stage_name, backup, candidate, rollback, cleanup) in [
        (10, "known_good:after_checks", false, false, false, false),
        (14, "backup_created:after_checks", true, false, false, true),
        (18, "upgrade_failed:after_checks", true, true, false, true),
        (22, "rolled_back:after_checks", true, true, true, true),
        (9, "known_good", false, false, false, false),
    ] {
        let (_temp, result, env) = run(
            &format!("incomplete:{capture}"),
            false,
            ProfileName::User,
            None,
        );
        assert_eq!(
            result.report.outcome,
            RunOutcome::Incomplete,
            "{stage_name}"
        );
        let scenario = &result.report.scenarios[0];
        let incomplete = scenario
            .stages
            .iter()
            .find(|s| s.name == stage_name)
            .unwrap();
        assert!(matches!(
            incomplete.snapshots[0].completeness,
            ObservationStatus::Incomplete { .. }
        ));
        // Later complete observations must never re-enable a failed prerequisite.
        let fresh = scenario
            .stages
            .iter()
            .find(|s| s.name == "before_rollback")
            .unwrap();
        assert!(!fresh.snapshots.is_empty());
        assert!(
            fresh
                .snapshots
                .iter()
                .all(|s| s.completeness == ObservationStatus::Complete)
        );
        for (phase, expected) in [
            ("backup", backup),
            ("attempt_upgrade", candidate),
            ("rollback", rollback),
            ("exercise_rolled_back", false),
            ("cleanup_backup", cleanup),
            ("uninstall", true),
        ] {
            let executed = env.calls.iter().any(|q| q.phase == phase);
            if executed != expected {
                violations.push(format!("{stage_name} capture={capture}: {phase} executed={executed}, expected={expected}"));
            }
        }
        assert_eq!(
            theseus::report::load(&result.report_path).unwrap().outcome,
            RunOutcome::Incomplete,
            "{stage_name}"
        );
    }
    assert!(
        violations.is_empty(),
        "incomplete observation authorized forbidden commands: {violations:#?}"
    );
}

#[test]
fn every_required_capture_revokes_forward_authorization_permanently() {
    // Removing the central observation gate must expose actual forbidden commands.
    // Literal positions/order come from the independently reviewed lifecycle contract.
    let mut violations = vec![];
    for (capture, stage_name, backup, candidate, rollback, exercise, cleanup) in [
        (1, "baseline", false, false, false, false, false),
        (2, "install_base", false, false, false, false, false),
        (3, "base_installed", false, false, false, false, false),
        (
            4,
            "base_installed:after_checks",
            false,
            false,
            false,
            false,
            false,
        ),
        (5, "configure", false, false, false, false, false),
        (6, "configured", false, false, false, false, false),
        (
            7,
            "configured:after_checks",
            false,
            false,
            false,
            false,
            false,
        ),
        (
            8,
            "exercise_before_upgrade",
            false,
            false,
            false,
            false,
            false,
        ),
        (9, "known_good", false, false, false, false, false),
        (
            10,
            "known_good:after_checks",
            false,
            false,
            false,
            false,
            false,
        ),
        (11, "before_upgrade", false, false, false, false, false),
        (12, "backup", true, false, false, false, true),
        (13, "backup_created", true, false, false, false, true),
        (
            14,
            "backup_created:after_checks",
            true,
            false,
            false,
            false,
            true,
        ),
        (15, "before_attempt", true, false, false, false, true),
        (16, "attempt_upgrade", true, true, false, false, true),
        (17, "upgrade_failed", true, true, false, false, true),
        (
            18,
            "upgrade_failed:after_checks",
            true,
            true,
            false,
            false,
            true,
        ),
        (19, "before_rollback", true, true, false, false, true),
        (20, "rollback", true, true, true, false, true),
        (21, "rolled_back", true, true, true, false, true),
        (
            22,
            "rolled_back:after_checks",
            true,
            true,
            true,
            false,
            true,
        ),
        (23, "exercise_rolled_back", true, true, true, true, true),
        (24, "cleanup_backup", true, true, true, true, true),
        (25, "backup_removed", true, true, true, true, true),
        (
            26,
            "backup_removed:after_checks",
            true,
            true,
            true,
            true,
            true,
        ),
        (27, "uninstall", true, true, true, true, true),
        (28, "uninstalled", true, true, true, true, true),
        (29, "uninstalled:after_checks", true, true, true, true, true),
        (30, "final", true, true, true, true, true),
    ] {
        for fault in ["incomplete", "empty", "error"] {
            let (_temp, result, env) = run(
                &format!("{fault}:{capture}"),
                false,
                ProfileName::User,
                None,
            );
            let scenario = &result.report.scenarios[0];
            let target = scenario
                .stages
                .iter()
                .find(|s| s.name == stage_name)
                .unwrap();
            assert!(
                if fault == "empty" {
                    target.snapshots.is_empty()
                } else {
                    target
                        .snapshots
                        .iter()
                        .any(|s| matches!(s.completeness, ObservationStatus::Incomplete { .. }))
                },
                "{fault} {stage_name}: injection missed target"
            );
            for (phase, expected) in [
                ("resolve:base", capture > 3),
                ("check:base", capture > 3),
                ("check:known", capture > 9),
                ("backup", backup),
                ("attempt_upgrade", candidate),
                ("rollback", rollback),
                ("exercise_rolled_back", exercise),
                ("cleanup_backup", cleanup),
                ("uninstall", true),
                ("configure", capture >= 5),
                ("exercise_before_upgrade", capture >= 8),
                ("install_base", capture >= 2),
            ] {
                let executed = env.calls.iter().any(|q| q.phase == phase);
                if executed != expected {
                    violations.push(format!("{fault} {stage_name} capture={capture}: {phase} executed={executed}, expected={expected}"));
                }
            }
            for (kind, outcome) in [
                ("live", result.report.outcome),
                (
                    "offline",
                    theseus::report::load(&result.report_path).unwrap().outcome,
                ),
            ] {
                if outcome != RunOutcome::Incomplete {
                    violations.push(format!(
                        "{fault} {stage_name}: {kind} outcome={outcome:?}, expected=Incomplete"
                    ));
                }
            }
            if stage_name != "final" {
                let later = scenario.stages.iter().find(|s| s.name == "final").unwrap();
                assert!(!later.snapshots.is_empty());
                assert!(
                    later
                        .snapshots
                        .iter()
                        .all(|s| s.completeness == ObservationStatus::Complete)
                );
            }
            assert_eq!(scenario.teardown, TeardownStatus::Complete);
        }
    }
    assert!(
        violations.is_empty(),
        "unverified capture authorized forbidden commands or lost Incomplete: {violations:#?}"
    );
}

#[test]
fn verified_observations_preserve_diagnostics_after_known_install_failure() {
    let (_temp, result, env) = run("install-fails", false, ProfileName::User, None);
    assert_eq!(result.report.outcome, RunOutcome::Failed);
    assert!(env.calls.iter().any(|q| q.phase == "check:base"));
    assert!(env.calls.iter().any(|q| q.phase == "uninstall"));
    assert!(!env.calls.iter().any(|q| matches!(
        q.phase.as_str(),
        "backup"
            | "attempt_upgrade"
            | "interrupt_upgrade"
            | "rollback"
            | "exercise_rolled_back"
            | "cleanup_backup"
    )));
}
