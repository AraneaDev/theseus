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
        ensure!(
            scenarios.iter().all(|s| *s == ScenarioKind::Clean),
            "native integration currently supports Clean only; Upgrade/Reinstall require admitted native configuration transitions"
        );
        self.recipe.validate()?;
        self.core.validate_selection(scenarios, limits)?;
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
                u32::from(s.controls.configured_activation == ActivationCause::RehearsalStart)
                    + u32::from(
                        s.controls.uninstall_precondition == RuntimePrecondition::HarnessStopped,
                    )
                    + u32::from(
                        s.controls.upgrade_precondition == RuntimePrecondition::HarnessStopped,
                    )
                    + u32::from(
                        s.controls.reinstall_precondition == RuntimePrecondition::HarnessStopped,
                    )
            });
            ensure!(
                catalog + controls <= limits.max_operations,
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
            }
        }
        if let Some(desktop) = &self.desktop {
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
                        IntegrationPhase::ExerciseFinal,
                        Some(DefinitionRole::Configured),
                        Some(ArtifactRole::Target),
                        ServicePredicate::ApplicationReady,
                    ),
                    (
                        IntegrationPhase::ExerciseFinal,
                        Some(DefinitionRole::Configured),
                        Some(ArtifactRole::Target),
                        ServicePredicate::Running,
                    ),
                    (
                        IntegrationPhase::Uninstalled,
                        None,
                        None,
                        ServicePredicate::RegistrationAbsent,
                    ),
                ];
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
