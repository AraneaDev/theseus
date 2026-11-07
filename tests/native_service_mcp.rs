#![cfg(all(feature = "native-integration-test-support", target_os = "linux"))]
//! Actual inherited-pipe product routing against the explicitly labeled local manager model.
//! This is not supplied systemd-manager acceptance and never imports an earlier CLI run.
#[path = "support/native_service_setup.rs"]
#[allow(
    dead_code,
    reason = "This positive pipe case reuses the full fault-capable fixture builder"
)]
mod setup;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use setup::{Fault, IntegrationFixture};
use std::{
    path::PathBuf,
    process::Command,
    thread,
    time::{Duration, Instant},
};
#[path = "support/native_service_frontend.rs"]
mod frontend;
use frontend::{Controller, FRAME, saved_views as views};
use theseus::{
    model::RunOutcome,
    native::{integration::model::*, storage},
};

const TOOLS: [&str; 6] = [
    "recipe_inspect",
    "run_start",
    "run_list",
    "run_status",
    "run_cancel",
    "run_evidence",
];

fn status_poll_can_restart(application: &Value, execution_active: bool) -> bool {
    if application["ok"] != false {
        return false;
    }
    match application["error"]["code"].as_str() {
        Some("query_busy") => true,
        // The live writer may advance either saved file during a strict pair read.
        // Start a fresh read, retaining the original admitted handle and run deadline.
        Some("revision_changed") => {
            execution_active && application["error"]["restart_pagination"] == true
        }
        _ => false,
    }
}
#[test]
fn live_status_revision_race_restarts_only_the_read_only_snapshot() {
    let changed = json!({"ok":false,"error":{"code":"revision_changed","retryable":false,"restart_pagination":true}});
    assert!(status_poll_can_restart(&changed, true));
    assert!(!status_poll_can_restart(&changed, false));
    let mut unmarked = changed.clone();
    unmarked["error"]["restart_pagination"] = json!(false);
    assert!(!status_poll_can_restart(&unmarked, true));
    for code in [
        "evidence_invalid",
        "io_error",
        "recipe_changed",
        "worker_error",
        "unknown_run",
    ] {
        let mut rejected = changed.clone();
        rejected["error"]["code"] = json!(code);
        assert!(
            !status_poll_can_restart(&rejected, true),
            "unexpected retry: {code}"
        );
    }
    assert!(status_poll_can_restart(
        &json!({"ok":false,"error":{"code":"query_busy"}}),
        true
    ));
}

fn evidence_rows(
    server: &mut Controller,
    instance: &str,
    handle: &str,
    revision: &str,
) -> Result<Vec<Value>> {
    let mut rows = vec![];
    for collection in [
        "run",
        "services",
        "readiness",
        "integration_checks",
        "integration_dispositions",
    ] {
        let mut cursor = None;
        for page_index in 0..256 {
            let mut args = json!({"server_instance":instance,"handle":handle,"revision":revision,"selector":{"kind":"catalog","collection":collection},"limit":1});
            if let Some(value) = cursor.take() {
                args["cursor"] = value;
            }
            let page = server.tool("run_evidence", args)?["data"].clone();
            ensure!(page["revision"] == revision, "catalog revision changed");
            for summary in page["records"]
                .as_array()
                .context("evidence catalog records")?
            {
                let mut bytes = vec![];
                let mut detail_cursor = None;
                let mut completed = false;
                for _ in 0..256 {
                    let mut args = json!({"server_instance":instance,"handle":handle,"revision":revision,"selector":{"kind":"record","record_id":summary["record_id"]},"limit":256});
                    if let Some(value) = detail_cursor.take() {
                        args["cursor"] = value;
                    }
                    let detail = server.tool("run_evidence", args)?["data"].clone();
                    ensure!(
                        detail["revision"] == revision
                            && detail["record_id"] == summary["record_id"]
                            && detail["record_kind"] == summary["record_kind"],
                        "record page correlation"
                    );
                    ensure!(
                        detail["offset"].as_u64() == Some(bytes.len() as u64),
                        "record byte offset"
                    );
                    let chunk: Vec<u8> = serde_json::from_value(detail["bytes"].clone())?;
                    ensure!(
                        !chunk.is_empty() && bytes.len() + chunk.len() <= FRAME,
                        "record bound/progress"
                    );
                    bytes.extend(chunk);
                    if detail["next_cursor"].is_null() {
                        ensure!(
                            detail["total_bytes"].as_u64() == Some(bytes.len() as u64),
                            "record final length"
                        );
                        completed = true;
                        break;
                    }
                    detail_cursor = Some(detail["next_cursor"].clone());
                }
                ensure!(completed, "bounded record pagination");
                rows.push(json!({"record_kind":summary["record_kind"],"record_id":summary["record_id"],"detail":serde_json::from_slice::<Value>(&bytes)?}));
            }
            if page["next_cursor"].is_null() {
                break;
            }
            ensure!(page_index < 255, "bounded catalog pagination");
            cursor = Some(page["next_cursor"].clone());
        }
    }
    Ok(rows)
}

