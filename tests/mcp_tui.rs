//! Presentation uses evaluated, received-response evidence; SYNTHETIC, not native acceptance.
#[path = "support/mcp.rs"]
mod fixture;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use theseus::{
    recipe::McpClientClaim,
    tui::{
        App, Focus, Tab,
        evidence::{Catalog, Record},
        input, render,
    },
};

#[test]
fn retained_probe_renders_protocol_observations_and_shutdown_without_raw_frames() {
    for (fault, tool) in [("", "hello_v2"), ("wrong-tools", "wrong")] {
        let (run, _) = fixture::run(fault);
        let check = run.report.scenarios[0]
            .stages
            .iter()
            .flat_map(|s| &s.checks)
            .find(|c| c.mcp_required)
            .unwrap();
        let text = Record::Check(check).text();
        for expected in ["2026-07-28", "fixture", "Observed", tool, "Graceful"] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        assert!(!text.contains("\"jsonrpc\""));
        if !fault.is_empty() {
            assert!(text.contains("Inventory"));
            assert!(
                !text.contains("hello_v2"),
                "expected tool is not an observation"
            );
        }
        let page = Record::Check(check).page(0, 80).unwrap();
        assert!(page.text.len() <= 80 && page.next_offset.is_some());
    }
}

#[test]
fn configuration_only_contract_exposes_activation_boundary_without_connected_claim() {
    let (run, _) = fixture::run("");
    let mut contract = run.report.scenarios[0].mcp.clone().unwrap();
    contract.client_claim = McpClientClaim::ConfigurationOnly;
    let text = Record::Mcp(&contract).text();
    assert!(text.contains("native activation not exposed/tested"));
    assert!(!text.contains("Connected"));
    assert!(text.contains("scope: user"));
}

