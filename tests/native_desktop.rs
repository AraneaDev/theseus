#![cfg(all(feature = "native-integration-test-support", target_os = "linux"))]
//! SourceOwned desktop fixtures; Combined labels real and private-model manager evidence separately.
#[path = "support/native_desktop_combined.rs"]
mod combined;
#[path = "support/native_service_frontend.rs"]
mod frontend;
#[path = "support/native_desktop_setup.rs"]
mod setup;
use anyhow::{Context, Result, ensure};
use setup::{DesktopFixture, Fault};
use std::{
    process::Command,
    time::{Duration, Instant},
};
use theseus::{
    engine::domain::StoredRun,
    model::{RunOutcome, ScenarioKind},
    native::{integration::model::*, storage},
};
fn cli(f: &DesktopFixture) -> Result<(NativeIntegrationReport, std::path::PathBuf)> {
    cli_scenario(f, ScenarioKind::Clean)
}
fn cli_scenario(
    f: &DesktopFixture,
    scenario: ScenarioKind,
) -> Result<(NativeIntegrationReport, std::path::PathBuf)> {
    let output = Command::new(&f.controller)
        .arg("--fixture-graph")
        .arg(&f.graph)
        .args(["--no-animation", "run", "--headless", "--recipe"])
        .arg(&f.recipe.path)
        .arg("--native-target")
        .arg(&f.launch)
        .args(["--scenario", &scenario.to_string()])
        .output()?;
    std::fs::write(f.root().join("cli.stdout.raw"), &output.stdout)?;
    std::fs::write(f.root().join("cli.stderr.raw"), &output.stderr)?;
    let mut entries = std::fs::read_dir(&f.output)
        .with_context(|| {
            format!(
                "CLI created no output directory ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )
        })?
        .map(|p| p.map(|p| p.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    ensure!(
        entries.len() == 1,
        "one original CLI run: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let directory = entries.remove(0);
    let StoredRun::NativeIntegration(report) = storage::load_run(&directory)? else {
        anyhow::bail!("schema2 desktop evidence required")
    };
    if matches!(f.fault, Fault::None) {
        ensure!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    } else {
        ensure!(!output.status.success(), "broken SUT cannot pass");
    }
    Ok((report, directory))
}
fn assert_desktop(report: &NativeIntegrationReport) {
    assert_eq!(report.outcome, RunOutcome::Passed);
    assert!(report.requirements.service.is_none());
    assert!(report.integrations[0].services.is_empty());
    assert_eq!(report.integrations[0].desktops.len(), 1);
    assert!(
        report.integrations[0]
            .checks
            .iter()
            .all(|c| c.outcome == theseus::model::CheckOutcome::Passed)
    );
    assert!(
        report.integrations[0]
            .checks
            .iter()
            .any(|c| c.predicate == IntegrationPredicate::ApplicationReady)
    );
    assert!(report.integrations[0].checks.iter().any(|c| c.predicate
        == IntegrationPredicate::Lifetime(LifetimeRequirement::OwnedProcessTreeSettled)));
}
#[test]
fn source_desktop_clean_uses_original_broker_child_and_public_cli_saved_views() -> Result<()> {
    let f = DesktopFixture::new(Fault::None)?;
    let (report, directory) = cli(&f)?;
    assert_desktop(&report);
    setup::assert_saved_views(&report, &directory)?;
    f.finish()?;
    Ok(())
}
#[test]
fn source_desktop_missing_and_leaked_entries_fail_exact_sut_postconditions() -> Result<()> {
    for fault in [Fault::MissingEntry, Fault::LeakedEntry] {
        let f = DesktopFixture::new(fault)?;
        let (report, directory) = cli(&f)?;
        assert_ne!(report.outcome, RunOutcome::Passed);
        setup::assert_entry_fault(&report, fault)?;
        setup::assert_saved_views(&report, &directory)?;
        f.finish()?;
    }
    Ok(())
}
#[test]
fn source_desktop_fresh_six_tool_mcp_matches_own_saved_desktop_and_tui() -> Result<()> {
    let f = DesktopFixture::new(Fault::None)?;
    let (report, _) = mcp_scenario(
        f.root(),
        &f.controller,
        &f.graph,
        &f.output,
        &f.launch,
        &f.recipe.digest,
        ScenarioKind::Clean,
    )?;
    assert_desktop(&report);
    f.finish()
}
fn mcp_scenario(
    root: &std::path::Path,
    controller: &std::path::Path,
    graph: &std::path::Path,
    output: &std::path::Path,
    launch: &std::path::Path,
    recipe_digest: &str,
    scenario: ScenarioKind,
) -> Result<(NativeIntegrationReport, std::path::PathBuf)> {
    let mut command = Command::new(controller);
    command
        .arg("--fixture-graph")
        .arg(graph)
        .args(["--no-animation", "serve", "--project-root"])
        .arg(root)
        .arg("--output-root")
        .arg(output)
        .arg("--native-target")
        .arg(launch)
        .args(["--max-run-seconds", "120"]);
    let mut server = frontend::Controller::spawn(&mut command, root, "desktop-mcp")?;
    let initialized=server.request("initialize",serde_json::json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"source-desktop","version":"1"}}))?;
    ensure!(
        initialized["result"]["protocolVersion"] == "2025-11-25",
        "MCP version"
    );
    server.send(
        &serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
        Instant::now() + Duration::from_secs(5),
    )?;
    let tools = server.request("tools/list", serde_json::json!({}))?;
    assert_eq!(
        tools["result"]["tools"]
            .as_array()
            .context("six tools")?
            .iter()
            .map(|v| v["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "recipe_inspect",
            "run_start",
            "run_list",
            "run_status",
            "run_cancel",
            "run_evidence"
        ]
    );
    let inspected = server.tool(
        "recipe_inspect",
        serde_json::json!({"recipe_path":"recipe.json","selector":{"kind":"overview"}}),
    )?;
    ensure!(
        inspected["data"]["recipe_digest"] == recipe_digest,
        "exact fresh source"
    );
    let instance = inspected["server_instance"]
        .as_str()
        .context("instance")?
        .to_owned();
    let started=server.tool("run_start",serde_json::json!({"server_instance":instance,"request_key":"fresh-desktop","recipe_path":"recipe.json","recipe_digest":recipe_digest,"scenarios":[scenario.to_string()]}))?;
    let handle = started["data"]["handle"]
        .as_str()
        .context("handle")?
        .to_owned();
    server.active = Some((instance.clone(), handle.clone()));
    let until = Instant::now() + Duration::from_secs(120);
    let status = loop {
        ensure!(Instant::now() < until, "desktop MCP completion deadline");
        let status = server.application(
            "run_status",
            serde_json::json!({"server_instance":instance,"handle":handle,"refresh":true}),
        )?;
        if status["ok"] != true {
            ensure!(
                status["error"]["code"] == "query_busy"
                    || (status["error"]["code"] == "revision_changed"
                        && status["error"]["restart_pagination"] == true),
                "status: {status}"
            );
        } else if status["data"]["run"]["state"] == "finished"
            && status["data"]["refresh"] == "loaded"
        {
            break status;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    ensure!(
        status["data"]["run"]["saved"]["outcome"] == "passed",
        "desktop status: {status}"
    );
    let listed = server.tool(
        "run_list",
        serde_json::json!({"server_instance":instance,"limit":1}),
    )?;
    ensure!(
        listed["data"]["runs"]
            .as_array()
            .context("run list")?
            .iter()
            .any(|r| r["handle"] == handle),
        "original admitted handle listed"
    );
    let report_id = status["data"]["run"]["report_id"]["text"]
        .as_str()
        .context("report id")?;
    ensure!(!report_id.contains(['/', '\\']), "one output child");
    let directory = output.join(report_id);
    let StoredRun::NativeIntegration(report) = storage::load_run(&directory)? else {
        anyhow::bail!("schema2 pair")
    };
    setup::assert_saved_views(&report, &directory)?;
    setup::assert_live_evidence(&mut server, &instance, &handle, &status, &report)?;
    let canceled = server.tool(
        "run_cancel",
        serde_json::json!({"server_instance":instance,"handle":handle}),
    )?;
    ensure!(
        canceled["data"]["requested_now"] == false,
        "finished handle stays finished"
    );
    server.active = None;
    ensure!(server.close()?.success(), "original MCP controller joined");
    drop(server);
    Ok((report, directory))
}

fn assert_ordinary_generations(
    report: &NativeIntegrationReport,
    directory: &std::path::Path,
    scenario: ScenarioKind,
) -> Result<()> {
    use theseus_native_protocol::v2::broker::{
        BrokerEffect, DesktopAction, DesktopEvidence as BrokerEvidence,
    };
    assert_desktop(report);
    let theseus::engine::domain::StoredJournal::NativeIntegration(journal) =
        storage::load_journal(directory)?
    else {
        anyhow::bail!("same schema2 journal required")
    };
    validate_pair(report, &journal)?;
    assert_eq!(
        journal.ownership.len(),
        1,
        "one original provider ownership"
    );
    let activations = journal
        .integration_actions
        .iter()
        .filter_map(|a| {
            if let IntegrationRequest::Desktop { request, .. } = &a.request {
                matches!(request.action, DesktopAction::Activate { .. }).then_some((a, request))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(
        activations.len(),
        2,
        "two exact original app generations, not replayed effects"
    );
    assert_eq!(activations[0].0.lease, activations[1].0.lease);
    assert_eq!(activations[0].1.broker_lease, activations[1].1.broker_lease);
    assert_ne!(activations[0].0.operation, activations[1].0.operation);
    let first = activations[0]
        .1
        .transition()
        .context("initial generation witness")?;
    let next = activations[1]
        .1
        .transition()
        .context("next generation witness")?;
    assert_ne!(
        first.sut.operation, next.sut.operation,
        "same bytes reinstall still needs a new original SUT Run"
    );
    assert_eq!(
        next.previous_activation.as_ref(),
        Some(&activations[0].1.identity()?)
    );
    assert_eq!(
        next.definition,
        theseus_native_protocol::v2::DesktopDefinitionKey::CONFIGURED_TARGET
    );
    if scenario == ScenarioKind::Upgrade {
        assert_eq!(
            first.definition.artifact,
            theseus_native_protocol::v2::DesktopArtifactRole::Base
        );
    } else {
        assert_eq!(first.definition, next.definition);
        assert_eq!(first.tuple_digest, next.tuple_digest);
    }
    for (action, _) in activations {
        let latest = report.integrations[0].desktops[0]
            .observations
            .iter()
            .filter(|o| o.binding.action.as_ref() == Some(&action.operation))
            .filter_map(|o| o.broker_state.as_ref())
            .next_back()
            .context("original generation lifetime")?;
        assert!(latest.original_retired());
        assert!(
            matches!(&latest.effect,BrokerEffect::Receipt {receipt} if receipt.evidence==BrokerEvidence::SourceOwnedExecutable)
        );
    }
    for phase in if scenario == ScenarioKind::Upgrade {
        vec![
            IntegrationPhase::Configured,
            IntegrationPhase::TargetInstalledAfterUpgrade,
        ]
    } else {
        vec![IntegrationPhase::Configured, IntegrationPhase::Reinstalled]
    } {
        assert!(
            report.integrations[0]
                .checks
                .iter()
                .any(|c| c.phase == phase
                    && c.predicate == IntegrationPredicate::ApplicationReady
                    && c.outcome == theseus::model::CheckOutcome::Passed)
        );
    }
    Ok(())
}
#[test]
fn source_desktop_upgrade_keeps_original_base_and_target_generations() -> Result<()> {
    let f = DesktopFixture::new_ordinary(Fault::None, ScenarioKind::Upgrade)?;
    let (report, directory) = cli_scenario(&f, ScenarioKind::Upgrade)?;
    assert_ordinary_generations(&report, &directory, ScenarioKind::Upgrade)?;
    setup::assert_saved_views(&report, &directory)?;
    f.finish()
}
#[test]
fn source_desktop_reinstall_same_tuple_requires_new_original_sut_generation() -> Result<()> {
    let f = DesktopFixture::new_ordinary(Fault::None, ScenarioKind::Reinstall)?;
    let (report, directory) = cli_scenario(&f, ScenarioKind::Reinstall)?;
    assert_ordinary_generations(&report, &directory, ScenarioKind::Reinstall)?;
    setup::assert_saved_views(&report, &directory)?;
    f.finish()
}

#[test]
fn source_combined_service_desktop_share_one_sequence_provider_and_lease() -> Result<()> {
    for scenario in [
        ScenarioKind::Clean,
        ScenarioKind::Upgrade,
        ScenarioKind::Reinstall,
    ] {
        let f = combined::CombinedFixture::new(scenario)?;
        let output = Command::new(&f.service.controller_path)
            .arg("--fixture-graph")
            .arg(&f.service.graph_path)
            .args(["--no-animation", "run", "--headless", "--recipe"])
            .arg(&f.service.recipe_path)
            .arg("--native-target")
            .arg(f.launch())
            .args(["--scenario", &scenario.to_string()])
            .output()?;
        std::fs::write(f.root().join("combined-cli.stdout.raw"), &output.stdout)?;
        std::fs::write(f.root().join("combined-cli.stderr.raw"), &output.stderr)?;
        ensure!(
            output.status.success(),
            "combined original run: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let paths = std::fs::read_dir(&f.service.output)?
            .map(|e| e.map(|e| e.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        ensure!(paths.len() == 1, "one combined original run");
        let directory = &paths[0];
        let StoredRun::NativeIntegration(report) = storage::load_run(directory)? else {
            anyhow::bail!("combined schema2")
        };
        combined::assert_shared_original(&report, directory, scenario)?;
        setup::assert_saved_views(&report, directory)?;
        f.finish()?;
    }
    Ok(())
}

#[test]
fn source_desktop_ordinary_fresh_six_tool_mcp_keeps_exact_generations_and_saved_views() -> Result<()>
{
    for scenario in [ScenarioKind::Upgrade, ScenarioKind::Reinstall] {
        let f = DesktopFixture::new_ordinary(Fault::None, scenario)?;
        let (report, directory) = mcp_scenario(
            f.root(),
            &f.controller,
            &f.graph,
            &f.output,
            &f.launch,
            &f.recipe.digest,
            scenario,
        )?;
        assert_ordinary_generations(&report, &directory, scenario)?;
        f.finish()?;
    }
    Ok(())
}
#[test]
fn source_combined_ordinary_fresh_six_tool_mcp_keeps_one_shared_lease_and_saved_views() -> Result<()>
{
    for scenario in [
        ScenarioKind::Clean,
        ScenarioKind::Upgrade,
        ScenarioKind::Reinstall,
    ] {
        let f = combined::CombinedFixture::new(scenario)?;
        let (report, directory) = mcp_scenario(
            f.root(),
            &f.service.controller_path,
            &f.service.graph_path,
            &f.service.output,
            f.launch(),
            &f.service.recipe.digest,
            scenario,
        )?;
        combined::assert_shared_original(&report, &directory, scenario)?;
        f.finish()?;
    }
    Ok(())
}

/// Private D-Bus manager model evidence; does not qualify native/supplied-manager Combined.
#[test]
fn private_model_combined_service_desktop_ordinary_cli_one_shared_lease() -> Result<()> {
    for scenario in [
        ScenarioKind::Clean,
        ScenarioKind::Upgrade,
        ScenarioKind::Reinstall,
    ] {
        let f = combined::CombinedFixture::private_model(scenario)?;
        let output = Command::new(&f.service.controller_path)
            .arg("--fixture-graph")
            .arg(&f.service.graph_path)
            .args(["--no-animation", "run", "--headless", "--recipe"])
            .arg(&f.service.recipe_path)
            .arg("--native-target")
            .arg(f.launch())
            .args(["--scenario", &scenario.to_string()])
            .output()?;
        std::fs::write(
            f.root().join("private-model-combined-cli.stdout.raw"),
            &output.stdout,
        )?;
        std::fs::write(
            f.root().join("private-model-combined-cli.stderr.raw"),
            &output.stderr,
        )?;
        ensure!(
            output.status.success(),
            "private-model Combined original run: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let paths = std::fs::read_dir(&f.service.output)?
            .map(|e| e.map(|e| e.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        ensure!(paths.len() == 1, "one private-model Combined original run");
        let directory = &paths[0];
        let StoredRun::NativeIntegration(report) = storage::load_run(directory)? else {
            anyhow::bail!("combined schema2")
        };
        combined::assert_shared_original(&report, directory, scenario)?;
        setup::assert_saved_views(&report, directory)?;
        f.finish()?;
    }
    Ok(())
}
/// Fresh private-model contexts, never reused to claim native manager qualification.
#[test]
fn private_model_combined_ordinary_fresh_six_tool_mcp_one_shared_lease() -> Result<()> {
    for scenario in [
        ScenarioKind::Clean,
        ScenarioKind::Upgrade,
        ScenarioKind::Reinstall,
    ] {
        let f = combined::CombinedFixture::private_model(scenario)?;
        let (report, directory) = mcp_scenario(
            f.root(),
            &f.service.controller_path,
            &f.service.graph_path,
            &f.service.output,
            f.launch(),
            &f.service.recipe.digest,
            scenario,
        )?;
        combined::assert_shared_original(&report, &directory, scenario)?;
        f.finish()?;
    }
    Ok(())
}
