#[path = "support/mcp.rs"]
mod fixture;
use theseus::model::RunOutcome;
#[test]
fn native_registration_contract_is_retained_and_rechecked() {
    let (run, _) = fixture::run("");
    assert_eq!(
        run.report.aggregate_outcome(),
        RunOutcome::Passed,
        "{:#?}",
        run.report
    );
    assert!(
        serde_json::to_value(&run.report.scenarios[0])
            .unwrap()
            .get("mcp")
            .is_some()
    );
    let mut value = serde_json::to_value(&run.report).unwrap();
    value["scenarios"][0]["mcp"]["client_alias"] = "other".into();
    let mut report: theseus::model::RunReport = serde_json::from_value(value).unwrap();
    report.recompute_outcome();
    assert_eq!(report.outcome, RunOutcome::Incomplete);
}
#[test]
fn native_observation_and_artifact_failures_cannot_pass() {
    for fault in [
        "duplicate",
        "wrong-scope",
        "wrong-argv",
        "dropped-companion",
        "missing-artifact",
        "wrong-tools",
    ] {
        let (run, _) = fixture::run(fault);
        assert_ne!(
            run.report.aggregate_outcome(),
            RunOutcome::Passed,
            "{fault}"
        );
    }
}
#[test]
fn forced_probe_abort_blocks_further_guest_work() {
    let (run, calls) = fixture::run("configured-probe-ignore-eof");
    assert_eq!(run.report.aggregate_outcome(), RunOutcome::Incomplete);
    let stop = calls.iter().position(|c| c == "stop").unwrap();
    assert!(calls[stop + 1..].iter().all(|c| c == "remove"));
}
#[test]
fn retained_roles_cannot_be_moved_outside_lifecycle_order() {
    let (run, _) = fixture::run("");
    let mut report = run.report;
    let stages = &mut report.scenarios[0].stages;
    let i = stages.iter().position(|s| s.name == "configured").unwrap();
    let configured = stages.remove(i);
    let after = stages.remove(i);
    let at = stages.iter().position(|s| s.name == "uninstalled").unwrap();
    stages.insert(at, configured);
    stages.insert(at + 1, after);
    report.recompute_outcome();
    assert_eq!(report.outcome, RunOutcome::Incomplete);
}
#[test]
fn retained_proof_roles_and_artifact_bindings_are_recomputed() {
    let (run, _) = fixture::run("");
    for mutation in 0..9 {
        let mut report = run.report.clone();
        let s = &mut report.scenarios[0];
        match mutation {
            0 => {
                s.mcp.as_mut().unwrap().client_claim =
                    theseus::recipe::McpClientClaim::ConfigurationOnly
            }
            1 => s.mcp.as_mut().unwrap().client_artifact = "missing".into(),
            2 => s
                .stages
                .iter_mut()
                .flat_map(|s| &mut s.checks)
                .find(|c| c.id == "configured-registration")
                .unwrap()
                .json_expectation
                .as_mut()
                .unwrap()
                .assertions
                .clear(),
            3 => {
                s.stages
                    .iter_mut()
                    .flat_map(|s| &mut s.commands)
                    .find(|c| c.phase == "check:configured-registration")
                    .unwrap()
                    .phase = "other".into()
            }
            4 => {
                s.stages
                    .iter_mut()
                    .flat_map(|s| &mut s.checks)
                    .find(|c| c.id == "configured-probe")
                    .unwrap()
                    .mcp
                    .as_mut()
                    .unwrap()
                    .probe
                    .argv[1] = "/wrong".into()
            }
            5 => {
                s.stages
                    .iter_mut()
                    .flat_map(|s| &mut s.commands)
                    .find(|c| c.phase == "check:configured-preservation")
                    .unwrap()
                    .identity
                    .uid = 0
            }
            6 => report.network = theseus::recipe::Network::Bridge,
            7 => report.artifacts[0].files[0].sha256.clear(),
            _ => {
                let c = s
                    .stages
                    .iter()
                    .flat_map(|s| &s.checks)
                    .find(|c| c.id == "configured-registration")
                    .unwrap()
                    .clone();
                s.stages
                    .iter_mut()
                    .find(|s| s.name == "configured")
                    .unwrap()
                    .checks
                    .push(c);
            }
        }
        report.recompute_outcome();
        assert_eq!(
            report.outcome,
            RunOutcome::Incomplete,
            "mutation {mutation}"
        );
    }
}
#[test]
fn lifecycle_contract_cannot_be_loaded_as_historical_report() {
    let (run, _) = fixture::run("");
    let root = tempfile::tempdir().unwrap();
    let mut report = run.report;
    for stage in &mut report.scenarios[0].stages {
        for c in &mut stage.checks {
            c.mcp_required = false;
            c.mcp = None;
        }
    }
    report.schema_version = 7;
    std::fs::write(
        root.path().join("report.json"),
        serde_json::to_vec(&report).unwrap(),
    )
    .unwrap();
    assert!(theseus::report::load(root.path()).is_err());
}

