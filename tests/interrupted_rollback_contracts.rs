use serde_json::{Value, json};
use theseus::{
    model::{ProfileName, RunReport},
    recipe::{
        InterruptedRollbackContract, Recipe, RollbackContract, RollbackRecoveryContract,
        validate_interrupted_rollback_contract, validate_rollback_contract,
        validate_rollback_recovery,
    },
};

fn recovery_literal() -> Value {
    json!({
        "change_checks": ["changed"], "binary_checks": ["binary"],
        "runtime_checks": ["runtime"], "data_checks": ["reader"],
        "restoration": {"mode": "restore_data", "backup_root": "/workspace/backup",
            "scopes": [{"id": "data", "source": "/workspace/data", "stored": "data"}]}
    })
}

#[test]
fn interruption_policy_has_no_normal_failure_and_no_nested_wire_object() {
    let literal = json!({
        "change_checks": ["changed"], "binary_checks": ["binary"],
        "runtime_checks": ["runtime"], "data_checks": ["reader"],
        "restoration": {"mode": "binary_only"}
    });
    let interrupted: InterruptedRollbackContract = serde_json::from_value(literal.clone()).unwrap();
    assert_eq!(serde_json::to_value(interrupted).unwrap(), literal);
    assert!(serde_json::from_value::<RollbackContract>(literal.clone()).is_err());
    let mut wrong = literal;
    wrong["failure"] = json!({"expected_exit": 9, "stdout_contains": "failed"});
    assert!(serde_json::from_value::<InterruptedRollbackContract>(wrong).is_err());
}

#[test]
fn literal_schema_five_recipe_and_report_keep_existing_policy_keys() {
    let recipe: Recipe = toml::from_str(include_str!("fixtures/recipes/rollback.toml")).unwrap();
    assert_eq!(recipe.schema_version, 5);
    let expected = json!({
        "failure": {"expected_exit": 9, "stdout_contains": "migration failed"},
        "change_checks": ["changed"], "binary_checks": ["binary"],
        "runtime_checks": ["runtime"], "data_checks": ["reader"],
        "restoration": {"mode": "restore_data", "backup_root": "/workspace/backup",
            "scopes": [{"id": "data", "source": "/workspace/data", "stored": "data"}]}
    });
    let policy = recipe.profiles[&ProfileName::User]
        .rollback
        .as_ref()
        .unwrap();
    assert_eq!(serde_json::to_value(policy).unwrap(), expected);
    let recipe_wire = toml::to_string(&recipe).unwrap();
    let decoded: Recipe = toml::from_str(&recipe_wire).unwrap();
    assert_eq!(
        decoded.profiles[&ProfileName::User].rollback.as_ref(),
        Some(policy)
    );

    // Start from independent historical bytes, then add literal schema-5 policy bytes.
    let mut report: Value =
        serde_json::from_str(include_str!("fixtures/reports/schema2-report.json")).unwrap();
    report["schema_version"] = json!(5);
    report["scenarios"][0]["rollback"] = expected.clone();
    let decoded: RunReport = serde_json::from_value(report).unwrap();
    assert_eq!(
        serde_json::to_value(decoded).unwrap()["scenarios"][0]["rollback"],
        expected
    );
}

