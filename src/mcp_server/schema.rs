//! JSON Schema 2020-12 contracts. Each advertised schema carries only reachable definitions.
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn r(name: &str) -> Value {
    json!({"$ref":format!("#/$defs/{name}")})
}
fn nullable(value: Value) -> Value {
    json!({"anyOf":[value,{"type":"null"}]})
}
fn array(value: Value, max: u64) -> Value {
    json!({"type":"array","items":value,"maxItems":max})
}
fn enumeration(values: &[&str]) -> Value {
    json!({"type":"string","enum":values})
}
fn object(properties: &[(&str, Value)], required: &[&str]) -> Value {
    json!({"type":"object","properties":properties.iter().map(|(k,v)|((*k).to_owned(),v.clone())).collect::<Map<_,_>>(),"required":required,"additionalProperties":false})
}
fn exact(properties: &[(&str, Value)]) -> Value {
    object(
        properties,
        &properties.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
    )
}
fn text(max: u64) -> Value {
    json!({"type":"string","maxLength":max})
}
fn bounded(min: u64, max: u64) -> Value {
    json!({"type":"integer","minimum":min,"maximum":max})
}
fn selector(collection: &[&str], coordinates: &[(&str, Value)]) -> Value {
    let mut p = vec![
        ("kind", json!({"const":"catalog"})),
        ("collection", enumeration(collection)),
    ];
    p.extend_from_slice(coordinates);
    exact(&p)
}
fn definitions() -> BTreeMap<String, Value> {
    let mut d = BTreeMap::new();
    macro_rules! def {
        ($name:literal, $value:expr) => {
            d.insert($name.into(), $value);
        };
    }
    def!(
        "Instance",
        json!({"type":"string","pattern":"^[0-9a-f]{32}$","minLength":32,"maxLength":32})
    );
    def!(
        "Handle",
        json!({"type":"string","pattern":"^[0-9a-f]{32}:(?:[1-9]|[1-9][0-9]|1[01][0-9]|12[0-8])$","maxLength":36})
    );
    def!(
        "Digest",
        json!({"type":"string","pattern":"^[0-9a-f]{64}$","minLength":64,"maxLength":64})
    );
    def!("Revision", r("Digest"));
    def!(
        "Key",
        json!({"type":"string","minLength":1,"maxLength":128,"pattern":"^[^\\u0000]+$"})
    );
    def!(
        "Cursor",
        json!({"type":"string","maxLength":256,"pattern":"^[\\u0000-\\u007f]*$"})
    );
    def!("UInt", bounded(0, 9_007_199_254_740_991));
    def!(
        "Path",
        json!({"type":"string","minLength":1,"maxLength":4096,"pattern":"^(?:(?!(?:[Cc][Oo][Nn](?:[Ii][Nn]\\$|[Oo][Uu][Tt]\\$)?|[Pp][Rr][Nn]|[Aa][Uu][Xx]|[Nn][Uu][Ll]|[Cc][Oo][Mm][1-9¹²³]|[Ll][Pp][Tt][1-9¹²³])(?:\\.|/|$))[^/\\\\:<>\"|?*\\u0000-\\u001f]*[^/\\\\:<>\"|?*\\u0000-\\u001f. ])(?:/(?!(?:[Cc][Oo][Nn](?:[Ii][Nn]\\$|[Oo][Uu][Tt]\\$)?|[Pp][Rr][Nn]|[Aa][Uu][Xx]|[Nn][Uu][Ll]|[Cc][Oo][Mm][1-9¹²³]|[Ll][Pp][Tt][1-9¹²³])(?:\\.|/|$))[^/\\\\:<>\"|?*\\u0000-\\u001f]*[^/\\\\:<>\"|?*\\u0000-\\u001f. ])*$"})
    );
    let index = "(?:0|[1-9][0-9]*)";
    let steps = "install_base|install_target|configure|upgrade|exercise|reinstall|uninstall|interrupt_install|recover_install|interrupt_upgrade|recover_upgrade|downgrade|exercise_downgraded|backup|attempt_upgrade|rollback|exercise_rolled_back|cleanup_backup|disable_plugin|enable_plugin";
    def!(
        "RecordId",
        json!({"type":"string","maxLength":128,"pattern":format!("^(?:run|native_recipe|native/(?:leaves|limits|artifact/(?:base|target)|check/{index}|step/(?:{steps}))|native_identity/{index}|native_disposition/{index}|artifact/(?:base|target)|(?:env|observe|error|artifact)/{index}|artifact/{index}/file/{index}|p/(?:user|system)/(?:step/(?:{steps})|check/{index}|policy/(?:mcp|plugin|downgrade|rollback|interrupted_rollback))|s/{index}(?:/(?:finding|error)/{index}|/stage/{index}(?:/(?:command|check|baseline|change|error|native_file)/{index}|/snapshot/{index}(?:/entry/{index})?)?)?)$")})
    );
    def!("Profile", enumeration(&["user", "system"]));
    def!(
        "Scenario",
        enumeration(&[
            "clean",
            "upgrade",
            "reinstall",
            "interrupted_install",
            "interrupted_upgrade",
            "downgrade",
            "rollback",
            "interrupted_rollback",
            "plugin_lifecycle"
        ])
    );
    def!(
        "Outcome",
        enumeration(&["passed", "failed", "incomplete", "cancelled"])
    );
    def!(
        "RunState",
        enumeration(&[
            "validating",
            "admitted",
            "running",
            "finished",
            "start_error",
            "worker_error"
        ])
    );
    def!(
        "RecordKind",
        enumeration(&[
            "native_recipe",
            "native_file",
            "native_disposition",
            "native_identity",
            "recipe_step",
            "recipe_check",
            "recipe_artifact",
            "recipe_environment",
            "recipe_observed_root",
            "recipe_policy",
            "run_header",
            "scenario_header",
            "stage_header",
            "command",
            "check",
            "finding",
            "artifact_header",
            "artifact_file",
            "snapshot_header",
            "snapshot_entry",
            "baseline_change",
            "stage_change",
            "error"
        ])
    );
    def!(
        "TextPreview",
        exact(&[
            ("text", text(512)),
            ("truncated", json!({"type":"boolean"})),
            ("lossy", json!({"type":"boolean"}))
        ])
    );
    def!(
        "ShortPreview",
        exact(&[
            ("text", text(128)),
            ("truncated", json!({"type":"boolean"})),
            ("lossy", json!({"type":"boolean"}))
        ])
    );
    def!(
        "Counts",
        exact(
            &[
                "passed_checks",
                "failed_checks",
                "incomplete_checks",
                "skipped_checks",
                "review_findings",
                "commands"
            ]
            .map(|n| (n, r("UInt")))
        )
    );
    def!(
        "ErrorCode",
        enumeration(&[
            "invalid_arguments",
            "invalid_cursor",
            "unknown_run",
            "unknown_record",
            "ambiguous_record",
            "stale_instance",
            "idempotency_conflict",
            "registry_full",
            "busy",
            "query_busy",
            "recipe_changed",
            "revision_changed",
            "log_changed",
            "recipe_invalid",
            "source_too_large",
            "unsafe_path",
            "root_changed",
            "query_timeout",
            "io_error",
            "evidence_invalid",
            "cancelled_before_start",
            "worker_error",
            "cleanup_pending",
            "response_too_large",
            "internal_error"
        ])
    );
    def!(
        "FailureError",
        exact(&[
            ("code", r("ErrorCode")),
            ("message", r("TextPreview")),
            ("retryable", json!({"type":"boolean"})),
            ("handle", nullable(r("Handle"))),
            ("restart_pagination", json!({"type":"boolean"}))
        ])
    );
    def!(
        "Failure",
        exact(&[
            ("format_version", json!({"const":1})),
            ("server_instance", r("Instance")),
            ("ok", json!({"const":false})),
            ("error", r("FailureError"))
        ])
    );
    def!(
        "CleanupView",
        exact(&[
            (
                "native",
                enumeration(&[
                    "not_started",
                    "in_progress",
                    "complete",
                    "pending",
                    "unknown"
                ])
            ),
            (
                "guest",
                enumeration(&[
                    "not_started",
                    "in_progress",
                    "complete",
                    "failed",
                    "unknown"
                ])
            ),
            (
                "launch",
                nullable(enumeration(&[
                    "never_created",
                    "created_suspended",
                    "may_run"
                ]))
            ),
            ("recovery_required", json!({"type":"boolean"}))
        ])
    );
    def!(
        "NativeDispositionView",
        json!({"oneOf":[exact(&[("state",json!({"const":"not_started"}))]),exact(&[("state",json!({"const":"complete"})),("receipt",r("TextPreview"))]),exact(&[("state",json!({"const":"unknown"})),("reason",r("TextPreview"))]),exact(&[("state",json!({"const":"failed"})),("reason",r("TextPreview"))])]})
    );
    def!(
        "NativeScenarioDispositions",
        exact(&[
            ("scenario_id", r("TextPreview")),
            ("local", r("NativeDispositionView")),
            ("remote", r("NativeDispositionView")),
            ("release", r("NativeDispositionView")),
            ("reset", r("NativeDispositionView"))
        ])
    );
    def!(
        "SavedView",
        object(
            &[
                ("revision", r("Revision")),
                ("schema_version", bounded(1, 8)),
                ("outcome", r("Outcome")),
                ("counts", r("Counts")),
                ("finished_at", nullable(r("TextPreview"))),
                (
                    "native_dispositions",
                    array(r("NativeScenarioDispositions"), 3)
                )
            ],
            &[
                "revision",
                "schema_version",
                "outcome",
                "counts",
                "finished_at"
            ]
        )
    );
    def!(
        "EngineResult",
        exact(&[("outcome", r("Outcome")), ("counts", r("Counts"))])
    );
    def!(
        "RunView",
        exact(&[
            ("handle", r("Handle")),
            ("admission_sequence", bounded(1, 128)),
            ("state", r("RunState")),
            ("cancellation_requested", json!({"type":"boolean"})),
            (
                "cancellation_reason",
                nullable(enumeration(&[
                    "client",
                    "request_cancelled",
                    "runtime_limit",
                    "server_shutdown"
                ]))
            ),
            ("report_id", nullable(r("TextPreview"))),
            ("report_path", nullable(r("TextPreview"))),
            ("journal_path", nullable(r("TextPreview"))),
            ("saved", nullable(r("SavedView"))),
            ("engine_result", nullable(r("EngineResult"))),
            ("saved_hint_sequence", r("UInt")),
            ("snapshot_hint_sequence", nullable(r("UInt"))),
            ("cleanup", r("CleanupView")),
            ("error", nullable(r("FailureError")))
        ])
    );
    let mut profile_array = array(r("Profile"), 2);
    profile_array["minItems"] = json!(1);
    profile_array["uniqueItems"] = json!(true);
    let mut scenario_array = array(r("Scenario"), 9);
    scenario_array["minItems"] = json!(1);
    scenario_array["uniqueItems"] = json!(true);
    def!(
        "OverviewData",
        exact(&[
            ("kind", json!({"const":"overview"})),
            ("recipe_path", r("Path")),
            ("recipe_digest", r("Digest")),
            ("schema_version", bounded(1, 8)),
            ("name", r("TextPreview")),
            ("image", r("TextPreview")),
            ("network", enumeration(&["none", "bridge"])),
            ("profiles", profile_array.clone()),
            ("scenarios", scenario_array.clone()),
            (
                "artifacts",
                exact(&[
                    ("base", nullable(r("TextPreview"))),
                    ("target", r("TextPreview"))
                ])
            ),
            ("lifetime", json!({"const":"server_process"})),
            ("max_run_seconds", bounded(1, 86400))
        ])
    );
    def!(
        "NativeOverviewData",
        exact(&[
            ("kind", json!({"const":"native_overview"})),
            ("recipe_path", r("Path")),
            ("recipe_digest", r("Digest")),
            ("schema", json!({"const":1})),
            ("os", enumeration(&["linux", "macos", "windows"])),
            ("role", json!({"const":"current_helper_account"})),
            ("scenarios", scenario_array.clone()),
            ("lifetime", json!({"const":"server_process"})),
            ("max_run_seconds", bounded(1, 86400))
        ])
    );
    def!(
        "StartData",
        exact(&[
            ("handle", r("Handle")),
            ("state", r("RunState")),
            ("replayed", json!({"type":"boolean"})),
            ("cancellation_requested", json!({"type":"boolean"})),
            ("lifetime", json!({"const":"server_process"})),
            ("poll_after_ms", json!({"const":500}))
        ])
    );
    def!(
        "ListData",
        exact(&[
            ("runs", array(r("RunView"), 32)),
            ("high_water", bounded(0, 128)),
            ("next_cursor", nullable(r("Cursor")))
        ])
    );
    def!(
        "StatusData",
        exact(&[
            ("run", r("RunView")),
            (
                "refresh",
                enumeration(&["loaded", "cached", "busy", "unavailable"])
            ),
            ("poll_after_ms", json!({"const":500}))
        ])
    );
    def!(
        "CancelData",
        exact(&[
            ("run", r("RunView")),
            ("requested_now", json!({"type":"boolean"}))
        ])
    );
    def!(
        "RecordSummary",
        exact(&[
            ("record_id", r("RecordId")),
            ("record_kind", r("RecordKind")),
            ("source_id", nullable(r("ShortPreview"))),
            ("summary", r("ShortPreview")),
            ("outcome", nullable(json!({"type":"string"})))
        ])
    );
    def!(
        "CatalogPage",
        exact(&[
            ("kind", json!({"const":"catalog"})),
            ("revision", r("Revision")),
            ("records", array(r("RecordSummary"), 32)),
            ("next_cursor", nullable(r("Cursor")))
        ])
    );
    let bytes = array(bounded(0, 255), 4096);
    def!(
        "RecordPage",
        exact(&[
            ("kind", json!({"const":"record"})),
            ("revision", r("Revision")),
            ("record_id", r("RecordId")),
            ("record_kind", r("RecordKind")),
            ("encoding", json!({"const":"json_utf8"})),
            ("offset", r("UInt")),
            ("bytes", bytes.clone()),
            ("total_bytes", r("UInt")),
            ("next_cursor", nullable(r("Cursor")))
        ])
    );
    let source_id = json!({"type":"string","minLength":1,"maxLength":4096});
    def!(
        "LogPage",
        exact(&[
            ("kind", json!({"const":"log"})),
            ("revision", r("Revision")),
            ("command_id", source_id.clone()),
            ("stream", enumeration(&["stdout", "stderr"])),
            ("offset", r("UInt")),
            ("bytes", bytes),
            ("recorded_bytes", r("UInt")),
            ("current_bytes", r("UInt")),
            ("readable_total", r("UInt")),
            ("capture_truncated", json!({"type":"boolean"})),
            (
                "change_token",
                json!({"type":"string","pattern":"^[0-9a-f]{32}$","minLength":32,"maxLength":32})
            ),
            ("next_cursor", nullable(r("Cursor")))
        ])
    );
    def!(
        "InspectData",
        json!({"oneOf":[r("OverviewData"),r("NativeOverviewData"),r("CatalogPage"),r("RecordPage")]})
    );
    def!(
        "EvidenceData",
        json!({"oneOf":[r("CatalogPage"),r("RecordPage"),r("LogPage")]})
    );
    let inspect_selector = json!({"oneOf":[exact(&[("kind",json!({"const":"overview"}))]),object(&[("kind",json!({"const":"catalog"})),("collection",enumeration(&["steps","checks","environment","policies"])),("profile",r("Profile"))],&["kind","collection"]),selector(&["artifacts","observed_roots"],&[]),exact(&[("kind",json!({"const":"record"})),("record_id",r("RecordId"))])]});
    def!("InspectSelector", inspect_selector);
    let mut inspect = object(
        &[
            ("recipe_path", r("Path")),
            ("recipe_digest", r("Digest")),
            ("selector", r("InspectSelector")),
            ("limit", bounded(1, 4096)),
            ("cursor", r("Cursor")),
        ],
        &["recipe_path"],
    );
    inspect["allOf"] = json!([
        {"if":{"properties":{"selector":{"properties":{"kind":{"const":"catalog"}},"required":["kind"]}},"required":["selector"]},"then":{"required":["recipe_digest"],"properties":{"limit":bounded(1,32)}}},
        {"if":{"properties":{"selector":{"properties":{"kind":{"const":"record"}},"required":["kind"]}},"required":["selector"]},"then":{"required":["recipe_digest"]}},
        {"if":{"anyOf":[{"not":{"required":["selector"]}},{"properties":{"selector":{"properties":{"kind":{"const":"overview"}},"required":["kind"]}},"required":["selector"]}]},"then":{"not":{"anyOf":[{"required":["limit"]},{"required":["cursor"]}]}}}
    ]);
    def!("InspectArgs", inspect);
    def!(
        "StartArgs",
        object(
            &[
                ("server_instance", r("Instance")),
                ("request_key", r("Key")),
                ("recipe_path", r("Path")),
                ("recipe_digest", r("Digest")),
                ("profiles", profile_array),
                ("scenarios", scenario_array),
                ("pull", json!({"type":"boolean","default":false})),
                (
                    "log_limit",
                    json!({"type":"integer","minimum":1,"maximum":16_777_216,"default":16_777_216})
                )
            ],
            &[
                "server_instance",
                "request_key",
                "recipe_path",
                "recipe_digest",
                "scenarios"
            ]
        )
    );
    def!(
        "ListArgs",
        object(
            &[
                ("server_instance", r("Instance")),
                (
                    "limit",
                    json!({"type":"integer","minimum":1,"maximum":32,"default":32})
                ),
                ("cursor", r("Cursor"))
            ],
            &["server_instance"]
        )
    );
    def!(
        "StatusArgs",
        object(
            &[
                ("server_instance", r("Instance")),
                ("handle", r("Handle")),
                ("refresh", json!({"type":"boolean","default":true}))
            ],
            &["server_instance", "handle"]
        )
    );
    def!(
        "CancelArgs",
        exact(&[("server_instance", r("Instance")), ("handle", r("Handle"))])
    );
    def!(
        "EvidenceSelector",
        json!({"oneOf":[
            selector(&["run","scenarios","artifacts","errors"],&[]),
            selector(&["stages","findings","scenario_errors"],&[("scenario_id",source_id.clone())]),
            selector(&["commands","checks","snapshots","baseline_changes","stage_changes","stage_errors","native_files"],&[("scenario_id",source_id.clone()),("stage_index",r("UInt"))]),
            selector(&["artifact_files"],&[("artifact_index",r("UInt"))]),
            selector(&["snapshot_entries"],&[("scenario_id",source_id.clone()),("stage_index",r("UInt")),("snapshot_index",r("UInt"))]),
            exact(&[("kind",json!({"const":"record"})),("record_id",r("RecordId"))]),
            exact(&[("kind",json!({"const":"log"})),("command_id",source_id),("stream",enumeration(&["stdout","stderr"]))])
        ]})
    );
    let mut evidence = object(
        &[
            ("server_instance", r("Instance")),
            ("handle", r("Handle")),
            ("revision", r("Revision")),
            ("selector", r("EvidenceSelector")),
            ("limit", bounded(1, 4096)),
            ("cursor", r("Cursor")),
        ],
        &["server_instance", "handle", "selector"],
    );
    evidence["allOf"] = json!([
        {"if":{"anyOf":[{"required":["cursor"]},{"properties":{"selector":{"properties":{"kind":{"enum":["record","log"]}}}},"required":["selector"]}]},"then":{"required":["revision"]}},
        {"if":{"properties":{"selector":{"properties":{"kind":{"const":"catalog"}}}}},"then":{"properties":{"limit":bounded(1,32)}}}
    ]);
    def!("EvidenceArgs", evidence);
    d
}
fn collect_refs(value: &Value, names: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            if let Some(name) = map
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|s| s.strip_prefix("#/$defs/"))
            {
                names.insert(name.into());
            }
            for value in map.values() {
                collect_refs(value, names);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_refs(item, names);
            }
        }
        _ => {}
    }
}
fn standalone(mut value: Value, definitions: &BTreeMap<String, Value>) -> Value {
    let mut names = BTreeSet::new();
    collect_refs(&value, &mut names);
    loop {
        let old = names.len();
        for name in names.clone() {
            collect_refs(&definitions[&name], &mut names);
        }
        if old == names.len() {
            break;
        }
    }
    value["$schema"] = json!("https://json-schema.org/draft/2020-12/schema");
    value["$defs"] = Value::Object(
        names
            .into_iter()
            .map(|n| {
                let v = definitions[&n].clone();
                (n, v)
            })
            .collect(),
    );
    value
}
pub(crate) fn schemas() -> Value {
    let definitions = definitions();
    let tools = [
        ("recipe_inspect","Inspect a recipe without execution.","InspectArgs","InspectData",true,false),
        ("run_start","Admit one process-owned run.","StartArgs","StartData",false,false),
        ("run_list","List process admissions.","ListArgs","ListData",true,false),
        ("run_status","Read a run's current state.","StatusArgs","StatusData",true,false),
        ("run_cancel","Request run cancellation.","CancelArgs","CancelData",false,true),
        ("run_evidence","Read bounded saved evidence.","EvidenceArgs","EvidenceData",true,false),
    ].map(|(name,description,input,output,read,idempotent)| {
        let success = exact(&[("format_version",json!({"const":1})),("server_instance",r("Instance")),("ok",json!({"const":true})),("data",r(output))]);
        json!({"name":name,"description":description,"inputSchema":standalone(definitions[input].clone(),&definitions),"outputSchema":standalone(json!({"type":"object","oneOf":[success,r("Failure")]}),&definitions),"annotations":{"readOnlyHint":read,"idempotentHint":idempotent}})
    });
    json!({"tools":tools})
}
