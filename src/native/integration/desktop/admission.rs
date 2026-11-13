//! Pure desktop admission and readiness classification.
use super::*;

pub(super) fn desktop_readiness_expectation(
    expected: &v2::ReadinessDefinitionExpectation,
) -> Result<ReadinessExpectation> {
    expected.validate()?;
    let role = match expected.role.as_str() {
        "base" => DefinitionRole::Base,
        "configured" => DefinitionRole::Configured,
        "target" => DefinitionRole::Target,
        _ => bail!("unsupported desktop readiness definition role"),
    };
    let artifact = match expected.artifact.as_str() {
        "base" => ArtifactRole::Base,
        "target" => ArtifactRole::Target,
        _ => bail!("unsupported desktop readiness artifact role"),
    };
    let expectation = ReadinessExpectation {
        role,
        artifact,
        artifact_length: expected.artifact_length,
        artifact_digest: expected.artifact_sha256.clone(),
        version: expected.version_literal.clone(),
        data_digest: expected.data_sha256.clone(),
        configuration_digest: expected.configuration_digest.clone(),
        initialized: true,
    };
    ensure!(
        expectation.digest()? == expected.digest()?.to_hex(),
        "desktop readiness canonical expectation changed"
    );
    Ok(expectation)
}
pub(crate) fn desktop_readiness_completion(observation: &BoundDesktopObservation) -> Disposition {
    if desktop_predicate_satisfied(observation, &IntegrationPredicate::ApplicationReady)
        && let Some(receipt) = observation
            .readiness
            .as_ref()
            .and_then(|r| r.response_digest.as_ref())
    {
        return Disposition::Complete {
            receipt: receipt.clone(),
        };
    }
    Disposition::Unknown {
        reason: "original desktop readiness unproved".into(),
    }
}
pub(super) fn registration_removal_disposition(
    service: Option<&Disposition>,
    desktop: Disposition,
) -> Disposition {
    match service {
        Some(service) if !matches!(service, Disposition::Complete { .. }) => service.clone(),
        _ => desktop,
    }
}
pub(super) fn selected_desktop_mode(
    modes: &[DesktopMode],
    lifetime: LifetimeRequirement,
    native: Option<&v2::NativeDesktopDefinition>,
) -> Result<broker::DesktopMode> {
    match (modes, native) {
        ([DesktopMode::OwnedExecutable], None)
            if lifetime != LifetimeRequirement::ActivationInstanceTerminated =>
        {
            Ok(broker::DesktopMode::OwnedExecutable)
        }
        ([DesktopMode::NativeActivation], Some(definition))
            if lifetime.native() == definition.lifetime()
                && lifetime != LifetimeRequirement::OwnedProcessTreeSettled =>
        {
            Ok(broker::DesktopMode::NativeActivation)
        }
        _ => bail!("selected desktop mode/lifetime differs from exact admitted native capability"),
    }
}
/// Pure exact-description admission, called before preparing an intent/Acquire.
pub(crate) fn validate_admission(
    recipe: &ValidatedIntegrationRecipe,
    manifest: &v2::IntegrationManifest,
    header: &broker::IntegrationBrokerHeader,
    scenario: ScenarioKind,
) -> Result<broker::DesktopMode> {
    manifest.validate()?;
    header.validate()?;
    let requirement = recipe
        .recipe
        .integrations
        .desktop
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("desktop requirement missing"))?;
    let desktop = manifest
        .desktop
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("desktop capability missing"))?;
    let mode = selected_desktop_mode(
        &requirement.modes,
        requirement.lifetime,
        manifest.desktop_native.as_ref(),
    )?;
    ensure!(
        desktop.slot.as_str() == requirement.resource.as_str()
            && desktop.allowed_modes == [mode]
            && header.allowed_modes == [mode],
        "desktop authority/requirements differ"
    );
    if let Some(entry) = &manifest.desktop_owned_entry {
        ensure!(
            mode == broker::DesktopMode::OwnedExecutable && entry.os() == header.os,
            "owned native entry differs from selected mode/platform"
        );
    }
    if let Some(owned) = manifest.desktop_owned_lifetime {
        owned.validate_platform(&header.os)?;
        ensure!(
            mode == broker::DesktopMode::OwnedExecutable
                && requirement.lifetime == LifetimeRequirement::OwnedPrimaryExited,
            "owned lifetime differs from the selected exact requirement"
        );
    } else {
        ensure!(
            header.os != "macos" || mode != broker::DesktopMode::OwnedExecutable,
            "OwnedExecutable lifetime on macOS requires the committed original-primary declaration; choose NativeActivation for manager-instance termination"
        );
    }
    #[cfg(feature = "native-integration-test-support")]
    let source_headless = header.display == broker::SuppliedDisplay::SourceOwnedHeadless;
    #[cfg(not(feature = "native-integration-test-support"))]
    let source_headless = false;
    ensure!(
        mode != broker::DesktopMode::OwnedExecutable
            || header.os == "linux"
            || source_headless
            || manifest.desktop_owned_entry.is_some(),
        "interactive owned execution requires exact native entry declaration before acquisition"
    );
    if let Some(native) = &manifest.desktop_native {
        ensure!(
            native.os() == header.os,
            "native desktop platform differs from original broker"
        );
        // This pure description check grants no Linux authority. The original
        // session must also retain its authenticated provider/helper admission
        // before Acquire and again at driver construction.
    }
    let expected = manifest
        .desktop_readiness
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("desktop readiness manifest missing"))?;
    ensure!(
        expected.artifact_length == recipe.core.recipe.target.length
            && expected.artifact_sha256 == recipe.core.recipe.target.sha256,
        "desktop executable differs from original admitted target artifact"
    );
    desktop_readiness_expectation(expected)?;
    if scenario != ScenarioKind::Clean || !manifest.desktop_definitions.is_empty() {
        ensure!(
            !manifest.desktop_definitions.is_empty(),
            "ordinary desktop transition requires precommitted per-definition tuples"
        );
        let phases = if scenario == ScenarioKind::Upgrade {
            vec![
                IntegrationPhase::BaseInstalled,
                IntegrationPhase::Configured,
                IntegrationPhase::TargetInstalledAfterUpgrade,
            ]
        } else {
            vec![
                IntegrationPhase::TargetInstalledInitial,
                IntegrationPhase::Configured,
            ]
        };
        for phase in phases {
            let key = desktop_phase_definition(scenario, phase)
                .unwrap()
                .desktop_wire();
            let tuple = manifest.desktop_definition(&key)?;
            tuple.validate()?;
            ensure!(
                selected_desktop_mode(
                    &requirement.modes,
                    requirement.lifetime,
                    tuple.native.as_ref()
                )? == mode,
                "desktop tuple changes admitted mode/lifetime"
            );
            let artifact = if key.artifact == v2::DesktopArtifactRole::Base {
                &recipe.core.recipe.base
            } else {
                &recipe.core.recipe.target
            };
            ensure!(
                tuple.executable.length == artifact.length
                    && tuple.executable.sha256 == artifact.sha256,
                "desktop tuple differs from original admitted reader artifact"
            );
        }
    }
    Ok(mode)
}
pub(super) fn nonce() -> Result<v2::BindingId> {
    let mut bytes = [0; 16];
    theseus_host::random_bytes(&mut bytes)?;
    ensure!(bytes != [0; 16], "random desktop nonce is zero");
    Ok(v2::BindingId::from_bytes(bytes))
}
pub(super) fn operation() -> Result<v2::OperationId> {
    Ok(v2::OperationId::from_bytes(*nonce()?.as_bytes()))
}
