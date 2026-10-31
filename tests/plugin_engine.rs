#[path = "support/plugin.rs"]
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
#[test]
fn healthy_scoped_lifecycle_uses_one_owned_container_and_runtime_identity() {
    for scope in [PluginScope::User, PluginScope::Project, PluginScope::Local] {
        let (result, environment) = run(scope, "");
        assert_eq!(
            result.report.outcome,
            RunOutcome::Passed,
            "{:#?}",
            result.report.scenarios[0]
        );
        let s = &result.report.scenarios[0];
        let steps: Vec<_> = s.stages.iter().filter_map(|s| s.step).collect();
        assert_eq!(
            steps,
            vec![
                StepName::InstallBase,
                StepName::Upgrade,
                StepName::DisablePlugin,
                StepName::EnablePlugin,
                StepName::Uninstall
            ]
        );
        assert!(environment.calls.iter().all(|r| r.identity.uid == 10001));
        assert_eq!(environment.created, 1);
        assert_eq!(environment.setups, 1);
        assert_eq!(environment.removed, 1);
    }
}
#[test]
fn failures_remain_sticky_and_allow_uninstall() {
    for fault in [
        "install_base",
        "upgrade",
        "disable_plugin",
        "enable_plugin",
        "uninstall",
        "check:client-version",
        "check:base-inventory",
        "check:target-inventory",
        "check:disabled-inventory",
        "check:enabled-inventory",
        "check:removed-inventory",
        "check:base-components",
        "check:target-components",
        "check:base-preserved",
        "check:target-preserved",
        "check:disabled-preserved",
        "check:enabled-preserved",
        "check:removed-preserved",
        "duplicate",
        "wrong-scope",
        "wrong-version",
        "invalid-json",
        "duplicate-key",
        "preservation-loss",
        "companion-mutation",
        "hook-marker",
        "invalid-utf8",
    ] {
        let (result, environment) = run(PluginScope::User, fault);
        assert_eq!(result.report.outcome, RunOutcome::Failed, "{fault}");
        assert!(
            environment.calls.iter().any(|c| c.phase == "uninstall"),
            "{fault}"
        );
        assert_eq!(environment.removed, 1);
        if fault == "upgrade" {
            assert!(
                !environment
                    .calls
                    .iter()
                    .any(|c| c.phase == "disable_plugin" || c.phase == "enable_plugin")
            );
        }
    }
}
#[test]
fn interrupted_commands_stop_before_later_mutation_and_cleanup() {
    for phase in [
        "install_base",
        "upgrade",
        "disable_plugin",
        "enable_plugin",
        "uninstall",
        "check:base-inventory",
        "check:target-inventory",
        "check:disabled-inventory",
        "check:enabled-inventory",
        "check:removed-inventory",
    ] {
        for mode in ["timeout", "cancel"] {
            let fault = format!("{mode}:{phase}");
            let (result, environment) = run(PluginScope::User, &fault);
            assert_eq!(
                result.report.outcome,
                if mode == "cancel" {
                    RunOutcome::Cancelled
                } else {
                    RunOutcome::Incomplete
                },
                "{fault}"
            );
            assert_eq!(environment.calls.last().unwrap().phase, phase);
            assert_eq!(environment.stopped, 1);
            assert_eq!(environment.removed, 1);
        }
    }
}
#[test]
fn unhealthy_observation_and_truncated_json_block_mutations() {
    for fault in ["observation", "truncated"] {
        let (result, environment) = run(PluginScope::User, fault);
        assert_eq!(result.report.outcome, RunOutcome::Incomplete, "{fault}");
        assert!(
            !environment
                .calls
                .iter()
                .any(|c| c.phase == "disable_plugin")
        );
        assert_eq!(environment.removed, 1);
    }
}
#[test]
fn offline_plugin_proof_cannot_pass_missing_duplicate_or_contradictory_evidence() {
    let (result, _) = run(PluginScope::User, "");
    for fault in [
        "missing-stage",
        "duplicate-stage",
        "reorder",
        "missing-json",
        "client-version",
        "command-exit",
        "missing-file",
        "duplicate-file",
        "orphan-contract",
        "scope",
        "component-proof",
        "wrong-checkpoint",
        "duplicate-check",
        "missing-contract",
        "schema6",
    ] {
        let mut report = result.report.clone();
        let s = &mut report.scenarios[0];
        match fault {
            "missing-stage" => {
                s.stages.remove(1);
            }
            "duplicate-stage" => s.stages.push(s.stages[1].clone()),
            "reorder" => s.stages.swap(1, 4),
            "missing-json" => {
                s.stages
                    .iter_mut()
                    .flat_map(|s| &mut s.checks)
                    .find(|c| c.id == "base-inventory")
                    .unwrap()
                    .json_expectation = None
            }
            "client-version" => s.plugin.as_mut().unwrap().client_version = "other".into(),
            "command-exit" => {
                s.stages
                    .iter_mut()
                    .flat_map(|s| &mut s.commands)
                    .find(|c| c.phase == "check:base-inventory")
                    .unwrap()
                    .exit_status = Some(1)
            }
            "missing-file" => report.artifacts[0].files.clear(),
            "duplicate-file" => {
                let f = report.artifacts[0].files[0].clone();
                report.artifacts[0].files.push(f);
            }
            "orphan-contract" => s.scenario = ScenarioKind::Clean,
            "scope" => s.plugin.as_mut().unwrap().scope = PluginScope::Local,
            "component-proof" => {
                s.stages
                    .iter_mut()
                    .flat_map(|s| &mut s.checks)
                    .find(|c| c.id == "base-components")
                    .unwrap()
                    .command_expectation = None
            }
            "wrong-checkpoint" => {
                s.stages
                    .iter_mut()
                    .flat_map(|s| &mut s.checks)
                    .find(|c| c.id == "base-inventory")
                    .unwrap()
                    .checkpoint = Checkpoint::TargetInstalled
            }
            "duplicate-check" => {
                let st = s
                    .stages
                    .iter_mut()
                    .find(|st| st.checkpoint == Some(Checkpoint::BaseInstalled))
                    .unwrap();
                st.checks.push(st.checks[0].clone());
            }
            "missing-contract" => s.plugin = None,
            "schema6" => report.schema_version = 6,
            _ => unreachable!(),
        };
        report.recompute_outcome();
        if fault != "schema6" {
            assert_ne!(report.outcome, RunOutcome::Passed, "{fault}");
        }
        let t = tempfile::tempdir().unwrap();
        let path = t.path().canonicalize().unwrap().join("report.json");
        std::fs::write(&path, serde_json::to_vec(&report).unwrap()).unwrap();
        if let Ok(loaded) = theseus::report::load(t.path()) {
            assert_ne!(loaded.outcome, RunOutcome::Passed, "{fault}");
        }
    }
}
#[test]
fn all_plugin_checkpoint_names_compare_and_source_failure_takes_precedence() {
    use theseus::comparison::{self, SourceReport, SuiteManifest};
    let (result, _) = run(PluginScope::User, "");
    let manifest:SuiteManifest=toml::from_str("schema_version=1\nname='plugins'\nbaseline='a'\nprofiles=['user']\nscenarios=['plugin_lifecycle']\nstages=['base_installed','target_installed','plugin_disabled','plugin_enabled','uninstalled']\n[[variants]]\nid='a'\nrecipe='a.toml'\n[[variants]]\nid='b'\nrecipe='b.toml'\n[[scopes]]\nid='workspace'\nprofile='user'\npath='/workspace'\n").unwrap();
    comparison::validate_manifest(&manifest).unwrap();
    let mut sources = vec![
        SourceReport {
            variant_id: "a".into(),
            report: Some(result.report.clone()),
            error: None,
        },
        SourceReport {
            variant_id: "b".into(),
            report: Some(result.report),
            error: None,
        },
    ];
    let derived = comparison::derive(&manifest, &sources);
    assert_eq!(derived.outcome, RunOutcome::Passed);
    assert_eq!(derived.cells.len(), 5);
    assert!(
        derived
            .cells
            .iter()
            .all(|c| c.comparison.status == comparison::ComparisonStatus::Equivalent)
    );
    sources[1].report.as_mut().unwrap().scenarios[0].stages[1].outcome = StageOutcome::Failed;
    assert_eq!(
        comparison::derive(&manifest, &sources).outcome,
        RunOutcome::Failed
    );
}
#[test]
fn offline_plugin_report_rejects_runtime_identity_and_forwarding_contradictions() {
    let (result, _) = run(PluginScope::User, "");
    for fault in [
        "uid",
        "gid",
        "home",
        "forwarding",
        "network",
        "orphan-command",
        "missing-final",
    ] {
        let mut report = result.report.clone();
        let s = &mut report.scenarios[0];
        let cmd = &mut s
            .stages
            .iter_mut()
            .flat_map(|s| &mut s.commands)
            .find(|c| c.phase == "upgrade")
            .unwrap();
        match fault {
            "uid" => cmd.identity.uid = 0,
            "gid" => cmd.identity.gid = 0,
            "home" => cmd.identity.home = "/root".into(),
            "forwarding" => cmd.forwarded_environment.push("TOKEN".into()),
            "network" => report.network = recipe::Network::Bridge,
            "orphan-command" => {
                let mut c = cmd.clone();
                c.id = "orphan".into();
                s.stages.last_mut().unwrap().commands.push(c);
            }
            "missing-final" => {
                s.stages.retain(|s| s.name != "final");
            }
            _ => unreachable!(),
        };
        report.recompute_outcome();
        assert_ne!(report.outcome, RunOutcome::Passed, "{fault}");
    }
}
#[test]
fn component_literal_proof_requires_its_complete_log_and_shell_invocation() {
    let (result, _) = run(PluginScope::User, "");
    for fault in [
        "component-truncated",
        "component-missing-log",
        "component-short-log",
        "component-argv",
        "mutation-argv",
    ] {
        let mut report = result.report.clone();
        let command = report.scenarios[0]
            .stages
            .iter_mut()
            .flat_map(|st| &mut st.commands)
            .find(|c| {
                c.phase
                    == if fault == "mutation-argv" {
                        "upgrade"
                    } else {
                        "check:base-components"
                    }
            })
            .unwrap();
        match fault {
            "component-truncated" => command.stdout.as_mut().unwrap().truncated = true,
            "component-missing-log" => command.stdout = None,
            "component-short-log" => command.stdout.as_mut().unwrap().bytes = 1,
            "component-argv" | "mutation-argv" => command.argv = vec!["/bin/true".into()],
            _ => unreachable!(),
        };
        report.recompute_outcome();
        assert_ne!(report.outcome, RunOutcome::Passed, "{fault}");
    }
}

