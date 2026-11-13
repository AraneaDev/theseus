use super::*;
fn requirement() -> DesktopRequirement {
    DesktopRequirement {
        resource: ResourceSlot::new("desktop").unwrap(),
        modes: vec![DesktopMode::OwnedExecutable],
        readiness: ReadinessRequirement {
            kind: ReadyKind::LocalChallenge,
            protocol: ReadyProtocol::TheseusFixtureReadyV1,
            deadline_ms: 1000,
        },
        lifetime: LifetimeRequirement::OwnedProcessTreeSettled,
    }
}
#[test]
fn desktop_ordinary_checks_keep_legacy_clean_and_distinct_original_generations() {
    let r = requirement();
    assert_eq!(
        desktop_scenario_check_specs(&r, ScenarioKind::Clean),
        desktop_check_specs(&r)
    );
    let upgrade = desktop_scenario_check_specs(&r, ScenarioKind::Upgrade);
    assert!(
        upgrade
            .iter()
            .any(|(_, phase, p, _)| *phase == IntegrationPhase::BeforeUpgrade
                && *p == IntegrationPredicate::Lifetime(r.lifetime))
    );
    assert!(upgrade.iter().any(|(_, phase, p, _)| *phase
        == IntegrationPhase::TargetInstalledAfterUpgrade
        && *p == IntegrationPredicate::ApplicationReady));
    let reinstall = desktop_scenario_check_specs(&r, ScenarioKind::Reinstall);
    for phase in [
        IntegrationPhase::ExerciseBeforeReinstall,
        IntegrationPhase::ExerciseAfterReinstall,
    ] {
        assert!(reinstall.iter().any(|(_, p, predicate, _)| *p == phase
            && *predicate == IntegrationPredicate::ApplicationReady));
    }
    assert!(reinstall.iter().any(
        |(_, phase, p, _)| *phase == IntegrationPhase::BeforeReinstall
            && *p == IntegrationPredicate::Lifetime(r.lifetime)
    ));
    assert_eq!(
        desktop_phase_definition(ScenarioKind::Upgrade, IntegrationPhase::Configured),
        Some(DefinitionKey {
            role: DefinitionRole::Configured,
            artifact: ArtifactRole::Base
        })
    );
    assert_eq!(
        desktop_phase_definition(
            ScenarioKind::Upgrade,
            IntegrationPhase::TargetInstalledAfterUpgrade
        ),
        Some(DefinitionKey {
            role: DefinitionRole::Configured,
            artifact: ArtifactRole::Target
        })
    );
    assert_eq!(
        desktop_phase_definition(ScenarioKind::Reinstall, IntegrationPhase::BeforeReinstall),
        Some(DefinitionKey {
            role: DefinitionRole::Configured,
            artifact: ArtifactRole::Target
        })
    );
}
