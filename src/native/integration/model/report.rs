use super::serde_util::required_optional;
use super::*;
use crate::model::{CheckOutcome, RunOutcome, ScenarioKind};
use crate::native::model::*;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "provenance", rename_all = "snake_case", deny_unknown_fields)]
pub enum IntegrationProvenance<T> {
    Observed { value: T, source: String },
    ManagerReported { value: T, manager: String },
    ApplicationReported { value: T, application: String },
    ProviderReported { value: T, provider: String },
    Unavailable { reason: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServiceState {
    Absent,
    Present {
        registered: bool,
        configured: bool,
        running: bool,
        definition_digest: Digest,
        origin: String,
    },
    Unknown {
        reason: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationBinding {
    pub id: String,
    pub run_id: String,
    pub scenario_id: String,
    pub ownership_id: String,
    pub lease: LeaseId,
    pub slot: ResourceSlot,
    pub request_id: String,
    #[serde(deserialize_with = "required_optional")]
    pub action: Option<OperationId>,
    pub binding_digest: Digest,
    pub source_profile: String,
    pub source_version: String,
    pub phase: IntegrationPhase,
    #[serde(deserialize_with = "required_optional")]
    pub definition: Option<DefinitionKey>,
    pub sequence: u32,
    pub started_ms: u64,
    pub finished_ms: u64,
    pub complete: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundServiceObservation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<theseus_native_protocol::v2::ServicePlatformFacts>,
    #[serde(deserialize_with = "required_optional")]
    pub readiness: Option<theseus_native_protocol::v2::ReadinessObservation>,
    pub binding: ObservationBinding,
    pub state: IntegrationProvenance<ServiceState>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundDesktopObservation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<DesktopGenerationEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_valid: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broker_state: Option<theseus_native_protocol::v2::broker::BrokerState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<theseus_native_protocol::v2::ReadinessObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<theseus_native_protocol::v2::broker::DesktopEvidence>,
    pub binding: ObservationBinding,
    pub entry_present: IntegrationProvenance<bool>,
    pub activated: IntegrationProvenance<bool>,
    pub primary_exited: IntegrationProvenance<bool>,
    pub descendant_scope_settled: IntegrationProvenance<bool>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceEvidence {
    pub slot: ResourceSlot,
    pub observations: Vec<BoundServiceObservation>,
    pub actions: Vec<ActionReference>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopEvidence {
    pub slot: ResourceSlot,
    pub observations: Vec<BoundDesktopObservation>,
    pub actions: Vec<ActionReference>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationCheck {
    pub id: String,
    pub scenario_id: String,
    pub phase: IntegrationPhase,
    pub slot: ResourceSlot,
    #[serde(deserialize_with = "required_optional")]
    pub role: Option<DefinitionRole>,
    #[serde(deserialize_with = "required_optional")]
    pub artifact: Option<ArtifactRole>,
    pub predicate: IntegrationPredicate,
    pub origin: CheckOrigin,
    #[serde(deserialize_with = "required_optional")]
    pub sut_operation: Option<OperationId>,
    pub observation_ids: Vec<String>,
    pub outcome: CheckOutcome,
    pub detail: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationDispositions {
    pub helper_worker: Disposition,
    pub registration: Disposition,
    pub manager_action: Disposition,
    pub broker: Disposition,
    pub primary: Disposition,
    pub descendant_scope: Disposition,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeDesktopLifetimeProjection {
    pub original: ActionReference,
    pub state: theseus_native_protocol::v2::broker::NativeLifetimeState,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedDesktopLifetimeProjection {
    pub original: ActionReference,
    pub state: theseus_native_protocol::v2::broker::OwnedLifetimeState,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationScenarioEvidence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned_lifetime: Option<OwnedDesktopLifetimeProjection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_lifetime: Option<NativeDesktopLifetimeProjection>,
    pub scenario_id: String,
    pub ownership_id: String,
    pub services: Vec<ServiceEvidence>,
    pub desktops: Vec<DesktopEvidence>,
    pub checks: Vec<IntegrationCheck>,
    pub dispositions: IntegrationDispositions,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogRunEvidence {
    pub scenarios: Vec<NativeScenario>,
    pub identities: Vec<Provenance<String>>,
    pub provider_claims: Vec<ProviderReceipt>,
    pub failures: Vec<String>,
    pub outcome: RunOutcome,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationReport {
    pub kind: String,
    pub schema: u32,
    pub profile: String,
    pub run_id: String,
    pub recipe_digest: Digest,
    pub core_recipe_digest: Digest,
    pub core_manifest_digest: Digest,
    pub integration_manifest_digest: Digest,
    pub manifest_digest: Digest,
    pub requirements: IntegrationRequirements,
    pub compiled_requirements_digest: Digest,
    pub selected_scenarios: Vec<ScenarioKind>,
    pub catalog: CatalogRunEvidence,
    pub integrations: Vec<IntegrationScenarioEvidence>,
    pub outcome: RunOutcome,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationResourceKey {
    pub scenario_id: String,
    pub ownership_id: String,
    pub slot: ResourceSlot,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceBindingSnapshot {
    pub digest: Digest,
    pub basis: BindingBasis,
    pub evidence_ids: Vec<String>,
    #[serde(deserialize_with = "required_optional")]
    pub definition: Option<DefinitionKey>,
    pub artifact_commitment: Digest,
    pub namespace_or_session_receipt_digest: Digest,
    #[serde(deserialize_with = "required_optional")]
    pub original_action: Option<OperationId>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationResourceRecord {
    pub key: IntegrationResourceKey,
    pub kind: IntegrationResourceKind,
    pub manifest_digest: Digest,
    pub initial_binding: ResourceBindingSnapshot,
    pub binding_history: Vec<ResourceBindingSnapshot>,
    pub current_binding_digest: Digest,
    #[serde(deserialize_with = "required_optional")]
    pub revocation: Option<AuthorityRevocation>,
    pub disposition: Disposition,
}

impl IntegrationDispositions {
    pub fn all(&self) -> [&Disposition; 6] {
        [
            &self.helper_worker,
            &self.registration,
            &self.manager_action,
            &self.broker,
            &self.primary,
            &self.descendant_scope,
        ]
    }
    pub fn complete(&self) -> bool {
        self.all()
            .iter()
            .all(|d| matches!(d, Disposition::Complete { .. }))
    }
}
impl IntegrationScenarioEvidence {
    pub fn complete(&self) -> bool {
        if self.owned_lifetime.is_some()
            || self
                .desktops
                .iter()
                .flat_map(|d| &d.observations)
                .filter_map(|o| o.broker_state.as_ref())
                .any(|s| s.owned_lifetime.is_some())
        {
            return crate::native::integration::owned_lifetime::scenario_complete(self);
        }
        let Some(native) = &self.native_lifetime else {
            let native_observed=self.desktops.iter().flat_map(|d|&d.observations).filter_map(|o|o.broker_state.as_ref()).any(|s|s.native_lifetime.is_some() || matches!(&s.effect,theseus_native_protocol::v2::broker::BrokerEffect::Receipt {receipt} if receipt.evidence==theseus_native_protocol::v2::broker::DesktopEvidence::NativeActivation));
            return !native_observed && self.dispositions.complete();
        };
        use theseus_native_protocol::v2::broker::*;
        if native.state.validate().is_err() {
            return false;
        }
        let d = &self.dispositions;
        let complete = |d: &Disposition| matches!(d, Disposition::Complete { .. });
        [
            &d.helper_worker,
            &d.registration,
            &d.manager_action,
            &d.broker,
        ]
        .into_iter()
        .all(complete)
            && match native.state.primary {
                NativePrimaryState::Unavailable
                    if native.state.authority == NativeLifetimeAuthority::ManagerInstance =>
                {
                    matches!(d.primary, Disposition::NotStarted)
                }
                NativePrimaryState::Exited => complete(&d.primary),
                _ => false,
            }
            && native.state.descendants == NativeTreeState::Unavailable
            && matches!(d.descendant_scope, Disposition::NotStarted)
            && self.desktops.iter().all(|desktop| {
                let mut latest = BTreeMap::new();
                for o in &desktop.observations {
                    if let (Some(action), Some(state)) = (&o.binding.action, &o.broker_state) {
                        latest.insert(action, state);
                    }
                }
                latest
                    .values()
                    .filter(|s| s.native_lifetime.is_some())
                    .all(|s| s.original_retired())
            })
            && self.desktops.iter().any(|desktop| {
                desktop.actions.contains(&native.original)
                    && desktop.observations.iter().any(|o| {
                        o.binding.action.as_ref() == Some(&native.original.operation)
                            && o.broker_state.as_ref().is_some_and(|s| {
                                s.native_lifetime.as_ref() == Some(&native.state)
                                    && s.original_retired()
                            })
                    })
            })
    }
}
impl ObservationBinding {
    pub fn validate(&self) -> Result<()> {
        for value in [
            &self.id,
            &self.run_id,
            &self.scenario_id,
            &self.ownership_id,
            &self.request_id,
            &self.source_profile,
            &self.source_version,
        ] {
            text(value, 256)?;
        }
        hex(&self.lease, 32)?;
        self.slot.validate()?;
        digest(&self.binding_digest)?;
        if let Some(action) = &self.action {
            hex(action, 32)?;
        }
        ensure!(
            self.finished_ms >= self.started_ms,
            "reversed observation interval"
        );
        Ok(())
    }
}

impl NativeIntegrationReport {
    pub fn new(
        run_id: String,
        recipe: &crate::native::integration::ValidatedIntegrationRecipe,
        selected_scenarios: Vec<ScenarioKind>,
    ) -> Result<Self> {
        Ok(Self {
            kind: "theseus_native_report".into(),
            schema: 2,
            profile: crate::native::integration::PROFILE.into(),
            run_id,
            recipe_digest: recipe.digest.clone(),
            core_recipe_digest: recipe.core.digest.clone(),
            core_manifest_digest: recipe.core.recipe.manifest_digest.clone(),
            integration_manifest_digest: recipe.recipe.integration_manifest_digest.clone(),
            manifest_digest: recipe.full_manifest_digest()?,
            requirements: recipe.recipe.integrations.clone(),
            compiled_requirements_digest: recipe.recipe.integrations.digest()?,
            selected_scenarios,
            catalog: CatalogRunEvidence {
                scenarios: vec![],
                identities: vec![],
                provider_claims: vec![],
                failures: vec![],
                outcome: RunOutcome::Incomplete,
            },
            integrations: vec![],
            outcome: RunOutcome::Incomplete,
        })
    }
}
