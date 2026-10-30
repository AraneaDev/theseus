#[test]
fn mcp_expectation_has_explicit_protocol_and_no_host_forwarding() {
    let check: theseus::recipe::Check = toml::from_str(
        r#"
id = "discovery"
checkpoint = "configured"
kind = "mcp_stdio"
[probe]
argv = ["/inputs/target/server"]
protocol = "2026-07-28"
server_name = "fixture"
server_version = "2"
tools = ["hello_v2"]
timeout_seconds = 15
"#,
    )
    .unwrap();
    let theseus::recipe::Expectation::McpStdio { probe } = check.expectation else {
        panic!()
    };
    assert!(probe.validate().is_ok());
    assert!(probe.env.is_empty());
}

use std::collections::BTreeMap;
use theseus::{
    mcp::*,
    model::*,
    runner::{ExecutedCommand, ExecutedMcp},
};
fn observation(tool: &str) -> (theseus::recipe::Check, ExecutedMcp) {
    let probe = McpProbe {
        argv: vec!["/inputs/target/server".into()],
        env: BTreeMap::new(),
        timeout_seconds: 3,
        protocol: McpProtocol::Modern20260728,
        server_name: "fixture".into(),
        server_version: "2".into(),
        tools: vec!["hello_v2".into()],
    };
    let mut session = McpSession::new(probe.clone()).unwrap();
    session.written(session.pending_write().len()).unwrap();
    session.receive(b"{\"jsonrpc\":\"2.0\",\"id\":\"theseus-discover-1\",\"result\":{\"resultType\":\"complete\",\"supportedVersions\":[\"2026-07-28\"],\"capabilities\":{\"tools\":{}},\"ttlMs\":0,\"cacheScope\":\"public\",\"_meta\":{\"io.modelcontextprotocol/serverInfo\":{\"name\":\"fixture\",\"version\":\"2\"}}}}\n");
    session.written(session.pending_write().len()).unwrap();
    session.receive(format!("{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"resultType\":\"complete\",\"tools\":[{{\"name\":\"{tool}\",\"inputSchema\":{{\"type\":\"object\"}}}}],\"ttlMs\":0,\"cacheScope\":\"public\"}}}}\n").as_bytes());
    session.eof();
    let evidence = session.finish(McpShutdown::Graceful);
    let mut record = CommandRecord::new("probe", "check:discovery");
    record.identity = ExecutionIdentity::runtime();
    record.argv = probe.argv.clone();
    // The Docker request preparation contract supplies these public defaults.
    record.environment = BTreeMap::from([
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
    record.exit_status = Some(0);
    record.outcome = CommandOutcome::Succeeded;
    record.stdout = Some(LogReference {
        path: "logs/out".into(),
        bytes: evidence.stdout_bytes,
        truncated: false,
    });
    record.stderr = Some(LogReference {
        path: "logs/err".into(),
        bytes: 0,
        truncated: false,
    });
    let check = theseus::recipe::Check {
        id: "discovery".into(),
        checkpoint: Checkpoint::Configured,
        expectation: theseus::recipe::Expectation::McpStdio { probe },
    };
    (
        check,
        ExecutedMcp {
            command: ExecutedCommand {
                record,
                stdout: String::new(),
                stdout_complete: true,
                stdout_utf8: true,
            },
            evidence,
            container_available: true,
        },
    )
}
fn stage(check: &theseus::recipe::Check, observed: &ExecutedMcp) -> StageReport {
    let mut stage = StageReport::new("configured");
    stage.outcome = StageOutcome::Passed;
    let mut result = theseus::checks::evaluate_mcp(check, observed);
    result.command_ids = vec![observed.command.record.id.clone()];
    stage.commands.push(observed.command.record.clone());
    stage.checks.push(result);
    stage
}
#[test]
fn actual_inventory_and_acquisition_gaps_have_distinct_outcomes() {
    let (check, mut o) = observation("hello_v2");
    assert_eq!(
        theseus::checks::evaluate_mcp(&check, &o).outcome,
        CheckOutcome::Passed
    );
    o.command.record.exit_status = Some(17);
    assert_eq!(
        theseus::checks::evaluate_mcp(&check, &o).outcome,
        CheckOutcome::Failed
    );
    o.command.record.timed_out = true;
    assert_eq!(
        theseus::checks::evaluate_mcp(&check, &o).outcome,
        CheckOutcome::Incomplete
    );
    let (check, mut o) = observation("wrong");
    assert_eq!(
        theseus::checks::evaluate_mcp(&check, &o).outcome,
        CheckOutcome::Failed
    );
    o.evidence.shutdown = McpShutdown::ForcedOwnedRemoval;
    o.container_available = false;
    let result = theseus::checks::evaluate_mcp(&check, &o);
    assert_eq!(result.outcome, CheckOutcome::Incomplete);
    assert_eq!(
        result.mcp.unwrap().issue.unwrap().kind,
        McpIssueKind::Inventory
    );
}
#[test]
fn saved_success_requires_replayed_response_and_same_stage_command() {
    let (check, o) = observation("hello_v2");
    let original = stage(&check, &o);
    assert_eq!(original.aggregate_outcome(), RunOutcome::Passed);
    for mutation in 0..11 {
        let mut s = original.clone();
        match mutation {
            0 => s.checks[0].mcp = None,
            1 => s.checks[0].mcp.as_mut().unwrap().probe.tools = vec!["wrong".into()],
            2 => {
                let p = s.checks[0].mcp.as_mut().unwrap();
                p.response_lines[1] = p.response_lines[1].replace("hello_v2", "wrong");
            }
            3 => {
                let p = s.checks[0].mcp.as_mut().unwrap();
                p.response_lines[1] = p.response_lines[1].replace("\"id\":1", "\"id\":2");
            }
            4 => s.commands[0].stdout.as_mut().unwrap().truncated = true,
            5 => s.commands[0].exit_status = None,
            6 => s.commands[0].phase = "other".into(),
            7 => s.commands[0].argv.push("extra".into()),
            8 => s.commands[0]
                .environment
                .insert("EXTRA".into(), "value".into())
                .map(|_| ())
                .unwrap_or(()),
            9 => s.commands[0].identity = ExecutionIdentity::root(),
            _ => s.checks[0].mcp_required = false,
        }
        assert_eq!(
            s.aggregate_outcome(),
            RunOutcome::Incomplete,
            "mutation {mutation}"
        );
    }
    let mut s = original;
    s.commands[0].stderr.as_mut().unwrap().truncated = true;
    assert_eq!(s.aggregate_outcome(), RunOutcome::Passed);
}
#[test]
fn typed_probe_is_schema_eight_only_and_roundtrips() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("target")).unwrap();
    let check = "\n[[profiles.user.checks]]\nid='discovery'\ncheckpoint='configured'\nkind='mcp_stdio'\n[profiles.user.checks.probe]\nargv=['/inputs/target/server']\nprotocol='2026-07-28'\nserver_name='fixture'\nserver_version='2'\ntools=['hello_v2']\n";
    let base = include_str!("fixtures/recipes/v2_clean.toml");
    for version in [7, 8] {
        let text =
            base.replace("schema_version = 2", &format!("schema_version = {version}")) + check;
        let result = theseus::recipe::parse_bytes(&dir.path().join("recipe.toml"), text.as_bytes());
        if version == 7 {
            assert!(result.is_err());
        } else {
            let recipe = result.unwrap();
            let text = toml::to_string(&recipe.recipe).unwrap();
            assert!(text.contains("[profiles.user.checks.probe]"));
            assert!(
                theseus::recipe::parse_bytes(&dir.path().join("recipe.toml"), text.as_bytes())
                    .is_ok()
            );
        }
    }
}

