use super::*;
fn journal() -> NativeIntegrationJournal {
    let report = crate::native::integration::view::tests::report();
    NativeIntegrationJournal {
        kind: "theseus_native_journal".into(),
        schema: 2,
        profile: super::super::PROFILE.into(),
        run_id: report.run_id,
        launch_fingerprint: Some("aa".repeat(32)),
        recipe_digest: report.recipe_digest,
        core_recipe_digest: report.core_recipe_digest,
        core_manifest_digest: report.core_manifest_digest,
        integration_manifest_digest: report.integration_manifest_digest,
        manifest_digest: report.manifest_digest,
        requirements: report.requirements,
        compiled_requirements_digest: report.compiled_requirements_digest,
        selected_scenarios: report.selected_scenarios,
        ownership: vec![],
        operations: vec![],
        provider_calls: vec![],
        releases: vec![],
        recovery_attempts: vec![],
        failures: vec![],
        integration_ownership: vec![],
        integration_actions: vec![],
        broker_bindings: vec![],
        readiness_probes: vec![],
        integration_recoveries: vec![],
    }
}
#[test]
fn malformed_readiness_probe_cannot_enter_saved_evidence() {
    let mut journal = journal();
    journal.readiness_probes.push(ReadinessProbeRecord {
        challenge: ReadinessChallenge {
            ownership_id: "missing-owner".into(),
            lease: "11".repeat(16),
            slot: ResourceSlot::new("fixture_service").unwrap(),
            operation: "22".repeat(16),
            nonce: "33".repeat(16),
            deadline_ms: 30_001,
            resource_binding_digest: "44".repeat(32),
            action_digest: "55".repeat(32),
            expectation_digest: "66".repeat(32),
        },
        expectation: ReadinessExpectation {
            role: DefinitionRole::Configured,
            artifact: ArtifactRole::Target,
            artifact_length: 1,
            configuration_digest: "77".repeat(32),
            artifact_digest: "88".repeat(32),
            version: "target".into(),
            data_digest: "99".repeat(32),
            initialized: true,
        },
        expectation_digest: "00".repeat(32),
        observation_ids: vec![],
        completion: Disposition::Complete {
            receipt: "unproved".into(),
        },
    });
    assert!(
        journal.validate().is_err(),
        "malformed orphan probe must refuse saved evidence"
    );
}
#[test]
fn schema2_missing_disposition_and_legacy_decoder_refuse() {
    let report = crate::native::integration::view::tests::report();
    let bytes = report.encode().unwrap();
    assert!(NativeReport::decode(&bytes).is_err());
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    value["integrations"][0]["dispositions"]
        .as_object_mut()
        .unwrap()
        .remove("helper_worker");
    assert!(NativeIntegrationReport::decode(&serde_json::to_vec(&value).unwrap()).is_err());
}