#[test]
fn offline_plugin_bindings_accept_equivalent_scalar_literals() {
    let (result, _) = run(PluginScope::User, "");
    for field in ["client", "id", "scope", "version", "enabled"] {
        let mut report = result.report.clone();
        for check in report.scenarios[0]
            .stages
            .iter_mut()
            .flat_map(|s| &mut s.checks)
        {
            if check.id == "client-version" && field == "client" {
                let JsonAssertion::Equals { equals_json, .. } =
                    &mut check.json_expectation.as_mut().unwrap().assertions[0]
                else {
                    panic!()
                };
                *equals_json = " \n\"2.1.312 (Claude Code)\"\t ".into();
            } else if check.id.ends_with("-inventory") {
                let JsonAssertion::ArrayMatch {
                    identity, values, ..
                } = &mut check.json_expectation.as_mut().unwrap().assertions[0]
                else {
                    panic!()
                };
                if field == "id" {
                    identity[0].equals_json = r#" "probe\u0040market" "#.into();
                }
                for value in values {
                    if value.pointer == format!("/{field}") {
                        value.equals_json = format!(" \n{}\t ", value.equals_json);
                    }
                }
            }
        }
        report.recompute_outcome();
        assert_eq!(report.outcome, RunOutcome::Passed, "{field}");
        let t = tempfile::tempdir().unwrap();
        std::fs::write(
            t.path().canonicalize().unwrap().join("report.json"),
            serde_json::to_vec(&report).unwrap(),
        )
        .unwrap();
        assert_eq!(
            theseus::report::load(t.path()).unwrap().outcome,
            RunOutcome::Passed,
            "{field}"
        );
    }
}