#[test]
fn saved_report_gates_mcp_markers_and_revalidates_actual_responses() {
    let (check, observed) = observation("hello_v2");
    let root = tempfile::tempdir().unwrap();
    let store = theseus::report::create(&root.path().join("run")).unwrap();
    let mut report = RunReport::new("run");
    let mut scenario = ScenarioReport::new("user-clean", ProfileName::User, ScenarioKind::Clean);
    scenario.stages.push(stage(&check, &observed));
    report.scenarios.push(scenario);
    store.save(&report).unwrap();
    assert!(theseus::report::load(store.report_path().parent().unwrap()).is_ok());
    report.schema_version = 7;
    std::fs::write(store.report_path(), serde_json::to_vec(&report).unwrap()).unwrap();
    assert!(theseus::report::load(store.report_path().parent().unwrap()).is_err());
    report.schema_version = 8;
    let p = report.scenarios[0].stages[0].checks[0]
        .mcp
        .as_mut()
        .unwrap();
    p.response_lines[1] = p.response_lines[1].replace("hello_v2", "wrong");
    std::fs::write(store.report_path(), serde_json::to_vec(&report).unwrap()).unwrap();
    assert!(theseus::report::load(store.report_path().parent().unwrap()).is_err());
}

struct Unsupported {
    calls: Vec<String>,
    stops: usize,
    removals: usize,
}
impl theseus::runner::Environment for Unsupported {
    fn prepare(
        &mut self,
        _: &theseus::recipe::ValidatedRecipe,
        _: bool,
        _: &theseus::engine::CancellationToken,
    ) -> anyhow::Result<theseus::runner::PreparedEnvironment> {
        Ok(theseus::runner::PreparedEnvironment {
            image_id: "sha256:fixture".into(),
            artifacts: vec![],
        })
    }
    fn create(
        &mut self,
        _: &str,
        _: &str,
        _: &theseus::recipe::ValidatedRecipe,
    ) -> anyhow::Result<OwnedResource> {
        Ok(OwnedResource {
            container_id: "owned".into(),
            labels: BTreeMap::new(),
            removed: false,
        })
    }
    fn setup(
        &mut self,
        _: &OwnedResource,
        _: &theseus::recipe::ValidatedRecipe,
        _: &theseus::engine::CancellationToken,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    fn execute(
        &mut self,
        _: &OwnedResource,
        r: theseus::runner::CommandRequest,
        _: &theseus::report::ReportStore,
        _: &theseus::engine::CancellationToken,
    ) -> anyhow::Result<ExecutedCommand> {
        self.calls.push(r.phase.clone());
        let mut record = CommandRecord::new(r.id, r.phase);
        record.exit_status = Some(0);
        record.outcome = CommandOutcome::Succeeded;
        Ok(ExecutedCommand {
            record,
            stdout: String::new(),
            stdout_complete: true,
            stdout_utf8: true,
        })
    }
    fn observe(
        &mut self,
        _: &OwnedResource,
        roots: &[String],
        _: theseus::recipe::Limits,
        _: &theseus::engine::CancellationToken,
    ) -> anyhow::Result<Vec<theseus::snapshot::Snapshot>> {
        Ok(roots
            .iter()
            .map(theseus::snapshot::Snapshot::missing)
            .collect())
    }
    fn stop(&mut self, _: &OwnedResource) -> anyhow::Result<()> {
        self.stops += 1;
        Ok(())
    }
    fn remove(&mut self, _: &OwnedResource) -> anyhow::Result<()> {
        self.removals += 1;
        Ok(())
    }
}
#[test]
fn unsupported_probe_stops_guest_work_and_marks_unavailable_checks() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("target")).unwrap();
    let mut text = include_str!("fixtures/recipes/v2_clean.toml")
        .replace("schema_version = 2", "schema_version = 8");
    for id in ["first", "second"] {
        text += &format!(
            "\n[[profiles.user.checks]]\nid='{id}'\ncheckpoint='configured'\nkind='mcp_stdio'\n[profiles.user.checks.probe]\nargv=['/inputs/target/server']\nprotocol='2026-07-28'\nserver_name='fixture'\nserver_version='2'\ntools=['hello_v2']\n"
        );
    }
    let recipe =
        theseus::recipe::parse_bytes(&root.path().join("recipe.toml"), text.as_bytes()).unwrap();
    let mut env = Unsupported {
        calls: vec![],
        stops: 0,
        removals: 0,
    };
    let options = theseus::engine::RunOptions::all(&recipe);
    let result = theseus::engine::run_with(
        &recipe,
        options,
        theseus::engine::CancellationToken::default(),
        &mut env,
        |_| {},
    )
    .unwrap();
    assert_eq!(result.report.outcome, RunOutcome::Incomplete);
    assert_eq!(env.calls, vec!["install_target", "configure"]);
    assert_eq!((env.stops, env.removals), (1, 1));
    let checks: Vec<_> = result.report.scenarios[0]
        .stages
        .iter()
        .flat_map(|s| &s.checks)
        .collect();
    assert_eq!(checks.len(), 2);
    assert!(checks.iter().all(|c| c.mcp_required));
    assert_eq!(checks[0].outcome, CheckOutcome::Incomplete);
    assert_eq!(checks[1].outcome, CheckOutcome::Incomplete);
    assert!(checks[1].command_ids.is_empty());
}

