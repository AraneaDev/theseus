#![allow(dead_code)]
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

/// Explicit artifact staging only: the supplied file is never executed here.
pub fn stage_plugin(
    client: &std::path::Path,
    expected_version: &str,
    scope: &str,
) -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(if cfg!(windows) { "python" } else { "python3" })
        .args(["examples/plugins/prepare.py", "--client"])
        .arg(client)
        .args([
            "--client-version",
            expected_version,
            "--scope",
            scope,
            "--output",
        ])
        .arg(temp.path().join("prepared"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "preparation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    temp
}

/// The only shell-client staging path. Production prepare.py remains ELF-only.
/// Both manifest and report name explicitly identify synthetic behavior.
pub fn replace_with_replay(root: &std::path::Path) {
    use sha2::{Digest, Sha256};
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    let bytes = fs::read("tests/fixtures/plugins/replay-client").unwrap();
    let client = root.join("target/client/claude");
    fs::write(&client, &bytes).unwrap();
    #[cfg(unix)]
    fs::set_permissions(&client, fs::Permissions::from_mode(0o755)).unwrap();
    let path = root.join("input-manifest.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    manifest["prepared_dummy_elf"] = manifest["client"].clone();
    manifest["provenance"] = "SYNTHETIC stateful shell replay; not native Claude acceptance".into();
    manifest["client"]["sha256"] = format!("{:x}", Sha256::digest(&bytes)).into();
    manifest["client"]["size"] = bytes.len().into();
    fs::write(path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
}
