//! Helper-side original bind and action ledger protocol.
use super::*;
/// Helpers call this only after controller publication of this exact BrokerBind.
/// A restarted or repeated bind verifies the original file read-only and never
/// calls prepare/Acquire again, including after a missing/uncertain publication.
pub fn begin_bind(
    store: &mut LedgerStore,
    intents: &mut IntentStore,
    original: BrokerBind,
) -> io::Result<BindDispatch> {
    original.validate().map_err(io::Error::other)?;
    let view = projection(store)?;
    if let Some(stored) = view.binding_original() {
        if stored != &original {
            return Err(invalid("changed original broker binding"));
        }
        if let Some(receipt) = view.binding(&original).map_err(io::Error::other)? {
            return Ok(BindDispatch::Cached(receipt.clone()));
        }
        let verified = intents
            .verify_original(&intent(&original)?)
            .map_err(io::Error::other)?;
        return Ok(BindDispatch::QueryOriginal { original, verified });
    }
    append(
        store,
        BrokerEvent::BindAccepted {
            request_digest: original.digest().map_err(io::Error::other)?.to_hex(),
            original: original.clone(),
        },
    )?;
    let prepared = intents
        .prepare(intent(&original)?)
        .map_err(io::Error::other)?;
    append(
        store,
        BrokerEvent::BindIntentPublished {
            operation: original.helper_operation.clone(),
            scope: publication_scope(prepared.commit_scope()),
        },
    )?;
    Ok(BindDispatch::AcquireOnce(PreparedBrokerAcquire {
        original,
        prepared,
        sent: false,
    }))
}
pub fn query_original_acquire(verified: &VerifiedOriginalIntent) -> v2::Request {
    let r = verified.record();
    v2::Request::QueryAcquire {
        intent: r.intent,
        binding: r.binding,
        manifest_digest: r.manifest_digest,
    }
}
fn publication_scope(scope: theseus_host::fs::CommitScope) -> IntentPublicationScope {
    match scope {
        theseus_host::fs::CommitScope::FileAndDirectorySynced => {
            IntentPublicationScope::FileAndDirectorySynced
        }
        theseus_host::fs::CommitScope::FileFlushedAtomicReplace => {
            IntentPublicationScope::FileFlushedAtomicReplace
        }
    }
}
/// Commit the exact acquired original lease in the helper ledger BEFORE the
/// caller publishes it to the controller and permits any desktop action.
pub fn finish_bind(
    store: &mut LedgerStore,
    acquire: PreparedBrokerAcquire,
    lease: v2::LeaseId,
) -> io::Result<BrokerBindingReceipt> {
    if !acquire.sent {
        return Err(invalid("broker Acquire was not dispatched"));
    }
    let receipt = BrokerBindingReceipt {
        request_digest: acquire
            .original
            .digest()
            .map_err(io::Error::other)?
            .to_hex(),
        original: acquire.original,
        publication_scope: publication_scope(acquire.prepared.commit_scope()),
        broker_lease: lease.to_hex(),
    };
    if let Some(existing) = projection(store)?
        .binding(&receipt.original)
        .map_err(io::Error::other)?
    {
        if existing == &receipt {
            return Ok(existing.clone());
        }
        return Err(invalid("changed original acquired broker lease"));
    }
    append(
        store,
        BrokerEvent::BindingCommitted {
            receipt: receipt.clone(),
        },
    )?;
    Ok(receipt)
}
/// QueryAcquire recovery needs the ORIGINAL observed publication scope, supplied
/// from the controller/helper journal; verifying file bytes grants no barrier.
pub fn finish_original_bind(
    store: &mut LedgerStore,
    original: &BrokerBind,
    verified: &VerifiedOriginalIntent,
    known_publication_scope: IntentPublicationScope,
    lease: v2::LeaseId,
) -> io::Result<BrokerBindingReceipt> {
    if verified.record() != &intent(original)? {
        return Err(invalid("wrong original broker intent"));
    }
    if projection(store)?.binding_scope(original) != Some(known_publication_scope) {
        return Err(invalid("original intent publication scope unavailable"));
    }
    let receipt = BrokerBindingReceipt {
        original: original.clone(),
        request_digest: original.digest().map_err(io::Error::other)?.to_hex(),
        publication_scope: known_publication_scope,
        broker_lease: lease.to_hex(),
    };
    if let Some(existing) = projection(store)?
        .binding(original)
        .map_err(io::Error::other)?
    {
        if existing == &receipt {
            return Ok(existing.clone());
        }
        return Err(invalid("changed original recovered broker lease"));
    }
    append(
        store,
        BrokerEvent::BindingCommitted {
            receipt: receipt.clone(),
        },
    )?;
    Ok(receipt)
}
pub fn verify_action_mapping(
    store: &LedgerStore,
    original: &BrokerBind,
    request: &DesktopRequest,
) -> io::Result<BrokerOperationIdentity> {
    let view = projection(store)?;
    let receipt = view
        .binding(original)
        .map_err(io::Error::other)?
        .ok_or_else(|| invalid("broker lease mapping not committed"))?;
    if request.helper_lease != original.helper_lease
        || request.slot != original.slot
        || request.broker_lease != receipt.broker_lease
        || request.binding_digest
            != original
                .expected_header
                .digest()
                .map_err(io::Error::other)?
                .to_hex()
    {
        return Err(invalid("desktop action changed original broker mapping"));
    }
    request.identity().map_err(io::Error::other)
}

/// Commit this exact original helper action before any broker transmission.
/// A duplicate is query-only; it never authorizes a second native dispatch.
pub fn prepare_helper_action(
    store: &mut LedgerStore,
    original: &BrokerBind,
    request: &DesktopRequest,
) -> io::Result<bool> {
    let identity = verify_action_mapping(store, original, request)?;
    let view = projection(store)?;
    if view
        .helper_request(&identity)
        .map_err(io::Error::other)?
        .is_some()
    {
        return Ok(false);
    }
    append(
        store,
        BrokerEvent::HelperActionAccepted {
            request: request.clone(),
            request_digest: request.digest().map_err(io::Error::other)?.to_hex(),
        },
    )?;
    Ok(true)
}
pub fn record_helper_state(
    store: &mut LedgerStore,
    identity: &BrokerOperationIdentity,
    state: &BrokerState,
) -> io::Result<()> {
    let lease = v2::LeaseId::parse(&identity.helper_lease).map_err(io::Error::other)?;
    if !matches!(
        store.restart_view().query_lease(lease),
        Ok(v2::LeaseView::Held { .. })
    ) {
        // A closed original helper ledger remains immutable. Exact original
        // broker query/Cancel observations are journaled by the controller;
        // they cannot reopen this helper lease or create action authority.
        projection(store)?
            .helper_request(identity)
            .map_err(io::Error::other)?
            .ok_or_else(|| invalid("original helper action missing"))?;
        state.validate().map_err(io::Error::other)?;
        return Ok(());
    }
    if projection(store)?
        .helper_state(identity)
        .map_err(io::Error::other)?
        == Some(state)
    {
        return Ok(());
    }
    append(
        store,
        BrokerEvent::HelperStateObserved {
            identity: identity.clone(),
            state: state.clone(),
        },
    )
}
pub fn record_helper_release(
    store: &mut LedgerStore,
    original: &BrokerBind,
    broker_lease: &str,
) -> io::Result<()> {
    if projection(store)?.helper_released() {
        return Ok(());
    }
    append(
        store,
        BrokerEvent::HelperBrokerReleased {
            helper_lease: original.helper_lease.clone(),
            broker_lease: broker_lease.into(),
        },
    )
}
