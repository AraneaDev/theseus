//! Pure native admission checks. No session, SDK, manager or process is created.
use super::*;
fn fixture() -> (
    ValidatedIntegrationRecipe,
    v2::IntegrationManifest,
    broker::IntegrationBrokerHeader,
) {
    let mut recipe = ValidatedIntegrationRecipe::decode(
        include_bytes!("../../../examples/native/service-recipe.example.json"),
        "pure-recipe.json".into(),
    )
    .unwrap();
    recipe.recipe.integrations.desktop = Some(DesktopRequirement {
        resource: ResourceSlot::new("desktop").unwrap(),
        modes: vec![DesktopMode::NativeActivation],
        readiness: ReadinessRequirement {
            kind: ReadyKind::LocalChallenge,
            protocol: ReadyProtocol::TheseusFixtureReadyV1,
            deadline_ms: 1000,
        },
        lifetime: LifetimeRequirement::OwnedPrimaryExited,
    });
    let mut manifest = readiness_role_review_tests::manifest("configured", "target");
    manifest.desktop.as_mut().unwrap().allowed_modes = vec![broker::DesktopMode::NativeActivation];
    manifest.desktop_capabilities = vec![
        "inspect_desktop".into(),
        "native_activation".into(),
        "request_close".into(),
    ];
    manifest.desktop_native = Some(serde_json::from_value(serde_json::json!({"kind":"linux_gio","bootstrap":"theseus_fixture_ready_bootstrap_v1","lifetime":"owned_primary_exited","sdk":{"glib_version":[2,80,0],"native_abi":"linux_x86_64","helper_root":"/fixture","helper_relative_path":"gio-helper","helper":{"length":128,"sha256":"66".repeat(32)},"sdk_receipt_sha256":"77".repeat(32),"helper_receipt_sha256":"88".repeat(32),"application_receipt_sha256":"99".repeat(32),"provenance":"provider_build_attested"}})).unwrap());
    let original = manifest
        .desktop_definition(&v2::DesktopDefinitionKey::CONFIGURED_TARGET)
        .unwrap();
    for (role, artifact) in [
        (
            v2::DesktopDefinitionRole::Base,
            v2::DesktopArtifactRole::Base,
        ),
        (
            v2::DesktopDefinitionRole::Configured,
            v2::DesktopArtifactRole::Base,
        ),
        (
            v2::DesktopDefinitionRole::Target,
            v2::DesktopArtifactRole::Target,
        ),
    ] {
        let mut tuple = original.clone();
        tuple.key = v2::DesktopDefinitionKey { role, artifact };
        tuple.readiness.role = role.as_str().into();
        tuple.readiness.artifact = artifact.as_str().into();
        tuple.desktop.expectation_digest = tuple.readiness.digest().unwrap().to_hex();
        manifest.desktop_definitions.push(tuple);
    }
    let mut raw = serde_json::to_value(manifest).unwrap();
    raw["service_authority"] = serde_json::json!({"kind":"systemd","slot":"fixture_service","scope":"user","bus_endpoint":{"kind":"unix_path","path":"/fixture/private-bus"},"qualification":{"manager":"systemd","version":"fixture-v1","api_profile":"systemd_dbus_private_v1","features":["unit_properties","start_stop","manager_reload"],"receipt_sha256":"88".repeat(32)},"namespace":{"kind":"exclusive_owned_registration","slot":"fixture_service","unit_load_roots":["/fixture/units"],"dropins":"forbidden","provider_context":"parser-only-owned-fixture"},"manager_context":{"kind":"whole_private","slot":"fixture_service","sut_allowed_actions":["daemon_reload"],"rehearsal_allowed_actions":[],"provider_receipt_required":true},"expected_peer_uid":1000,"expected_manager_uid":1000,"definition_root":"/fixture/units","unit":"fixture.service","managed_by":"installer","sut_effects":["registration","configuration","removal","daemon_reload"],"allowed_actions":["inspect","start","stop"]});
    raw["definitions"] = serde_json::json!([{"role":"configured","artifact":"target","executable":"/fixture/service","arguments":["/fixture/service"],"environment":[],"working_directory":"/fixture","definition_sha256":"aa".repeat(32)}]);
    let manifest: v2::IntegrationManifest = serde_json::from_value(raw).unwrap();
    recipe.core.recipe.target.length = 123;
    recipe.core.recipe.target.sha256 = "33".repeat(32);
    recipe.core.recipe.base = recipe.core.recipe.target.clone();
    let header = serde_json::from_value(serde_json::json!({"profile":"native_integration_broker_v1","binding":"01".repeat(16),"manifest_digest":"11".repeat(32),"workspace_marker":"02".repeat(16),"os":"linux","target_id":"fixture","ownership_id":"03".repeat(16),"provider_epoch":"04".repeat(32),"session_id":"supplied","session_epoch":"05".repeat(32),"broker_endpoint":{"kind":"unix_path","path":"/fixture/broker.sock"},"broker_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"helper_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"display":{"kind":"x11","display":":92","authority_file":"/fixture/auth","bus":"/fixture/bus.sock"},"allowed_modes":["native_activation"]})).unwrap();
    (recipe, manifest, header)
}
#[test]
fn native_ordinary_admission_requires_every_committed_tuple_and_exact_lifetime() {
    let (mut recipe, mut manifest, mut header) = fixture();
    for combined in [true, false] {
        if !combined {
            recipe.recipe.integrations.service = None;
            manifest.service_authority = None;
            manifest.definitions.clear();
        }
        for scenario in [
            ScenarioKind::Clean,
            ScenarioKind::Upgrade,
            ScenarioKind::Reinstall,
        ] {
            assert_eq!(
                validate_admission(&recipe, &manifest, &header, scenario).unwrap(),
                broker::DesktopMode::NativeActivation
            );
        }
    }
    let mut missing = manifest.clone();
    missing
        .desktop_definitions
        .retain(|d| d.key.role != v2::DesktopDefinitionRole::Base);
    assert!(validate_admission(&recipe, &missing, &header, ScenarioKind::Upgrade).is_err());
    recipe
        .recipe
        .integrations
        .desktop
        .as_mut()
        .unwrap()
        .lifetime = LifetimeRequirement::OwnedProcessTreeSettled;
    assert!(validate_admission(&recipe, &manifest, &header, ScenarioKind::Reinstall).is_err());
    recipe
        .recipe
        .integrations
        .desktop
        .as_mut()
        .unwrap()
        .lifetime = LifetimeRequirement::ActivationInstanceTerminated;
    assert!(validate_admission(&recipe, &manifest, &header, ScenarioKind::Clean).is_err());
    recipe
        .recipe
        .integrations
        .desktop
        .as_mut()
        .unwrap()
        .lifetime = LifetimeRequirement::OwnedPrimaryExited;
    header
        .allowed_modes
        .push(broker::DesktopMode::OwnedExecutable);
    assert!(validate_admission(&recipe, &manifest, &header, ScenarioKind::Clean).is_err());
}
