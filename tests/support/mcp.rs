#![allow(dead_code)]
//! SYNTHETIC finite native-document fixtures, never vendor acceptance.
use serde_json::{Value, json};
use theseus::recipe::{self, ValidatedRecipe};
pub struct Fixture {
    pub root: tempfile::TempDir,
}
impl Fixture {
    pub fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("target/client")).unwrap();
        std::fs::create_dir(root.path().join("base")).unwrap();
        std::fs::write(
            root.path().join("target/client/claude"),
            b"synthetic client",
        )
        .unwrap();
        Self { root }
    }
    pub fn validate(&self, value: &Value) -> anyhow::Result<ValidatedRecipe> {
        recipe::parse_bytes(
            &self.root.path().join("recipe.toml"),
            toml::to_string(value)?.as_bytes(),
        )
    }
}
pub fn eq(pointer: &str, value: Value) -> Value {
    json!({"kind":"equals","pointer":pointer,"equals_json":value.to_string()})
}
pub fn absent(pointer: &str) -> Value {
    json!({"kind":"absent","pointer":pointer})
}
pub fn declaration() -> Value {
    let mut value: Value = serde_json::to_value(
        toml::from_str::<toml::Value>(include_str!("../fixtures/recipes/v2_clean.toml")).unwrap(),
    )
    .unwrap();
    value["schema_version"] = 8.into();
    let mut checks = vec![
        json!({"id":"client-version","checkpoint":"configured","kind":"json_command","script":"version","expected_exit":0,"assertions":[eq("/version",json!("2.1.312 (Claude Code)"))]}),
    ];
    let mut checkpoints = serde_json::Map::new();
    for (cp, active) in [("configured", true), ("uninstalled", false)] {
        let registration = format!("{cp}-registration");
        let preserved = format!("{cp}-preservation");
        let activation = format!("{cp}-activation");
        let mut assertions = vec![
            absent("/client/projects/~1workspace/mcpServers/probe"),
            absent("/project/mcpServers/probe"),
        ];
        if active {
            assertions.extend([
                eq("/client/mcpServers/probe/command", json!("/bin/sh")),
                eq(
                    "/client/mcpServers/probe/args/0",
                    json!("/inputs/target/server.sh"),
                ),
                absent("/client/mcpServers/probe/args/1"),
            ]);
        } else {
            assertions.push(absent("/client/mcpServers/probe"));
        }
        checks.push(json!({"id":registration,"checkpoint":cp,"kind":"json_command","script":"registration","expected_exit":0,"assertions":assertions}));
        checks.push(json!({"id":preserved,"checkpoint":cp,"kind":"json_command","script":"preservation","expected_exit":0,"assertions":[eq("/client/mcpServers/companion/command",json!("/bin/true")),absent("/client/mcpServers/companion/args/0"),absent("/client/projects/~1workspace/mcpServers/companion"),absent("/project/mcpServers/companion"),eq("/client/hasCompletedOnboarding",json!(true))]}));
        checks.push(json!({"id":activation,"checkpoint":cp,"kind":"json_command","script":"health","expected_exit":0,"assertions":[eq("/status",json!(0)),eq("/alias",json!("probe")),eq("/source",json!(if active {"/bin/sh /inputs/target/server.sh"} else {""})),eq("/connected",json!(active))]}));
        let probe = if active {
            let id = format!("{cp}-probe");
            checks.push(json!({"id":id,"checkpoint":cp,"kind":"mcp_stdio","probe":{"argv":["/bin/sh","/inputs/target/server.sh"],"protocol":"2026-07-28","server_name":"fixture","server_version":"2","tools":["hello_v2"],"timeout_seconds":3}}));
            Some(id)
        } else {
            None
        };
        let mut contract = json!({"state":if active {"active"}else{"removed"},"registration":registration,"preservation":[preserved],"activation":activation});
        if let Some(probe) = probe {
            contract["probe"] = probe.into();
        }
        checkpoints.insert(cp.into(), contract);
    }
    value["profiles"]["user"]["checks"] = checks.into();
    value["profiles"]["user"]["mcp"] = json!({"adapter":"claude-code-2.1.312","delivery":"standalone","client_claim":"native_health","server_id":"probe","client_alias":"probe","scope":"user","client_version":"2.1.312 (Claude Code)","client_check":"client-version","client_artifact":"client/claude","checkpoints":checkpoints});
    value
}
use std::collections::BTreeMap;
use theseus::{
    engine::{self, CancellationToken, RunOptions, RunResult},
    mcp::*,
    model::*,
    report::ReportStore,
    runner::{CommandRequest, Environment, ExecutedCommand, ExecutedMcp, PreparedEnvironment},
    snapshot::Snapshot,
};
struct SyntheticEnvironment {
    fault: String,
    calls: Vec<String>,
    unavailable: bool,
    plugin: bool,
    upgrade: bool,
    phase: String,
}
fn record(r: CommandRequest, stdout: &str, store: &ReportStore) -> anyhow::Result<ExecutedCommand> {
    let mut record = CommandRecord::new(r.id, r.phase);
    record.argv = r.argv;
    record.identity = r.identity;
    record.environment = r.env;
    record.forwarded_environment = r.pass_env;
    record.exit_status = Some(0);
    record.outcome = CommandOutcome::Succeeded;
    let path = format!("logs/{}.stdout", record.id);
    std::io::Write::write_all(&mut store.create_log(&path)?, stdout.as_bytes())?;
    record.stdout = Some(LogReference {
        path,
        bytes: stdout.len() as u64,
        truncated: false,
    });
    Ok(ExecutedCommand {
        record,
        stdout: stdout.into(),
        stdout_complete: true,
        stdout_utf8: true,
    })
}
impl Environment for SyntheticEnvironment {
    fn prepare(
        &mut self,
        r: &ValidatedRecipe,
        _: bool,
        _: &CancellationToken,
    ) -> anyhow::Result<PreparedEnvironment> {
        use sha2::{Digest, Sha256};
        let bytes = std::fs::read(r.target_artifact.join("client/claude"))?;
        Ok(PreparedEnvironment {
            image_id: "sha256:SYNTHETIC".into(),
            artifacts: if self.fault == "missing-artifact" {
                vec![]
            } else {
                vec![ArtifactDigest {
                    name: "target".into(),
                    sha256: "a".repeat(64),
                    files: vec![ArtifactFileDigest {
                        path: "client/claude".into(),
                        size: bytes.len() as u64,
                        sha256: format!("{:x}", Sha256::digest(bytes)),
                    }],
                }]
            },
        })
    }
    fn create(&mut self, _: &str, _: &str, _: &ValidatedRecipe) -> anyhow::Result<OwnedResource> {
        self.calls.push("create".into());
        Ok(OwnedResource {
            container_id: "owned-SYNTHETIC".into(),
            labels: BTreeMap::new(),
            removed: false,
        })
    }
    fn setup(
        &mut self,
        _: &OwnedResource,
        _: &ValidatedRecipe,
        _: &CancellationToken,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    fn execute(
        &mut self,
        _: &OwnedResource,
        r: CommandRequest,
        s: &ReportStore,
        _: &CancellationToken,
    ) -> anyhow::Result<ExecutedCommand> {
        assert!(!self.unavailable, "no guest execution after unavailability");
        self.calls.push(r.phase.clone());
        if !r.phase.starts_with("check:") {
            self.phase = r.phase.clone();
        }
        let removed = self.phase == "uninstall";
        let disabled = self.phase == "disable_plugin";
        let version = if self.phase == "install_base" || (self.upgrade && self.phase == "configure")
        {
            "1"
        } else {
            "2"
        };
        let plugins = if removed {
            json!([])
        } else {
            json!([{"id":"probe@market","scope":"user","enabled":!disabled,"version":version,"readFromFolder":"/market/probe","installPath":format!("/cache/{version}"),"folderVersion":version,"mcpServers":{"probe":{"command":"/bin/sh","args":["${CLAUDE_PLUGIN_ROOT}/server.sh"]}}}])
        };
        let mut doc = json!({"client":{"mcpServers":{"companion":{"command":"/bin/true","args":[]}},"projects":{"/workspace":{"mcpServers":{}}},"hasCompletedOnboarding":true},"project":{"mcpServers":{}}});
        if !removed && !self.plugin {
            doc["client"]["mcpServers"]["probe"] = json!({"command":"/bin/sh","args":[if self.upgrade&&self.phase=="configure"{"/inputs/base/server.sh"}else{"/inputs/target/server.sh"}]});
        }
        if self.plugin {
            doc["plugins"] = plugins.clone();
        }
        if self.fault == "wrong-source" && r.phase == "check:target_installed-registration" {
            doc["plugins"][0]["readFromFolder"] = "/cache/2".into();
        }
        if self.fault == "plugin-duplicate" && r.phase == "check:target_installed-registration" {
            let other = doc["plugins"][0].clone();
            doc["plugins"].as_array_mut().unwrap().push(other);
        }
        if r.phase == "check:configured-registration" {
            match self.fault.as_str() {
                "wrong-argv" => doc["client"]["mcpServers"]["probe"]["args"][0] = "/wrong".into(),
                "duplicate" | "wrong-scope" => {
                    doc["project"]["mcpServers"]["probe"] =
                        doc["client"]["mcpServers"]["probe"].clone();
                    if self.fault == "wrong-scope" {
                        doc["client"]["mcpServers"]
                            .as_object_mut()
                            .unwrap()
                            .remove("probe");
                    }
                }
                _ => {}
            }
        }
        if self.fault == "dropped-companion" {
            doc["client"]["mcpServers"]
                .as_object_mut()
                .unwrap()
                .remove("companion");
        }
        let out = if r.phase == "check:mcp-client-version" || r.phase == "check:client-version" {
            json!({"version":"2.1.312 (Claude Code)"})
        } else if r.phase.ends_with("-activation") {
            json!({"status":0,"alias":if self.plugin {"plugin:probe:probe"}else{"probe"},"source":if removed||disabled{""}else if self.plugin {"/bin/sh /market/probe/server.sh"}else if self.upgrade&&self.phase=="configure"{"/bin/sh /inputs/base/server.sh"}else{"/bin/sh /inputs/target/server.sh"},"connected":!removed&&!disabled})
        } else if r.phase.ends_with("-inventory") {
            plugins
        } else if r.phase.ends_with("-preserved") {
            json!({"unrelated":{"theme":"dark"},"companions":[{"id":"companion@market","version":"1","enabled":true,"installPath":"/cache/companion"}]})
        } else {
            doc
        };
        if r.phase.ends_with("-components") {
            return record(
                r,
                "source/cache manifests skills legacy-command manual-hook verified",
                s,
            );
        }
        if self.fault == "ordinary-health" && r.phase == "check:configured-activation" {
            return record(
                r,
                "probe: /bin/sh /inputs/target/server.sh - ✔ Connected",
                s,
            );
        }
        record(r, &out.to_string(), s)
    }
    fn probe_mcp(
        &mut self,
        _: &OwnedResource,
        r: CommandRequest,
        probe: McpProbe,
        s: &ReportStore,
        _: &CancellationToken,
    ) -> anyhow::Result<ExecutedMcp> {
        assert!(!self.unavailable);
        self.calls.push(r.phase.clone());
        let legacy = probe.protocol == McpProtocol::Legacy20251125;
        let mut session = McpSession::new(probe)?;
        session.written(session.pending_write().len())?;
        let version = if (self.plugin && self.phase == "install_base")
            || (self.upgrade && self.phase == "configure")
        {
            "1"
        } else {
            "2"
        };
        let response = json!({"jsonrpc":"2.0","id":"theseus-discover-1","result":{"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"public","_meta":{"io.modelcontextprotocol/serverInfo":{"name":"fixture","version":version}}}});
        let response = if legacy {
            json!({"jsonrpc":"2.0","id":"theseus-initialize-1","result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":version}}})
        } else {
            response
        };
        session.receive(format!("{response}\n").as_bytes());
        session.written(session.pending_write().len())?;
        if legacy {
            session.written(session.pending_write().len())?;
        }
        let tool = if self.fault == "wrong-tools" {
            "wrong"
        } else if version == "1" {
            "hello_v1"
        } else {
            "hello_v2"
        };
        let result = if legacy {
            json!({"tools":[{"name":tool,"inputSchema":{"type":"object"}}]})
        } else {
            json!({"resultType":"complete","tools":[{"name":tool,"inputSchema":{"type":"object"}}],"ttlMs":0,"cacheScope":"public"})
        };
        session
            .receive(format!("{}\n", json!({"jsonrpc":"2.0","id":1,"result":result})).as_bytes());
        session.eof();
        let abort = self.fault == "configured-probe-ignore-eof";
        let evidence = session.finish(if abort {
            McpShutdown::ForcedOwnedRemoval
        } else {
            McpShutdown::Graceful
        });
        let mut command = record(r, &evidence.response_lines.concat(), s)?;
        // Docker supplies public runtime defaults rather than host environment.
        command.record.environment = BTreeMap::from([
            ("HOME".into(), "/home/theseus".into()),
            ("XDG_CONFIG_HOME".into(), "/home/theseus/.config".into()),
            ("XDG_DATA_HOME".into(), "/home/theseus/.local/share".into()),
            ("XDG_CACHE_HOME".into(), "/home/theseus/.cache".into()),
            (
                "PATH".into(),
                "/home/theseus/.local/bin:/usr/local/bin:/usr/bin:/bin".into(),
            ),
            ("USER".into(), "theseus".into()),
            ("LOGNAME".into(), "theseus".into()),
        ]);
        command.record.stdout.as_mut().unwrap().bytes = evidence.stdout_bytes;
        command.record.stderr = Some(LogReference {
            path: "logs/stderr".into(),
            bytes: 0,
            truncated: false,
        });
        if abort {
            self.unavailable = true;
            self.calls.push("stop".into());
            command.record.exit_status = None;
            command.record.outcome = CommandOutcome::Error;
            command.record.error = Some("SYNTHETIC forced owned removal".into());
        }
        Ok(ExecutedMcp {
            command,
            evidence,
            container_available: !abort,
        })
    }
    fn observe(
        &mut self,
        _: &OwnedResource,
        roots: &[String],
        _: recipe::Limits,
        _: &CancellationToken,
    ) -> anyhow::Result<Vec<Snapshot>> {
        assert!(!self.unavailable);
        self.calls.push("observe".into());
        Ok(roots.iter().map(Snapshot::missing).collect())
    }
    fn stop(&mut self, _: &OwnedResource) -> anyhow::Result<()> {
        self.calls.push("stop".into());
        Ok(())
    }
    fn remove(&mut self, _: &OwnedResource) -> anyhow::Result<()> {
        self.calls.push("remove".into());
        Ok(())
    }
}
pub fn run(fault: &str) -> (RunResult, Vec<String>) {
    let f = Fixture::new();
    let mut value = if fault.starts_with("plugin:") {
        plugin_declaration()
    } else if fault.starts_with("upgrade:") {
        upgrade_declaration()
    } else {
        declaration()
    };
    if fault == "ordinary-health" {
        let check = value["profiles"]["user"]["checks"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|c| c["id"] == "configured-activation")
            .unwrap();
        *check = json!({"id":"configured-activation","checkpoint":"configured","kind":"command","script":"native-health","expected_exit":0,"stdout_contains":"probe: /bin/sh /inputs/target/server.sh - ✔ Connected"});
    }
    if fault.ends_with(":legacy") {
        for c in value["profiles"]["user"]["checks"].as_array_mut().unwrap() {
            if c["kind"] == "mcp_stdio" {
                c["probe"]["protocol"] = "2025-11-25".into();
            }
        }
    }
    let recipe = f.validate(&value).unwrap();
    if fault == "client-file-missing" {
        std::fs::remove_file(f.root.path().join("target/client/claude")).unwrap();
    }
    let mut env = SyntheticEnvironment {
        fault: fault
            .strip_prefix("plugin:")
            .or_else(|| fault.strip_prefix("upgrade:"))
            .unwrap_or(fault)
            .into(),
        plugin: fault.starts_with("plugin:"),
        upgrade: fault.starts_with("upgrade:"),
        phase: String::new(),
        calls: vec![],
        unavailable: false,
    };
    let options = RunOptions {
        output_root: f.root.path().join("runs"),
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
    let saved = theseus::report::load(result.report_path.parent().unwrap()).unwrap();
    assert_eq!(saved.outcome, result.report.outcome);
    (result, env.calls)
}
#[path = "plugin.rs"]
mod plugin_fixture;
pub fn plugin_declaration() -> Value {
    let mut value = plugin_fixture::declaration("user");
    value["schema_version"] = 8.into();
    let mut mcp = declaration()["profiles"]["user"]["mcp"].clone();
    mcp["delivery"] = "plugin".into();
    mcp["client_alias"] = "plugin:probe:probe".into();
    mcp["client_check"] = "mcp-client-version".into();
    let mut checkpoints = serde_json::Map::new();
    let checks = value["profiles"]["user"]["checks"].as_array_mut().unwrap();
    checks.push(json!({"id":"mcp-client-version","checkpoint":"base_installed","kind":"json_command","script":"version","expected_exit":0,"assertions":[eq("/version",json!("2.1.312 (Claude Code)"))]}));
    for (cp, state) in [
        ("base_installed", "active"),
        ("target_installed", "active"),
        ("plugin_disabled", "disabled"),
        ("plugin_enabled", "active"),
        ("uninstalled", "removed"),
    ] {
        let removed = state == "removed";
        let active = state == "active";
        let reg = format!("{cp}-registration");
        let preserved = format!("{cp}-preservation");
        let health = format!("{cp}-activation");
        let probe = format!("{cp}-probe");
        let mut assertions = vec![
            absent("/client/mcpServers/probe"),
            absent("/project/mcpServers/probe"),
            absent("/client/projects/~1workspace/mcpServers/probe"),
            json!({"kind":"array_match","pointer":"/plugins","identity":[{"pointer":"/id","equals_json":"\"probe@market\""}],"count":if removed {0}else{1},"values":if removed {vec![]}else{vec![json!({"pointer":"/scope","equals_json":"\"user\""}),json!({"pointer":"/enabled","equals_json":active.to_string()})]}}),
        ];
        if !removed {
            assertions.extend([
                eq("/plugins/0/id", json!("probe@market")),
                eq("/plugins/0/readFromFolder", json!("/market/probe")),
                eq("/plugins/0/mcpServers/probe/command", json!("/bin/sh")),
                eq(
                    "/plugins/0/mcpServers/probe/args/0",
                    json!("${CLAUDE_PLUGIN_ROOT}/server.sh"),
                ),
                absent("/plugins/0/mcpServers/probe/args/1"),
            ]);
        }
        checks.push(json!({"id":reg,"checkpoint":cp,"kind":"json_command","script":"registration","expected_exit":0,"assertions":assertions}));
        let mut preservation = declaration()["profiles"]["user"]["checks"][2].clone();
        preservation["id"] = preserved.clone().into();
        preservation["checkpoint"] = cp.into();
        checks.push(preservation);
        checks.push(json!({"id":health,"checkpoint":cp,"kind":"json_command","script":"health","expected_exit":0,"assertions":[eq("/status",json!(0)),eq("/alias",json!("plugin:probe:probe")),eq("/source",json!(if active{"/bin/sh /market/probe/server.sh"}else{""})),eq("/connected",json!(active))]}));
        let mut c = json!({"state":state,"registration":reg,"preservation":[preserved],"activation":health});
        if active {
            c["probe"] = probe.clone().into();
            checks.push(json!({"id":probe,"checkpoint":cp,"kind":"mcp_stdio","probe":{"argv":["/bin/sh","/market/probe/server.sh"],"protocol":"2026-07-28","server_name":"fixture","server_version":if cp=="base_installed"{"1"}else{"2"},"tools":[if cp=="base_installed"{"hello_v1"}else{"hello_v2"}],"timeout_seconds":3}}));
        }
        checkpoints.insert(cp.into(), c);
    }
    mcp["checkpoints"] = checkpoints.into();
    value["profiles"]["user"]["mcp"] = mcp;
    value
}
pub fn upgrade_declaration() -> Value {
    let mut value = declaration();
    value["scenarios"] = json!(["upgrade"]);
    value["artifacts"]["base"] = "base".into();
    let profile = &mut value["profiles"]["user"];
    profile["steps"]
        .as_object_mut()
        .unwrap()
        .remove("install_target");
    profile["steps"]["install_base"] = json!({"script":"install_base"});
    profile["steps"]["upgrade"] = json!({"script":"upgrade"});
    let original = profile["checks"].as_array().unwrap().clone();
    let target = profile["mcp"]["checkpoints"]["configured"].clone();
    profile["mcp"]["checkpoints"]["target_installed"] = serde_json::from_str::<Value>(
        &target.to_string().replace("configured", "target_installed"),
    )
    .unwrap();
    let checks = profile["checks"].as_array_mut().unwrap();
    for check in checks
        .iter_mut()
        .filter(|c| c["checkpoint"] == "configured")
    {
        *check = serde_json::from_str::<Value>(
            &check
                .to_string()
                .replace("/inputs/target/server.sh", "/inputs/base/server.sh"),
        )
        .unwrap();
        if check["kind"] == "mcp_stdio" {
            check["probe"]["server_version"] = "1".into();
            check["probe"]["tools"] = json!(["hello_v1"]);
        }
    }
    for check in original
        .iter()
        .filter(|c| c["checkpoint"] == "configured" && c["id"] != "client-version")
    {
        checks.push(
            serde_json::from_str::<Value>(
                &check.to_string().replace("configured", "target_installed"),
            )
            .unwrap(),
        );
    }
    value
}
