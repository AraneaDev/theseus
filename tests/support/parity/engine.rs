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
        if self.fault == "panic-setup" {
            panic!("original setup panic");
        }
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
        if request.phase == "check:json" {
            assert_eq!(request.identity.uid, 10001);
            assert_eq!(request.identity.gid, 10001);
            assert_eq!(request.argv, vec!["/bin/sh", "-c", "printf true"]);
            assert_eq!(
                request.env.get("JSON_TEST_LITERAL").map(String::as_str),
                Some("value")
            );
            assert_eq!(request.timeout_seconds, 7);
            assert_eq!(
                request.log_limit,
                if self.fault == "json-small" { 2 } else { 4 }
            );
            let truncated = self.fault == "json-small";
            let mut record = CommandRecord::new(&request.id, &request.phase);
            record.identity = request.identity;
            record.exit_status = Some(0);
            record.outcome = CommandOutcome::Succeeded;
            record.stdout = Some(LogReference {
                path: "logs/json.stdout".into(),
                bytes: if truncated { 2 } else { 4 },
                truncated,
            });
            record.stderr = Some(LogReference {
                path: "logs/json.stderr".into(),
                bytes: 4,
                truncated: true,
            });
            return Ok(ExecutedCommand {
                record,
                stdout: if truncated { "tr" } else { "true" }.into(),
                stdout_complete: !truncated,
                stdout_utf8: true,
            });
        }
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
        if self.fault == "panic-setup" {
            self.fault = "removed-after-panic";
        }
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
            stdout_utf8: true,
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
    let user = include_str!("../../fixtures/recipes/v2_reinstall.toml");
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
            stdout_utf8: true,
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
            stdout_utf8: true,
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
    let user = include_str!("../../fixtures/recipes/v3_interruption.toml");
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
            stdout_utf8: true,
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
    downgrade_recipe_from_template(
        rejection,
        include_str!("../../fixtures/recipes/v4_downgrade.toml"),
    )
}
fn downgrade_recipe_from_template(
    rejection: bool,
    template: &str,
) -> (tempfile::TempDir, ValidatedRecipe) {
    let temp = tempfile::tempdir().unwrap();
    for name in ["base", "target"] {
        std::fs::create_dir(temp.path().join(name)).unwrap();
    }
    // This repository-owned test template is transformed by literal LF patterns.
    let mut text = template.replace("\r\n", "\n");
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

fn json_recipe() -> (tempfile::TempDir, ValidatedRecipe) {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("target")).unwrap();
    let text = include_str!("../../fixtures/recipes/v2_clean.toml")
        .replace("schema_version = 2", "schema_version = 7")
        + r#"
[[profiles.user.checks]]
id = "json"
checkpoint = "target_installed"
kind = "json_command"
script = "printf true"
timeout_seconds = 7
expected_exit = 0
max_stdout_bytes = 4
env = { JSON_TEST_LITERAL = "value" }
assertions = [{kind = "equals", pointer = "", equals_json = "true"}]
"#;
    let path = temp.path().join("recipe.toml");
    std::fs::write(&path, text).unwrap();
    let loaded = load(&path).unwrap();
    (temp, loaded)
}
