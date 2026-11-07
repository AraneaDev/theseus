//! Saved original-owned lifetime knowledge; these projections grant no authority.
use super::model::*;
use anyhow::{Result, ensure};
use theseus_native_protocol::v2::broker::{self, DesktopMode as WireMode};

pub(crate) fn scenario_complete(scenario: &IntegrationScenarioEvidence) -> bool {
    let Some(owned) = &scenario.owned_lifetime else {
        return false;
    };
    if scenario.native_lifetime.is_some()
        || owned.state.validate().is_err()
        || !owned.state.required_lifetime_satisfied()
        || !owned.state.local_retired()
        || owned.state.descendants != broker::OwnedTreeState::Unavailable
    {
        return false;
    }
    let d = &scenario.dispositions;
    if ![
        &d.helper_worker,
        &d.registration,
        &d.manager_action,
        &d.broker,
        &d.primary,
    ]
    .into_iter()
    .all(|d| matches!(d, crate::native::model::Disposition::Complete { .. }))
        || !matches!(
            d.descendant_scope,
            crate::native::model::Disposition::NotStarted
        )
    {
        return false;
    }
    let mut exact_original = false;
    for desktop in &scenario.desktops {
        let mut latest = std::collections::BTreeMap::new();
        let mut owned_actions = std::collections::BTreeSet::new();
        for observation in &desktop.observations {
            if let (Some(action), Some(state)) =
                (&observation.binding.action, &observation.broker_state)
            {
                latest.insert(action, state);
                if state.owned_lifetime.is_some() {
                    owned_actions.insert(action);
                }
            }
        }
        if owned_actions.iter().any(|action| {
            latest
                .get(*action)
                .is_none_or(|state| state.owned_lifetime.is_none() || !state.original_retired())
        }) {
            return false;
        }
        if desktop.actions.contains(&owned.original)
            && latest.get(&owned.original.operation).is_some_and(|state| {
                state.owned_lifetime.as_ref() == Some(&owned.state) && state.original_retired()
            })
        {
            exact_original = true;
        }
    }
    exact_original
}

pub(crate) fn validate_requirement(
    required: Option<&DesktopRequirement>,
) -> Result<&DesktopRequirement> {
    let required =
        required.ok_or_else(|| anyhow::anyhow!("owned lifetime without desktop requirement"))?;
    ensure!(
        required.modes == [DesktopMode::OwnedExecutable]
            && required.lifetime == LifetimeRequirement::OwnedPrimaryExited,
        "owned primary lifetime exceeds selected mode/requirement"
    );
    Ok(required)
}

pub(crate) fn validate_report_scenario(
    requirements: &IntegrationRequirements,
    scenario: &IntegrationScenarioEvidence,
) -> Result<()> {
    ensure!(
        scenario.native_lifetime.is_none() || scenario.owned_lifetime.is_none(),
        "mixed native and owned lifetime projections"
    );
    for state in scenario
        .desktops
        .iter()
        .flat_map(|d| &d.observations)
        .filter_map(|o| o.broker_state.as_ref())
        .filter(|s| s.owned_lifetime.is_some())
    {
        validate_requirement(requirements.desktop.as_ref())?;
        state.validate()?;
    }
    if let Some(owned) = &scenario.owned_lifetime {
        owned.state.validate()?;
        let required = validate_requirement(requirements.desktop.as_ref())?;
        ensure!(
            scenario
                .desktops
                .iter()
                .filter(|d| d.slot == required.resource)
                .any(|d| d.actions.contains(&owned.original)
                    && d.observations.iter().any(|o| o.binding.action.as_ref()
                        == Some(&owned.original.operation)
                        && o.broker_state
                            .as_ref()
                            .is_some_and(|s| s.owned_lifetime.as_ref() == Some(&owned.state)))),
            "owned lifetime projection lacks exact original observed action"
        );
    }
    Ok(())
}