fn source(fixture: &IntegrationFixture) -> frontend::Source<'_> {
    frontend::Source {
        controller: &fixture.controller_path,
        graph: &fixture.graph_path,
        project: fixture.root.path(),
        recipe_path: &fixture.recipe_path,
        recipe: &fixture.recipe,
        authority: &fixture.authority,
        output: &fixture.output,
    }
}
fn run_mcp_fixture(fixture: &IntegrationFixture) -> Result<frontend::SavedRun> {
    let source = source(fixture);
    fixture.authority.validate()?;
    ensure!(
        fixture.ready_root.path().is_dir() && !fixture.output.exists(),
        "fresh MCP fixture context"
    );
    let expected_fingerprint = frontend::fingerprint(&source)?;
    let graph: Value = serde_json::from_slice(&std::fs::read(&fixture.graph_path)?)?;
    let binary = &fixture.controller_path;
    let output_root = fixture.root.path().join("mcp-runs");
    let mut command = Command::new(binary);
    command
        .arg("--fixture-graph")
        .arg(&fixture.graph_path)
        .args(["--no-animation", "serve", "--project-root"])
        .arg(fixture.root.path())
        .arg("--output-root")
        .arg(&output_root)
        .arg("--native-target")
        .arg(fixture.root.path().join("launch.json"))
        .args(["--max-run-seconds", "120"]);
    let mut server = Controller::spawn(&mut command, fixture.root.path(), "mcp")?;
    let initialized=server.request("initialize",json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"native-service-model-pipe","version":"1"}}))?;
    ensure!(
        initialized["result"]["protocolVersion"] == "2025-11-25",
        "protocol agreement"
    );
    server.send(
        &json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
        Instant::now() + Duration::from_secs(5),
    )?;
    let inventory = server.request("tools/list", json!({}))?;
    ensure!(
        inventory["result"]["tools"]
            .as_array()
            .context("six-tool inventory")?
            .iter()
            .map(|tool| tool["name"].as_str().unwrap_or(""))
            .collect::<Vec<_>>()
            == TOOLS,
        "existing six tools unchanged"
    );
    let inspected = server.tool(
        "recipe_inspect",
        json!({"recipe_path":"recipe.json","selector":{"kind":"overview"}}),
    )?;
    ensure!(
        inspected["data"]["schema"] == 2
            && inspected["data"]["recipe_digest"] == fixture.recipe.digest
            && inspected["data"]["role"] == "current_helper_account",
        "actual schema2 source/role inspection"
    );
    let instance = inspected["server_instance"]
        .as_str()
        .context("instance")?
        .to_owned();
    let started=server.tool("run_start",json!({"server_instance":instance,"request_key":"fresh-model-service","recipe_path":"recipe.json","recipe_digest":inspected["data"]["recipe_digest"],"scenarios":["clean"]}))?;
    let handle = started["data"]["handle"]
        .as_str()
        .context("actual admitted handle")?
        .to_owned();
    server.active = Some((instance.clone(), handle.clone()));
    let completion_deadline = Instant::now() + Duration::from_secs(90);
    let mut execution_active = matches!(
        started["data"]["state"].as_str(),
        Some("validating" | "admitted" | "running")
    );
    let status = loop {
        ensure!(
            Instant::now() < completion_deadline,
            "service run completion deadline"
        );
        let status = server.application(
            "run_status",
            json!({"server_instance":instance,"handle":handle,"refresh":true}),
        )?;
        if status["ok"] != true {
            ensure!(
                status_poll_can_restart(&status, execution_active),
                "status rejected: {status}"
            );
            thread::sleep(Duration::from_millis(25));
            continue;
        }
        let listed = server.tool("run_list", json!({"server_instance":instance,"limit":1}))?;
        ensure!(
            listed["data"]["runs"]
                .as_array()
                .context("admission list")?
                .iter()
                .any(|run| run["handle"] == handle),
            "own process admission listed"
        );
        let state = status["data"]["run"]["state"]
            .as_str()
            .context("run state")?;
        execution_active = matches!(state, "validating" | "admitted" | "running");
        ensure!(
            !["start_error", "worker_error"].contains(&state),
            "native run failed: {status}"
        );
        if state == "finished" && status["data"]["refresh"] == "loaded" {
            break status;
        }
        thread::sleep(Duration::from_millis(25));
    };
    ensure!(
        status["data"]["run"]["saved"]["outcome"] == "passed",
        "positive actual model run: {status}"
    );
    ensure!(
        status["data"]["run"]["cleanup"]["native"] == "complete"
            && status["data"]["run"]["cleanup"]["guest"] == "complete"
            && status["data"]["run"]["cleanup"]["recovery_required"] == false,
        "independent completed native/guest ownership gates"
    );
    let report_id = &status["data"]["run"]["report_id"];
    ensure!(
        report_id["truncated"] == false && report_id["lossy"] == false,
        "complete trusted report id"
    );
    let report_id = report_id["text"]
        .as_str()
        .context("actual saved report id")?;
    ensure!(
        !report_id.contains(['/', '\\']) && !report_id.is_empty(),
        "one exact output child"
    );
    let directory = output_root.join(report_id);
    let store = theseus::report::open_existing(&directory)?;
    let (NativeReportVersion::IntegrationV2(report), NativeJournalVersion::IntegrationV2(journal)) =
        storage::load_native_versioned(&store)?
    else {
        bail!("actual schema2 pair required")
    };
    ensure!(
        report.outcome == RunOutcome::Passed
            && report.recipe_digest == fixture.recipe.digest
            && report.core_recipe_digest == fixture.recipe.core.digest,
        "own saved source bindings"
    );
    ensure!(
        report.manifest_digest == graph["manifest_digest"]
            && report.integration_manifest_digest == graph["integration_manifest_digest"]
            && report.core_manifest_digest == graph["core_manifest_digest"],
        "own pinned manifest commitments"
    );
    ensure!(
        journal.launch_fingerprint.as_deref() == Some(expected_fingerprint.as_str()),
        "exact original authority fingerprint"
    );
    ensure!(
        journal.ownership.len() == 1
            && journal.ownership[0].ownership_id == report.integrations[0].ownership_id
            && journal.ownership[0].scenario_id == report.integrations[0].scenario_id,
        "same original scenario/ownership through saved journal"
    );
    let revision = status["data"]["run"]["saved"]["revision"]
        .as_str()
        .context("actual evidence revision")?
        .to_owned();
    let rows = evidence_rows(&mut server, &instance, &handle, &revision)?;
    views::assert_mcp_service_records(&report, &rows);
    views::assert_service_views(report.clone(), &directory);
    let projected = theseus::cli::fixture_saved_native_projection(&directory)?;
    ensure!(
        projected["saved"] == status["data"]["run"]["saved"],
        "MCP status agrees with its own real saved projection"
    );
    let cancelled = server.tool(
        "run_cancel",
        json!({"server_instance":instance,"handle":handle}),
    )?;
    ensure!(
        cancelled["data"]["requested_now"] == false
            && cancelled["data"]["run"]["state"] == "finished",
        "completed-handle cancellation stays completed"
    );
    server.active = None;
    let saved = frontend::saved_run(&source, &directory, &expected_fingerprint)?;
    let mut endpoint = frontend::PrivateEndpointGuard::disable(
        &fixture.authority,
        &fixture.root.path().join("mcp-endpoint-offline.json"),
    )?;
    endpoint.assert_unavailable()?;
    frontend::repeat_saved_without_authority(&saved)?;
    let offline_inspect = server.tool(
        "recipe_inspect",
        json!({"recipe_path":"recipe.json","selector":{"kind":"overview"}}),
    )?;
    ensure!(
        offline_inspect["data"]["recipe_digest"] == fixture.recipe.digest,
        "offline recipe inspection stays pinned"
    );
    let offline_status = server.tool(
        "run_status",
        json!({"server_instance":instance,"handle":handle,"refresh":true}),
    )?;
    ensure!(
        offline_status["data"]["run"]["saved"] == status["data"]["run"]["saved"],
        "saved status changed after authority access was removed"
    );
    let offline_rows = evidence_rows(&mut server, &instance, &handle, &revision)?;
    ensure!(
        offline_rows == rows,
        "saved evidence changed after authority access was removed"
    );
    views::assert_service_views(report.clone(), &directory);
    endpoint.assert_unavailable()?;
    endpoint.restore()?;
    std::fs::write(
        fixture.root.path().join("mcp-status.json"),
        serde_json::to_vec_pretty(&status)?,
    )?;
    std::fs::write(
        fixture.root.path().join("mcp-records.json"),
        serde_json::to_vec_pretty(&rows)?,
    )?;
    ensure!(
        server.close()?.success(),
        "orderly original controller join"
    );
    ensure!(
        server.stderr.is_empty(),
        "quiet MCP stderr: {}",
        String::from_utf8_lossy(&server.stderr)
    );
    ensure!(
        server.pending.is_empty(),
        "no unterminated trailing stdout frame"
    );
    for line in server.stdout.split_inclusive(|byte| *byte == b'\n') {
        ensure!(line.last() == Some(&b'\n'), "trailing frame");
        let _: Value = serde_json::from_slice(line)?;
    }
    Ok(saved)
}

