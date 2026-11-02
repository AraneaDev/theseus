// Test-only declaration copied unchanged from tests/support/mcp.rs.
use serde_json::{Value, json};
pub fn eq(pointer: &str, value: Value) -> Value {
    json!({"kind":"equals","pointer":pointer,"equals_json":value.to_string()})
}
pub fn absent(pointer: &str) -> Value {
    json!({"kind":"absent","pointer":pointer})
}
pub fn declaration() -> Value {
    let mut value: Value = serde_json::to_value(
        toml::from_str::<toml::Value>(include_str!("../../tests/fixtures/recipes/v2_clean.toml"))
            .unwrap(),
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
