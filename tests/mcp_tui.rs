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
    let (run, _) = fixture::run("");
    let mut app = App::offline(run.report, std::path::PathBuf::from("/tmp/unused-mcp-ui"));
    app.stage_index = app.report.as_ref().unwrap().scenarios[0]
        .stages
        .iter()
        .position(|s| s.checks.iter().any(|c| c.mcp_required))
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
        assert!(text.contains("2026-07-28"), "{text}");
        assert!(text.contains("hello_v2"), "{text}");
        assert!(!text.contains("\"jsonrpc\""));
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
    let output = std::process::Command::new("python3")
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
