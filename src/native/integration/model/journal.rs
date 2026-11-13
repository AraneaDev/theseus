use super::serde_util::required_optional;
use super::*;
use crate::model::ScenarioKind;
use crate::native::model::*;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum IntegrationRequest {
    Service {
        lease: LeaseId,
        operation: OperationId,
        slot: ResourceSlot,
        prior_binding_digest: Digest,
        control: ServiceAction,
    },
    Desktop {
        lease: LeaseId,
        operation: OperationId,
        slot: ResourceSlot,
        prior_binding_digest: Digest,
        mode: DesktopMode,
        request: Box<theseus_native_protocol::v2::broker::DesktopRequest>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum IntegrationOutcome {
    Complete { receipt: Digest },
    Unknown { reason: String },
    Failed { reason: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerOperationIdentity {
    pub helper_lease: LeaseId,
    pub helper_operation: OperationId,
    pub broker_lease: LeaseId,
    pub request_digest: Digest,
    pub binding_digest: Digest,
    pub activation_nonce: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationActionRecord {
    pub ownership_id: String,
    pub lease: LeaseId,
    pub slot: ResourceSlot,
    pub operation: OperationId,
    pub request: IntegrationRequest,
    pub request_digest: Digest,
    pub prior_binding_digest: Digest,
    #[serde(deserialize_with = "required_optional")]
    pub broker_identity: Option<BrokerOperationIdentity>,
    pub completion: Disposition,
    #[serde(deserialize_with = "required_optional")]
    pub terminal: Option<IntegrationOutcome>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerIntentRecord {
    pub intent: String,
    pub binding: String,
    pub manifest_digest: String,
    pub workspace_marker: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerBindingRecord {
    pub original: theseus_native_protocol::v2::broker::BrokerBind,
    pub ownership_id: String,
    pub helper_lease: LeaseId,
    pub slot: ResourceSlot,
    pub original_intent: BrokerIntentRecord,
    pub publication_scope: Option<theseus_native_protocol::v2::broker::IntentPublicationScope>,
    pub original_intent_reference: String,
    pub expected_header_digest: Digest,
    pub session_id: String,
    pub session_epoch: Digest,
    #[serde(deserialize_with = "required_optional")]
    pub broker_lease: Option<LeaseId>,
    pub disposition: Disposition,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadinessChallenge {
    pub resource_binding_digest: String,
    pub action_digest: String,
    pub expectation_digest: String,
    pub ownership_id: String,
    pub lease: LeaseId,
    pub slot: ResourceSlot,
    pub operation: OperationId,
    pub nonce: String,
    pub deadline_ms: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadinessExpectation {
    pub role: DefinitionRole,
    pub artifact: ArtifactRole,
    pub artifact_length: u64,
    pub configuration_digest: String,
    pub artifact_digest: Digest,
    pub version: String,
    pub data_digest: Digest,
    pub initialized: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadinessProbeRecord {
    pub challenge: ReadinessChallenge,
    pub expectation: ReadinessExpectation,
    pub expectation_digest: Digest,
    pub observation_ids: Vec<String>,
    pub completion: Disposition,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginalQueryOutcome {
    pub original: ActionReference,
    pub view: OriginalQueryView,
    pub outcome: IntegrationOutcome,
}
/// Authenticated original broker lifetime knowledge, separate from the immutable
/// application/activation effect terminal stored in OriginalQueryOutcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerObservationSource {
    Query,
    Cancel,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveredBrokerObservation {
    pub original: ActionReference,
    pub identity: theseus_native_protocol::v2::broker::BrokerOperationIdentity,
    pub source: BrokerObservationSource,
    pub state: theseus_native_protocol::v2::broker::BrokerState,
}
/// Immutable knowledge at recovery publication, before any Query or Cancel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginalBrokerEffectBaseline {
    pub original: ActionReference,
    #[serde(deserialize_with = "required_optional")]
    pub terminal: Option<IntegrationOutcome>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerReleaseSource {
    ReleaseReply,
    QueryLease,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveredBrokerRelease {
    pub helper_lease: LeaseId,
    pub broker_lease: LeaseId,
    pub helper_intent: String,
    pub source: BrokerReleaseSource,
}

/// Saved knowledge about an exact original action, never execution authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginalQueryView {
    CachedTerminal,
    NonterminalUnknown,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationRecoveryRecord {
    pub recovery_id: String,
    pub ownership_id: String,
    pub original_actions: Vec<ActionReference>,
    pub prior_local: Vec<Disposition>,
    pub local: Disposition,
    pub broker: Disposition,
    pub provider: Disposition,
    pub queries: Vec<OriginalQueryOutcome>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub broker_effect_baselines: Vec<OriginalBrokerEffectBaseline>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub broker_observations: Vec<RecoveredBrokerObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broker_release: Option<RecoveredBrokerRelease>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationJournal {
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
    #[serde(deserialize_with = "required_optional")]
    pub launch_fingerprint: Option<Digest>,
    pub ownership: Vec<NativeResource>,
    pub operations: Vec<NativeOperation>,
    pub provider_calls: Vec<NativeProviderCall>,
    pub releases: Vec<NativeRelease>,
    pub recovery_attempts: Vec<NativeRecoveryAttempt>,
    pub failures: Vec<String>,
    pub integration_ownership: Vec<IntegrationResourceRecord>,
    pub integration_actions: Vec<IntegrationActionRecord>,
    pub broker_bindings: Vec<BrokerBindingRecord>,
    pub readiness_probes: Vec<ReadinessProbeRecord>,
    pub integration_recoveries: Vec<IntegrationRecoveryRecord>,
}

#[derive(Debug, Clone)]
// Preserve the concrete versioned DTO contract; no live native owners are stored.
#[allow(clippy::large_enum_variant)]
pub enum NativeReportVersion {
    CatalogV1(NativeReport),
    IntegrationV2(NativeIntegrationReport),
}
#[derive(Debug, Clone)]
// Preserve the concrete versioned DTO contract; no live native owners are stored.
#[allow(clippy::large_enum_variant)]
pub enum NativeJournalVersion {
    CatalogV1(NativeJournal),
    IntegrationV2(NativeIntegrationJournal),
}

impl NativeIntegrationJournal {
    pub fn new(
        run_id: String,
        recipe: &crate::native::integration::ValidatedIntegrationRecipe,
        selected_scenarios: Vec<ScenarioKind>,
    ) -> Result<Self> {
        Ok(Self {
            kind: "theseus_native_journal".into(),
            schema: 2,
            profile: crate::native::integration::PROFILE.into(),
            run_id,
            launch_fingerprint: None,
            recipe_digest: recipe.digest.clone(),
            core_recipe_digest: recipe.core.digest.clone(),
            core_manifest_digest: recipe.core.recipe.manifest_digest.clone(),
            integration_manifest_digest: recipe.recipe.integration_manifest_digest.clone(),
            manifest_digest: recipe.full_manifest_digest()?,
            requirements: recipe.recipe.integrations.clone(),
            compiled_requirements_digest: recipe.recipe.integrations.digest()?,
            selected_scenarios,
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
        })
    }
}

impl ReadinessExpectation {
    pub fn wire(&self) -> theseus_native_protocol::v2::ReadinessDefinitionExpectation {
        theseus_native_protocol::v2::ReadinessDefinitionExpectation {
            role: match self.role {
                DefinitionRole::Base => "base",
                DefinitionRole::Configured => "configured",
                DefinitionRole::Target => "target",
            }
            .into(),
            artifact: match self.artifact {
                ArtifactRole::Base => "base",
                ArtifactRole::Target => "target",
            }
            .into(),
            artifact_length: self.artifact_length,
            artifact_sha256: self.artifact_digest.clone(),
            version_literal: self.version.clone(),
            data_sha256: self.data_digest.clone(),
            configuration_digest: self.configuration_digest.clone(),
        }
    }
    pub fn digest(&self) -> Result<String> {
        ensure!(
            self.initialized,
            "readiness expectation requires initialized application"
        );
        Ok(self.wire().digest()?.to_hex())
    }
}
impl ReadinessChallenge {
    pub fn digest(&self) -> Result<String> {
        Ok(theseus_native_protocol::v2::ReadinessChallengeFields {
            lease: self.lease.clone(),
            slot: self.slot.as_str().into(),
            resource_binding_digest: self.resource_binding_digest.clone(),
            operation: self.operation.clone(),
            action_digest: self.action_digest.clone(),
            nonce: self.nonce.clone(),
            expectation_digest: self.expectation_digest.clone(),
        }
        .digest()?
        .to_hex())
    }
}

impl IntegrationActionRecord {
    /// Cross-check duplicated diagnostic IDs against the exact canonical request
    /// before those IDs can select an original owner for Query/Cancel.
    pub fn validate_identity(&self) -> Result<()> {
        let (lease, operation, slot, prior_binding_digest) = match &self.request {
            IntegrationRequest::Service {
                lease,
                operation,
                slot,
                prior_binding_digest,
                ..
            }
            | IntegrationRequest::Desktop {
                lease,
                operation,
                slot,
                prior_binding_digest,
                ..
            } => (lease, operation, slot, prior_binding_digest),
        };
        ensure!(
            lease == &self.lease
                && operation == &self.operation
                && slot == &self.slot
                && prior_binding_digest == &self.prior_binding_digest,
            "integration action differs from canonical original identity"
        );
        ensure!(
            theseus_native_protocol::v2::request_digest(&self.request.wire()?)?.to_hex()
                == self.request_digest,
            "integration canonical request digest differs"
        );
        if let IntegrationRequest::Desktop { request, .. } = &self.request {
            let identity = request.identity()?;
            let expected: BrokerOperationIdentity =
                serde_json::from_value(serde_json::to_value(identity)?)?;
            ensure!(
                self.broker_identity.as_ref() == Some(&expected),
                "desktop original broker identity changed"
            );
        } else {
            ensure!(
                self.broker_identity.is_none(),
                "service action borrowed desktop identity"
            );
        }
        Ok(())
    }
}

impl IntegrationRequest {
    pub fn wire(&self) -> Result<theseus_native_protocol::v2::Request> {
        use theseus_native_protocol::v2;
        match self {
            Self::Service {
                lease,
                operation,
                slot,
                prior_binding_digest,
                control,
            } => Ok(v2::Request::ServiceAction {
                lease: v2::LeaseId::parse(lease)?,
                operation: v2::OperationId::parse(operation)?,
                slot: v2::ResourceSlot::new(slot.as_str())?,
                prior_binding_digest: v2::Digest::parse(prior_binding_digest)?,
                action: match control {
                    ServiceAction::Start => v2::ServiceAction::Start,
                    ServiceAction::Stop => v2::ServiceAction::Stop,
                },
            }),
            Self::Desktop {
                lease,
                operation,
                slot,
                prior_binding_digest,
                mode,
                request,
            } => {
                request.validate()?;
                ensure!(
                    &request.helper_lease == lease
                        && &request.helper_operation == operation
                        && request.slot.as_str() == slot.as_str()
                        && &request.prior_binding_digest == prior_binding_digest,
                    "desktop canonical request identity changed"
                );
                if let v2::broker::DesktopAction::Activate {
                    mode: wire_mode, ..
                } = &request.action
                {
                    ensure!(
                        matches!(
                            (mode, wire_mode),
                            (
                                DesktopMode::OwnedExecutable,
                                v2::broker::DesktopMode::OwnedExecutable
                            ) | (
                                DesktopMode::NativeActivation,
                                v2::broker::DesktopMode::NativeActivation
                            )
                        ),
                        "desktop mode changed"
                    );
                }
                Ok(v2::Request::DesktopAction {
                    request: request.clone(),
                })
            }
        }
    }
}
