#[path = "support/plugin.rs"]
mod fixture;
use theseus::recipe;
#[test]
fn accepts_scoped_plugin_contracts() {
    for scope in ["user", "project", "local"] {
        let t = fixture::project();
        let r = recipe::parse_bytes(
            &t.path().join("recipe.toml"),
            &fixture::bytes(&fixture::declaration(scope)),
        )
        .unwrap();
        r.validate_selection(
            &[theseus::model::ProfileName::User],
            &r.declared_scenarios(),
        )
        .unwrap();
    }
}
#[test]
fn rejects_invalid_plugin_declarations() {
    for (case, fragment) in [
        (
            "schema",
            "plugin_lifecycle requires recipe schema version 7",
        ),
        ("system", "plugin_lifecycle requires user profile"),
        ("network", "plugin_lifecycle requires network none"),
        ("forward", "plugin_lifecycle forbids pass_env"),
        ("step", "requires step disable_plugin"),
        ("checkpoint", "five checkpoint"),
        ("duplicate", "unique proof ID"),
        ("wrong-checkpoint", "requires checkpoint"),
        ("text", "inventory requires json_command"),
        ("identity", "inventory identity"),
        ("count", "inventory count"),
        ("enabled", "inventory values"),
        ("scope", "inventory values"),
        ("version", "inventory values"),
        ("preservation", "preservation proof"),
        ("components", "component proof"),
        ("path", "client_artifact"),
        ("equal", "distinct"),
    ] {
        let t = fixture::project();
        let mut v = fixture::declaration("user");
        match case {
            "schema" => v["schema_version"] = 6.into(),
            "system" => {
                let p = v["profiles"]["user"].take();
                v["profiles"] = serde_json::json!({"system":p});
            }
            "network" => v["network"] = "bridge".into(),
            "forward" => {
                v["profiles"]["user"]["steps"]["upgrade"]["pass_env"] = serde_json::json!(["TOKEN"])
            }
            "step" => {
                v["profiles"]["user"]["steps"]
                    .as_object_mut()
                    .unwrap()
                    .remove("disable_plugin");
            }
            "checkpoint" => {
                v["profiles"]["user"]["plugin"]["checkpoints"]
                    .as_object_mut()
                    .unwrap()
                    .remove("plugin_enabled");
            }
            "duplicate" => {
                v["profiles"]["user"]["plugin"]["client_check"] = "base-inventory".into()
            }
            "wrong-checkpoint" => {
                v["profiles"]["user"]["checks"][1]["checkpoint"] = "target_installed".into()
            }
            "text" => {
                v["profiles"]["user"]["checks"][1] = serde_json::json!({"id":"base-inventory","checkpoint":"base_installed","kind":"command","script":"inventory","expected_exit":0});
            }
            "identity" => v["profiles"]["user"]["checks"][1]["assertions"][0]["identity"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({"pointer":"/version","equals_json":"\"1\""})),
            "count" => v["profiles"]["user"]["checks"][1]["assertions"][0]["count"] = 2.into(),
            "enabled" => {
                v["profiles"]["user"]["checks"][1]["assertions"][0]["values"][2]["equals_json"] =
                    "false".into()
            }
            "scope" => {
                v["profiles"]["user"]["checks"][1]["assertions"][0]["values"][0]["equals_json"] =
                    "\"local\"".into()
            }
            "version" => {
                v["profiles"]["user"]["checks"][1]["assertions"][0]["values"][1]["equals_json"] =
                    "\"2\"".into()
            }
            "preservation" => {
                v["profiles"]["user"]["plugin"]["checkpoints"]["uninstalled"]["preservation"] =
                    serde_json::json!([])
            }
            "components" => {
                v["profiles"]["user"]["plugin"]["checkpoints"]["base_installed"]["components"] =
                    serde_json::json!([])
            }
            "path" => v["profiles"]["user"]["plugin"]["client_artifact"] = "../client".into(),
            "equal" => v["profiles"]["user"]["plugin"]["target_version"] = "1".into(),
            _ => unreachable!(),
        }
        let path = t.path().join("recipe.toml");
        let bytes = fixture::bytes(&v);
        assert!(
            recipe::parse_bytes(&path, &bytes)
                .unwrap_err()
                .to_string()
                .contains(fragment),
            "{case}: {}",
            recipe::parse_bytes(&path, &bytes).unwrap_err()
        );
    }
}
#[test]
fn inventory_can_add_companion_assertions_without_filtering_target_identity() {
    let t = fixture::project();
    let mut value = fixture::declaration("user");
    value["profiles"]["user"]["checks"][1]["assertions"].as_array_mut().unwrap().push(serde_json::json!({"kind":"array_match","pointer":"","identity":[{"pointer":"/id","equals_json":"\"companion@market\""}],"count":1,"values":[{"pointer":"/enabled","equals_json":"true"}]}));
    recipe::parse_bytes(&t.path().join("plugin.toml"), &fixture::bytes(&value)).unwrap();
}
#[test]
fn plugin_features_require_declaration_and_explicit_contract() {
    for case in ["contract", "steps", "checkpoints", "missing-contract"] {
        let t = fixture::project();
        let mut v = fixture::declaration("user");
        if case == "missing-contract" {
            v["profiles"]["user"]
                .as_object_mut()
                .unwrap()
                .remove("plugin");
        } else {
            v["scenarios"] = serde_json::json!(["clean"]);
            if case != "contract" {
                v["profiles"]["user"]
                    .as_object_mut()
                    .unwrap()
                    .remove("plugin");
            }
            if case == "checkpoints" {
                v["profiles"]["user"]["steps"]
                    .as_object_mut()
                    .unwrap()
                    .remove("disable_plugin");
                v["profiles"]["user"]["steps"]
                    .as_object_mut()
                    .unwrap()
                    .remove("enable_plugin");
            }
        }
        let path = t.path().join("plugin.toml");
        let bytes = fixture::bytes(&v);
        assert!(
            recipe::parse_bytes(&path, &bytes)
                .unwrap_err()
                .to_string()
                .contains(if case == "missing-contract" {
                    "requires plugin contract"
                } else {
                    "require declared plugin_lifecycle"
                }),
            "{case}"
        );
    }
}
