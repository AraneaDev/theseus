//! Schema2 recipes pin their full original source and explicit integration manifest.
use super::{PROFILE, model::*};
use crate::{
    model::ScenarioKind,
    native::{
        model::{decode, digest, ordinary},
        recipe::{NativeRecipe, NativeRunLimits, ValidatedNativeRecipe},
    },
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeSet, path::PathBuf};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationRecipe {
    pub kind: String,
    pub schema: u32,
    pub integration_manifest_digest: String,
    pub catalog: NativeRecipe,
    pub integrations: IntegrationRequirements,
}
#[derive(Debug, Clone)]
pub struct ValidatedIntegrationRecipe {
    pub recipe: NativeIntegrationRecipe,
    pub path: PathBuf,
    pub digest: String,
    pub core: ValidatedNativeRecipe,
    pub integration_manifest_digest: theseus_native_protocol::v2::Digest,
}
impl ValidatedIntegrationRecipe {
    pub fn decode(bytes: &[u8], path: PathBuf) -> Result<Self> {
        let recipe: NativeIntegrationRecipe = decode(bytes)?;
        recipe.validate()?;
        let integration_manifest_digest =
            theseus_native_protocol::v2::Digest::parse(&recipe.integration_manifest_digest)?;
        let core =
            ValidatedNativeRecipe::decode(&serde_json::to_vec(&recipe.catalog)?, path.clone())?;
        Ok(Self {
            recipe,
            path,
            digest: format!("{:x}", Sha256::digest(bytes)),
            core,
            integration_manifest_digest,
        })
    }
    pub fn validate_selection(
        &self,
        scenarios: &[ScenarioKind],
        limits: &NativeRunLimits,
    ) -> Result<()> {
        self.recipe.validate()?;
        self.core.validate_selection(scenarios, limits)?;
        if self.core.recipe.os == crate::native::recipe::NativeOs::Macos
            && self
                .recipe
                .integrations
                .desktop
                .as_ref()
                .is_some_and(|d| d.lifetime == LifetimeRequirement::OwnedProcessTreeSettled)
        {
            anyhow::bail!(
                "macOS original descendant scope cannot prove escaped descendants joined"
            );
        }
        if let Some(desktop) = &self.recipe.integrations.desktop
            && desktop.modes == [DesktopMode::NativeActivation]
        {
            let required = if self.core.recipe.os == crate::native::recipe::NativeOs::Macos {
                LifetimeRequirement::ActivationInstanceTerminated
            } else {
                LifetimeRequirement::OwnedPrimaryExited
            };
            ensure!(
                desktop.lifetime == required,
                "native mandatory lifetime differs from selected platform capability"
            );
        }
        // All catalog commands plus the declared rehearsal controls share one lease budget.
        for scenario in scenarios {
            let (steps, checkpoints): (u32, &[crate::model::Checkpoint]) = match scenario {
                ScenarioKind::Clean => (
                    4,
                    &[
                        crate::model::Checkpoint::TargetInstalled,
                        crate::model::Checkpoint::Configured,
                        crate::model::Checkpoint::Uninstalled,
                    ],
                ),
                ScenarioKind::Upgrade => (
                    5,
                    &[
                        crate::model::Checkpoint::BaseInstalled,
                        crate::model::Checkpoint::Configured,
                        crate::model::Checkpoint::TargetInstalled,
                        crate::model::Checkpoint::Uninstalled,
                    ],
                ),
                ScenarioKind::Reinstall => (
                    6,
                    &[
                        crate::model::Checkpoint::TargetInstalled,
                        crate::model::Checkpoint::Configured,
                        crate::model::Checkpoint::Reinstalled,
                        crate::model::Checkpoint::Uninstalled,
                    ],
                ),
                _ => unreachable!(),
            };
            let catalog = steps
                + self
                    .core
                    .recipe
                    .checks
                    .iter()
                    .filter(|c| checkpoints.contains(&c.checkpoint))
                    .count() as u32;
            let controls = self.recipe.integrations.service.as_ref().map_or(0, |s| {
                let start =
                    u32::from(s.controls.configured_activation == ActivationCause::RehearsalStart);
                start
                    + u32::from(
                        s.controls.uninstall_precondition == RuntimePrecondition::HarnessStopped,
                    )
                    + match scenario {
                        ScenarioKind::Upgrade => u32::from(
                            s.controls.upgrade_precondition == RuntimePrecondition::HarnessStopped,
                        ),
                        ScenarioKind::Reinstall => u32::from(
                            s.controls.reinstall_precondition
                                == RuntimePrecondition::HarnessStopped,
                        ),
                        _ => 0,
                    }
            });
            ensure!(
                catalog
                    + controls
                    + if self.recipe.integrations.desktop.is_some() {
                        if *scenario == ScenarioKind::Clean {
                            3
                        } else {
                            5
                        }
                    } else {
                        0
                    }
                    <= limits.max_operations,
                "integration operation budget exceeded"
            );
        }
        Ok(())
    }
    pub fn full_manifest_digest(&self) -> Result<String> {
        Ok(theseus_native_protocol::v2::integration_manifest_digest(
            theseus_native_protocol::v2::Digest::parse(&self.core.recipe.manifest_digest)?,
            self.integration_manifest_digest,
        )
        .to_hex())
    }
}
impl NativeIntegrationRecipe {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.kind == "theseus_native_recipe" && self.schema == 2,
            "unsupported integration recipe envelope"
        );
        digest(&self.integration_manifest_digest)?;
        self.catalog.validate()?;
        self.integrations.validate_required(&self.catalog.scenarios)
    }
}
impl IntegrationRequirements {
    pub fn validate(&self, scenarios: &[ScenarioKind]) -> Result<()> {
        ensure!(
            self.service.is_some() || self.desktop.is_some(),
            "empty integration requirements"
        );
        if let Some(service) = &self.service {
            service.resource.validate()?;
            service.readiness.validate()?;
            ensure!(
                !service.definitions.is_empty() && service.definitions.len() <= 6,
                "invalid definition count"
            );
            let mut keys = BTreeSet::new();
            for d in &service.definitions {
                ensure!(
                    keys.insert((d.role, d.artifact)),
                    "duplicate definition key"
                );
            }
            ensure!(
                !service.checks.is_empty() && service.checks.len() <= 64,
                "invalid service checks"
            );
            let mut ids = BTreeSet::new();
            for check in &service.checks {
                crate::native::model::text(&check.id, 128)?;
                ensure!(
                    ids.insert(&check.id) && !check.scenarios.is_empty(),
                    "duplicate or empty integration check"
                );
                for scenario in &check.scenarios {
                    ordinary(*scenario)?;
                    ensure!(
                        scenarios.contains(scenario),
                        "undeclared integration scenario"
                    );
                }
                ensure!(
                    check.role.is_some() == check.artifact.is_some(),
                    "partial definition key"
                );
                ensure!(
                    check
                        .scenarios
                        .iter()
                        .copied()
                        .collect::<BTreeSet<_>>()
                        .len()
                        == check.scenarios.len(),
                    "duplicate integration check scenario"
                );
                if let (Some(role), Some(artifact)) = (check.role, check.artifact) {
                    ensure!(
                        service
                            .definitions
                            .contains(&DefinitionKey { role, artifact }),
                        "check definition is undeclared"
                    );
                }
            }
        }
        if let Some(desktop) = &self.desktop {
            for scenario in scenarios {
                ordinary(*scenario)?;
            }
            ensure!(
                desktop.modes == [DesktopMode::OwnedExecutable]
                    || desktop.modes == [DesktopMode::NativeActivation],
                "select exactly one explicit desktop mode"
            );
            ensure!(
                desktop.modes != [DesktopMode::NativeActivation]
                    || desktop.lifetime != LifetimeRequirement::OwnedProcessTreeSettled,
                "native activation cannot prove owned process-tree settlement"
            );
            ensure!(
                desktop.readiness.kind == ReadyKind::LocalChallenge,
                "desktop readiness requires an authenticated local challenge"
            );
            ensure!(
                desktop.modes != [DesktopMode::OwnedExecutable]
                    || desktop.lifetime != LifetimeRequirement::ActivationInstanceTerminated,
                "owned executable cannot prove native activation instance termination"
            );
            desktop.resource.validate()?;
            desktop.readiness.validate()?;
            ensure!(
                !desktop.modes.is_empty() && desktop.modes.len() <= 2,
                "invalid desktop modes"
            );
            if let Some(service) = &self.service {
                ensure!(
                    service.resource != desktop.resource,
                    "overlapping integration slots"
                );
            }
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<String> {
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(PROFILE, self))?)
        ))
    }
}
impl ReadinessRequirement {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=30_000).contains(&self.deadline_ms),
            "invalid readiness deadline"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn required_clean_table_cannot_be_replaced_by_only_readiness_check() {
        let requirements = crate::native::integration::view::tests::report().requirements;
        assert!(
            requirements
                .validate_required(&[ScenarioKind::Clean])
                .is_err()
        );
    }
}