#[test]
fn contradictory_launch_is_rejected_before_docker_is_invoked() {
    use theseus::runner::Environment;
    let (_, o) = observation("hello_v2");
    let probe = o.evidence.probe;
    let request = theseus::runner::CommandRequest {
        id: "mismatch".into(),
        phase: "check:discovery".into(),
        identity: ExecutionIdentity::runtime(),
        argv: vec!["/bin/other".into()],
        env: probe.env.clone(),
        pass_env: vec![],
        timeout_seconds: 3,
        expected_exit: 0,
        log_limit: 1024,
    };
    let root = tempfile::tempdir().unwrap();
    let store = theseus::report::create(&root.path().join("run")).unwrap();
    let resource = OwnedResource {
        container_id: "owned".into(),
        labels: BTreeMap::new(),
        removed: false,
    };
    let error = theseus::runner::Docker::new("/definitely/missing/docker")
        .probe_mcp(
            &resource,
            request,
            probe,
            &store,
            &theseus::engine::CancellationToken::default(),
        )
        .err()
        .unwrap();
    assert!(error.to_string().contains("launch fields disagree"));
}
#[test]
fn exhausted_preparation_does_not_spawn_interactive_guest_or_use_ordinary_abort() {
    use std::os::unix::fs::PermissionsExt;
    use theseus::runner::Environment;
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("docker");
    let calls = root.path().join("calls");
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
case "$1 $2" in
 'exec --user') sleep 2.2; exit 0;;
 'container inspect') printf 'Error: No such object: %s\n' "$5" >&2; exit 1;;
 *) exit 98;;
