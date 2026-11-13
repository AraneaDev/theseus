use super::*;
use anyhow::{Result, ensure};

pub fn settled_original_broker_state(
    state: &theseus_native_protocol::v2::broker::BrokerState,
) -> bool {
    state.original_retired()
}
/// An effect terminal is knowledge about this original action, not its lifetime.
/// Check it before admitting any recovered state or issuing further control.
pub fn validate_original_broker_effect(
    action: &IntegrationActionRecord,
    state: &theseus_native_protocol::v2::broker::BrokerState,
) -> Result<()> {
    ensure!(
        matches!(action.request, IntegrationRequest::Desktop { .. }),
        "broker state borrowed service effect authority"
    );
    if let IntegrationRequest::Desktop { request, .. } = &action.request
        && matches!(
            request.action,
            theseus_native_protocol::v2::broker::DesktopAction::Activate { .. }
        )
    {
        ensure!(
            request.owned_lifetime_definition()
                == state.owned_lifetime.as_ref().map(|s| s.definition),
            "recovered owned lifetime changed the original Activate declaration"
        );
    }
    validate_broker_effect_terminal(action.terminal.as_ref(), state)
}
pub fn validate_broker_effect_terminal(
    terminal: Option<&IntegrationOutcome>,
    state: &theseus_native_protocol::v2::broker::BrokerState,
) -> Result<()> {
    use theseus_native_protocol::v2::broker::BrokerEffect;
    state.validate()?;
    match terminal {
        Some(IntegrationOutcome::Complete { receipt: known }) => ensure!(
            matches!(&state.effect,BrokerEffect::Receipt {receipt} if receipt.acknowledged && &receipt.receipt_digest==known),
            "recovered broker effect contradicts original acknowledged terminal"
        ),
        Some(IntegrationOutcome::Failed { .. }) => ensure!(
            matches!(&state.effect,BrokerEffect::Receipt {receipt} if !receipt.acknowledged),
            "recovered broker effect contradicts original failed terminal"
        ),
        Some(IntegrationOutcome::Unknown { .. }) => ensure!(
            matches!(state.effect, BrokerEffect::Unknown),
            "recovered broker effect contradicts original cached Unknown"
        ),
        None => {}
    }
    Ok(())
}
/// Original report observations precede recovery. They retain independently
/// bound irreversible facts even when the recovery journal has no history yet.
pub fn validate_original_broker_report_progress(
    report: &NativeIntegrationReport,
    action: &IntegrationActionRecord,
    next: &theseus_native_protocol::v2::broker::BrokerState,
) -> Result<()> {
    for observation in report
        .integrations
        .iter()
        .filter(|s| s.ownership_id == action.ownership_id)
        .flat_map(|s| &s.desktops)
        .filter(|d| d.slot == action.slot)
        .flat_map(|d| &d.observations)
        .filter(|o| {
            o.binding.ownership_id == action.ownership_id
                && o.binding.lease == action.lease
                && o.binding.action.as_ref() == Some(&action.operation)
                && o.binding.binding_digest == action.prior_binding_digest
        })
    {
        if let Some(previous) = &observation.broker_state {
            previous.validate()?;
            validate_broker_progress(previous, next)?;
        }
    }
    Ok(())
}
pub fn validate_broker_progress(
    previous: &theseus_native_protocol::v2::broker::BrokerState,
    next: &theseus_native_protocol::v2::broker::BrokerState,
) -> Result<()> {
    use theseus_native_protocol::v2::broker::*;
    next.validate()?;
    match (&previous.owned_lifetime, &next.owned_lifetime) {
        (Some(old), Some(new)) => new.validate_progress_from(old)?,
        (Some(_), None) => anyhow::bail!("original owned lifetime projection disappeared"),
        (None, Some(_)) => anyhow::bail!("owned lifetime invented after original initial state"),
        _ => {}
    }
    match (&previous.native_lifetime, &next.native_lifetime) {
        (Some(old), Some(new)) => new.validate_progress_from(old)?,
        (Some(_), None) => anyhow::bail!("original native lifetime projection disappeared"),
        (None, Some(_)) => ensure!(
            !previous.quarantined
                && previous.owner != BrokerOwner::Lost
                && !matches!(previous.effect, BrokerEffect::Receipt { .. }),
            "native authority invented after original terminal or lost owner"
        ),
        _ => {}
    }
    ensure!(
        !matches!(previous.effect, BrokerEffect::Receipt { .. }) || previous.effect == next.effect,
        "original broker effect receipt changed"
    );
    ensure!(
        !previous.quarantined || next.quarantined,
        "original broker quarantine cleared"
    );
    ensure!(
        !previous.cleanup_complete || next.cleanup_complete,
        "original broker cleanup fact regressed"
    );
    ensure!(
        !matches!(
            previous.primary,
            BrokerPrimary::Exited | BrokerPrimary::NotCreated
        ) || next.primary == previous.primary,
        "original primary exit/noncreation fact regressed"
    );
    ensure!(
        !matches!(
            previous.descendants,
            BrokerDescendants::Joined | BrokerDescendants::NotCreated
        ) || next.descendants == previous.descendants,
        "original scope join/noncreation fact regressed"
    );
    ensure!(
        !matches!(previous.owner, BrokerOwner::Lost | BrokerOwner::Settled)
            || next.owner != BrokerOwner::Retained,
        "lost or settled original native owner was reconstructed"
    );
    Ok(())
}