impl IntegrationRequirements {
    /// Required service predicates are compiled from the ordinary scenario, not supplied by omission.
    pub fn validate_required(&self, scenarios: &[ScenarioKind]) -> Result<()> {
        self.validate(scenarios)?;
        if let Some(service) = &self.service {
            ensure!(
                service.readiness.kind == ReadyKind::LocalChallenge,
                "service lifecycle requires independent application challenge"
            );
            for scenario in scenarios {
                let initial_artifact = if *scenario == ScenarioKind::Upgrade {
                    ArtifactRole::Base
                } else {
                    ArtifactRole::Target
                };
                let initial_phase = if *scenario == ScenarioKind::Upgrade {
                    IntegrationPhase::BaseInstalled
                } else {
                    IntegrationPhase::TargetInstalledInitial
                };
                let initial_role = if *scenario == ScenarioKind::Upgrade {
                    DefinitionRole::Base
                } else {
                    DefinitionRole::Target
                };
                let mut required = vec![
                    (
                        IntegrationPhase::Baseline,
                        None,
                        None,
                        ServicePredicate::RegistrationAbsent,
                    ),
                    (
                        initial_phase,
                        Some(initial_role),
                        Some(initial_artifact),
                        ServicePredicate::Registration,
                    ),
                    (
                        initial_phase,
                        Some(initial_role),
                        Some(initial_artifact),
                        ServicePredicate::Configuration,
                    ),
                    (
                        initial_phase,
                        Some(initial_role),
                        Some(initial_artifact),
                        ServicePredicate::Stopped,
                    ),
                    (
                        IntegrationPhase::Configured,
                        Some(DefinitionRole::Configured),
                        Some(initial_artifact),
                        ServicePredicate::Configuration,
                    ),
                    (
                        IntegrationPhase::Configured,
                        Some(DefinitionRole::Configured),
                        Some(initial_artifact),
                        ServicePredicate::Running,
                    ),
                    (
                        IntegrationPhase::Configured,
                        Some(DefinitionRole::Configured),
                        Some(initial_artifact),
                        ServicePredicate::ApplicationReady,
                    ),
                    (
                        IntegrationPhase::Uninstalled,
                        None,
                        None,
                        ServicePredicate::RegistrationAbsent,
                    ),
                ];
                let exercise_phases: &[IntegrationPhase] = if *scenario == ScenarioKind::Reinstall {
                    &[
                        IntegrationPhase::ExerciseBeforeReinstall,
                        IntegrationPhase::ExerciseAfterReinstall,
                    ]
                } else {
                    &[IntegrationPhase::ExerciseFinal]
                };
                for phase in exercise_phases {
                    for predicate in [
                        ServicePredicate::Running,
                        ServicePredicate::ApplicationReady,
                    ] {
                        required.push((
                            *phase,
                            Some(DefinitionRole::Configured),
                            Some(ArtifactRole::Target),
                            predicate,
                        ));
                    }
                }
                let transition_phase = match scenario {
                    ScenarioKind::Upgrade => Some(IntegrationPhase::TargetInstalledAfterUpgrade),
                    ScenarioKind::Reinstall => Some(IntegrationPhase::Reinstalled),
                    _ => None,
                };
                if let Some(phase) = transition_phase {
                    for predicate in [
                        ServicePredicate::Registration,
                        ServicePredicate::Configuration,
                        ServicePredicate::Running,
                        ServicePredicate::ApplicationReady,
                    ] {
                        required.push((
                            phase,
                            Some(DefinitionRole::Configured),
                            Some(ArtifactRole::Target),
                            predicate,
                        ));
                    }
                }
                if service.controls.uninstall_precondition == RuntimePrecondition::Running {
                    required.push((
                        IntegrationPhase::Uninstalled,
                        None,
                        None,
                        ServicePredicate::Stopped,
                    ));
                }
                for (phase, role, artifact, predicate) in required {
                    ensure!(
                        service.checks.iter().any(|c| c.scenarios.contains(scenario)
                            && c.phase == phase
                            && c.role == role
                            && c.artifact == artifact
                            && c.predicate == predicate),
                        "missing required service predicate {predicate:?} at {phase:?}"
                    );
                    if let (Some(role), Some(artifact)) = (role, artifact) {
                        ensure!(
                            service
                                .definitions
                                .contains(&DefinitionKey { role, artifact }),
                            "required service definition missing"
                        );
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod required_table_review_tests {
    use super::*;
    #[test]
    fn review_rejects_missing_initial_and_final_runtime_predicates() {
        let mut requirements = crate::native::integration::view::tests::report().requirements;
        let service = requirements.service.as_mut().unwrap();
        service.definitions = vec![
            DefinitionKey {
                role: DefinitionRole::Target,
                artifact: ArtifactRole::Target,
            },
            DefinitionKey {
                role: DefinitionRole::Configured,
                artifact: ArtifactRole::Target,
            },
        ];
        service.checks = vec![];
        for (phase, role, predicates) in [
            (
                IntegrationPhase::Baseline,
                None,
                vec![ServicePredicate::RegistrationAbsent],
            ),
            (
                IntegrationPhase::TargetInstalledInitial,
                Some(DefinitionRole::Target),
                vec![
                    ServicePredicate::Registration,
                    ServicePredicate::Configuration,
                    ServicePredicate::Stopped,
                ],
            ),
            (
                IntegrationPhase::Configured,
                Some(DefinitionRole::Configured),
                vec![
                    ServicePredicate::Configuration,
                    ServicePredicate::Running,
                    ServicePredicate::ApplicationReady,
                ],
            ),
            (
                IntegrationPhase::ExerciseFinal,
                Some(DefinitionRole::Configured),
                vec![
                    ServicePredicate::Running,
                    ServicePredicate::ApplicationReady,
                ],
            ),
            (
                IntegrationPhase::Uninstalled,
                None,
                vec![
                    ServicePredicate::Stopped,
                    ServicePredicate::RegistrationAbsent,
                ],
            ),
        ] {
            for predicate in predicates {
                service.checks.push(ServiceCheckRequirement {
                    id: format!("{phase:?}-{predicate:?}"),
                    scenarios: vec![ScenarioKind::Clean],
                    phase,
                    role,
                    artifact: role.map(|_| ArtifactRole::Target),
                    predicate,
                });
            }
        }
        requirements
            .validate_required(&[ScenarioKind::Clean])
            .unwrap();
        for (phase, predicate) in [
            (
                IntegrationPhase::TargetInstalledInitial,
                ServicePredicate::Configuration,
            ),
            (
                IntegrationPhase::TargetInstalledInitial,
                ServicePredicate::Stopped,
            ),
            (IntegrationPhase::ExerciseFinal, ServicePredicate::Running),
        ] {
            let mut missing = requirements.clone();
            missing
                .service
                .as_mut()
                .unwrap()
                .checks
                .retain(|c| !(c.phase == phase && c.predicate == predicate));
            assert!(
                missing.validate_required(&[ScenarioKind::Clean]).is_err(),
                "missing {predicate:?} at {phase:?} must refuse"
            );
        }
    }
}

#[cfg(test)]
mod transition_contract_tests {
    use super::*;

    fn requirements(scenario: ScenarioKind) -> IntegrationRequirements {
        let mut requirements = crate::native::integration::view::tests::report().requirements;
        let service = requirements.service.as_mut().unwrap();
        service.definitions = vec![
            DefinitionKey {
                role: DefinitionRole::Base,
                artifact: ArtifactRole::Base,
            },
            DefinitionKey {
                role: DefinitionRole::Target,
                artifact: ArtifactRole::Target,
            },
            DefinitionKey {
                role: DefinitionRole::Configured,
                artifact: ArtifactRole::Base,
            },
            DefinitionKey {
                role: DefinitionRole::Configured,
                artifact: ArtifactRole::Target,
            },
        ];
        service.checks.clear();
        let initial_artifact = if scenario == ScenarioKind::Upgrade {
            ArtifactRole::Base
        } else {
            ArtifactRole::Target
        };
        let initial_role = if scenario == ScenarioKind::Upgrade {
            DefinitionRole::Base
        } else {
            DefinitionRole::Target
        };
        let initial_phase = if scenario == ScenarioKind::Upgrade {
            IntegrationPhase::BaseInstalled
        } else {
            IntegrationPhase::TargetInstalledInitial
        };
        let mut rows = vec![
            (
                IntegrationPhase::Baseline,
                None,
                None,
                vec![ServicePredicate::RegistrationAbsent],
            ),
            (
                initial_phase,
                Some(initial_role),
                Some(initial_artifact),
                vec![
                    ServicePredicate::Registration,
                    ServicePredicate::Configuration,
                    ServicePredicate::Stopped,
                ],
            ),
            (
                IntegrationPhase::Configured,
                Some(DefinitionRole::Configured),
                Some(initial_artifact),
                vec![
                    ServicePredicate::Configuration,
                    ServicePredicate::Running,
                    ServicePredicate::ApplicationReady,
                ],
            ),
            (
                IntegrationPhase::Uninstalled,
                None,
                None,
                vec![
                    ServicePredicate::Stopped,
                    ServicePredicate::RegistrationAbsent,
                ],
            ),
        ];
        let runtime = vec![
            ServicePredicate::Running,
            ServicePredicate::ApplicationReady,
        ];
        if scenario == ScenarioKind::Upgrade {
            rows.push((
                IntegrationPhase::TargetInstalledAfterUpgrade,
                Some(DefinitionRole::Configured),
                Some(ArtifactRole::Target),
                vec![
                    ServicePredicate::Registration,
                    ServicePredicate::Configuration,
                    ServicePredicate::Running,
                    ServicePredicate::ApplicationReady,
                ],
            ));
            rows.push((
                IntegrationPhase::ExerciseFinal,
                Some(DefinitionRole::Configured),
                Some(ArtifactRole::Target),
                runtime,
            ));
        } else if scenario == ScenarioKind::Reinstall {
            rows.push((
                IntegrationPhase::Reinstalled,
                Some(DefinitionRole::Configured),
                Some(ArtifactRole::Target),
                vec![
                    ServicePredicate::Registration,
                    ServicePredicate::Configuration,
                    ServicePredicate::Running,
                    ServicePredicate::ApplicationReady,
                ],
            ));
            rows.push((
                IntegrationPhase::ExerciseBeforeReinstall,
                Some(DefinitionRole::Configured),
                Some(ArtifactRole::Target),
                runtime.clone(),
            ));
            rows.push((
                IntegrationPhase::ExerciseAfterReinstall,
                Some(DefinitionRole::Configured),
                Some(ArtifactRole::Target),
                runtime,
            ));
        }
        for (phase, role, artifact, predicates) in rows {
            for predicate in predicates {
                service.checks.push(ServiceCheckRequirement {
                    id: format!("{phase:?}-{predicate:?}"),
                    scenarios: vec![scenario],
                    phase,
                    role,
                    artifact,
                    predicate,
                });
            }
        }
        requirements
    }

    #[test]
    fn service_upgrade_sut_transitions_from_declared_runtime_state_and_preserves_data() {
        let requirements = requirements(ScenarioKind::Upgrade);
        requirements
            .validate_required(&[ScenarioKind::Upgrade])
            .unwrap();
        for predicate in [
            ServicePredicate::Registration,
            ServicePredicate::Configuration,
            ServicePredicate::Running,
            ServicePredicate::ApplicationReady,
        ] {
            let mut missing = requirements.clone();
            missing.service.as_mut().unwrap().checks.retain(|c| {
                !(c.phase == IntegrationPhase::TargetInstalledAfterUpgrade
                    && c.predicate == predicate)
            });
            assert!(
                missing.validate_required(&[ScenarioKind::Upgrade]).is_err(),
                "missing Upgrade postcondition {predicate:?} must refuse"
            );
        }
    }

    #[test]
    fn service_reinstall_compares_original_definition_reader_and_data() {
        let requirements = requirements(ScenarioKind::Reinstall);
        requirements
            .validate_required(&[ScenarioKind::Reinstall])
            .unwrap();
        for phase in [
            IntegrationPhase::Reinstalled,
            IntegrationPhase::ExerciseBeforeReinstall,
            IntegrationPhase::ExerciseAfterReinstall,
        ] {
            let mut missing = requirements.clone();
            missing
                .service
                .as_mut()
                .unwrap()
                .checks
                .retain(|c| c.phase != phase);
            assert!(
                missing
                    .validate_required(&[ScenarioKind::Reinstall])
                    .is_err(),
                "missing Reinstall phase {phase:?} must refuse"
            );
        }
    }
}

#[cfg(test)]
mod desktop_selection_tests {
    use super::*;
    #[test]
    fn macos_owned_primary_selection_defers_to_committed_dynamic_admission() {
        use crate::native::recipe::NativeOs;
        let original = ValidatedIntegrationRecipe::decode(
            include_bytes!("../../../examples/native/service-recipe.example.json"),
            "pure-recipe.json".into(),
        )
        .unwrap();
        for os in [NativeOs::Linux, NativeOs::Macos, NativeOs::Windows] {
            let mut recipe = original.clone();
            recipe.core.recipe.os = os;
            recipe.recipe.catalog.os = os;
            let reader = if os == NativeOs::Windows {
                "reader.exe"
            } else {
                "reader"
            };
            recipe.core.recipe.leaves.reader = reader.into();
            recipe.recipe.catalog.leaves.reader = reader.into();
            recipe.recipe.integrations.service = None;
            recipe.recipe.integrations.desktop = Some(DesktopRequirement {
                resource: ResourceSlot::new("desktop").unwrap(),
                modes: vec![DesktopMode::OwnedExecutable],
                readiness: ReadinessRequirement {
                    kind: ReadyKind::LocalChallenge,
                    protocol: ReadyProtocol::TheseusFixtureReadyV1,
                    deadline_ms: 1000,
                },
                lifetime: LifetimeRequirement::OwnedPrimaryExited,
            });
            let limits = recipe.core.recipe.limits.clone();
            let selected = recipe.validate_selection(&[ScenarioKind::Clean], &limits);
            if os == NativeOs::Macos {
                selected.unwrap(); // The dynamic pre-Acquire gate requires the exact committed declaration.
                recipe
                    .recipe
                    .integrations
                    .desktop
                    .as_mut()
                    .unwrap()
                    .lifetime = LifetimeRequirement::OwnedProcessTreeSettled;
                assert!(
                    recipe
                        .validate_selection(&[ScenarioKind::Clean], &limits)
                        .unwrap_err()
                        .to_string()
                        .contains("descendant scope")
                );
                let desktop = recipe.recipe.integrations.desktop.as_mut().unwrap();
                desktop.modes = vec![DesktopMode::NativeActivation];
                desktop.lifetime = LifetimeRequirement::ActivationInstanceTerminated;
                recipe
                    .validate_selection(&[ScenarioKind::Clean], &limits)
                    .unwrap();
            } else {
                selected.unwrap();
            }
        }
    }
    #[test]
    fn native_selection_keeps_each_os_lifetime_and_single_lease_budget() {
        use crate::native::recipe::NativeOs;
        let original = ValidatedIntegrationRecipe::decode(
            include_bytes!("../../../examples/native/service-recipe.example.json"),
            "pure-recipe.json".into(),
        )
        .unwrap();
        for os in [NativeOs::Linux, NativeOs::Macos, NativeOs::Windows] {
            let mut recipe = original.clone();
            recipe.core.recipe.os = os;
            recipe.recipe.catalog.os = os;
            let reader = if os == NativeOs::Windows {
                "reader.exe"
            } else {
                "reader"
            };
            recipe.core.recipe.leaves.reader = reader.into();
            recipe.recipe.catalog.leaves.reader = reader.into();
            recipe.recipe.integrations.service = None;
            let lifetime = if os == NativeOs::Macos {
                LifetimeRequirement::ActivationInstanceTerminated
            } else {
                LifetimeRequirement::OwnedPrimaryExited
            };
            recipe.recipe.integrations.desktop = Some(DesktopRequirement {
                resource: ResourceSlot::new("desktop").unwrap(),
                modes: vec![DesktopMode::NativeActivation],
                readiness: ReadinessRequirement {
                    kind: ReadyKind::LocalChallenge,
                    protocol: ReadyProtocol::TheseusFixtureReadyV1,
                    deadline_ms: 1000,
                },
                lifetime,
            });
            let limits = recipe.core.recipe.limits.clone();
            recipe
                .validate_selection(&[ScenarioKind::Clean], &limits)
                .unwrap();
            let mut too_small = limits.clone();
            too_small.max_operations = 1;
            assert!(
                recipe
                    .validate_selection(&[ScenarioKind::Clean], &too_small)
                    .is_err()
            );
            recipe
                .recipe
                .integrations
                .desktop
                .as_mut()
                .unwrap()
                .lifetime = if os == NativeOs::Macos {
                LifetimeRequirement::OwnedPrimaryExited
            } else {
                LifetimeRequirement::ActivationInstanceTerminated
            };
            assert!(
                recipe
                    .validate_selection(&[ScenarioKind::Clean], &limits)
                    .is_err()
            );
        }
    }
    #[test]
    fn native_ordinary_and_combined_require_one_mode_and_real_lifetime() {
        let mut requirements = crate::native::integration::view::tests::report().requirements;
        requirements.desktop = Some(DesktopRequirement {
            resource: ResourceSlot::new("desktop").unwrap(),
            modes: vec![DesktopMode::NativeActivation],
            readiness: ReadinessRequirement {
                kind: ReadyKind::LocalChallenge,
                protocol: ReadyProtocol::TheseusFixtureReadyV1,
                deadline_ms: 1000,
            },
            lifetime: LifetimeRequirement::OwnedPrimaryExited,
        });
        // Recipe commitments only; no session, SDK, lease or manager is created.
        for combined in [true, false] {
            if !combined {
                requirements.service = None;
            }
            for scenario in [
                ScenarioKind::Clean,
                ScenarioKind::Upgrade,
                ScenarioKind::Reinstall,
            ] {
                // This boundary checks declared mode/scenario shape. Mandatory
                // service checks are independently enforced by validate_required.
                if let Some(service) = &mut requirements.service {
                    for check in &mut service.checks {
                        check.scenarios = vec![scenario];
                    }
                }
                requirements.validate(&[scenario]).unwrap();
            }
        }
        requirements
            .desktop
            .as_mut()
            .unwrap()
            .modes
            .push(DesktopMode::OwnedExecutable);
        assert!(requirements.validate(&[ScenarioKind::Clean]).is_err());
        requirements.desktop.as_mut().unwrap().modes = vec![DesktopMode::NativeActivation];
        requirements.desktop.as_mut().unwrap().lifetime =
            LifetimeRequirement::OwnedProcessTreeSettled;
        assert!(requirements.validate(&[ScenarioKind::Clean]).is_err());
    }
    #[test]
    fn owned_desktop_ordinary_and_combined_admission_preserves_native_and_slot_boundaries() {
        let mut requirements = crate::native::integration::view::tests::report().requirements;
        requirements.service = None;
        requirements.desktop = Some(DesktopRequirement {
            resource: ResourceSlot::new("desktop").unwrap(),
            modes: vec![DesktopMode::OwnedExecutable],
            readiness: ReadinessRequirement {
                kind: ReadyKind::LocalChallenge,
                protocol: ReadyProtocol::TheseusFixtureReadyV1,
                deadline_ms: 1000,
            },
            lifetime: LifetimeRequirement::OwnedProcessTreeSettled,
        });
        requirements
            .validate_required(&[ScenarioKind::Clean])
            .unwrap();
        requirements
            .validate_required(&[ScenarioKind::Upgrade])
            .unwrap();
        requirements
            .validate_required(&[ScenarioKind::Reinstall])
            .unwrap();
        requirements.service = crate::native::integration::view::tests::report()
            .requirements
            .service;
        requirements.validate(&[ScenarioKind::Clean]).unwrap();
        requirements.desktop.as_mut().unwrap().resource =
            requirements.service.as_ref().unwrap().resource.clone();
        assert!(requirements.validate(&[ScenarioKind::Clean]).is_err());
        requirements.desktop.as_mut().unwrap().resource = ResourceSlot::new("desktop").unwrap();
        requirements.desktop.as_mut().unwrap().modes = vec![DesktopMode::NativeActivation];
        requirements.desktop.as_mut().unwrap().lifetime = LifetimeRequirement::OwnedPrimaryExited;
        requirements.validate(&[ScenarioKind::Clean]).unwrap();
        requirements.desktop.as_mut().unwrap().lifetime =
            LifetimeRequirement::OwnedProcessTreeSettled;
        requirements.service = None;
        requirements.desktop.as_mut().unwrap().modes = vec![DesktopMode::NativeActivation];
        assert!(
            requirements
                .validate_required(&[ScenarioKind::Clean])
                .is_err()
        );
        requirements.desktop.as_mut().unwrap().lifetime = LifetimeRequirement::OwnedPrimaryExited;
        requirements
            .validate_required(&[ScenarioKind::Clean])
            .unwrap();
        requirements.desktop.as_mut().unwrap().lifetime =
            LifetimeRequirement::ActivationInstanceTerminated;
        requirements
            .validate_required(&[ScenarioKind::Clean])
            .unwrap();
    }
}