esac
"#,
            calls.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let (_, o) = observation("hello_v2");
    let probe = o.evidence.probe;
    let request = theseus::runner::CommandRequest {
        id: "preparation".into(),
        phase: "check:discovery".into(),
        identity: ExecutionIdentity::runtime(),
        argv: probe.argv.clone(),
        env: probe.env.clone(),
        pass_env: vec![],
        timeout_seconds: 3,
        expected_exit: 0,
        log_limit: 1024,
    };
    let store = theseus::report::create(&root.path().join("run")).unwrap();
    let resource = OwnedResource {
        container_id: "a".repeat(64),
        labels: BTreeMap::from([
            ("io.theseus.owner".into(), "v1".into()),
            ("io.theseus.run".into(), "test".into()),
            ("io.theseus.scenario".into(), "user-clean".into()),
        ]),
        removed: false,
    };
    let started = std::time::Instant::now();
    let observed = theseus::runner::Docker::new(script)
        .probe_mcp(
            &resource,
            request,
            probe,
            &store,
            &theseus::engine::CancellationToken::default(),
        )
        .unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(4));
    assert!(!observed.container_available);
    assert!(observed.command.record.timed_out);
    assert_eq!(observed.command.record.exit_status, None);
    let calls = std::fs::read_to_string(calls).unwrap();
    assert!(!calls.contains("exec -i"));
    assert!(!calls.lines().any(|line| line.starts_with("rm ")));
    assert_eq!(
        observed.evidence.shutdown,
        McpShutdown::NotStarted,
        "{:?}",
        observed.command.record.error
    );
}

