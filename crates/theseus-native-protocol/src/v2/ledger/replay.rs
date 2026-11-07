use super::*;
use crate::v2::*;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplayDecision {
    Recorded(OperationView),
    NotRecorded,
}
#[derive(Clone, Debug)]
struct Operation {
    operation: OperationId,
    kind: OperationKind,
    digest: Digest,
    view: OperationView,
}
/// Verified immutable restart state. Absence never grants effect authority.
#[derive(Clone, Debug)]
pub struct LedgerView {
    binding: BindingId,
    manifest: Digest,
    lease: Option<(IntentId, LeaseId, LeaseView)>,
    operations: Vec<Operation>,
    authority_failure: Option<PrefixAuthorityFailure>,
}
fn capacity(l: usize, n: usize, unfinished: bool, closed: bool) -> Result<usize, LedgerError> {
    let next = l.checked_add(n).ok_or(LedgerError::Capacity)?;
    next.checked_add(if unfinished { FINISH_RESERVE } else { 0 })
        .and_then(|v| v.checked_add(if closed { 0 } else { CLOSE_RESERVE }))
        .filter(|v| *v <= MAX_LEDGER_BYTES)
        .ok_or(LedgerError::Capacity)?;
    Ok(next)
}
impl LedgerView {
    pub fn replay(records: &[Record]) -> Result<Self, LedgerError> {
        if records.is_empty() {
            return Err(LedgerError::InvalidTransition);
        }
        if records.len() > MAX_RECORDS {
            return Err(LedgerError::Capacity);
        }
        let (binding, manifest_digest, os) = match records[0].entry() {
            Entry::IntegrationBrokerHeader { header } => {
                header.validate().map_err(|_| LedgerError::InvalidRecord)?;
                (
                    BindingId::parse(&header.binding).map_err(|_| LedgerError::InvalidRecord)?,
                    Digest::parse(&header.manifest_digest)
                        .map_err(|_| LedgerError::InvalidRecord)?,
                    match header.os.as_str() {
                        "linux" => Os::Linux,
                        "macos" => Os::Macos,
                        "windows" => Os::Windows,
                        _ => return Err(LedgerError::InvalidRecord),
                    },
                )
            }
            Entry::Header {
                binding,
                manifest_digest,
                os,
                ..
            }
            | Entry::IntegrationHeader {
                binding,
                manifest_digest,
                os,
                ..
            }
            | Entry::NativeHeader {
                binding,
                manifest_digest,
                os,
                ..
            } => (*binding, *manifest_digest, *os),
            _ => return Err(LedgerError::InvalidTransition),
        };
        let mut v = Self {
            binding,
            manifest: manifest_digest,
            lease: None,
            operations: Vec::new(),
            authority_failure: None,
        };
        let mut active: Option<usize> = None;
        let mut closed = false;
        let mut length = 0;
        for (i, r) in records.iter().enumerate() {
            if usize::from(r.index()) != i || closed {
                return Err(LedgerError::InvalidTransition);
            }
            let n = super::encode(r)?.len();
            match r.entry() {
                Entry::Header { .. }
                | Entry::NativeHeader { .. }
                | Entry::IntegrationBrokerHeader { .. }
                | Entry::IntegrationHeader { .. } => {
                    if i != 0 {
                        return Err(LedgerError::InvalidTransition);
                    }
                }
                Entry::LinuxGioQualificationDeclared { .. } => {
                    crate::v2::declared_linux_gio_qualification(records)?
                        .ok_or(LedgerError::InvalidTransition)?;
                }
                Entry::DesktopAuthorityDeclared { .. } => {
                    if i != 1 {
                        return Err(LedgerError::InvalidTransition);
                    }
                    crate::v2::broker::declared_desktop(records)?
                        .ok_or(LedgerError::InvalidTransition)?;
                }
                Entry::Broker { .. } => {
                    if !matches!(
                        records[0].entry(),
                        Entry::IntegrationHeader { .. } | Entry::IntegrationBrokerHeader { .. }
                    ) {
                        return Err(LedgerError::InvalidTransition);
                    }
                }
                Entry::NativeAuthorityRevoked { lease, reason } => {
                    if !matches!(
                        records[0].entry(),
                        Entry::NativeHeader { .. } | Entry::IntegrationHeader { .. }
                    ) || active.is_some()
                        || v.authority_failure.is_some()
                    {
                        return Err(LedgerError::InvalidTransition);
                    }
                    v.check_lease(*lease)
                        .map_err(|_| LedgerError::InvalidTransition)?;
                    v.authority_failure = Some(*reason);
                }
                Entry::LeaseReserved { intent, lease } => {
                    let (expected, qualified) = match records[0].entry() {
                        Entry::IntegrationBrokerHeader { header } => (
                            if header.linux_gio_qualification.is_some() {
                                2
                            } else {
                                1
                            },
                            header.linux_gio_qualification.is_some(),
                        ),
                        Entry::IntegrationHeader { .. } => {
                            match records.get(1).map(Record::entry) {
                                Some(Entry::DesktopAuthorityDeclared { context, .. }) => (
                                    if context.linux_gio_qualification.is_some() {
                                        3
                                    } else {
                                        2
                                    },
                                    context.linux_gio_qualification.is_some(),
                                ),
                                _ => (1, false),
                            }
                        }
                        _ => (1, false),
                    };
                    if i != expected
                        || v.lease.is_some()
                        || (qualified
                            && crate::v2::declared_linux_gio_qualification(records)?.is_none())
                    {
                        return Err(LedgerError::InvalidTransition);
                    }
                    v.lease = Some((*intent, *lease, LeaseView::Held { intent: *intent }));
                }
                Entry::OperationAccepted {
                    lease,
                    operation,
                    request_digest,
                    operation_kind,
                } => {
                    v.check_lease(*lease)
                        .map_err(|_| LedgerError::InvalidTransition)?;
                    if matches!(records[0].entry(), Entry::IntegrationBrokerHeader { .. })
                        || (*operation_kind == OperationKind::ServiceAction
                            && !matches!(records[0].entry(), Entry::IntegrationHeader { .. }))
                        || v.authority_failure.is_some()
                        || active.is_some()
                        || v.operations.iter().any(|o| o.operation == *operation)
                    {
                        return Err(LedgerError::InvalidTransition);
                    }
                    if v.operations.len() >= MAX_OPERATIONS {
                        return Err(LedgerError::Capacity);
                    }
                    active = Some(v.operations.len());
                    v.operations.push(Operation {
                        operation: *operation,
                        kind: *operation_kind,
                        digest: *request_digest,
                        view: OperationView::Accepted {
                            operation_kind: *operation_kind,
                        },
                    });
                }
                Entry::OperationFinished {
                    lease,
                    operation,
                    outcome,
                } => {
                    v.check_lease(*lease)
                        .map_err(|_| LedgerError::InvalidTransition)?;
                    let a = active.ok_or(LedgerError::InvalidTransition)?;
                    let op = &mut v.operations[a];
                    if op.operation != *operation
                        || !matches!(
                            (op.kind, outcome),
                            (
                                OperationKind::CreateFile,
                                Outcome::FileCreated { .. } | Outcome::FileCreateFailed { .. }
                            ) | (
                                OperationKind::Run,
                                Outcome::RunCompleted { .. } | Outcome::LaunchFailed { .. }
                            ) | (
                                OperationKind::ServiceAction,
                                Outcome::ServiceActionCompleted { .. }
                            ) | (_, Outcome::NotStarted { .. })
                        )
                    {
                        return Err(LedgerError::InvalidTransition);
                    }
                    super::super::types::validate_outcome_for_os(outcome, os)
                        .map_err(|_| LedgerError::InvalidTransition)?;
                    if n > FINISH_RESERVE {
                        return Err(LedgerError::Capacity);
                    }
                    op.view = OperationView::Finished {
                        outcome: outcome.clone(),
                    };
                    active = None;
                }
                Entry::LeaseClosed { lease, disposition } => {
                    v.check_lease(*lease)
                        .map_err(|_| LedgerError::InvalidTransition)?;
                    if matches!(records[0].entry(), Entry::IntegrationBrokerHeader { .. })
                        && matches!(disposition, Disposition::AwaitingExternalReset)
                    {
                        let broker = crate::v2::broker::BrokerLedgerView::replay(&records[..i])?;
                        if broker.requests().any(|request| {
                            broker
                                .query(request)
                                .map_or(true, |state| !state.original_retired())
                        }) {
                            return Err(LedgerError::InvalidTransition);
                        }
                    }
                    if matches!(records[0].entry(), Entry::IntegrationHeader { .. })
                        && matches!(disposition, Disposition::AwaitingExternalReset)
                    {
                        let broker = crate::v2::broker::BrokerLedgerView::replay(&records[..i])?;
                        if broker.binding_original().is_some() && !broker.helper_released() {
                            return Err(LedgerError::InvalidTransition);
                        }
                    }
                    if n > CLOSE_RESERVE {
                        return Err(LedgerError::Capacity);
                    }
                    let (intent, _, state) =
                        v.lease.as_mut().ok_or(LedgerError::InvalidTransition)?;
                    *state = match disposition {
                        Disposition::AwaitingExternalReset => {
                            if active.is_some() {
                                return Err(LedgerError::InvalidTransition);
                            }
                            LeaseView::AwaitingExternalReset { intent: *intent }
                        }
                        Disposition::Quarantined { reason } => {
                            if let Some(a) = active {
                                v.operations[a].view = OperationView::Unknown { reason: *reason }
                            }
                            LeaseView::Quarantined {
                                intent: *intent,
                                reason: *reason,
                            }
                        }
                    };
                    active = None;
                    closed = true;
                }
            }
            length = capacity(length, n, active.is_some(), closed)?;
        }
        if let Some(a) = active {
            let reason = UnknownReason::HelperRestartedWithUnfinishedOperation;
            v.operations[a].view = OperationView::Unknown { reason };
            if let Some((intent, _, state)) = &mut v.lease {
                *state = LeaseView::Quarantined {
                    intent: *intent,
                    reason,
                }
            }
        }
        let broker = crate::v2::broker::BrokerLedgerView::replay(records)?;
        if matches!(records[0].entry(), Entry::IntegrationBrokerHeader { .. })
            && broker
                .requests()
                .any(|r| broker.query(r).is_ok_and(|s| !s.original_retired()))
            && let Some((intent, _, state)) = &mut v.lease
        {
            *state = LeaseView::Quarantined {
                intent: *intent,
                reason: UnknownReason::ProcessStateUnproved,
            };
        }
        Ok(v)
    }
    pub fn native_authority_failure(&self) -> Option<PrefixAuthorityFailure> {
        self.authority_failure
    }
    fn check_lease(&self, lease: LeaseId) -> Result<(), RefusalCode> {
        if self.lease.as_ref().is_some_and(|(_, l, _)| *l == lease) {
            Ok(())
        } else {
            Err(RefusalCode::WrongLease)
        }
    }
    pub fn query_acquire(
        &self,
        intent: IntentId,
        binding: BindingId,
        manifest: Digest,
    ) -> Result<AcquireView, RefusalCode> {
        if binding != self.binding {
            return Err(RefusalCode::WrongBinding);
        }
        if manifest != self.manifest {
            return Err(RefusalCode::WrongManifest);
        }
        Ok(match &self.lease {
            Some((i, lease, state)) if *i == intent => AcquireView::Lease {
                lease: *lease,
                state: state.clone(),
            },
            _ => AcquireView::NotFound,
        })
    }
    pub fn query_operation(
        &self,
        lease: LeaseId,
        operation: OperationId,
    ) -> Result<OperationView, RefusalCode> {
        self.check_lease(lease)?;
        Ok(self
            .operations
            .iter()
            .find(|o| o.operation == operation)
            .map(|o| o.view.clone())
            .unwrap_or(OperationView::Missing))
    }
    pub fn query_lease(&self, lease: LeaseId) -> Result<LeaseView, RefusalCode> {
        self.check_lease(lease)?;
        Ok(self
            .lease
            .as_ref()
            .ok_or(RefusalCode::WrongLease)?
            .2
            .clone())
    }
    pub fn replay_operation(&self, request: &Request) -> Result<ReplayDecision, RefusalCode> {
        let (lease, operation, kind) = match request {
            Request::CreateFile {
                lease, operation, ..
            } => (*lease, *operation, OperationKind::CreateFile),
            Request::Run {
                lease, operation, ..
            } => (*lease, *operation, OperationKind::Run),
            Request::ServiceAction {
                lease, operation, ..
            } => (*lease, *operation, OperationKind::ServiceAction),
            _ => return Err(RefusalCode::InvalidRequest),
        };
        self.check_lease(lease)?;
        if self.authority_failure.is_some() {
            return Err(RefusalCode::LeaseQuarantined);
        }
        let digest = request_digest(request).map_err(|_| RefusalCode::InvalidRequest)?;
        if let Some(o) = self.operations.iter().find(|o| o.operation == operation) {
            return if o.kind == kind && o.digest == digest {
                Ok(ReplayDecision::Recorded(o.view.clone()))
            } else {
                Err(RefusalCode::ReplayConflict)
            };
        }
        match self.query_lease(lease)? {
            LeaseView::Held { .. } => Ok(ReplayDecision::NotRecorded),
            LeaseView::Quarantined { .. } => Err(RefusalCode::LeaseQuarantined),
            LeaseView::AwaitingExternalReset { .. } => Err(RefusalCode::AwaitingExternalReset),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arithmetic_checked_and_reserved() {
        assert_eq!(
            capacity(usize::MAX, 1, false, false),
            Err(LedgerError::Capacity)
        );
        assert_eq!(
            capacity(MAX_LEDGER_BYTES - CLOSE_RESERVE, 0, false, false),
            Ok(MAX_LEDGER_BYTES - CLOSE_RESERVE)
        );
        assert_eq!(
            capacity(MAX_LEDGER_BYTES - CLOSE_RESERVE, 1, false, false),
            Err(LedgerError::Capacity)
        );
        assert!(
            capacity(
                MAX_LEDGER_BYTES - CLOSE_RESERVE - FINISH_RESERVE,
                0,
                true,
                false
            )
            .is_ok()
        );
        assert!(
            capacity(
                MAX_LEDGER_BYTES - CLOSE_RESERVE - FINISH_RESERVE,
                1,
                true,
                false
            )
            .is_err()
        );
    }
}