#[test]
fn offline_plugin_bindings_reject_changed_scalar_values_and_types() {
    let (result, _) = run(PluginScope::User, "");
    for (field, literals) in [
        ("client", [r#""other""#, "true"]),
        ("id", [r#""other@market""#, "null"]),
        ("scope", [r#""local""#, "false"]),
        ("version", [r#""2""#, "1"]),
        ("enabled", ["false", r#""true""#]),
    ] {
        for literal in literals {
            let mut report = result.report.clone();
            let check = report.scenarios[0]
                .stages
                .iter_mut()
                .flat_map(|s| &mut s.checks)
                .find(|c| {
                    c.id == if field == "client" {
                        "client-version"
                    } else {
                        "base-inventory"
                    }
                })
                .unwrap();
            let proof = check.json_expectation.as_mut().unwrap();
            if field == "client" {
                let JsonAssertion::Equals { equals_json, .. } = &mut proof.assertions[0] else {
                    panic!()
                };
                *equals_json = literal.into();
                let JsonAssertionObservation::Equals { actual } = &mut proof.observations[0] else {
                    panic!()
                };
                *actual = JsonValueObservation::Scalar {
                    value: serde_json::from_str(literal).unwrap(),
                };
            } else {
                let JsonAssertion::ArrayMatch {
                    identity, values, ..
                } = &mut proof.assertions[0]
                else {
                    panic!()
                };
                if field == "id" {
                    identity[0].equals_json = literal.into();
                } else {
                    let i = values
                        .iter()
                        .position(|v| v.pointer == format!("/{field}"))
                        .unwrap();
                    values[i].equals_json = literal.into();
                    let JsonAssertionObservation::ArrayMatch { values, .. } =
                        &mut proof.observations[0]
                    else {
                        panic!()
                    };
                    values[i] = JsonValueObservation::Scalar {
                        value: serde_json::from_str(literal).unwrap(),
                    };
                }
            }
            // Even self-consistent generic proof must bind the plugin contract.
            assert!(proof.verified_pass(), "{field}: {literal}");
            report.recompute_outcome();
            assert_ne!(report.outcome, RunOutcome::Passed, "{field}: {literal}");
            let t = tempfile::tempdir().unwrap();
            std::fs::write(
                t.path().canonicalize().unwrap().join("report.json"),
                serde_json::to_vec(&report).unwrap(),
            )
            .unwrap();
            if let Ok(loaded) = theseus::report::load(t.path()) {
                assert_ne!(loaded.outcome, RunOutcome::Passed, "{field}: {literal}");
            }
        }
    }
}