struct Supported {
    base: Unsupported,
    available: bool,
}
impl theseus::runner::Environment for Supported {
    fn prepare(
        &mut self,
        r: &theseus::recipe::ValidatedRecipe,
        p: bool,
        c: &theseus::engine::CancellationToken,
    ) -> anyhow::Result<theseus::runner::PreparedEnvironment> {
        self.base.prepare(r, p, c)
    }
    fn create(
        &mut self,
        r: &str,
        s: &str,
        v: &theseus::recipe::ValidatedRecipe,
    ) -> anyhow::Result<OwnedResource> {
        self.base.create(r, s, v)
    }
    fn setup(
        &mut self,
        r: &OwnedResource,
        v: &theseus::recipe::ValidatedRecipe,
        c: &theseus::engine::CancellationToken,
    ) -> anyhow::Result<()> {
        self.base.setup(r, v, c)
    }
    fn execute(
        &mut self,
        r: &OwnedResource,
        q: theseus::runner::CommandRequest,
        s: &theseus::report::ReportStore,
        c: &theseus::engine::CancellationToken,
    ) -> anyhow::Result<ExecutedCommand> {
        self.base.execute(r, q, s, c)
    }
    fn observe(
        &mut self,
        r: &OwnedResource,
        p: &[String],
        l: theseus::recipe::Limits,
        c: &theseus::engine::CancellationToken,
    ) -> anyhow::Result<Vec<theseus::snapshot::Snapshot>> {
        self.base.observe(r, p, l, c)
    }
    fn stop(&mut self, r: &OwnedResource) -> anyhow::Result<()> {
        self.base.stop(r)
    }
    fn remove(&mut self, r: &OwnedResource) -> anyhow::Result<()> {
        self.base.remove(r)
    }
    fn probe_mcp(
        &mut self,
        _: &OwnedResource,
        r: theseus::runner::CommandRequest,
        probe: McpProbe,
        _: &theseus::report::ReportStore,
        _: &theseus::engine::CancellationToken,
    ) -> anyhow::Result<ExecutedMcp> {
        self.base.calls.push(r.phase.clone());
        let (_, mut observed) = observation("wrong");
        observed.command.record.id = r.id;
        observed.command.record.phase = r.phase;
        observed.evidence.probe = probe;
        if !self.available {
            observed.container_available = false;
            observed.command.stdout_complete = false;
            observed.command.record.exit_status = None;
            observed.command.record.outcome = CommandOutcome::Error;
            observed.command.record.error = Some("fixture transport failure".into());
            observed.evidence.shutdown = McpShutdown::ForcedOwnedRemoval;
        }
        Ok(observed)
    }
}
#[test]
fn returned_unavailability_stops_work_but_clean_mismatch_allows_uninstall() {
    for available in [false, true] {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("target")).unwrap();
        let mut text = include_str!("fixtures/recipes/v2_clean.toml")
            .replace("schema_version = 2", "schema_version = 8");
        for (id, checkpoint) in [("first", "configured"), ("second", "configured")] {
            text += &format!(
                "\n[[profiles.user.checks]]\nid='{id}'\ncheckpoint='{checkpoint}'\nkind='mcp_stdio'\n[profiles.user.checks.probe]\nargv=['/inputs/target/server']\nprotocol='2026-07-28'\nserver_name='fixture'\nserver_version='2'\ntools=['hello_v2']\n"
            );
        }
        let recipe =
            theseus::recipe::parse_bytes(&root.path().join("recipe.toml"), text.as_bytes())
                .unwrap();
        let mut env = Supported {
            base: Unsupported {
                calls: vec![],
                stops: 0,
                removals: 0,
            },
            available,
        };
        let result = theseus::engine::run_with(
            &recipe,
            theseus::engine::RunOptions::all(&recipe),
            theseus::engine::CancellationToken::default(),
            &mut env,
            |_| {},
        )
        .unwrap();
        assert_eq!(
            result.report.outcome,
            if available {
                RunOutcome::Failed
            } else {
                RunOutcome::Incomplete
            }
        );
        assert_eq!(
            env.base.stops, 0,
            "returned disposition already owns cleanup"
        );
        assert_eq!(env.base.removals, 1);
        assert_eq!(
            env.base.calls,
            if available {
                vec![
                    "install_target",
                    "configure",
                    "check:first",
                    "check:second",
                    "uninstall",
                ]
            } else {
                vec!["install_target", "configure", "check:first"]
            }
        );
        let checks: Vec<_> = result.report.scenarios[0]
            .stages
            .iter()
            .flat_map(|s| &s.checks)
            .collect();
        assert!(checks.iter().all(|c| c.mcp_required));
        if available {
            assert_eq!(checks[1].outcome, CheckOutcome::Failed);
        } else {
            assert_eq!(
                checks[0].mcp.as_ref().unwrap().issue.as_ref().unwrap().kind,
                McpIssueKind::Inventory
            );
        }
    }
}