#[test]
fn offline_catalog_and_current_screen_expose_separate_contract_and_probe_records() {
    for protocol in [
        theseus::mcp::McpProtocol::Modern20260728,
        theseus::mcp::McpProtocol::Legacy20251125,
    ] {
        let report = retained_identity_report(
            protocol,
            Some(serde_json::json!({"name":"fixture","version":"2"})),
            serde_json::json!({"name":"other-era-decoy","version":"9.9.9"}),
        );
        let mut app = App::offline(report, std::path::PathBuf::from("/tmp/unused-mcp-ui"));
        app.stage_index = app.report.as_ref().unwrap().scenarios[0]
            .stages
            .iter()
            .position(|s| {
                s.checks.iter().any(|c| {
                    c.mcp
                        .as_ref()
                        .is_some_and(|p| p.probe.server_version == "2")
                })
            })
            .unwrap();
        app.tab = Tab::Stages;
        app.focus = Focus::Evidence;
        let records = Catalog::for_app(&app);
        assert!(
            records
                .records()
                .any(|r| matches!(r.record, Record::Mcp(_)))
        );
        let selected = records
            .records()
            .find(|r| matches!(r.record, Record::Check(c) if c.mcp_required))
            .unwrap()
            .key;
        for _ in 0..selected {
            input(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        for (w, h) in [(120, 36), (80, 40)] {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal.draw(|f| render(f, &app)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(text.contains(protocol.version()), "{text}");
            assert!(
                text.contains("Observed server: name=\"fixture\" version=\"2\""),
                "{text}"
            );
            assert!(
                !text.contains("other-era-decoy") && !text.contains("9.9.9"),
                "{text}"
            );
            assert!(text.contains("hello_v2"), "{text}");
            assert!(!text.contains("\"jsonrpc\""));
        }
    }
}

#[test]
fn malformed_or_oversized_observed_fields_stay_bounded_metadata() {
    use theseus::{
        mcp::*,
        model::*,
        runner::{ExecutedCommand, ExecutedMcp},
    };
    let (run, _) = fixture::run("");
    let source = run.report.scenarios[0]
        .stages
        .iter()
        .flat_map(|s| &s.checks)
        .find(|c| c.mcp_required)
        .unwrap();
    for name in [
        serde_json::json!({"jsonrpc":"raw-object-body"}),
        serde_json::json!("x".repeat(5000)),
    ] {
        let probe = source.mcp.as_ref().unwrap().probe.clone();
        let check = theseus::recipe::Check {
            id: "bounded".into(),
            checkpoint: Checkpoint::Configured,
            expectation: theseus::recipe::Expectation::McpStdio {
                probe: probe.clone(),
            },
        };
        let mut session = McpSession::new(probe).unwrap();
        session.written(session.pending_write().len()).unwrap();
        let frame = serde_json::json!({"jsonrpc":"2.0","id":"theseus-discover-1","result":{"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"public","_meta":{"io.modelcontextprotocol/serverInfo":{"name":name,"version":"2"}}}});
        session.receive(format!("{frame}\n").as_bytes());
        session.eof();
        let evidence = session.finish(McpShutdown::Graceful);
        let mut record = CommandRecord::new("bounded", "check:bounded");
        record.exit_status = Some(0);
        record.outcome = CommandOutcome::Succeeded;
        let result = theseus::checks::evaluate_mcp(
            &check,
            &ExecutedMcp {
                command: ExecutedCommand {
                    record,
                    stdout: String::new(),
                    stdout_complete: true,
                    stdout_utf8: true,
                },
                evidence,
                container_available: true,
            },
        );
        assert_ne!(result.outcome, CheckOutcome::Passed);
        let text = Record::Check(&result).text();
        assert!(
            !text.contains("raw-object-body"),
            "raw nested object leaked: {text}"
        );
        assert!(
            text.len() < 2000,
            "oversized observed identity leaked into metadata: {}",
            text.len()
        );
    }
}

#[test]
#[ignore = "UNRUN without explicit supplied native clients/image, reachable Docker and Unix PTY"]
fn supplied_native_live_offline_and_cancel_public_pty() {
    let output = std::process::Command::new(if cfg!(windows) { "python" } else { "python3" })
        .args([
            "tests/support/mcp_native_pty.py",
            env!("CARGO_BIN_EXE_theseus"),
        ])
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .unwrap();
    println!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// Feed altered received frames through the real protocol/evaluator, then save
// and reload their report. Expectations below are literal identities, not values
// recovered by the renderer or by its namespace-selection code.
fn retained_identity_report(
    protocol: theseus::mcp::McpProtocol,
    canonical: Option<serde_json::Value>,
    other: serde_json::Value,
) -> theseus::model::RunReport {
    use theseus::{
        mcp::*,
        model::*,
        runner::{ExecutedCommand, ExecutedMcp},
    };
    let (run, _) = fixture::run(if protocol == McpProtocol::Legacy20251125 {
        "upgrade:legacy"
    } else {
        ""
    });
    let mut report = run.report;
    let stage = report.scenarios[0]
        .stages
        .iter_mut()
        .find(|s| {
            s.checks.iter().any(|c| {
                c.mcp
                    .as_ref()
                    .is_some_and(|p| p.probe.server_version == "2")
            })
        })
        .unwrap();
    let at = stage
        .checks
        .iter()
        .position(|c| {
            c.mcp
                .as_ref()
                .is_some_and(|p| p.probe.server_version == "2")
        })
        .unwrap();
    let source = stage.checks[at].clone();
    let proof = source.mcp.as_ref().unwrap();
    let mut frames = proof.response_lines.clone();
    let mut startup: serde_json::Value = serde_json::from_str(&frames[0]).unwrap();
    let result = startup["result"].as_object_mut().unwrap();
    match protocol {
        McpProtocol::Modern20260728 => {
            result.insert("serverInfo".into(), other);
            let meta = result.get_mut("_meta").unwrap().as_object_mut().unwrap();
            if let Some(value) = canonical {
                meta.insert("io.modelcontextprotocol/serverInfo".into(), value);
            } else {
                meta.remove("io.modelcontextprotocol/serverInfo");
            }
        }
        McpProtocol::Legacy20251125 => {
            result.insert(
                "_meta".into(),
                serde_json::json!({"io.modelcontextprotocol/serverInfo":other}),
            );
            if let Some(value) = canonical {
                result.insert("serverInfo".into(), value);
            } else {
                result.remove("serverInfo");
            }
        }
    }
    frames[0] = format!("{startup}\n");
    let mut session = McpSession::new(proof.probe.clone()).unwrap();
    for frame in frames {
        // Legacy initialization adds a notification before its tools request.
        for _ in 0..2 {
            let bytes = session.pending_write().len();
            if bytes != 0 {
                session.written(bytes).unwrap();
            }
        }
        if session.wants_close() {
            break;
        }
        session.receive(frame.as_bytes());
    }
    session.eof();
    let evidence = session.finish(McpShutdown::Graceful);
    assert!(evidence.valid());
    let command_at = stage
        .commands
        .iter()
        .position(|c| c.id == source.command_ids[0])
        .unwrap();
    let mut record = stage.commands[command_at].clone();
    record.stdout.as_mut().unwrap().bytes = evidence.stdout_bytes;
    let declaration = theseus::recipe::Check {
        id: source.id.clone(),
        checkpoint: source.checkpoint,
        expectation: theseus::recipe::Expectation::McpStdio {
            probe: proof.probe.clone(),
        },
    };
    let mut evaluated = theseus::checks::evaluate_mcp(
        &declaration,
        &ExecutedMcp {
            command: ExecutedCommand {
                record: record.clone(),
                stdout: evidence.response_lines.concat(),
                stdout_complete: true,
                stdout_utf8: true,
            },
            evidence,
            container_available: true,
        },
    );
    evaluated.command_ids = source.command_ids;
    if evaluated.outcome != CheckOutcome::Passed {
        stage.outcome = StageOutcome::Failed;
    }
    stage.commands[command_at] = record;
    stage.checks[at] = evaluated;
    report.recompute_outcome();
    let root = tempfile::tempdir().unwrap();
    let store = theseus::report::create(&root.path().join("retained")).unwrap();
    store.save(&report).unwrap();
    theseus::report::load(store.root()).unwrap()
}

#[test]
fn canonical_observed_identity_follows_explicit_era_despite_conflicting_other_namespace() {
    use theseus::{
        mcp::McpProtocol,
        model::{CheckOutcome, RunOutcome},
    };
    for protocol in [McpProtocol::Modern20260728, McpProtocol::Legacy20251125] {
        let report = retained_identity_report(
            protocol,
            Some(serde_json::json!({"name":"fixture","version":"2"})),
            serde_json::json!({"name":"other-era-decoy","version":"9.9.9"}),
        );
        assert_eq!(report.aggregate_outcome(), RunOutcome::Passed);
        let check = report.scenarios[0]
            .stages
            .iter()
            .flat_map(|s| &s.checks)
            .find(|c| {
                c.mcp
                    .as_ref()
                    .is_some_and(|p| p.probe.server_version == "2")
            })
            .unwrap();
        assert_eq!(check.outcome, CheckOutcome::Passed);
        assert!(check.mcp.as_ref().unwrap().verified_pass());
        let text = Record::Check(check).text();
        assert!(
            text.contains("Observed server: name=\"fixture\" version=\"2\""),
            "{text}"
        );
        assert!(
            !text.contains("other-era-decoy") && !text.contains("9.9.9"),
            "{text}"
        );
        assert!(!text.contains("\"jsonrpc\""));
    }
}

#[test]
fn canonical_identity_mismatch_is_observed_without_expected_or_other_era_substitution() {
    use theseus::{mcp::McpProtocol, model::CheckOutcome};
    for protocol in [McpProtocol::Modern20260728, McpProtocol::Legacy20251125] {
        let report = retained_identity_report(
            protocol,
            Some(serde_json::json!({"name":"actually-observed","version":"7.8.9"})),
            serde_json::json!({"name":"fixture","version":"2"}),
        );
        let check = report.scenarios[0]
            .stages
            .iter()
            .flat_map(|s| &s.checks)
            .find(|c| {
                c.mcp
                    .as_ref()
                    .is_some_and(|p| p.probe.server_version == "2")
            })
            .unwrap();
        assert_eq!(check.outcome, CheckOutcome::Failed);
        let text = Record::Check(check).text();
        assert!(
            text.contains("Observed server: name=\"actually-observed\" version=\"7.8.9\""),
            "{text}"
        );
        assert!(
            !text.contains("Observed server: name=\"fixture\""),
            "{text}"
        );
    }
}

#[test]
fn absent_or_malformed_canonical_identity_never_falls_back_to_other_era() {
    use theseus::{mcp::McpProtocol, model::CheckOutcome};
    for protocol in [McpProtocol::Modern20260728, McpProtocol::Legacy20251125] {
        for canonical in [
            None,
            Some(serde_json::Value::Null),
            Some(serde_json::json!({"name":false,"version":"partial-canonical-version"})),
        ] {
            let partial = canonical.as_ref().is_some_and(|v| v.is_object());
            let report = retained_identity_report(
                protocol,
                canonical,
                serde_json::json!({"name":"other-era-decoy","version":"9.9.9"}),
            );
            let check = report.scenarios[0]
                .stages
                .iter()
                .flat_map(|s| &s.checks)
                .find(|c| {
                    c.mcp
                        .as_ref()
                        .is_some_and(|p| p.probe.server_version == "2")
                })
                .unwrap();
            assert_eq!(check.outcome, CheckOutcome::Failed);
            let text = Record::Check(check).text();
            assert!(
                !text.contains("other-era-decoy") && !text.contains("9.9.9"),
                "{text}"
            );
            assert!(!text.contains("Observed server: name=\"fixture\""));
            assert!(!text.contains("\"jsonrpc\""));
            if partial {
                assert!(text.contains("partial-canonical-version"), "{text}");
            }
        }
    }
}
