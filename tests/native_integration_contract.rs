#![cfg(feature = "native-integration-test-support")]
//! Offline refusal contracts: none of these tests acquire runtime authority.
use std::path::PathBuf;
use theseus::native::integration::recipe::ValidatedIntegrationRecipe;
fn source() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../examples/native/service-recipe.example.json"
    ))
    .unwrap()
}
fn decode(value: &serde_json::Value) -> anyhow::Result<ValidatedIntegrationRecipe> {
    ValidatedIntegrationRecipe::decode(
        &serde_json::to_vec(value)?,
        PathBuf::from("service-recipe.json"),
    )
}
#[test]
fn clean_requires_every_declared_installer_service_checkpoint() {
    let original = source();
    decode(&original).unwrap();
    let checks = original["integrations"]["service"]["checks"]
        .as_array()
        .unwrap();
    for index in 0..checks.len() {
        let mut missing = original.clone();
        missing["integrations"]["service"]["checks"]
            .as_array_mut()
            .unwrap()
            .remove(index);
        assert!(
            decode(&missing).is_err(),
            "removing {} must refuse offline",
            checks[index]["id"]
        );
    }
}
#[test]
fn manager_running_cannot_replace_application_readiness() {
    let mut value = source();
    value["integrations"]["service"]["readiness"]["kind"] = "manager_only".into();
    assert!(decode(&value).is_err());
    let mut value = source();
    value["integrations"]["service"]["checks"][4]["predicate"] = "running".into();
    assert!(decode(&value).is_err());
}
#[test]
fn integration_source_digest_commits_outer_requirements_and_rejects_duplicate_fields() {
    let value = source();
    let original = decode(&value).unwrap();
    let mut changed = value.clone();
    changed["integrations"]["service"]["readiness"]["deadline_ms"] = 9999.into();
    let changed = decode(&changed).unwrap();
    assert_ne!(original.digest, changed.digest);
    assert_eq!(original.core.digest, changed.core.digest);
    let source = serde_json::to_string(&value).unwrap();
    let duplicate = source.replacen("\"schema\":2", "\"schema\":2,\"schema\":2", 1);
    assert!(
        ValidatedIntegrationRecipe::decode(duplicate.as_bytes(), PathBuf::from("duplicate.json"))
            .is_err()
    );
}
#[test]
fn full_manifest_composition_matches_independent_pinned_example() {
    use theseus_native_protocol::v2;
    assert_eq!(
        v2::integration_manifest_digest(
            v2::Digest::from_bytes([2; 32]),
            v2::Digest::from_bytes([17; 32])
        )
        .to_hex(),
        "cd64ea4df93a58dc89771b1f60cd2b829e3e45e0bdb78650ee6a672cd7971d3c"
    );
}
