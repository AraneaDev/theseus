use super::*;
use crate::native::model::*;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopPreviousActivationEvidence {
    pub operation: OperationId,
    /// Canonical broker request digest; exact full identity stays private.
    pub broker_request_digest: Digest,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopGenerationEvidence {
    pub definition: theseus_native_protocol::v2::DesktopDefinitionKey,
    pub tuple_digest: Digest,
    pub sut: theseus_native_protocol::v2::broker::DesktopSutRunWitness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_activation: Option<DesktopPreviousActivationEvidence>,
}
impl DesktopGenerationEvidence {
    pub fn validate(&self) -> Result<()> {
        self.sut.validate()?;
        digest(&self.tuple_digest)?;
        if let Some(previous) = &self.previous_activation {
            hex(&previous.operation, 32)?;
            digest(&previous.broker_request_digest)?;
            ensure!(
                previous.operation != self.sut.operation,
                "generation reused original activation as SUT Run"
            );
        }
        Ok(())
    }
}
/// Public evidence carries correlations, never an original private activation nonce.
pub fn desktop_generation_evidence(
    generation: &theseus_native_protocol::v2::broker::DesktopActivationTransition,
) -> DesktopGenerationEvidence {
    DesktopGenerationEvidence {
        definition: generation.definition,
        tuple_digest: generation.tuple_digest.clone(),
        sut: generation.sut.clone(),
        previous_activation: generation.previous_activation.as_ref().map(|original| {
            DesktopPreviousActivationEvidence {
                operation: original.helper_operation.clone(),
                broker_request_digest: original.request_digest.clone(),
            }
        }),
    }
}
pub(crate) fn validate_desktop_observation_generation(
    report: &NativeIntegrationReport,
    journal: &NativeIntegrationJournal,
    observation: &BoundDesktopObservation,
) -> Result<()> {
    let b = &observation.binding;
    // A legacy-looking public binding cannot erase a keyed private Activate.
    if observation.broker_state.is_some() || observation.readiness.is_some() {
        let original = journal
            .integration_actions
            .iter()
            .find(|a| {
                a.ownership_id == b.ownership_id
                    && a.lease == b.lease
                    && a.slot == b.slot
                    && b.action.as_ref() == Some(&a.operation)
            })
            .ok_or_else(|| anyhow::anyhow!("desktop generation activation missing"))?;
        ensure!(
            matches!(&original.request,IntegrationRequest::Desktop {request,..} if matches!(request.action,theseus_native_protocol::v2::broker::DesktopAction::Activate {..}) && request.transition().map(desktop_generation_evidence)==observation.generation),
            "desktop generation differs from exact accepted original Activate"
        );
    }
    let Some(generation) = &observation.generation else {
        return Ok(());
    };
    let operation = journal
        .operations
        .iter()
        .find(|op| {
            op.ownership_id == b.ownership_id
                && op.lease == b.lease
                && op.operation_id == generation.sut.operation
        })
        .ok_or_else(|| anyhow::anyhow!("desktop generation original SUT operation missing"))?;
    let measured = report
        .catalog
        .scenarios
        .iter()
        .filter(|s| s.ownership_id == b.ownership_id && s.id == b.scenario_id)
        .flat_map(|s| &s.stages)
        .flat_map(|s| &s.commands)
        .find(|c| {
            c.operation_id == generation.sut.operation
                && c.request_digest == generation.sut.request_digest
        })
        .ok_or_else(|| {
            anyhow::anyhow!("desktop generation measured original SUT command missing")
        })?;
    validate_desktop_sut_generation(
        &b.ownership_id,
        &b.lease,
        &generation.sut,
        operation,
        measured,
    )?;
    ensure!(
        b.definition == Some(DefinitionKey::from_desktop_wire(generation.definition)),
        "desktop generation definition binding changed"
    );
    if observation.broker_state.is_none() && observation.readiness.is_none() {
        ensure!(
            b.action.as_ref() == Some(&generation.sut.operation),
            "desktop entry inspection borrowed another SUT generation"
        );
    }
    if let Some(previous) = &generation.previous_activation {
        let original=journal.integration_actions.iter().find(|a|a.ownership_id==b.ownership_id && a.lease==b.lease && a.slot==b.slot && matches!(&a.request,IntegrationRequest::Desktop {request,..} if request.identity().is_ok_and(|identity|identity.helper_operation==previous.operation && identity.request_digest==previous.broker_request_digest && identity.helper_lease==b.lease))).ok_or_else(||anyhow::anyhow!("desktop generation prior original activation missing"))?;
        let prior = report
            .integrations
            .iter()
            .filter(|s| s.ownership_id == b.ownership_id)
            .flat_map(|s| &s.desktops)
            .filter(|d| d.slot == b.slot)
            .flat_map(|d| &d.observations)
            .filter(|o| o.binding.action.as_ref() == Some(&original.operation))
            .filter_map(|o| o.broker_state.as_ref())
            .next_back();
        ensure!(
            prior.is_some_and(settled_original_broker_state),
            "desktop generation replaced an unsettled/lost original activation"
        );
    }
    Ok(())
}
fn observed_bool(value: &IntegrationProvenance<bool>) -> Option<bool> {
    match value {
        IntegrationProvenance::Observed { value, .. } => Some(*value),
        _ => None,
    }
}
impl LifetimeRequirement {
    pub fn native(self) -> theseus_native_protocol::v2::broker::NativeLifetimeRequirement {
        use theseus_native_protocol::v2::broker::NativeLifetimeRequirement as Native;
        match self {
            Self::ActivationInstanceTerminated => Native::ActivationInstanceTerminated,
            Self::OwnedPrimaryExited => Native::OwnedPrimaryExited,
            Self::OwnedProcessTreeSettled => Native::OwnedProcessTreeSettled,
        }
    }
}
pub fn desktop_predicate_satisfied(
    observation: &BoundDesktopObservation,
    predicate: &IntegrationPredicate,
) -> bool {
    use theseus_native_protocol::v2::{ReadinessState, broker::*};
    let state = observation
        .broker_state
        .as_ref()
        .filter(|s| s.validate().is_ok());
    let owned = state.is_some_and(|s| matches!(&s.effect, BrokerEffect::Receipt { receipt } if receipt.acknowledged && matches!(receipt.evidence,DesktopEvidence::OwnedExecutable|DesktopEvidence::SourceOwnedExecutable)) && s.native_lifetime.is_none());
    let native = state.filter(|s| matches!(&s.effect,BrokerEffect::Receipt {receipt} if receipt.acknowledged && receipt.evidence==DesktopEvidence::NativeActivation)).and_then(|s|s.native_lifetime.as_ref());
    match predicate {
        IntegrationPredicate::DesktopEntryPresent => {
            observed_bool(&observation.entry_present) == Some(true)
                && observation.entry_valid == Some(true)
        }
        IntegrationPredicate::DesktopEntryAbsent => {
            observed_bool(&observation.entry_present) == Some(false)
        }
        IntegrationPredicate::ApplicationReady => {
            observation.readiness.as_ref().is_some_and(|r| {
                r.state == ReadinessState::Ready
                    && r.authenticated_instance
                    && r.initialized == Some(true)
                    && r.response_digest.is_some()
                    && r.resource_binding_digest == observation.binding.binding_digest
            }) && (owned || native.is_some())
        }
        IntegrationPredicate::Lifetime(required) => {
            if let Some(native) = native {
                native.required == required.native() && native.required_lifetime_satisfied()
            } else if owned {
                state.is_some_and(|s| match required {
                    LifetimeRequirement::OwnedPrimaryExited => s.primary == BrokerPrimary::Exited,
                    LifetimeRequirement::OwnedProcessTreeSettled => {
                        s.primary == BrokerPrimary::Exited
                            && s.descendants == BrokerDescendants::Joined
                            && s.cleanup_complete
                    }
                    LifetimeRequirement::ActivationInstanceTerminated => false,
                })
            } else {
                false
            }
        }
        _ => false,
    }
}