#[test]
fn recovery_and_both_policies_reject_unknown_or_missing_fields() {
    for field in [
        "change_checks",
        "binary_checks",
        "runtime_checks",
        "data_checks",
        "restoration",
    ] {
        let mut missing = recovery_literal();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<RollbackRecoveryContract>(missing.clone()).is_err(),
            "{field}"
        );
        assert!(
            serde_json::from_value::<InterruptedRollbackContract>(missing.clone()).is_err(),
            "{field}"
        );
        missing["failure"] = json!({"expected_exit":9,"stdout_contains":"failed"});
        assert!(
            serde_json::from_value::<RollbackContract>(missing).is_err(),
            "{field}"
        );
    }
    for pointer in ["", "/restoration", "/restoration/scopes/0"] {
        let mut wrong = recovery_literal();
        wrong.pointer_mut(pointer).unwrap()["unexpected"] = json!(true);
        assert!(
            serde_json::from_value::<RollbackRecoveryContract>(wrong.clone()).is_err(),
            "{pointer}"
        );
        assert!(
            serde_json::from_value::<InterruptedRollbackContract>(wrong.clone()).is_err(),
            "{pointer}"
        );
        wrong["failure"] = json!({"expected_exit":9,"stdout_contains":"failed"});
        assert!(
            serde_json::from_value::<RollbackContract>(wrong).is_err(),
            "{pointer}"
        );
    }
    let mut nested = recovery_literal();
    nested["recovery"] = recovery_literal();
    assert!(serde_json::from_value::<InterruptedRollbackContract>(nested.clone()).is_err());
    nested["failure"] = json!({"expected_exit":9,"stdout_contains":"failed"});
    assert!(serde_json::from_value::<RollbackContract>(nested).is_err());
}

#[test]
fn interrupted_toml_is_flat_strict_and_requires_no_failure() {
    let literal = r#"
change_checks = ['changed']
binary_checks = ['binary']
runtime_checks = ['runtime']
data_checks = ['reader']
[restoration]
mode = 'binary_only'
"#;
    let contract: InterruptedRollbackContract = toml::from_str(literal).unwrap();
    validate_interrupted_rollback_contract(&contract).unwrap();
    let value: toml::Value = toml::from_str(&toml::to_string(&contract).unwrap()).unwrap();
    assert_eq!(value, toml::from_str::<toml::Value>(literal).unwrap());
    assert!(toml::from_str::<RollbackContract>(literal).is_err());
    assert!(
        toml::from_str::<InterruptedRollbackContract>(&format!("unexpected = true\n{literal}"))
            .is_err()
    );
    let ordinary_literal =
        format!("{literal}\n[failure]\nexpected_exit = 9\nstdout_contains = 'failed'\n");
    let ordinary: RollbackContract = toml::from_str(&ordinary_literal).unwrap();
    validate_rollback_contract(&ordinary).unwrap();
    assert!(toml::from_str::<InterruptedRollbackContract>(&ordinary_literal).is_err());
    assert!(
        toml::from_str::<RollbackContract>(&format!("unexpected = true\n{ordinary_literal}"))
            .is_err()
    );
}

fn assert_invalid_recovery(literal: Value) {
    let recovery: RollbackRecoveryContract = serde_json::from_value(literal.clone()).unwrap();
    assert!(validate_rollback_recovery(&recovery).is_err(), "{literal}");
    let interrupted: InterruptedRollbackContract = serde_json::from_value(literal.clone()).unwrap();
    assert!(
        validate_interrupted_rollback_contract(&interrupted).is_err(),
        "{literal}"
    );
    let mut ordinary = literal;
    ordinary["failure"] = json!({"expected_exit":9,"stdout_contains":"failed"});
    assert!(validate_rollback_contract(&serde_json::from_value(ordinary).unwrap()).is_err());
}

#[test]
fn recovery_requires_nonempty_distinct_role_ids() {
    let roles = [
        "change_checks",
        "binary_checks",
        "runtime_checks",
        "data_checks",
    ];
    for role in roles {
        for ids in [json!([]), json!([""]), json!([" \t"]), json!(["x", "x"])] {
            let mut literal = recovery_literal();
            literal[role] = ids;
            assert_invalid_recovery(literal);
        }
    }
    for (index, role) in roles.iter().enumerate() {
        for other in &roles[..index] {
            let mut literal = recovery_literal();
            literal[role] = json!(["shared"]);
            literal[other] = json!(["shared"]);
            assert_invalid_recovery(literal);
        }
    }
}

