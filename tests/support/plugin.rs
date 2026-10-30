use serde_json::{Value, json};
pub fn declaration(scope: &str) -> Value {
    let mut checks = vec![
        json!({"id":"client-version","checkpoint":"base_installed","kind":"json_command","script":"client-version","expected_exit":0,"assertions":[{"kind":"equals","pointer":"/version","equals_json":"\"2.1.312 (Claude Code)\""}]}),
    ];
    let mut checkpoints = serde_json::Map::new();
    for (prefix, cp, version, enabled) in [
        ("base", "base_installed", "1", true),
        ("target", "target_installed", "2", true),
        ("disabled", "plugin_disabled", "2", false),
        ("enabled", "plugin_enabled", "2", true),
        ("removed", "uninstalled", "2", true),
    ] {
        let inventory = format!("{prefix}-inventory");
        let preservation = format!("{prefix}-preserved");
        let values = if prefix == "removed" {
            vec![]
        } else {
            vec![
                json!({"pointer":"/scope","equals_json":serde_json::to_string(scope).unwrap()}),
                json!({"pointer":"/version","equals_json":serde_json::to_string(version).unwrap()}),
                json!({"pointer":"/enabled","equals_json":enabled.to_string()}),
            ]
        };
        checks.push(json!({"id":inventory,"checkpoint":cp,"kind":"json_command","script":inventory,"expected_exit":0,"assertions":[{"kind":"array_match","pointer":"","identity":[{"pointer":"/id","equals_json":"\"probe@market\""}],"count":if prefix=="removed" {0}else{1},"values":values}]}));
        checks.push(json!({"id":preservation,"checkpoint":cp,"kind":"json_command","script":preservation,"expected_exit":0,"assertions":[{"kind":"equals","pointer":"/unrelated/theme","equals_json":"\"dark\""},{"kind":"array_match","pointer":"/companions","identity":[{"pointer":"/id","equals_json":"\"companion@market\""}],"count":1,"values":[{"pointer":"/version","equals_json":"\"1\""},{"pointer":"/enabled","equals_json":"true"},{"pointer":"/installPath","equals_json":"\"/cache/companion\""}]}]}));
        let components = if prefix == "base" || prefix == "target" {
            let id = format!("{prefix}-components");
            checks.push(json!({"id":id,"checkpoint":cp,"kind":"command","script":id,"expected_exit":0,"stdout_contains":"source/cache manifests skills legacy-command manual-hook verified"}));
            vec![id]
        } else {
            vec![]
        };
        checkpoints.insert(
            cp.into(),
            json!({"inventory":inventory,"preservation":[preservation],"components":components}),
        );
    }
    let mut steps = serde_json::Map::new();
    for name in [
        "install_base",
        "upgrade",
        "disable_plugin",
        "enable_plugin",
        "uninstall",
    ] {
        steps.insert(name.into(), json!({"script":name}));
    }
    json!({"schema_version":7,"scenarios":["plugin_lifecycle"],"name":"plugin","image":"replay","network":"none","artifacts":{"target":"target"},"observe":["/workspace"],"profiles":{"user":{"plugin":{"id":"probe@market","scope":scope,"base_version":"1","target_version":"2","client_version":"2.1.312 (Claude Code)","client_check":"client-version","client_artifact":"client/claude","checkpoints":checkpoints},"steps":steps,"checks":checks}}})
}
pub fn bytes(value: &Value) -> Vec<u8> {
    toml::to_string(value).unwrap().into_bytes()
}
pub fn project() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(t.path().join("target/client")).unwrap();
    std::fs::write(
        t.path().join("target/client/claude"),
        b"deterministic client artifact",
    )
    .unwrap();
    t
}
