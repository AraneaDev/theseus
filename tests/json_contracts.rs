use std::fs;
use theseus::recipe;

const CHECK: &str = r#"
[[profiles.user.checks]]
id = "inventory"
checkpoint = "target_installed"
kind = "json_command"
script = "printf '[]'"
expected_exit = 0
max_stdout_bytes = 262144
assertions = [{ kind = "array_match", pointer = "", identity = [{ pointer = "/id", equals_json = '"target@market"' }], count = 1, values = [{ pointer = "/scope", equals_json = '"user"' }, { pointer = "/enabled", equals_json = 'true' }] }]
"#;
fn load(version: u32, check: &str) -> anyhow::Result<recipe::ValidatedRecipe> {
    let dir = tempfile::tempdir()?;
    fs::create_dir(dir.path().join("target"))?;
    let text = include_str!("fixtures/recipes/v2_clean.toml")
        .replace("schema_version = 2", &format!("schema_version = {version}"))
        + check;
    fs::write(dir.path().join("recipe.toml"), text)?;
    recipe::load(&dir.path().join("recipe.toml"))
}
#[test]
fn json_command_requires_seven_and_roundtrips() {
    assert!(load(6, CHECK).is_err());
    let recipe = load(7, CHECK).unwrap().recipe;
    let serialized = toml::to_string(&recipe).unwrap();
    let decoded: recipe::Recipe = toml::from_str(&serialized).unwrap();
    assert_eq!(
        serde_json::to_value(recipe).unwrap(),
        serde_json::to_value(decoded).unwrap()
    );
}
#[test]
fn rejects_invalid_definitions() {
    for check in [
        CHECK.replace("262144", "0"),
        CHECK.replace("262144", "1048577"),
        CHECK.replace("count = 1", "count = 2"),
        CHECK.replace("count = 1", "count = 0"),
        CHECK.replace("/scope", "/bad~2"),
        CHECK.replace("/scope", "scope"),
        CHECK.replace("/scope", "/*"),
        CHECK.replace("'true'", "'1.0'"),
        CHECK.replace("'true'", "'9223372036854775808'"),
        CHECK.replace("'true'", "'{}'"),
        CHECK.replace("'true'", "'true false'"),
        CHECK.replace("'true'", "'[]'"),
        CHECK.replace(
            "kind = \"array_match\"",
            "typo = true, kind = \"array_match\"",
        ),
    ] {
        assert!(load(7, &check).is_err(), "accepted {check}");
    }
}
#[test]
fn predicate_scalar_pointer_and_definition_budgets_are_enforced() {
    use recipe::{JsonAssertion as A, JsonPredicate as P, validate_json_assertions as validate};
    let eq = |pointer: String, literal: String| A::Equals {
        pointer,
        equals_json: literal,
    };
    assert!(validate(&vec![eq("".into(), "true".into()); 32], 262144).is_ok());
    assert!(validate(&vec![eq("".into(), "true".into()); 33], 262144).is_err());
    assert!(validate(&[eq("".into(), format!("\"{}\"", "x".repeat(254)))], 262144).is_ok());
    assert!(validate(&[eq("".into(), format!("\"{}\"", "x".repeat(255)))], 262144).is_err());
    assert!(
        validate(
            &[eq(format!("/{}", "x".repeat(255)), "true".into())],
            262144
        )
        .is_ok()
    );
    assert!(
        validate(
            &[eq(format!("/{}", "x".repeat(256)), "true".into())],
            262144
        )
        .is_err()
    );
    let array = |count| A::ArrayMatch {
        pointer: "".into(),
        identity: vec![P {
            pointer: "/id".into(),
            equals_json: "1".into(),
        }],
        count: 1,
        values: (0..count)
            .map(|i| P {
                pointer: format!("/v{i}"),
                equals_json: "true".into(),
            })
            .collect(),
    };
    assert!(validate(&[array(15), array(15), array(15), array(15)], 262144).is_ok());
    assert!(validate(&[array(16), array(15), array(15), array(15)], 262144).is_err());
    let duplicate = A::ArrayMatch {
        pointer: "".into(),
        identity: vec![
            P {
                pointer: "/id".into(),
                equals_json: "1".into(),
            },
            P {
                pointer: "/id".into(),
                equals_json: "2".into(),
            },
        ],
        count: 0,
        values: vec![],
    };
    assert!(validate(&[duplicate], 262144).is_err());
    let large = vec![
        eq(
            format!("/{}", "p".repeat(255)),
            format!("\"{}\"", "x".repeat(254))
        );
        32
    ];
    assert!(validate(&large, 262144).is_err());
    for malformed in [
        "1.2",
        "1e1",
        "9223372036854775808",
        "-9223372036854775809",
        "[]",
        "{}",
        "",
        "true false",
    ] {
        assert!(validate(&[eq("".into(), malformed.into())], 262144).is_err());
    }
}
#[test]
fn scalar_budget_measures_serialized_value_not_literal_spelling() {
    let assertion = recipe::JsonAssertion::Equals {
        pointer: "".into(),
        equals_json: format!("\"{}\"", "\\u0061".repeat(100)),
    };
    assert!(recipe::validate_json_assertions(&[assertion], 262144).is_ok());
}