#[test]
fn recovery_rejects_unsafe_paths_and_overlapping_mappings() {
    for pointer in ["/restoration/backup_root", "/restoration/scopes/0/source"] {
        for path in [
            "",
            "relative",
            "/",
            "/workspace//data",
            "/workspace/./data",
            "/workspace/../data",
            "/workspace/data/",
            "/workspace/\0data",
        ] {
            let mut literal = recovery_literal();
            *literal.pointer_mut(pointer).unwrap() = json!(path);
            assert_invalid_recovery(literal);
        }
    }
    for path in [
        "/inputs",
        "/inputs/backup",
        "/proc",
        "/proc/backup",
        "/sys",
        "/sys/backup",
        "/dev",
        "/dev/backup",
        "/workspace",
        "/workspace/data",
        "/workspace/data/backup",
    ] {
        let mut literal = recovery_literal();
        literal["restoration"]["backup_root"] = json!(path);
        assert_invalid_recovery(literal);
    }
    for path in [
        "",
        "/absolute",
        ".",
        "..",
        "../copy",
        "data/../copy",
        "data/./copy",
        "data//copy",
        "data/",
        "data/\0copy",
    ] {
        let mut literal = recovery_literal();
        literal["restoration"]["scopes"][0]["stored"] = json!(path);
        assert_invalid_recovery(literal);
    }
    for (source, stored) in [
        ("/workspace/data", "other"),
        ("/workspace/data/child", "other"),
        ("/workspace", "other"),
        ("/other", "data"),
        ("/other", "data/child"),
    ] {
        let mut literal = recovery_literal();
        literal["restoration"]["scopes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"other","source":source,"stored":stored}));
        assert_invalid_recovery(literal);
    }
}

#[test]
fn recovery_preserves_scope_limits_and_id_rules() {
    for id in ["", "has space", "../id", "é", &"x".repeat(65)] {
        let mut literal = recovery_literal();
        literal["restoration"]["scopes"][0]["id"] = json!(id);
        assert_invalid_recovery(literal);
    }
    let mut duplicate = recovery_literal();
    duplicate["restoration"]["scopes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"data","source":"/other","stored":"other"}));
    assert_invalid_recovery(duplicate);
    for count in [0, 1, 64, 65] {
        let mut literal = recovery_literal();
        literal["restoration"]["scopes"] = (0..count).map(|i| json!({"id":format!("scope_{i}-A"),"source":format!("/data/{i}"),"stored":format!("copies/{i}")})).collect();
        let recovery: RollbackRecoveryContract = serde_json::from_value(literal).unwrap();
        assert_eq!(
            validate_rollback_recovery(&recovery).is_ok(),
            (1..=64).contains(&count)
        );
    }
}

#[test]
fn recovery_validation_is_pure_and_preserves_literal_values() {
    let mut literal = recovery_literal();
    literal["change_checks"] = json!([" changed "]);
    literal["restoration"]["backup_root"] = json!("/inputsevil/backup");
    literal["restoration"]["scopes"][0]["id"] = json!("x".repeat(64));
    // No host path or filesystem kind is consulted by offline validation.
    literal["restoration"]["scopes"][0]["source"] = json!("/does-not-exist/source");
    let recovery: RollbackRecoveryContract = serde_json::from_value(literal.clone()).unwrap();
    validate_rollback_recovery(&recovery).unwrap();
    assert_eq!(serde_json::to_value(&recovery).unwrap(), literal);
    let interrupted = InterruptedRollbackContract { recovery };
    validate_interrupted_rollback_contract(&interrupted).unwrap();
}

#[test]
fn interrupted_binary_only_rejects_backup_extras() {
    for extra in [
        json!({"backup_root":"/workspace/backup"}),
        json!({"scopes":[]}),
        json!({"unexpected":true}),
    ] {
        let mut literal = recovery_literal();
        literal["restoration"] = json!({"mode":"binary_only"});
        literal["restoration"]
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(serde_json::from_value::<InterruptedRollbackContract>(literal.clone()).is_err());
        assert!(serde_json::from_value::<RollbackRecoveryContract>(literal).is_err());
    }
}