/// Capture currently known original terminals without adding action authority.
/// A cached original query can precede the private terminal-completion barrier.
pub fn original_broker_effect_baselines(
    journal: &NativeIntegrationJournal,
    owner: &str,
) -> Result<Vec<OriginalBrokerEffectBaseline>> {
    journal
        .integration_actions
        .iter()
        .filter(|a| {
            a.ownership_id == owner && matches!(a.request, IntegrationRequest::Desktop { .. })
        })
        .map(|action| {
            let original = ActionReference {
                operation: action.operation.clone(),
                request_digest: action.request_digest.clone(),
            };
            let mut terminal = action.terminal.clone();
            for query in journal
                .integration_recoveries
                .iter()
                .filter(|r| r.ownership_id == owner)
                .flat_map(|r| &r.queries)
                .filter(|q| q.original == original && q.view == OriginalQueryView::CachedTerminal)
            {
                if let Some(known) = &terminal {
                    ensure!(
                        known == &query.outcome,
                        "original desktop terminal differs from prior cached query"
                    );
                } else {
                    terminal = Some(query.outcome.clone());
                }
            }
            Ok(OriginalBrokerEffectBaseline { original, terminal })
        })
        .collect()
}
/// Cached query provenance cannot disagree with an authenticated original
/// receipt. Earlier unavailable effects remain historical Unknown samples.
pub fn validate_original_query_broker_receipts(
    journal: &NativeIntegrationJournal,
    owner: &str,
    query: &OriginalQueryOutcome,
) -> Result<()> {
    if query.view != OriginalQueryView::CachedTerminal {
        return Ok(());
    }
    for observation in journal
        .integration_recoveries
        .iter()
        .filter(|r| r.ownership_id == owner)
        .flat_map(|r| &r.broker_observations)
        .filter(|o| o.original == query.original)
    {
        if matches!(
            observation.state.effect,
            theseus_native_protocol::v2::broker::BrokerEffect::Receipt { .. }
        ) {
            validate_broker_effect_terminal(Some(&query.outcome), &observation.state)?;
        }
    }
    Ok(())
}
pub fn original_desktop_scope_settled(
    journal: &NativeIntegrationJournal,
    owner: &str,
    slot: &ResourceSlot,
) -> bool {
    let actions = journal
        .integration_actions
        .iter()
        .filter(|a| {
            a.ownership_id == owner
                && &a.slot == slot
                && matches!(a.request, IntegrationRequest::Desktop { .. })
        })
        .collect::<Vec<_>>();
    !actions.is_empty()
        && actions.iter().all(|action| {
            journal
                .integration_recoveries
                .iter()
                .rev()
                .filter(|r| r.ownership_id == owner)
                .flat_map(|r| r.broker_observations.iter().rev())
                .find(|o| {
                    o.original.operation == action.operation
                        && o.original.request_digest == action.request_digest
                })
                .is_some_and(|o| settled_original_broker_state(&o.state))
        })
}