#[test]
fn missing_client_input_stops_before_guest_creation() {
    let (run, calls) = fixture::run("client-file-missing");
    assert_eq!(run.report.outcome, RunOutcome::Incomplete);
    assert!(calls.is_empty());
    assert!(
        run.report
            .errors
            .iter()
            .any(|e| e.contains("MCP client artifact"))
    );
}
#[test]
fn plugin_mcp_reuses_lifecycle_and_binds_source_independently_of_cache() {
    let (run, calls) = fixture::run("plugin:");
    assert_eq!(run.report.outcome, RunOutcome::Passed, "{:#?}", run.report);
    assert_eq!(calls.iter().filter(|c| c.ends_with("-probe")).count(), 3);
    assert!(calls.iter().any(|c| c == "disable_plugin"));
    assert!(calls.iter().any(|c| c == "enable_plugin"));
    for fault in [
        "plugin:wrong-source",
        "plugin:plugin-duplicate",
        "plugin:wrong-tools",
    ] {
        let (run, _) = fixture::run(fault);
        assert_ne!(run.report.outcome, RunOutcome::Passed, "{fault}");
    }
}
#[test]
fn upgrade_uses_two_independent_version_and_inventory_observations() {
    for mode in ["upgrade:", "upgrade:legacy"] {
        let (run, calls) = fixture::run(mode);
        assert_eq!(
            run.report.outcome,
            RunOutcome::Passed,
            "{mode}: {:#?}",
            run.report
        );
        assert_eq!(calls.iter().filter(|c| c.ends_with("-probe")).count(), 2);
        let probes = run.report.scenarios[0]
            .stages
            .iter()
            .flat_map(|s| &s.checks)
            .filter_map(|c| c.mcp.as_ref())
            .collect::<Vec<_>>();
        assert_eq!(probes[0].probe.server_version, "1");
        assert_eq!(probes[1].probe.server_version, "2");
    }
}
#[test]
fn ordinary_native_health_marker_retains_command_role_proof() {
    let (run, _) = fixture::run("ordinary-health");
    assert_eq!(run.report.outcome, RunOutcome::Passed);
    let mut report = run.report;
    let check = report.scenarios[0]
        .stages
        .iter_mut()
        .flat_map(|s| &mut s.checks)
        .find(|c| c.id == "configured-activation")
        .unwrap();
    assert!(check.command_expectation.is_some());
    check.command_expectation = None;
    report.recompute_outcome();
    assert_eq!(report.outcome, RunOutcome::Incomplete);
}
#[test]
fn retained_unbound_probe_cannot_be_added_at_removed_checkpoint() {
    let (run, _) = fixture::run("");
    let mut report = run.report;
    let s = &mut report.scenarios[0];
    let configured = s.stages.iter().find(|s| s.name == "configured").unwrap();
    let mut check = configured
        .checks
        .iter()
        .find(|c| c.id == "configured-probe")
        .unwrap()
        .clone();
    let mut cmd = configured
        .commands
        .iter()
        .find(|c| Some(&c.id) == check.command_ids.first())
        .unwrap()
        .clone();
    check.id = "unbound".into();
    check.checkpoint = theseus::model::Checkpoint::Uninstalled;
    check.command_ids = vec!["unbound-command".into()];
    cmd.id = "unbound-command".into();
    cmd.phase = "check:unbound".into();
    let removed = s
        .stages
        .iter_mut()
        .find(|s| s.name == "uninstalled")
        .unwrap();
    removed.checks.push(check);
    removed.commands.push(cmd);
    report.recompute_outcome();
    assert_eq!(report.outcome, RunOutcome::Incomplete);
}
