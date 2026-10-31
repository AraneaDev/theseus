#[path = "support/mcp.rs"]
mod fixture;
#[test]
fn owned_standalone_and_plugin_fixtures_have_canonical_source_parents() {
    let fixture = fixture::Fixture::new();
    let canonical_root = fixture.root.path().canonicalize().unwrap();
    for declaration in [fixture::declaration(), fixture::plugin_declaration()] {
        let validated = fixture
            .validate(&declaration)
            .expect("valid owned MCP baseline");
        assert_eq!(validated.project_root, canonical_root);
        assert_eq!(validated.path, canonical_root.join("recipe.toml"));
        // Canonicalize only the fixture input; production still rejects a real
        // noncanonical source parent, even when it resolves inside this root.
        // Build from the original owned root: pushing onto Windows' canonical
        // verbatim root would normalize away the very `..` we need to test.
        let noncanonical = fixture
            .root
            .path()
            .join("target")
            .join("..")
            .join("recipe.toml");
        assert!(
            noncanonical
                .components()
                .any(|component| component == std::path::Component::ParentDir),
            "negative witness must retain its noncanonical parent: {noncanonical:?}"
        );
        assert_eq!(
            noncanonical
                .parent()
                .unwrap()
                .canonicalize()
                .expect("negative witness parent must resolve before strict validation"),
            canonical_root
        );
        let error = theseus::recipe::parse_bytes(
            &noncanonical,
            toml::to_string(&declaration).unwrap().as_bytes(),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("recipe source directory must be canonical")
        );
    }
}
#[test]
fn configuration_only_adapter_cannot_claim_native_health() {
    let f = fixture::Fixture::new();
    let mut value = fixture::declaration();
    assert!(f.validate(&value).is_ok());
    value["profiles"]["user"]["mcp"]["client_claim"] = "configuration_only".into();
    value["profiles"]["user"]["mcp"]["checkpoints"]["configured"]["activation"] =
        "native-health".into();
    assert!(f.validate(&value).is_err());
}
#[test]
fn finite_adapter_bindings_reject_missing_or_contradictory_proof() {
    let f = fixture::Fixture::new();
    for fault in 0..16 {
        let mut v = fixture::declaration();
        match fault {
            0 => v["schema_version"] = 7.into(),
            1 => v["network"] = "bridge".into(),
            2 => v["scenarios"] = serde_json::json!(["clean", "upgrade"]),
            3 => v["profiles"]["user"]["mcp"]["adapter"] = "unknown".into(),
            4 => v["profiles"]["user"]["mcp"]["scope"] = "global".into(),
            5 => v["profiles"]["user"]["mcp"]["client_artifact"] = "../claude".into(),
            6 => {
                v["profiles"]["user"]["mcp"]["checkpoints"]["uninstalled"]["state"] =
                    "active".into()
            }
            7 => {
                v["profiles"]["user"]["mcp"]["checkpoints"]["configured"]["probe"] =
                    "configured-registration".into()
            }
            8 => {
                v["profiles"]["user"]["mcp"]["checkpoints"]["uninstalled"]["probe"] =
                    "configured-probe".into()
            }
            9 => v["profiles"]["user"]["checks"][1]["assertions"]
                .as_array_mut()
                .unwrap()
                .clear(),
            10 => {
                v["profiles"]["user"]["checks"][1]["assertions"]
                    .as_array_mut()
                    .unwrap()
                    .pop();
            }
            11 => {
                v["profiles"]["user"]["checks"][2]["assertions"][0]["equals_json"] =
                    "\"/wrong\"".into()
            }
            12 => {
                v["profiles"]["user"]["checks"][2]["assertions"][4]["equals_json"] = "false".into()
            }
            13 => v["profiles"]["user"]["checks"][3]["assertions"][0]["equals_json"] = "1".into(),
            14 => {
                v["profiles"]["user"]["steps"]["configure"]["pass_env"] =
                    serde_json::json!(["SECRET"])
            }
            _ => v["profiles"]["user"]["checks"][0]["checkpoint"] = "uninstalled".into(),
        }
        assert!(f.validate(&v).is_err(), "fault {fault}");
    }
}
#[test]
fn prepared_matrix_validates_offline_and_rejects_unsupported_combinations() {
    let root = tempfile::tempdir().unwrap();
    let client = root.path().join("SYNTHETIC-not-executed-elf");
    std::fs::write(&client, b"\x7fELF-SYNTHETIC").unwrap();
    let mut serial = 0;
    for (adapter, version, delivery, scopes, scenarios) in [
        (
            "claude-code-2.1.312",
            "2.1.312 (Claude Code)",
            "standalone",
            vec!["user", "project", "local"],
            vec!["clean", "upgrade"],
        ),
        (
            "claude-code-2.1.312",
            "2.1.312 (Claude Code)",
            "plugin",
            vec!["user"],
            vec!["plugin_lifecycle"],
        ),
        (
            "codex-cli-0.166.0",
            "codex-cli 0.166.0",
            "standalone",
            vec!["global"],
            vec!["clean", "upgrade"],
        ),
    ] {
        for scope in scopes {
            for scenario in &scenarios {
                for protocol in ["2026-07-28", "2025-11-25"] {
                    serial += 1;
                    let out = root.path().join(format!("case{serial}"));
                    let result = std::process::Command::new("python3")
                        .args(["examples/mcp/prepare.py", "--client"])
                        .arg(&client)
                        .args([
                            "--client-version",
                            version,
                            "--adapter",
                            adapter,
                            "--delivery",
                            delivery,
                            "--scope",
                            scope,
                            "--scenario",
                            scenario,
                            "--protocol",
                            protocol,
                            "--output",
                        ])
                        .arg(&out)
                        .output()
                        .unwrap();
                    assert!(
                        result.status.success(),
                        "{}",
                        String::from_utf8_lossy(&result.stderr)
                    );
                    let recipe =
                        theseus::recipe::load(&out.join("theseus.toml")).unwrap_or_else(|e| {
                            panic!("{adapter} {delivery} {scope} {scenario} {protocol}: {e:#}")
                        });
                    assert_eq!(recipe.declared_scenarios().len(), 1);
                    let manifest: serde_json::Value = serde_json::from_slice(
                        &std::fs::read(out.join("input-manifest.json")).unwrap(),
                    )
                    .unwrap();
                    assert_eq!(manifest["client"]["size"], 14);
                }
            }
        }
    }
    for (adapter, delivery, scope, scenario) in [
        ("codex-cli-0.166.0", "plugin", "global", "plugin_lifecycle"),
        ("codex-cli-0.166.0", "standalone", "project", "clean"),
        (
            "claude-code-2.1.312",
            "plugin",
            "project",
            "plugin_lifecycle",
        ),
        (
            "claude-code-2.1.312",
            "standalone",
            "user",
            "plugin_lifecycle",
        ),
    ] {
        let status = std::process::Command::new("python3")
            .args(["examples/mcp/prepare.py", "--client"])
            .arg(&client)
            .args([
                "--client-version",
                if adapter.starts_with("codex") {
                    "codex-cli 0.166.0"
                } else {
                    "2.1.312 (Claude Code)"
                },
                "--adapter",
                adapter,
                "--delivery",
                delivery,
                "--scope",
                scope,
                "--scenario",
                scenario,
                "--protocol",
                "2026-07-28",
                "--output",
            ])
            .arg(root.path().join("rejected"))
            .output()
            .unwrap();
        assert!(!status.status.success());
        assert!(!root.path().join("rejected").exists());
    }
}
#[test]
fn active_native_health_can_bind_the_exact_native_marker() {
    let f = fixture::Fixture::new();
    let mut v = fixture::declaration();
    let checks = v["profiles"]["user"]["checks"].as_array_mut().unwrap();
    let check = checks
        .iter_mut()
        .find(|c| c["id"] == "configured-activation")
        .unwrap();
    *check = serde_json::json!({"id":"configured-activation","checkpoint":"configured","kind":"command","script":"native-health","expected_exit":0,"stdout_contains":"probe: /bin/sh /inputs/target/server.sh - ✔ Connected"});
    assert!(f.validate(&v).is_ok());
    check_mutation(&f, &v, "probe: /bin/sh /wrong - ✔ Connected");
    fn check_mutation(f: &fixture::Fixture, v: &serde_json::Value, marker: &str) {
        let mut v = v.clone();
        v["profiles"]["user"]["checks"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|c| c["id"] == "configured-activation")
            .unwrap()["stdout_contains"] = marker.into();
        assert!(f.validate(&v).is_err());
    }
}
#[test]
fn plugin_index_guard_is_unique_and_source_matches_probe() {
    let f = fixture::Fixture::new();
    let value = fixture::plugin_declaration();
    assert!(f.validate(&value).is_ok());
    for fault in 0..4 {
        let mut v = value.clone();
        let check = v["profiles"]["user"]["checks"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|c| c["id"] == "target_installed-registration")
            .unwrap();
        let a = check["assertions"].as_array_mut().unwrap();
        match fault {
            0 => a.push(fixture::eq(
                "/plugins/1/id",
                serde_json::json!("probe@market"),
            )),
            1 => {
                a.iter_mut()
                    .find(|a| a["pointer"] == "/plugins/0/readFromFolder")
                    .unwrap()["equals_json"] = "\"/cache/2\"".into()
            }
            2 => {
                a.pop();
            }
            _ => a.iter_mut().find(|a| a["kind"] == "array_match").unwrap()["identity"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({"pointer":"/scope","equals_json":"\"user\""})),
        }
        assert!(f.validate(&v).is_err(), "fault {fault}");
    }
}
#[test]
fn plugin_alias_must_name_the_same_plugin_as_plugin_contract() {
    let f = fixture::Fixture::new();
    let mut v = fixture::plugin_declaration();
    v["profiles"]["user"]["mcp"]["client_alias"] = "plugin:other:probe".into();
    for check in v["profiles"]["user"]["checks"].as_array_mut().unwrap() {
        let id = check["id"].as_str().unwrap().to_owned();
        if id.ends_with("-registration") {
            let a = check["assertions"].as_array_mut().unwrap();
            let original = a
                .iter()
                .find(|a| a["kind"] == "array_match")
                .unwrap()
                .clone();
            for a in a.iter_mut() {
                if a["kind"] == "array_match" {
                    a["identity"][0]["equals_json"] = "\"other@market\"".into();
                } else if a["pointer"] == "/plugins/0/id" {
                    a["equals_json"] = "\"other@market\"".into();
                }
            }
            a.push(original);
        } else if id.ends_with("-activation") {
            check["assertions"][1]["equals_json"] = "\"plugin:other:probe\"".into();
        }
    }
    assert!(f.validate(&v).is_err());
}