#[test]
fn preflight_revalidates_probe_launch_fields_before_creating_resources() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("target")).unwrap();
    let mut text = include_str!("fixtures/recipes/v2_clean.toml")
        .replace("schema_version = 2", "schema_version = 8");
    text += "\n[[profiles.user.checks]]\nid='discovery'\ncheckpoint='configured'\nkind='mcp_stdio'\n[profiles.user.checks.probe]\nargv=['/inputs/target/server']\nprotocol='2026-07-28'\nserver_name='fixture'\nserver_version='2'\ntools=['hello_v2']\n";
    let mut recipe =
        theseus::recipe::parse_bytes(&root.path().join("recipe.toml"), text.as_bytes()).unwrap();
    let theseus::recipe::Expectation::McpStdio { probe } = &mut recipe
        .recipe
        .profiles
        .get_mut(&ProfileName::User)
        .unwrap()
        .checks[0]
        .expectation
    else {
        unreachable!()
    };
    probe.argv[0] = "/bin/../server".into();
    let mut env = Unsupported {
        calls: vec![],
        stops: 0,
        removals: 0,
    };
    let result = theseus::engine::run_with(
        &recipe,
        theseus::engine::RunOptions::all(&recipe),
        theseus::engine::CancellationToken::default(),
        &mut env,
        |_| {},
    );
    let report = result.unwrap().report;
    assert_eq!(report.outcome, RunOutcome::Incomplete);
    assert!(report.errors.iter().any(|e| e.contains("path traversal")));
    assert!(report.scenarios.is_empty());
    assert!(env.calls.is_empty());
    assert_eq!(env.removals, 0);
}

#[test]
fn emergency_cleanup_time_does_not_manufacture_an_execution_timeout() {
    use std::os::unix::fs::PermissionsExt;
    use theseus::runner::Environment;
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("docker");
    let calls = root.path().join("calls");
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
case "$1 $2" in
 'exec --user') sleep 0.8; exit 1;;
 'container inspect') sleep 1.2; printf 'Error: No such object: %s\n' "$5" >&2; exit 1;;
 *) exit 98;;
esac
"#,
            calls.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let (_, o) = observation("hello_v2");
    let probe = o.evidence.probe;
    let request = theseus::runner::CommandRequest {
        id: "preparation".into(),
        phase: "check:discovery".into(),
        identity: ExecutionIdentity::runtime(),
        argv: probe.argv.clone(),
        env: probe.env.clone(),
        pass_env: vec![],
        timeout_seconds: 3,
        expected_exit: 0,
        log_limit: 1024,
    };
    let store = theseus::report::create(&root.path().join("run")).unwrap();
    let resource = OwnedResource {
        container_id: "a".repeat(64),
        labels: BTreeMap::from([
            ("io.theseus.owner".into(), "v1".into()),
            ("io.theseus.run".into(), "test".into()),
            ("io.theseus.scenario".into(), "user-clean".into()),
        ]),
        removed: false,
    };
    let started = std::time::Instant::now();
    let observed = theseus::runner::Docker::new(script)
        .probe_mcp(
            &resource,
            request,
            probe,
            &store,
            &theseus::engine::CancellationToken::default(),
        )
        .unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(6));
    assert!(!observed.container_available);
    assert!(!observed.command.record.timed_out);
    assert_eq!(observed.command.record.outcome, CommandOutcome::Error);
    assert_eq!(observed.command.record.exit_status, None);
    let calls = std::fs::read_to_string(calls).unwrap();
    assert!(!calls.contains("exec -i"));
    assert!(!calls.lines().any(|line| line.starts_with("rm ")));
    assert_eq!(
        observed.evidence.shutdown,
        McpShutdown::NotStarted,
        "{:?}",
        observed.command.record.error
    );
}
