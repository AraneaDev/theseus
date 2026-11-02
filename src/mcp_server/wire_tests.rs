use super::*;
use serde_json::{Value, json};

// Versioned sources, independent of the guest probe:
// https://modelcontextprotocol.io/specification/2026-07-28/basic/index
// https://modelcontextprotocol.io/specification/2026-07-28/server/discover
// https://modelcontextprotocol.io/specification/2026-07-28/server/tools
// https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle
// https://modelcontextprotocol.io/specification/2025-11-25/basic/utilities/cancellation
fn modern(method: &str, mut params: Value) -> Vec<u8> {
    params["_meta"] = json!({"io.modelcontextprotocol/protocolVersion":"2026-07-28", "io.modelcontextprotocol/clientCapabilities":{}});
    serde_json::to_vec(&json!({"jsonrpc":"2.0", "id":"a", "method":method, "params":params}))
        .unwrap()
}
#[test]
fn modern_direct_call_selects_era_and_rejects_legacy_initialize() {
    let mut state = WireState::default();
    assert!(matches!(
        state.decode(&modern("tools/list", json!({}))),
        WireEvent::Request(Request {
            era: Era::Modern,
            method: Method::ToolsList,
            ..
        })
    ));
    assert_eq!(state.selected, Some(Era::Modern));
    assert!(matches!(
        state.decode(br#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{}}"#),
        WireEvent::ProtocolError(ProtocolError { code: -32601, .. })
    ));
}
#[test]
fn schema_listing_has_all_six_tools_with_complete_schemas() {
    let list = super::super::schema::schemas();
    let tools = list["tools"].as_array().unwrap();
    assert_eq!(
        tools
            .iter()
            .map(|t| t["name"].as_str().unwrap())
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
    assert!(serde_json::to_vec(&list).unwrap().len() < 65_000);
    for tool in tools {
        assert_eq!(
            tool["inputSchema"]["$schema"],
            "https://json-schema.org/draft/2020-12/schema"
        );
        assert_eq!(tool["outputSchema"]["type"], "object");
        assert!(tool["outputSchema"]["oneOf"].is_array());
    }
}
#[test]
fn identity_grammar_rejects_noncanonical_and_foreign_forms() {
    for text in [
        "",
        "A0000000000000000000000000000000",
        "000000000000000000000000000000000",
    ] {
        assert!(Instance::parse(text).is_err(), "{text}");
    }
    for text in [
        "00000000000000000000000000000000:0",
        "00000000000000000000000000000000:01",
        "00000000000000000000000000000000:129",
    ] {
        assert!(Handle::parse(text).is_err(), "{text}");
    }
    for text in [
        "s/01",
        "s/0/stage/0/snapshot/0/entry/01",
        "../run",
        "p/user/step/bogus",
        "artifact/0/file/00",
    ] {
        assert!(RecordId::parse(text).is_err(), "{text}");
    }
    for text in [
        "run",
        "s/0/stage/0/snapshot/0/entry/1",
        "p/user/step/install_target",
        "p/system/policy/interrupted_rollback",
        "artifact/base",
    ] {
        assert!(RecordId::parse(text).is_ok(), "{text}");
    }
}
#[test]
fn previews_cut_only_at_character_boundaries_and_policy_is_fixed() {
    let p = ShortPreview::new(&"é".repeat(65), true);
    assert_eq!(p.text.len(), 128);
    assert!(p.truncated && p.lossy);
    assert_eq!(
        ErrorCode::QueryTimeout.policy(),
        ErrorPolicy {
            retryable: true,
            restart_pagination: false
        }
    );
    assert_eq!(
        ErrorCode::RevisionChanged.policy(),
        ErrorPolicy {
            retryable: false,
            restart_pagination: true
        }
    );
}
fn code(event: WireEvent) -> i32 {
    match event {
        WireEvent::ProtocolError(e) => e.code,
        other => panic!("expected protocol error: {other:?}"),
    }
}
fn init(state: &mut WireState) {
    assert!(matches!(state.decode(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1900-01-01","capabilities":{},"clientInfo":{"name":"client","version":"1"}}}"#), WireEvent::Request(Request { method: Method::Initialize, .. })));
}
#[test]
fn legacy_handshake_requires_initialized_and_ping_does_not_select_era() {
    let mut state = WireState::default();
    assert!(matches!(
        state.decode(br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#),
        WireEvent::Request(Request {
            method: Method::Ping,
            ..
        })
    ));
    assert_eq!(state.selected, None);
    assert_eq!(
        code(state.decode(br#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#)),
        -32602
    );
    init(&mut state);
    assert!(!state.legacy_initialized);
    assert_eq!(
        code(state.decode(br#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#)),
        -32602
    );
    assert!(matches!(
        state.decode(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
        WireEvent::Notification(Notification::Initialized)
    ));
    assert!(matches!(
        state.decode(br#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#),
        WireEvent::Request(Request {
            era: Era::Legacy,
            method: Method::ToolsList,
            ..
        })
    ));
    assert_eq!(code(state.decode(&modern("tools/list", json!({})))), -32602);
}
#[test]
fn modern_metadata_is_revalidated_and_unsupported_version_selects_modern() {
    let mut state = WireState::default();
    let mut v: Value = serde_json::from_slice(&modern("server/discover", json!({}))).unwrap();
    v["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] = json!("2099-01-01");
    match state.decode(&serde_json::to_vec(&v).unwrap()) {
        WireEvent::ProtocolError(e) => {
            assert_eq!(e.code, -32022);
            assert_eq!(e.data.unwrap().requested, "2099-01-01");
        }
        e => panic!("{e:?}"),
    }
    assert_eq!(state.selected, Some(Era::Modern));
    assert!(matches!(
        state.decode(&modern("server/discover", json!({}))),
        WireEvent::Request(_)
    ));
    assert_eq!(
        code(state.decode(br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)),
        -32602
    );
    for capabilities in [
        Value::Null,
        json!([]),
        json!({"roots":{"listChanged":"yes"}}),
    ] {
        v["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] = json!(MODERN_VERSION);
        v["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"] = capabilities;
        assert_eq!(code(state.decode(&serde_json::to_vec(&v).unwrap())), -32602);
    }
}
#[test]
fn ids_are_exact_signed_integers_or_bounded_strings() {
    for id in [
        "-9223372036854775808",
        "9223372036854775807",
        "\"é\"",
        "\"\"",
    ] {
        let frame = format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"ping"}}"#);
        assert!(
            matches!(
                WireState::default().decode(frame.as_bytes()),
                WireEvent::Request(_)
            ),
            "{id}"
        );
    }
    for id in [
        "null",
        "1.0",
        "1e0",
        "9223372036854775808",
        "-9223372036854775809",
        "true",
    ] {
        let frame = format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"ping"}}"#);
        match WireState::default().decode(frame.as_bytes()) {
            WireEvent::ProtocolError(e) => {
                assert_eq!(e.code, -32600);
                assert_eq!(e.id, ReplyId::Null);
                assert!(e.close_after_send);
            }
            e => panic!("accepted {id}: {e:?}"),
        }
    }
    let frame =
        serde_json::to_vec(&json!({"jsonrpc":"2.0","id":"é".repeat(65),"method":"ping"})).unwrap();
    assert_eq!(code(WireState::default().decode(&frame)), -32600);
}
#[test]
fn malformed_frames_close_with_era_correct_unreadable_id_policy() {
    let mut state = WireState::default();
    for frame in [
        b"[{}]".as_slice(),
        br#"{"jsonrpc":"2.0","id":1,"id":2,"method":"ping"}"#,
        &[0xff],
    ] {
        assert!(matches!(
            state.decode(frame),
            WireEvent::ProtocolError(ProtocolError {
                close_after_send: true,
                id: ReplyId::Null,
                ..
            })
        ));
    }
    state.decode(&modern("ping", json!({})));
    assert!(matches!(
        state.decode(b"{"),
        WireEvent::ProtocolError(ProtocolError {
            code: -32700,
            id: ReplyId::Omit,
            ..
        })
    ));
    for depth in [64, 65] {
        let frame = format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
        assert_eq!(
            code(WireState::default().decode(frame.as_bytes())),
            if depth == 64 { -32600 } else { -32700 }
        );
    }
}
#[test]
fn notifications_never_become_tool_calls_and_invalid_cancels_are_ignored() {
    let mut state = WireState::default();
    init(&mut state);
    for frame in [br#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"run_cancel","arguments":{}}}"#.as_slice(), br#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":null}}"#, br#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1,"reason":false}}"#] {
        assert!(matches!(state.decode(frame), WireEvent::Ignore));
    }
    assert!(matches!(
        state.decode(
            br#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}"#
        ),
        WireEvent::Notification(Notification::Cancel(RpcId::Integer(1)))
    ));
    assert!(!state.legacy_initialized);
}
#[test]
fn known_tool_rejections_reserve_normal_requests_but_unknown_tools_are_protocol_errors() {
    let mut state = WireState::default();
    for arguments in [
        json!({}),
        json!({"server_instance":"0".repeat(32),"handle":format!("{}:1", "0".repeat(32)),"surprise":true}),
    ] {
        assert!(matches!(
            state.decode(&modern(
                "tools/call",
                json!({"name":"run_cancel","arguments":arguments})
            )),
            WireEvent::Request(Request {
                method: Method::RejectedTool(_),
                ..
            })
        ));
    }
    assert_eq!(
        code(state.decode(&modern(
            "tools/call",
            json!({"name":"unknown","arguments":{}})
        ))),
        -32602
    );
    assert_eq!(code(state.decode(&modern("unknown", json!({})))), -32601);
    for cursor in [json!(""), Value::Null, json!("cursor")] {
        assert_eq!(
            code(state.decode(&modern("tools/list", json!({"cursor":cursor})))),
            -32602
        );
    }
}
#[test]
fn typed_domain_validation_rejects_selector_mismatch_and_normalizes_defaults() {
    let i = "0".repeat(32);
    let d = "0".repeat(64);
    let valid = json!({"server_instance":i,"request_key":"k","recipe_path":"recipes/a.toml","recipe_digest":d,"profiles":["system","user"],"scenarios":["clean"]});
    let args: StartArgs = serde_json::from_value(valid.clone()).unwrap();
    assert!(!args.pull);
    assert_eq!(args.log_limit.get(), 16_777_216);
    assert!(ToolCall::RunStart(args).validate().is_ok());
    for (key, value) in [
        ("profiles", json!(["user", "user"])),
        ("log_limit", json!(0)),
        ("recipe_path", json!("C:/recipe")),
        ("recipe_path", json!("a\\b")),
        ("scenarios", json!([])),
    ] {
        let mut v = valid.clone();
        v[key] = value;
        assert!(
            ToolCall::RunStart(serde_json::from_value(v).unwrap())
                .validate()
                .is_err()
        );
    }
    for v in [
        json!({"recipe_path":"a","limit":1}),
        json!({"recipe_path":"a","selector":{"kind":"catalog","collection":"steps"}}),
        json!({"recipe_path":"a","recipe_digest":d,"selector":{"kind":"catalog","collection":"artifacts","profile":"user"}}),
    ] {
        assert!(
            ToolCall::RecipeInspect(serde_json::from_value(v).unwrap())
                .validate()
                .is_err()
        );
    }
    assert!(
        serde_json::from_value::<InspectArgs>(json!({"recipe_path":"a","recipe_digest":null}))
            .is_err()
    );
    assert!(
        serde_json::from_value::<EvidenceSelector>(
            json!({"kind":"catalog","collection":"commands","stage_index":0})
        )
        .is_err()
    );
    assert!(serde_json::from_value::<UInt>(json!(9007199254740992_u64)).is_err());
}
#[test]
fn worker_handoffs_are_send_and_query_errors_preserve_original_type() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<crate::report::ReportStore>();
    send_sync::<crate::recipe::ValidatedRecipe>();
    send_sync::<QueryError>();
    #[derive(Debug)]
    struct Original;
    impl std::fmt::Display for Original {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("original")
        }
    }
    impl std::error::Error for Original {}
    let error = QueryError::new(
        ErrorCode::IoError,
        anyhow::Error::new(Original).context("context"),
    );
    assert!(error.source.downcast_ref::<Original>().is_some());
}
#[test]
fn every_tool_decodes_typed_arguments_before_dispatch() {
    let i = "0".repeat(32);
    let h = format!("{i}:1");
    let d = "0".repeat(64);
    let cases = [
        ("recipe_inspect", json!({"recipe_path":"a.toml"})),
        (
            "run_start",
            json!({"server_instance":i,"request_key":"k","recipe_path":"a.toml","recipe_digest":d,"profiles":["user"],"scenarios":["clean"]}),
        ),
        ("run_list", json!({"server_instance":i})),
        ("run_status", json!({"server_instance":i,"handle":h})),
        ("run_cancel", json!({"server_instance":i,"handle":h})),
        (
            "run_evidence",
            json!({"server_instance":i,"handle":h,"selector":{"kind":"catalog","collection":"run"}}),
        ),
    ];
    for (name, arguments) in cases {
        assert!(
            matches!(
                WireState::default().decode(&modern(
                    "tools/call",
                    json!({"name":name,"arguments":arguments})
                )),
                WireEvent::Request(Request {
                    method: Method::ToolCall(_),
                    ..
                })
            ),
            "{name}"
        );
    }
}
#[test]
fn portable_recipe_paths_reject_windows_invalid_names_even_on_unix() {
    for path in [
        "a/CON.toml",
        "a/b.",
        "a/b ",
        "a/b?c",
        "a//b",
        "a/../b",
        "/a",
        "a/\u{1}b",
    ] {
        let args: InspectArgs = serde_json::from_value(json!({"recipe_path":path})).unwrap();
        assert!(ToolCall::RecipeInspect(args).validate().is_err(), "{path}");
    }
    // Opaque cursor grammar is separate from the v1 codec: empty reaches invalid_cursor there.
    assert!(Cursor::parse("").is_ok());
}
#[test]
fn protocol_results_use_only_tools_capability_and_era_correct_metadata() {
    for (era, method) in [
        (Era::Legacy, Method::Initialize),
        (Era::Modern, Method::Discover),
    ] {
        let result = protocol_result(era, &method).unwrap();
        assert_eq!(result["capabilities"], json!({"tools":{}}));
        if era == Era::Legacy {
            assert_eq!(result["protocolVersion"], "2025-11-25");
            assert!(result.get("resultType").is_none());
        } else {
            assert_eq!(result["resultType"], "complete");
            assert_eq!(result["supportedVersions"], json!(["2026-07-28"]));
            assert_eq!(result["ttlMs"], 0);
            assert_eq!(result["cacheScope"], "private");
            assert_eq!(
                result["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
                "theseus"
            );
        }
    }
    let result = protocol_result(Era::Modern, &Method::ToolsList).unwrap();
    assert!(result.get("nextCursor").is_none());
    assert_eq!(result["tools"][0]["annotations"]["readOnlyHint"], true);
    assert_eq!(result["tools"][1]["annotations"]["readOnlyHint"], false);
    assert_eq!(result["tools"][1]["annotations"]["idempotentHint"], false);
    assert_eq!(result["tools"][4]["annotations"]["idempotentHint"], true);
    assert!(
        serde_json::to_vec(&json!({"jsonrpc":"2.0","id":"\u{0}".repeat(128),"result":result}))
            .unwrap()
            .len()
            < 65536
    );
}
#[test]
fn schemas_resolve_locally_and_cover_required_null_and_selector_branches() {
    let list = super::super::schema::schemas();
    fn refs(value: &Value, root: &Value) {
        match value {
            Value::Object(m) => {
                if let Some(s) = m.get("$ref").and_then(Value::as_str) {
                    assert!(root.pointer(&s[1..]).is_some(), "unresolved {s}");
                }
                for v in m.values() {
                    refs(v, root);
                }
            }
            Value::Array(a) => {
                for v in a {
                    refs(v, root);
                }
            }
            _ => {}
        }
    }
    for tool in list["tools"].as_array().unwrap() {
        for key in ["inputSchema", "outputSchema"] {
            refs(&tool[key], &tool[key]);
        }
    }
    let defs = &list["tools"][5]["outputSchema"]["$defs"];
    assert_eq!(
        defs["LogPage"]["required"],
        json!([
            "kind",
            "revision",
            "command_id",
            "stream",
            "offset",
            "bytes",
            "recorded_bytes",
            "current_bytes",
            "readable_total",
            "capture_truncated",
            "change_token",
            "next_cursor"
        ])
    );
    assert_eq!(defs["LogPage"]["properties"]["bytes"]["maxItems"], 4096);
    assert_eq!(
        defs["RecordPage"]["properties"]["bytes"]["items"]["maximum"],
        255
    );
    assert_eq!(defs["FailureError"]["additionalProperties"], false);
    assert_eq!(defs["ErrorCode"]["enum"].as_array().unwrap().len(), 25);
    let defs = &list["tools"][2]["outputSchema"]["$defs"];
    assert_eq!(defs["RunView"]["required"].as_array().unwrap().len(), 14);
    assert_eq!(
        defs["RunView"]["properties"]["saved"]["anyOf"][1],
        json!({"type":"null"})
    );
    assert_eq!(
        list["tools"][5]["inputSchema"]["$defs"]["EvidenceSelector"]["oneOf"]
            .as_array()
            .unwrap()
            .len(),
        7
    );
}
#[test]
fn modern_cancellation_uses_notification_metadata_without_request_requirements() {
    // https://modelcontextprotocol.io/specification/2026-07-28/schema#cancellednotificationparams
    let mut state = WireState::default();
    state.decode(&modern("ping", json!({})));
    for params in [
        json!({"requestId":"a"}),
        json!({"requestId":"a","_meta":{"com.example/extra":true}}),
    ] {
        let frame = serde_json::to_vec(
            &json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":params}),
        )
        .unwrap();
        assert!(matches!(
            state.decode(&frame),
            WireEvent::Notification(Notification::Cancel(RpcId::String(_)))
        ));
    }
    assert!(matches!(state.decode(br#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1,"_meta":false}}"#),WireEvent::Ignore));
}
#[test]
fn standard_optional_metadata_is_typed_and_extension_metadata_is_tolerated() {
    for (key, value) in [
        (
            "io.modelcontextprotocol/clientCapabilities",
            json!({"sampling":{"tools":false}}),
        ),
        (
            "io.modelcontextprotocol/clientCapabilities",
            json!({"extensions":{"com.example/feature":false}}),
        ),
        (
            "io.modelcontextprotocol/clientInfo",
            json!({"name":"client","version":"1","icons":[{"src":"x","sizes":false}]}),
        ),
        ("io.modelcontextprotocol/logLevel", json!("verbose")),
    ] {
        let mut v: Value = serde_json::from_slice(&modern("ping", json!({}))).unwrap();
        v["params"]["_meta"][key] = value;
        assert_eq!(
            code(WireState::default().decode(&serde_json::to_vec(&v).unwrap())),
            -32602,
            "{key}"
        );
    }
    let mut v: Value = serde_json::from_slice(&modern("ping", json!({}))).unwrap();
    v["params"]["_meta"]["com.example/extension"] = json!([false, null, 3]);
    v["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"] =
        json!({"extensions":{"com.example/feature":{}},"customCapability":false});
    assert!(matches!(
        WireState::default().decode(&serde_json::to_vec(&v).unwrap()),
        WireEvent::Request(_)
    ));
}
#[test]
fn malformed_envelopes_before_era_selection_use_null_reply_id() {
    let event = WireState::default().decode(br#"{"jsonrpc":"wrong","id":7,"method":"ping"}"#);
    assert!(matches!(
        event,
        WireEvent::ProtocolError(ProtocolError {
            code: -32600,
            id: ReplyId::Null,
            close_after_send: true,
            ..
        })
    ));
}

#[test]
fn first_frame_modern_initialize_is_unknown_method_with_exact_id() {
    let mut state = WireState::default();
    let frame = br#"{"jsonrpc":"2.0","id":"first-modern-init","method":"initialize","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#;
    match state.decode(frame) {
        WireEvent::ProtocolError(error) => {
            assert_eq!(error.code, -32601);
            assert_eq!(
                error.id,
                ReplyId::Id(RpcId::String("first-modern-init".into()))
            );
        }
        other => panic!("modern initialize must not dispatch a request: {other:?}"),
    }
    assert_eq!(state.selected, Some(Era::Modern));
}