static PRIVATE_FRONTENDS: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[test]
fn local_model_service_clean_uses_fresh_six_tool_inherited_pipe_context() -> Result<()> {
    let _serial = PRIVATE_FRONTENDS
        .lock()
        .map_err(|_| anyhow::anyhow!("fixture endpoint lock poisoned"))?;
    let receipt = PathBuf::from(
        std::env::var_os("THESEUS_NATIVE_SYSTEMD_MODEL_RECEIPT")
            .context("explicit model receipt required")?,
    );
    let fixture = IntegrationFixture::model_manager(&receipt, Fault::None)?;
    run_mcp_fixture(&fixture)?;
    Ok(())
}
fn paired_cli_and_mcp(mut fixture: impl FnMut() -> Result<IntegrationFixture>) -> Result<()> {
    let cli_fixture = fixture()?;
    let cli = frontend::run_cli(&source(&cli_fixture))?;
    let mut endpoint = frontend::PrivateEndpointGuard::disable(
        &cli_fixture.authority,
        &cli_fixture.root.path().join("cli-endpoint-offline.json"),
    )?;
    endpoint.assert_unavailable()?;
    frontend::repeat_saved_without_authority(&cli)?;
    endpoint.assert_unavailable()?;
    endpoint.restore()?;
    let mcp_fixture = fixture()?;
    ensure!(
        cli_fixture.root.path() != mcp_fixture.root.path()
            && cli_fixture.graph_path != mcp_fixture.graph_path,
        "independent fresh CLI and MCP contexts"
    );
    let mcp = run_mcp_fixture(&mcp_fixture)?;
    let cli_facts = frontend::normalize(&cli.report)?;
    let mcp_facts = frontend::normalize(&mcp.report)?;
    ensure!(
        cli_facts == mcp_facts,
        "typed CLI/MCP phase/predicate/provenance/disposition parity:\nCLI={cli_facts:#?}\nMCP={mcp_facts:#?}"
    );
    for directory in [cli_fixture.root.path(), mcp_fixture.root.path()] {
        std::fs::write(
            directory.join("frontend-normalized-parity.json"),
            serde_json::to_vec_pretty(&cli_facts)?,
        )?;
    }
    Ok(())
}
#[test]
fn systemd_model_cli_and_fresh_mcp_normalized_frontend_parity() -> Result<()> {
    let _serial = PRIVATE_FRONTENDS
        .lock()
        .map_err(|_| anyhow::anyhow!("fixture endpoint lock poisoned"))?;
    let receipt = PathBuf::from(
        std::env::var_os("THESEUS_NATIVE_SYSTEMD_MODEL_RECEIPT")
            .context("explicit model receipt required; no supplied-manager fallback")?,
    );
    paired_cli_and_mcp(|| IntegrationFixture::model_manager(&receipt, Fault::None))
}
#[test]
fn systemd_supplied_cli_and_fresh_mcp_normalized_frontend_parity() -> Result<()> {
    let _serial = PRIVATE_FRONTENDS
        .lock()
        .map_err(|_| anyhow::anyhow!("fixture endpoint lock poisoned"))?;
    paired_cli_and_mcp(|| {
        IntegrationFixture::supplied_manager(ManagerKind::SystemdUser, Fault::None)
    })
}
