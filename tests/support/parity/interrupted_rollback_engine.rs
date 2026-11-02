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
            stdout_utf8: true,
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
            stdout_utf8: true,
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
    let mut text = include_str!("../../fixtures/recipes/rollback.toml").to_string();
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