/// Public references must resolve to the canonical PRIVATE original request,
/// not merely agree with another public copy of the same reference.
pub(crate) fn validate_pair_projection(
    scenario: &IntegrationScenarioEvidence,
    journal: &NativeIntegrationJournal,
) -> Result<()> {
    let Some(owned) = &scenario.owned_lifetime else {
        return Ok(());
    };
    let required = validate_requirement(journal.requirements.desktop.as_ref())?;
    let action = journal
        .integration_actions
        .iter()
        .find(|action| {
            action.ownership_id == scenario.ownership_id
                && action.slot == required.resource
                && action.operation == owned.original.operation
                && action.request_digest == owned.original.request_digest
        })
        .ok_or_else(|| {
            anyhow::anyhow!("owned projection lacks exact private original Activate digest")
        })?;
    ensure!(
        theseus_native_protocol::v2::request_digest(&action.request.wire()?)?.to_hex()
            == owned.original.request_digest,
        "owned projection changed canonical private original request digest"
    );
    let IntegrationRequest::Desktop { request, .. } = &action.request else {
        anyhow::bail!("owned projection borrowed private service action")
    };
    ensure!(
        matches!(
            request.action,
            broker::DesktopAction::Activate {
                mode: WireMode::OwnedExecutable,
                ..
            }
        ) && request.owned_lifetime_definition() == Some(owned.state.definition),
        "owned projection differs from private original Activate declaration"
    );
    ensure!(
        scenario
            .desktops
            .iter()
            .filter(|desktop| desktop.slot == action.slot)
            .any(
                |desktop| desktop.observations.iter().any(|observation| observation
                    .binding
                    .ownership_id
                    == action.ownership_id
                    && observation.binding.lease == action.lease
                    && observation.binding.slot == action.slot
                    && observation.binding.action.as_ref() == Some(&action.operation)
                    && observation
                        .broker_state
                        .as_ref()
                        .is_some_and(|state| state.owned_lifetime.as_ref() == Some(&owned.state)))
            ),
        "owned projection lacks its exact private original observation binding"
    );
    Ok(())
}

/// A report can retain Unknown-effect samples from BEFORE the eventual terminal.
/// Once a sample has a receipt, it must agree with any known original terminal;
/// normal per-action progress validation separately prevents receipt regression.
pub(crate) fn validate_report_effect(
    action: &IntegrationActionRecord,
    state: &broker::BrokerState,
) -> Result<()> {
    if matches!(state.effect, broker::BrokerEffect::Receipt { .. }) {
        validate_original_broker_effect(action, state)
    } else {
        validate_broker_effect_terminal(None, state)
    }
}

pub(crate) fn validate_original_observation(
    request: &broker::DesktopRequest,
    state: &broker::BrokerState,
    required: Option<&DesktopRequirement>,
    os: &str,
) -> Result<()> {
    ensure!(
        request.owned_lifetime_definition() == state.owned_lifetime.as_ref().map(|s| s.definition),
        "owned lifetime differs from original Activate declaration"
    );
    if let Some(owned) = &state.owned_lifetime {
        validate_requirement(required)?;
        owned.validate_platform(os)?;
        ensure!(
            matches!(
                request.action,
                broker::DesktopAction::Activate {
                    mode: WireMode::OwnedExecutable,
                    ..
                }
            ),
            "owned lifetime borrowed another original mode"
        );
    }
    Ok(())
}

pub(crate) fn validate_recovered_observation(
    journal: &NativeIntegrationJournal,
    action: &IntegrationActionRecord,
    state: &broker::BrokerState,
) -> Result<()> {
    let IntegrationRequest::Desktop { request, .. } = &action.request else {
        return Ok(());
    };
    let original = match &request.action {
        broker::DesktopAction::Activate { .. } => Some(request.as_ref()),
        broker::DesktopAction::RequestClose {
            original_activation_operation,
            ..
        } => journal
            .integration_actions
            .iter()
            .filter(|a| {
                a.ownership_id == action.ownership_id
                    && a.lease == action.lease
                    && a.slot == action.slot
                    && a.operation == *original_activation_operation
            })
            .find_map(|a| match &a.request {
                IntegrationRequest::Desktop { request, .. }
                    if matches!(request.action, broker::DesktopAction::Activate { .. }) =>
                {
                    Some(request.as_ref())
                }
                _ => None,
            }),
    };
    let declaration = original.and_then(|r| r.owned_lifetime_definition());
    if declaration.is_none() && state.owned_lifetime.is_none() {
        return Ok(());
    }
    let original = original
        .ok_or_else(|| anyhow::anyhow!("owned recovery lacks original activation declaration"))?;
    let binding = journal
        .broker_bindings
        .iter()
        .find(|b| {
            b.ownership_id == action.ownership_id
                && b.helper_lease == action.lease
                && b.broker_lease.as_ref() == Some(&original.broker_lease)
        })
        .ok_or_else(|| anyhow::anyhow!("owned recovery original broker mapping missing"))?;
    validate_original_observation(
        original,
        state,
        journal.requirements.desktop.as_ref(),
        &binding.original.expected_header.os,
    )
}
