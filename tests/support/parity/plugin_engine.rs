#[path = "../plugin.rs"]
mod fixture;
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use theseus::{
    engine::{self, CancellationToken, RunOptions, RunResult},
    model::*,
    recipe::{self, JsonAssertion, Limits, PluginScope, ValidatedRecipe},
    report::ReportStore,
    runner::{CommandRequest, Environment, ExecutedCommand, PreparedEnvironment},
    snapshot::{FileEntry, ObservationStatus, Snapshot},
};
struct ReplayEnvironment {
    scope: PluginScope,
    fault: String,
    calls: Vec<CommandRequest>,
    created: usize,
    setups: usize,
    removed: usize,
    stopped: usize,
    observations: usize,
    phase: String,
}
impl Environment for ReplayEnvironment {
    fn prepare(
        &mut self,
        recipe: &ValidatedRecipe,
        _: bool,
        _: &CancellationToken,
    ) -> Result<PreparedEnvironment> {
        let client = std::fs::read(recipe.target_artifact.join("client/claude"))?;
        Ok(PreparedEnvironment {
            image_id: "sha256:replay-image".into(),
            artifacts: vec![ArtifactDigest {
                name: "target".into(),
                sha256: "a".repeat(64),
                files: vec![ArtifactFileDigest {
                    path: "client/claude".into(),
                    sha256: format!("{:x}", Sha256::digest(&client)),
                    size: client.len() as u64,
                }],
            }],
        })
    }
    fn create(&mut self, _: &str, _: &str, _: &ValidatedRecipe) -> Result<OwnedResource> {
        self.created += 1;
        Ok(OwnedResource {
            container_id: "owned-replay".into(),
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
        _: &OwnedResource,
        r: CommandRequest,
        store: &ReportStore,
        cancel: &CancellationToken,
    ) -> Result<ExecutedCommand> {
        assert_eq!(self.stopped, 0, "no client command after stop");
        self.calls.push(r.clone());
        let phase = r.phase.clone();
        if !phase.starts_with("check:") {
            self.phase = phase.clone();
        }
        let mut stdout = if phase == "check:client-version" {
            r#"{"version":"2.1.312 (Claude Code)"}"#.into()
        } else if phase.ends_with("-preserved") {
            r#"{"unrelated":{"theme":"dark"},"companions":[{"id":"companion@market","version":"1","enabled":true,"installPath":"/cache/companion"}]}"#.into()
        } else if phase.ends_with("-inventory") {
            if self.phase == "uninstall" {
                "[]".into()
            } else {
                serde_json::json!([{"id":"probe@market","version":if self.phase=="install_base" {"1"}else{"2"},"scope":self.scope.as_str(),"enabled":self.phase!="disable_plugin","installPath":"/cache/probe","readFromFolder":"/market/probe","folderVersion":"2"}]).to_string()
            }
        } else {
            "source/cache manifests skills legacy-command manual-hook verified".into()
        };
        let mut record = CommandRecord::new(&r.id, &phase);
        record.identity = r.identity;
        record.argv = r.argv;
        record.environment = r.env;
        record.forwarded_environment = r.pass_env;
        record.exit_status = Some(0);
        record.outcome = CommandOutcome::Succeeded;
        if self.fault == phase {
            record.exit_status = Some(1);
            record.outcome = CommandOutcome::Failed;
        }
        if self.fault == format!("timeout:{phase}") {
            record.timed_out = true;
            record.outcome = CommandOutcome::TimedOut;
        }
        if self.fault == format!("cancel:{phase}") {
            cancel.cancel();
            record.cancelled = true;
            record.outcome = CommandOutcome::Cancelled;
        }
        if phase == "check:target-inventory" {
            match self.fault.as_str() {
                "duplicate" => {
                    let mut v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
                    let duplicate = v[0].clone();
                    v.as_array_mut().unwrap().push(duplicate);
                    stdout = v.to_string();
                }
                "wrong-scope" => stdout = stdout.replace(self.scope.as_str(), "other"),
                "wrong-version" => stdout = stdout.replace("\"2\"", "\"1\""),
                "invalid-json" => stdout = "[".into(),
                "duplicate-key" => stdout = "[{\"id\":\"probe@market\",\"id\":\"other\"}]".into(),
                _ => {}
            }
        }
        if self.fault == "preservation-loss" && phase == "check:target-preserved" {
            stdout = "{}".into();
        }
        if self.fault == "companion-mutation" && phase == "check:target-preserved" {
            stdout = stdout.replace("\"1\"", "\"2\"");
        }
        if self.fault == "hook-marker" && phase == "check:target-components" {
            stdout = "wrong manual hook".into();
        }
        let log = format!("logs/{}.stdout", record.id);
        std::io::Write::write_all(&mut store.create_log(&log)?, stdout.as_bytes())?;
        record.stdout = Some(LogReference {
            path: log,
            bytes: stdout.len() as u64,
            truncated: self.fault == "truncated" && phase == "check:target-inventory",
        });
        Ok(ExecutedCommand {
            record,
            stdout,
            stdout_utf8: !(self.fault == "invalid-utf8" && phase == "check:target-inventory"),
            stdout_complete: !(self.fault == "truncated" && phase == "check:target-inventory"),
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
        Ok(roots
            .iter()
            .map(|root| Snapshot {
                root: root.clone(),
                completeness: if self.fault == "observation" && self.observations == 3 {
                    ObservationStatus::Incomplete {
                        reason: "unhealthy observation".into(),
                    }
                } else {
                    ObservationStatus::Complete
                },
                entries: BTreeMap::from([(
                    "/workspace".into(),
                    FileEntry {
                        kind: FileKind::Directory,
                        mode: 0o755,
                        uid: 10001,
                        gid: 10001,
                        size: 0,
                        sha256: None,
                        link_target: None,
                    },
                )]),
            })
            .collect())
    }
    fn stop(&mut self, _: &OwnedResource) -> Result<()> {
        self.stopped += 1;
        Ok(())
    }
    fn remove(&mut self, _: &OwnedResource) -> Result<()> {
        self.removed += 1;
        Ok(())
    }
}
fn run(scope: PluginScope, fault: &str) -> (RunResult, ReplayEnvironment) {
    let t = fixture::project();
    let recipe = recipe::parse_bytes(
        &t.path().canonicalize().unwrap().join("recipe.toml"),
        &fixture::bytes(&fixture::declaration(scope.as_str())),
    )
    .unwrap();
    let mut environment = ReplayEnvironment {
        scope,
        fault: fault.into(),
        calls: vec![],
        created: 0,
        setups: 0,
        removed: 0,
        stopped: 0,
        observations: 0,
        phase: String::new(),
    };
    let options = RunOptions {
        profiles: vec![ProfileName::User],
        scenarios: vec![ScenarioKind::PluginLifecycle],
        output_root: t.path().canonicalize().unwrap().join("runs"),
        ..RunOptions::all(&recipe)
    };
    let result = engine::run_with(
        &recipe,
        options,
        CancellationToken::default(),
        &mut environment,
        |_| {},
    )
    .unwrap();
    let saved = theseus::report::load(result.report_path.parent().unwrap()).unwrap();
    assert_eq!(saved.outcome, result.report.outcome);
    (result, environment)
}
