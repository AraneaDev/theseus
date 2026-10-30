use serde_json::{Value, json};
use std::collections::BTreeMap;
use theseus::mcp::{McpIssueKind, McpProbe, McpProtocol, McpSession, McpShutdown};
fn probe() -> McpProbe {
    McpProbe {
        argv: vec!["/inputs/target/server".into()],
        env: BTreeMap::new(),
        timeout_seconds: 15,
        protocol: McpProtocol::Modern20260728,
        server_name: "fixture".into(),
        server_version: "2".into(),
        tools: vec!["hello_v2".into()],
    }
}
fn sent(s: &mut McpSession) -> Value {
    let bytes = s.pending_write().to_vec();
    assert!(!bytes.is_empty());
    s.written(bytes.len()).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
fn reply(s: &mut McpSession, value: Value) {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    for part in bytes.chunks(3) {
        s.receive(part);
    }
}
#[test]
fn actual_response_inventory_is_required_not_expected_requests() {
    let mut s = McpSession::new(probe()).unwrap();
    let discover = sent(&mut s);
    assert_eq!(discover["method"], "server/discover");
    assert!(discover["id"].is_string());
    reply(
        &mut s,
        json!({"jsonrpc":"2.0","id":discover["id"],"result":{
        "resultType":"complete","supportedVersions":["2026-07-28"],
        "capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"public",
        "_meta":{"io.modelcontextprotocol/serverInfo":{"name":"fixture","version":"2"}}}}),
    );
    let list = sent(&mut s);
    assert_eq!(list["method"], "tools/list");
    assert_eq!(list["id"], 1);
    assert_eq!(
        list["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
        "2026-07-28"
    );
    reply(
        &mut s,
        json!({"jsonrpc":"2.0","id":1,"result":{
        "resultType":"complete","tools":[{"name":"WRONG","inputSchema":{"type":"object"}}],
        "ttlMs":0,"cacheScope":"public"}}),
    );
    assert!(s.wants_close());
    s.eof();
    let evidence = s.finish(McpShutdown::Graceful);
    assert!(!evidence.verified_pass());
    assert_eq!(evidence.issue.unwrap().kind, McpIssueKind::Inventory);
}

fn startup(s: &mut McpSession) {
    let req = sent(s);
    if req["method"] == "initialize" {
        reply(
            s,
            json!({"jsonrpc":"2.0","id":req["id"],"result":{
            "protocolVersion":"2025-11-25","capabilities":{"tools":{}},
            "serverInfo":{"name":"fixture","version":"2"}}}),
        );
        assert_eq!(sent(s)["method"], "notifications/initialized");
    } else {
        reply(s, discovery(req["id"].clone()));
    }
}
fn discovery(id: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":{
        "resultType":"complete","supportedVersions":["2026-07-28"],
        "capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"public",
        "_meta":{"io.modelcontextprotocol/serverInfo":{"name":"fixture","version":"2"}}}})
}
fn page(id: Value, names: &[&str], cursor: Option<&str>) -> Value {
    let mut value = json!({"jsonrpc":"2.0","id":id,"result":{
        "resultType":"complete","tools":names.iter().map(|n| json!({"name":n,"inputSchema":{"type":"object"}})).collect::<Vec<_>>(),
        "ttlMs":0,"cacheScope":"public"}});
    if let Some(cursor) = cursor {
        value["result"]["nextCursor"] = json!(cursor);
    }
    value
}
fn complete() -> theseus::mcp::McpEvidence {
    let mut s = McpSession::new(probe()).unwrap();
    startup(&mut s);
    let req = sent(&mut s);
    reply(&mut s, page(req["id"].clone(), &["hello_v2"], None));
    assert!(s.wants_close());
    s.eof();
    s.finish(McpShutdown::Graceful)
}
fn issue(s: McpSession, expected: McpIssueKind, outcome: theseus::model::CheckOutcome) {
    let e = s.finish(McpShutdown::Graceful);
    assert_eq!(e.issue.as_ref().map(|i| i.kind), Some(expected));
    assert_eq!(e.protocol_outcome(), outcome);
    assert!(!e.verified_pass());
}
#[test]
fn valid_actual_named_inventory_replays_and_detects_tampering() {
    let e = complete();
    assert!(e.valid());
    assert!(e.verified_pass());
    assert_eq!(e.protocol_outcome(), theseus::model::CheckOutcome::Passed);
    let mut changed = e.clone();
    changed.probe.tools = vec!["different".into()];
    assert!(!changed.verified_pass());
    let mut changed = e.clone();
    changed.response_lines[1] = changed.response_lines[1].replace("hello_v2", "wrong");
    assert!(!changed.verified_pass());
    let mut changed = e.clone();
    changed.sent_ids.swap(0, 1);
    assert!(!changed.verified_pass());
    let mut changed = e.clone();
    changed.stdout_bytes = 0;
    assert!(!changed.valid());
    let mut changed = e.clone();
    changed.shutdown = McpShutdown::Unverified;
    assert!(!changed.verified_pass());
    let mut changed = e;
    changed.stdout_complete = false;
    assert!(!changed.verified_pass());
}
#[test]
fn legacy_requires_initialized_fully_written_before_tools() {
    let mut p = probe();
    p.protocol = McpProtocol::Legacy20251125;
    let mut s = McpSession::new(p).unwrap();
    let req = sent(&mut s);
    assert_eq!(req["method"], "initialize");
    assert_eq!(req["params"]["protocolVersion"], "2025-11-25");
    reply(
        &mut s,
        json!({"jsonrpc":"2.0","id":req["id"],"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"2"}}}),
    );
    let pending = s.pending_write().to_vec();
    s.written(1).unwrap();
    assert_eq!(s.pending_write(), &pending[1..]);
    s.written(pending.len() - 1).unwrap();
    let req = sent(&mut s);
    assert_eq!(req["id"], 1);
    assert!(req["params"].get("_meta").is_none());
    reply(
        &mut s,
        json!({"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"hello_v2","inputSchema":{"type":"object"}}]}}),
    );
    s.eof();
    assert!(s.finish(McpShutdown::Graceful).verified_pass());
}
#[test]
fn rpc_ids_preserve_unicode_string_and_integer_types() {
    use theseus::mcp::RpcId;
    let id: RpcId = serde_json::from_str(r#""a\u2603\ud83d\ude00""#).unwrap();
    assert_eq!(id, RpcId::String("a☃😀".into()));
    assert_eq!(
        serde_json::from_str::<RpcId>(&serde_json::to_string(&id).unwrap()).unwrap(),
        id
    );
    assert_ne!(
        serde_json::from_str::<RpcId>("1").unwrap(),
        RpcId::String("1".into())
    );
    for raw in ["null", "true", "1.0", "18446744073709551615"] {
        assert!(serde_json::from_str::<RpcId>(raw).is_err(), "{raw}");
    }
}
#[test]
fn duplicate_keys_are_protocol_failure_even_when_last_value_looks_valid() {
    let mut s = McpSession::new(probe()).unwrap();
    let _ = sent(&mut s);
    s.receive(
        b"{\"jsonrpc\":\"2.0\",\"id\":\"wrong\",\"id\":\"theseus-discover-1\",\"result\":{}}\n",
    );
    s.eof();
    issue(
        s,
        McpIssueKind::Protocol,
        theseus::model::CheckOutcome::Failed,
    );
}
#[test]
fn startup_rejects_wrong_ids_identity_versions_and_modern_shapes() {
    let good = discovery(json!("theseus-discover-1"));
    let mut cases = Vec::new();
    for id in [json!(1), json!("wrong"), json!(null), json!(1.5)] {
        let mut v = good.clone();
        v["id"] = id;
        cases.push(v);
    }
    for (pointer, value) in [
        (
            "/result/_meta/io.modelcontextprotocol~1serverInfo/name",
            json!("wrong"),
        ),
        (
            "/result/_meta/io.modelcontextprotocol~1serverInfo/version",
            json!("wrong"),
        ),
        ("/result/supportedVersions", json!(["2025-11-25"])),
        ("/result/ttlMs", json!(-1)),
        ("/result/cacheScope", json!("unknown")),
        ("/result/resultType", json!("partial")),
        ("/result/capabilities/tools", json!(true)),
    ] {
        let mut v = good.clone();
        *v.pointer_mut(pointer).unwrap() = value;
        cases.push(v);
    }
    for key in [
        "resultType",
        "ttlMs",
        "cacheScope",
        "supportedVersions",
        "capabilities",
        "_meta",
    ] {
        let mut v = good.clone();
        v["result"].as_object_mut().unwrap().remove(key);
        cases.push(v);
    }
    for value in cases {
        let mut s = McpSession::new(probe()).unwrap();
        sent(&mut s);
        reply(&mut s, value);
        s.eof();
        issue(
            s,
            McpIssueKind::Protocol,
            theseus::model::CheckOutcome::Failed,
        );
    }
}
#[test]
fn complete_invalid_frames_fail_but_partial_frames_are_incomplete() {
    for bytes in [
        b"{bad}\n".to_vec(),
        vec![0xff, b'\n'],
        format!("{}0{}\n", "[".repeat(65), "]".repeat(65)).into_bytes(),
    ] {
        let mut s = McpSession::new(probe()).unwrap();
        sent(&mut s);
        s.receive(&bytes);
        s.eof();
        issue(
            s,
            McpIssueKind::Protocol,
            theseus::model::CheckOutcome::Failed,
        );
    }
    for bytes in [vec![0xe2, 0x98], b"{\"jsonrpc\":".to_vec(), vec![]] {
        let mut s = McpSession::new(probe()).unwrap();
        sent(&mut s);
        s.receive(&bytes);
        s.eof();
        issue(
            s,
            McpIssueKind::Transport,
            theseus::model::CheckOutcome::Incomplete,
        );
    }
}
#[test]
fn premature_reply_and_trailing_duplicate_are_failures() {
    let mut s = McpSession::new(probe()).unwrap();
    s.written(1).unwrap();
    reply(&mut s, discovery(json!("theseus-discover-1")));
    s.eof();
    issue(
        s,
        McpIssueKind::Protocol,
        theseus::model::CheckOutcome::Failed,
    );
    let mut s = McpSession::new(probe()).unwrap();
    startup(&mut s);
    let req = sent(&mut s);
    let value = page(req["id"].clone(), &["hello_v2"], None);
    reply(&mut s, value.clone());
    reply(&mut s, value);
    s.eof();
    issue(
        s,
        McpIssueKind::Protocol,
        theseus::model::CheckOutcome::Failed,
    );
}
#[test]
fn pagination_accumulates_exact_inventory_and_rejects_repeated_cursor() {
    let mut p = probe();
    p.tools.push("other".into());
    let mut s = McpSession::new(p).unwrap();
    startup(&mut s);
    let req = sent(&mut s);
    reply(&mut s, page(req["id"].clone(), &["other"], Some("next")));
    let req = sent(&mut s);
    assert_eq!(req["params"]["cursor"], "next");
    assert_eq!(req["id"], 2);
    reply(&mut s, page(req["id"].clone(), &["hello_v2"], None));
    s.eof();
    assert!(s.finish(McpShutdown::Graceful).verified_pass());
    let mut s = McpSession::new(probe()).unwrap();
    startup(&mut s);
    let req = sent(&mut s);
    reply(&mut s, page(req["id"].clone(), &[], Some("next")));
    let req = sent(&mut s);
    reply(&mut s, page(req["id"].clone(), &[], Some("next")));
    s.eof();
    issue(
        s,
        McpIssueKind::Protocol,
        theseus::model::CheckOutcome::Failed,
    );
}
#[test]
fn invalid_tools_and_duplicate_names_fail_before_set_comparison() {
    for tools in [
        json!([{"name":"hello_v2","inputSchema":{"type":"array"}}]),
        json!([{"name":"","inputSchema":{"type":"object"}}]),
        json!([{"name":"hello_v2","inputSchema":{"type":"object"}},{"name":"hello_v2","inputSchema":{"type":"object"}}]),
    ] {
        let mut s = McpSession::new(probe()).unwrap();
        startup(&mut s);
        let req = sent(&mut s);
        let mut value = page(req["id"].clone(), &[], None);
        value["result"]["tools"] = tools;
        reply(&mut s, value);
        s.eof();
        issue(
            s,
            McpIssueKind::Protocol,
            theseus::model::CheckOutcome::Failed,
        );
    }
}
#[test]
fn acquisition_limits_remain_incomplete_and_drain_all_bytes() {
    let mut s = McpSession::new(probe()).unwrap();
    sent(&mut s);
    s.receive(&vec![b'x'; 65537]);
    s.receive(&vec![b'x'; 262144]);
    s.eof();
    let e = s.finish(McpShutdown::Graceful);
    assert_eq!(e.stdout_bytes, 327681);
    assert_eq!(e.issue.as_ref().unwrap().kind, McpIssueKind::Limit);
    assert_eq!(
        e.protocol_outcome(),
        theseus::model::CheckOutcome::Incomplete
    );
    let mut s = McpSession::new(probe()).unwrap();
    sent(&mut s);
    for _ in 0..257 {
        reply(
            &mut s,
            json!({"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info","data":"hello"}}),
        );
    }
    s.eof();
    issue(
        s,
        McpIssueKind::Limit,
        theseus::model::CheckOutcome::Incomplete,
    );
    let mut s = McpSession::new(probe()).unwrap();
    startup(&mut s);
    for n in 0..8 {
        let req = sent(&mut s);
        reply(
            &mut s,
            page(req["id"].clone(), &[], Some(&format!("cursor-{n}"))),
        );
    }
    s.eof();
    issue(
        s,
        McpIssueKind::Limit,
        theseus::model::CheckOutcome::Incomplete,
    );
    let mut s = McpSession::new(probe()).unwrap();
    startup(&mut s);
    let req = sent(&mut s);
    let names = (0..129).map(|n| format!("tool{n}")).collect::<Vec<_>>();
    let refs = names.iter().map(String::as_str).collect::<Vec<_>>();
    reply(&mut s, page(req["id"].clone(), &refs, None));
    s.eof();
    issue(
        s,
        McpIssueKind::Limit,
        theseus::model::CheckOutcome::Incomplete,
    );
}
#[test]
fn cancelled_legacy_initialize_closes_without_cancel_notification() {
    let mut p = probe();
    p.protocol = McpProtocol::Legacy20251125;
    let mut s = McpSession::new(p).unwrap();
    sent(&mut s);
    s.cancel();
    assert!(s.pending_write().is_empty());
    assert!(s.wants_close());
    s.eof();
    issue(
        s,
        McpIssueKind::Transport,
        theseus::model::CheckOutcome::Incomplete,
    );
}
#[test]
fn probe_defaults_validation_and_persisted_fields_are_strict() {
    let mut v = serde_json::to_value(probe()).unwrap();
    v.as_object_mut().unwrap().remove("env");
    v.as_object_mut().unwrap().remove("timeout_seconds");
    let p: McpProbe = serde_json::from_value(v.clone()).unwrap();
    assert!(p.env.is_empty());
    assert_eq!(p.timeout_seconds, 15);
    for key in ["argv", "protocol", "server_name", "server_version", "tools"] {
        let mut v = v.clone();
        v.as_object_mut().unwrap().remove(key);
        assert!(serde_json::from_value::<McpProbe>(v).is_err());
    }
    v["extra"] = json!(1);
    assert!(serde_json::from_value::<McpProbe>(v).is_err());
    for argv in [
        vec![],
        vec!["server".into()],
        vec!["/a/../server".into()],
        vec!["/a//server".into()],
        vec!["/server\0".into()],
    ] {
        let mut p = probe();
        p.argv = argv;
        assert!(p.validate().is_err());
    }
    for timeout in [0, 2, 121] {
        let mut p = probe();
        p.timeout_seconds = timeout;
        assert!(p.validate().is_err());
    }
    let mut p = probe();
    p.env.insert("bad-name".into(), "x".into());
    assert!(p.validate().is_err());
    let mut p = probe();
    p.tools.push("hello_v2".into());
    assert!(p.validate().is_err());
}

#[test]
fn received_text_bounds_are_limits_but_empty_values_are_protocol_errors() {
    for len in [128, 129] {
        let mut p = probe();
        p.tools = vec!["t".repeat(128)];
        let mut s = McpSession::new(p).unwrap();
        startup(&mut s);
        let req = sent(&mut s);
        reply(&mut s, page(req["id"].clone(), &[&"t".repeat(len)], None));
        s.eof();
        if len == 128 {
            assert!(s.finish(McpShutdown::Graceful).verified_pass());
        } else {
            issue(
                s,
                McpIssueKind::Limit,
                theseus::model::CheckOutcome::Incomplete,
            );
        }
    }
    for len in [256, 257] {
        let mut s = McpSession::new(probe()).unwrap();
        startup(&mut s);
        let req = sent(&mut s);
        reply(&mut s, page(req["id"].clone(), &[], Some(&"c".repeat(len))));
        if len == 256 {
            assert!(!s.pending_write().is_empty());
        } else {
            s.eof();
            issue(
                s,
                McpIssueKind::Limit,
                theseus::model::CheckOutcome::Incomplete,
            );
        }
    }
    let mut s = McpSession::new(probe()).unwrap();
    let req = sent(&mut s);
    let mut d = discovery(req["id"].clone());
    d["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"] = json!("s".repeat(257));
    reply(&mut s, d);
    s.eof();
    issue(
        s,
        McpIssueKind::Limit,
        theseus::model::CheckOutcome::Incomplete,
    );
}
#[test]
fn legacy_optional_metadata_and_capabilities_are_typed() {
    for field in ["_meta", "instructions", "logging"] {
        let mut p = probe();
        p.protocol = McpProtocol::Legacy20251125;
        let mut s = McpSession::new(p).unwrap();
        let req = sent(&mut s);
        let mut value = json!({"jsonrpc":"2.0","id":req["id"],"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"2"}}});
        if field == "logging" {
            value["result"]["capabilities"][field] = json!(false);
        } else {
            value["result"][field] = json!(false);
        }
        reply(&mut s, value);
        s.eof();
        issue(
            s,
            McpIssueKind::Protocol,
            theseus::model::CheckOutcome::Failed,
        );
    }
}
#[test]
fn notifications_are_bounded_typed_and_cannot_substitute_for_responses() {
    let mut s = McpSession::new(probe()).unwrap();
    sent(&mut s);
    reply(
        &mut s,
        json!({"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info","data":"☃"}}),
    );
    reply(
        &mut s,
        json!({"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":"task","progress":1,"total":2}}),
    );
    reply(&mut s, discovery(json!("theseus-discover-1")));
    let req = sent(&mut s);
    reply(&mut s, page(req["id"].clone(), &["hello_v2"], None));
    s.eof();
    let e = s.finish(McpShutdown::Graceful);
    assert_eq!(e.notification_count, 2);
    assert_eq!(e.response_lines.len(), 2);
    assert!(e.verified_pass());
    for message in [
        json!({"jsonrpc":"2.0","id":"server-call","method":"tools/call","params":{}}),
        json!({"jsonrpc":"2.0","method":"unknown","params":{}}),
        json!({"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":true,"progress":1}}),
    ] {
        let mut s = McpSession::new(probe()).unwrap();
        sent(&mut s);
        reply(&mut s, message);
        s.eof();
        issue(
            s,
            McpIssueKind::Protocol,
            theseus::model::CheckOutcome::Failed,
        );
    }
}
#[test]
fn cancellation_of_tools_preserves_id_without_corrupting_partial_write() {
    let mut s = McpSession::new(probe()).unwrap();
    startup(&mut s);
    let req = sent(&mut s);
    s.cancel();
    let cancel = sent(&mut s);
    assert_eq!(cancel["method"], "notifications/cancelled");
    assert_eq!(cancel["params"]["requestId"], req["id"]);
    assert!(s.wants_close());
    let mut s = McpSession::new(probe()).unwrap();
    startup(&mut s);
    s.written(1).unwrap();
    s.cancel();
    assert!(s.pending_write().is_empty());
    assert!(s.wants_close());
}
#[test]
fn exact_frame_budget_and_tool_count_can_pass() {
    let mut s = McpSession::new(probe()).unwrap();
    let req = sent(&mut s);
    let d = discovery(req["id"].clone());
    let mut frame = serde_json::to_vec(&d).unwrap();
    frame.resize(65535, b' ');
    frame.push(b'\n');
    s.receive(&frame);
    let req = sent(&mut s);
    reply(&mut s, page(req["id"].clone(), &["hello_v2"], None));
    s.eof();
    assert!(s.finish(McpShutdown::Graceful).verified_pass());
    let names = (0..128).map(|n| format!("tool{n}")).collect::<Vec<_>>();
    let mut p = probe();
    p.tools = names.clone();
    let mut s = McpSession::new(p).unwrap();
    startup(&mut s);
    let req = sent(&mut s);
    reply(
        &mut s,
        page(
            req["id"].clone(),
            &names.iter().map(String::as_str).collect::<Vec<_>>(),
            None,
        ),
    );
    s.eof();
    assert!(s.finish(McpShutdown::Graceful).verified_pass());
}
#[test]
fn diagnostics_are_bounded_first_issue_survives_later_drain_and_shutdown_gap() {
    let mut s = McpSession::new(probe()).unwrap();
    sent(&mut s);
    s.receive(b"not-json\n");
    s.receive(&vec![b'x'; 300000]);
    s.eof();
    let e = s.finish(McpShutdown::ForcedOwnedRemoval);
    assert_eq!(e.issue.as_ref().unwrap().kind, McpIssueKind::Protocol);
    assert!(e.issue.as_ref().unwrap().message.len() <= 512);
    assert!(e.valid());
    assert_eq!(
        e.protocol_outcome(),
        theseus::model::CheckOutcome::Incomplete
    );
}

#[test]
fn success_proof_rejects_nonresponse_frames_and_unaccounted_bytes() {
    let e = complete();
    let mut changed = e.clone();
    changed.stdout_bytes += 1;
    assert!(!changed.valid());
    assert!(!changed.verified_pass());
    let notification = "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\",\"params\":{\"level\":\"info\",\"data\":0}}\n";
    let mut changed = e.clone();
    changed.response_lines.insert(0, notification.into());
    changed.stdout_bytes += notification.len() as u64;
    assert!(!changed.valid());
    let mut changed = e;
    changed.notification_count = 1;
    changed.stdout_bytes += 1;
    assert!(!changed.valid());
}

#[test]
fn opaque_escaped_nul_cursor_roundtrips_and_complete_inventory_passes() {
    // Rejecting or changing an opaque cursor must prevent this second-page success.
    for protocol in [McpProtocol::Modern20260728, McpProtocol::Legacy20251125] {
        let mut p = probe();
        p.protocol = protocol;
        let mut s = McpSession::new(p).unwrap();
        startup(&mut s);
        let first = sent(&mut s);
        let mut first_page = page(first["id"].clone(), &[], Some("\0"));
        if protocol == McpProtocol::Legacy20251125 {
            for field in ["resultType", "ttlMs", "cacheScope"] {
                first_page["result"].as_object_mut().unwrap().remove(field);
            }
        }
        reply(&mut s, first_page);
        let pending = s.pending_write();
        assert!(!pending.is_empty(), "opaque cursor must queue page two");
        let wire = std::str::from_utf8(pending).unwrap();
        assert!(wire.contains(r#""cursor":"\u0000""#));
        assert!(!pending.contains(&0), "cursor NUL must be JSON-escaped");
        let second = sent(&mut s);
        assert_eq!(second["params"]["cursor"], json!("\0"));
        assert_eq!(second["id"], 2);
        let mut final_page = page(second["id"].clone(), &["hello_v2"], None);
        if protocol == McpProtocol::Legacy20251125 {
            for field in ["resultType", "ttlMs", "cacheScope"] {
                final_page["result"].as_object_mut().unwrap().remove(field);
            }
        }
        reply(&mut s, final_page);
        s.eof();
        let evidence = s.finish(McpShutdown::Graceful);
        assert!(evidence.verified_pass());
        assert_eq!(
            evidence.protocol_outcome(),
            theseus::model::CheckOutcome::Passed
        );
    }
}

#[test]
fn repeated_opaque_nul_cursor_is_a_protocol_failure() {
    let mut s = McpSession::new(probe()).unwrap();
    startup(&mut s);
    let first = sent(&mut s);
    reply(&mut s, page(first["id"].clone(), &[], Some("\0")));
    assert!(
        !s.pending_write().is_empty(),
        "first opaque cursor must be accepted"
    );
    let second = sent(&mut s);
    reply(&mut s, page(second["id"].clone(), &[], Some("\0")));
    s.eof();
    issue(
        s,
        McpIssueKind::Protocol,
        theseus::model::CheckOutcome::Failed,
    );
}