#[test]
fn plugin_launch_requires_source_bearing_normalized_code_path() {
    let f = fixture::Fixture::new();
    let mut accepted = vec![];
    for (raw, source) in [
        (
            vec!["/bin/sh", "/market/probe/server.sh"],
            "/unrelated/source",
        ),
        (
            vec![
                "/bin/sh",
                "/fixed/server.sh",
                "${CLAUDE_PLUGIN_ROOT}/data.json",
            ],
            "/market/probe",
        ),
        (
            vec!["/usr/bin/python3", "${CLAUDE_PLUGIN_ROOT}/server.py"],
            "/market/probe",
        ),
        (
            vec!["/bin/sh", "${CLAUDE_PLUGIN_ROOT}/../server.sh"],
            "/market/probe",
        ),
        (
            vec!["/bin/sh", "${CLAUDE_PLUGIN_ROOT}/./server.sh"],
            "/market/probe",
        ),
        (
            vec!["/bin/sh", "${CLAUDE_PLUGIN_ROOT}//server.sh"],
            "/market/probe",
        ),
        (
            vec!["/bin/sh", "${CLAUDE_PLUGIN_ROOT}/server.sh/"],
            "/market/probe",
        ),
        (vec!["/bin/sh", "${CLAUDE_PLUGIN_ROOT}/"], "/market/probe"),
    ] {
        let value = fixture::plugin_launch_declaration(&raw, source);
        if f.validate(&value).is_ok() {
            accepted.push(raw);
        }
    }
    assert!(
        accepted.is_empty(),
        "accepted source-independent/unnormalized launches: {accepted:?}"
    );
}
#[test]
fn plugin_launch_accepts_direct_and_shell_code_from_proven_source() {
    let f = fixture::Fixture::new();
    for raw in [
        vec!["${CLAUDE_PLUGIN_ROOT}/bin/server"],
        vec!["/bin/sh", "${CLAUDE_PLUGIN_ROOT}/scripts/server.sh", "mode"],
    ] {
        assert!(
            f.validate(&fixture::plugin_launch_declaration(
                &raw,
                "/different/proven/source"
            ))
            .is_ok(),
            "{raw:?}"
        );
    }
}
#[test]
fn plugin_registration_selects_exact_marketplace_identity() {
    let f = fixture::Fixture::new();
    assert!(
        f.validate(&fixture::plugin_other_market_declaration())
            .is_err()
    );
}
#[test]
fn disabled_plugin_registration_still_requires_source_bearing_launch() {
    let f = fixture::Fixture::new();
    let mut value = fixture::plugin_declaration();
    let bad = fixture::plugin_launch_declaration(
        &["/bin/sh", "/market/probe/server.sh"],
        "/unrelated/source",
    );
    let bad = bad["profiles"]["user"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "plugin_disabled-registration")
        .unwrap();
    *value["profiles"]["user"]["checks"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|c| c["id"] == "plugin_disabled-registration")
        .unwrap() = bad.clone();
    assert!(f.validate(&value).is_err());
}
