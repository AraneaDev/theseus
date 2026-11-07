//! Dispatcher-only snapshots; no filesystem or native cleanup authority.
use crate::{
    RuntimeError,
    contract::{CachePatch, LivePhase, RecoveredOperation, RunKey},
};
use std::time::Instant;
use theseus_native_protocol::v2;
pub(crate) struct Cache {
    desktop_states: Vec<(v2::broker::BrokerOperationIdentity, v2::broker::BrokerState)>,
    pub(crate) description: Option<v2::Reply>,
    pub(crate) lease: Option<(v2::IntentId, v2::LeaseId, v2::LeaseView)>,
    operations: Vec<RecoveredOperation>,
    terminals: Vec<(v2::LeaseId, v2::OperationId, v2::OperationView)>,
    wire_terminals: Vec<(v2::LeaseId, v2::OperationId)>,
    pub(crate) live: Option<Live>,
    pub(crate) poisoned: bool,
    pub(crate) stopped: bool,
    pub(crate) native_settled: bool,
}
pub(crate) struct Live {
    pub(crate) key: RunKey,
    kind: v2::OperationKind,
    phase: LivePhase,
    pub(crate) deadline: Option<Instant>,
    pub(crate) terminal: Option<v2::OperationView>,
}
impl Cache {
    pub(crate) fn desktop_query(
        &self,
        identity: &v2::broker::BrokerOperationIdentity,
    ) -> Option<v2::Reply> {
        self.desktop_states
            .iter()
            .find(|(i, _)| i == identity)
            .map(|(identity, state)| v2::Reply::DesktopState {
                identity: Box::new(identity.clone()),
                state: Box::new(state.clone()),
            })
    }
    pub(crate) fn new() -> Self {
        Self {
            desktop_states: vec![],
            description: None,
            lease: None,
            operations: Vec::new(),
            terminals: Vec::new(),
            wire_terminals: Vec::new(),
            live: None,
            poisoned: false,
            stopped: false,
            native_settled: false,
        }
    }
    #[cfg(test)]
    pub(crate) fn begin(&mut self, key: RunKey) -> Result<(), RuntimeError> {
        self.begin_kind(key, v2::OperationKind::Run)
    }
    pub(crate) fn begin_kind(
        &mut self,
        key: RunKey,
        kind: v2::OperationKind,
    ) -> Result<(), RuntimeError> {
        let recorded = |lease, operation| {
            self.operations
                .iter()
                .any(|o| o.lease == lease && o.operation == operation)
                || self
                    .terminals
                    .iter()
                    .any(|(l, o, _)| *l == lease && *o == operation)
        };
        let live_extra = usize::from(
            self.live
                .as_ref()
                .is_some_and(|l| !recorded(l.key.lease, l.key.operation)),
        );
        if !recorded(key.lease, key.operation)
            && self.operations.len() + self.terminals.len() + live_extra >= v2::MAX_OPERATIONS
        {
            return Err(RuntimeError::Capacity);
        }
        if let Some(old) = self.live.take() {
            if let Some(terminal) = old.terminal {
                if let Some(recovered) = self
                    .operations
                    .iter_mut()
                    .find(|o| o.lease == old.key.lease && o.operation == old.key.operation)
                {
                    recovered.view = terminal;
                } else {
                    self.terminals
                        .push((old.key.lease, old.key.operation, terminal));
                }
            } else {
                self.live = Some(old);
                return Err(RuntimeError::InvalidState);
            }
        }
        self.live = Some(Live {
            key,
            kind,
            phase: LivePhase::AdmissionPending,
            deadline: None,
            terminal: None,
        });
        Ok(())
    }
    pub(crate) fn apply(&mut self, patch: CachePatch) -> Result<(), RuntimeError> {
        match patch {
            CachePatch::DesktopState { identity, state } => {
                if let Some((old, saved)) = self
                    .desktop_states
                    .iter_mut()
                    .find(|(i, _)| i.helper_operation == identity.helper_operation)
                {
                    if old != &identity {
                        return Err(RuntimeError::InvalidState);
                    }
                    *saved = state;
                } else {
                    if self.desktop_states.len() >= v2::MAX_OPERATIONS {
                        return Err(RuntimeError::Capacity);
                    }
                    self.desktop_states.push((identity, state));
                }
            }
            CachePatch::Initialized {
                description,
                lease,
                operations,
            } => {
                if self.description.is_some()
                    || !matches!(
                        description,
                        v2::Reply::Description { .. }
                            | v2::Reply::NativeDescription { .. }
                            | v2::Reply::IntegrationDescription { .. }
                    )
                    || operations.len() > v2::MAX_OPERATIONS
                {
                    return Err(RuntimeError::InvalidState);
                }
                // Restart snapshots must never masquerade as a live process.
                if operations.iter().any(|o| {
                    matches!(
                        o.view,
                        v2::OperationView::Accepted { .. }
                            | v2::OperationView::Running
                            | v2::OperationView::Missing
                    )
                }) {
                    return Err(RuntimeError::InvalidState);
                }
                self.description = Some(description);
                self.lease = lease;
                self.operations = operations;
            }
            CachePatch::Lease {
                intent,
                lease,
                view,
            } => self.lease = Some((intent, lease, view)),
            CachePatch::Phase {
                key,
                kind,
                phase,
                accepted_at,
                deadline,
            } => {
                let live = self
                    .live
                    .as_mut()
                    .filter(|l| l.key == key)
                    .ok_or(RuntimeError::InvalidState)?;
                if matches!(phase, LivePhase::AdmissionPending) {
                    if accepted_at.is_some() || deadline.is_some() {
                        return Err(RuntimeError::InvalidState);
                    }
                } else if accepted_at.zip(deadline).is_none_or(|(a, d)| {
                    d.checked_duration_since(a) != Some(std::time::Duration::from_secs(30))
                }) {
                    return Err(RuntimeError::InvalidState);
                }
                if live.deadline.is_some() && live.deadline != deadline {
                    return Err(RuntimeError::InvalidState);
                }
                live.kind = kind;
                live.phase = phase;
                live.deadline = deadline;
            }
            CachePatch::Terminal {
                key,
                view,
                boundary,
            } => {
                if boundary.key != key
                    || !matches!(
                        view,
                        v2::OperationView::Finished { .. } | v2::OperationView::Unknown { .. }
                    )
                {
                    return Err(RuntimeError::InvalidState);
                }
                let live = self
                    .live
                    .as_mut()
                    .filter(|l| l.key == key)
                    .ok_or(RuntimeError::InvalidState)?;
                if live.terminal.as_ref().is_some_and(|v| v != &view) {
                    return Err(RuntimeError::InvalidState);
                }
                live.terminal = Some(view);
            }
            CachePatch::Stopped {
                store_poisoned,
                native_settled,
            } => {
                self.stopped = true;
                self.poisoned = store_poisoned;
                self.native_settled = native_settled;
            }
        }
        Ok(())
    }
    pub(crate) fn note_wire_terminal(
        &mut self,
        lease: v2::LeaseId,
        operation: v2::OperationId,
    ) -> Result<(), RuntimeError> {
        if !self.wire_terminals.contains(&(lease, operation)) {
            if self.wire_terminals.len() == v2::MAX_OPERATIONS {
                return Err(RuntimeError::Capacity);
            }
            self.wire_terminals.push((lease, operation));
        }
        Ok(())
    }
    pub(crate) fn replay_refusal(&self, request: &v2::Request) -> Option<v2::RefusalCode> {
        let (lease, operation, kind) = match request {
            v2::Request::Run {
                lease, operation, ..
            } => (lease, operation, v2::OperationKind::Run),
            v2::Request::CreateFile {
                lease, operation, ..
            } => (lease, operation, v2::OperationKind::CreateFile),
            v2::Request::ServiceAction {
                lease, operation, ..
            } => (lease, operation, v2::OperationKind::ServiceAction),
            _ => return None,
        };
        if self.wire_terminals.contains(&(*lease, *operation))
            || self
                .terminals
                .iter()
                .any(|(l, o, _)| l == lease && o == operation)
            || self.live.as_ref().is_some_and(|l| {
                l.key.lease == *lease && l.key.operation == *operation && l.terminal.is_some()
            })
        {
            return Some(v2::RefusalCode::InvalidRequest);
        }
        if let Some(old) = self
            .operations
            .iter()
            .find(|o| o.lease == *lease && o.operation == *operation)
            && (old.kind != kind || v2::request_digest(request).ok() != Some(old.request_digest))
        {
            return Some(v2::RefusalCode::ReplayConflict);
        }
        None
    }
    pub(crate) fn operation(&self, lease: v2::LeaseId, operation: v2::OperationId) -> v2::Reply {
        if let Some(live) = self
            .live
            .as_ref()
            .filter(|l| l.key.lease == lease && l.key.operation == operation)
        {
            if let Some(view) = &live.terminal {
                return v2::Reply::Operation {
                    lease,
                    operation,
                    state: view.clone(),
                };
            }
            let state = match live.phase {
                LivePhase::AdmissionPending | LivePhase::TerminalCommitPending => {
                    return refused(v2::RefusalCode::OwnershipPending);
                }
                LivePhase::Accepted => v2::OperationView::Accepted {
                    operation_kind: live.kind,
                },
                LivePhase::Running | LivePhase::CleanupPending => v2::OperationView::Running,
            };
            return v2::Reply::Operation {
                lease,
                operation,
                state,
            };
        }
        if let Some((_, _, state)) = self
            .terminals
            .iter()
            .find(|(l, o, _)| *l == lease && *o == operation)
        {
            return v2::Reply::Operation {
                lease,
                operation,
                state: state.clone(),
            };
        }
        let state = self
            .operations
            .iter()
            .find(|o| o.lease == lease && o.operation == operation)
            .map(|o| o.view.clone())
            .unwrap_or(v2::OperationView::Missing);
        v2::Reply::Operation {
            lease,
            operation,
            state,
        }
    }
    pub(crate) fn query(&self, request: &v2::Request) -> Option<v2::Reply> {
        use v2::{RefusalCode, Reply, Request};
        if self.description.is_none() {
            return Some(refused(RefusalCode::OwnershipPending));
        }
        Some(match request {
            Request::Describe => self.description.clone()?,
            Request::QueryAcquire {
                intent,
                binding,
                manifest_digest,
            } => {
                match &self.description {
                    Some(Reply::Description {
                        binding: b,
                        manifest_digest: d,
                        ..
                    }) if binding == b && manifest_digest == d => {}
                    Some(Reply::NativeDescription { description })
                        if binding == &description.binding
                            && manifest_digest == &description.manifest_digest => {}
                    Some(Reply::NativeDescription { description })
                        if binding != &description.binding =>
                    {
                        return Some(refused(RefusalCode::WrongBinding));
                    }
                    Some(Reply::Description { binding: b, .. }) if binding != b => {
                        return Some(refused(RefusalCode::WrongBinding));
                    }
                    _ => return Some(refused(RefusalCode::WrongManifest)),
                }
                let state = match &self.lease {
                    Some((i, l, s)) if i == intent => v2::AcquireView::Lease {
                        lease: *l,
                        state: s.clone(),
                    },
                    _ => v2::AcquireView::NotFound,
                };
                Reply::Acquired {
                    intent: *intent,
                    binding: *binding,
                    manifest_digest: *manifest_digest,
                    state,
                }
            }
            Request::QueryLease { lease } => match &self.lease {
                Some((_, id, state)) if id == lease => Reply::Lease {
                    lease: *lease,
                    state: state.clone(),
                },
                _ => refused(RefusalCode::WrongLease),
            },
            Request::QueryOperation { lease, operation } | Request::Cancel { lease, operation } => {
                if self.lease.as_ref().is_some_and(|(_, id, _)| id == lease) {
                    self.operation(*lease, *operation)
                } else {
                    refused(RefusalCode::WrongLease)
                }
            }
            _ => return None,
        })
    }
}
pub(crate) fn refused(code: v2::RefusalCode) -> v2::Reply {
    v2::Reply::Refused {
        code,
        inspect_failure: None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::FinalBoundary;
    fn key(n: u64) -> RunKey {
        RunKey {
            lease: v2::LeaseId::from_bytes([1; 16]),
            operation: v2::OperationId::from_bytes([n as u8; 16]),
            generation: n,
        }
    }
    fn terminal(c: &mut Cache, k: RunKey) {
        c.apply(CachePatch::Terminal {
            key: k,
            view: v2::OperationView::Finished {
                outcome: v2::Outcome::NotStarted {
                    reason: v2::NotStartedReason::CancelRequested,
                },
            },
            boundary: FinalBoundary {
                key: k,
                final_ordinal: 0,
                stdout_bytes: 0,
                stderr_bytes: 0,
                truncated: false,
            },
        })
        .unwrap();
    }
    #[test]
    fn file_requests_reach_serialized_worker() {
        let mut c = Cache::new();
        c.description = Some(v2::Reply::Refused {
            code: v2::RefusalCode::Unsupported,
            inspect_failure: None,
        });
        assert_eq!(
            c.query(&v2::Request::InspectFile {
                lease: key(1).lease,
                path: v2::LeafName::new("leaf").unwrap(),
            }),
            None
        );
    }
    #[test]
    fn cached_queries_require_exact_reserved_lease() {
        let mut c = Cache::new();
        c.description = Some(v2::Reply::Description {
            binding: v2::BindingId::from_bytes([1; 16]),
            manifest_digest: v2::Digest::from_bytes([2; 32]),
            workspace_marker: v2::WorkspaceMarker::from_bytes([3; 16]),
            os: v2::Os::Linux,
            helper_principal: v2::Principal::Posix {
                effective_uid: 1,
                effective_gid: 1,
            },
            programs: v2::ProgramList::new(vec![v2::ProgramId::new("probe").unwrap()]).unwrap(),
        });
        let k = key(1);
        for reserved in [false, true] {
            if reserved {
                c.lease = Some((
                    v2::IntentId::from_bytes([4; 16]),
                    k.lease,
                    v2::LeaseView::Held {
                        intent: v2::IntentId::from_bytes([4; 16]),
                    },
                ));
            }
            for lease in [k.lease, v2::LeaseId::from_bytes([9; 16])] {
                for request in [
                    v2::Request::QueryOperation {
                        lease,
                        operation: k.operation,
                    },
                    v2::Request::Cancel {
                        lease,
                        operation: k.operation,
                    },
                ] {
                    let expected = if reserved && lease == k.lease {
                        v2::Reply::Operation {
                            lease,
                            operation: k.operation,
                            state: v2::OperationView::Missing,
                        }
                    } else {
                        refused(v2::RefusalCode::WrongLease)
                    };
                    assert_eq!(c.query(&request), Some(expected));
                }
            }
        }
    }
    #[test]
    fn admission_and_commit_pending_never_claim_missing_or_finished() {
        let mut c = Cache::new();
        let k = key(1);
        c.begin(k).unwrap();
        assert_eq!(
            c.operation(k.lease, k.operation),
            refused(v2::RefusalCode::OwnershipPending)
        );
        let at = Instant::now();
        c.apply(CachePatch::Phase {
            key: k,
            kind: v2::OperationKind::Run,
            phase: LivePhase::TerminalCommitPending,
            accepted_at: Some(at),
            deadline: Some(at + std::time::Duration::from_secs(30)),
        })
        .unwrap();
        assert_eq!(
            c.operation(k.lease, k.operation),
            refused(v2::RefusalCode::OwnershipPending)
        );
        terminal(&mut c, k);
        assert!(matches!(
            c.operation(k.lease, k.operation),
            v2::Reply::Operation {
                state: v2::OperationView::Finished { .. },
                ..
            }
        ));
    }
    #[test]
    fn recorded_replay_remains_available_at_history_capacity() {
        let mut c = Cache::new();
        for n in 0..v2::MAX_OPERATIONS {
            let k = key(n as u64 + 1);
            c.operations.push(RecoveredOperation {
                lease: k.lease,
                operation: k.operation,
                request_digest: v2::Digest::from_bytes([0; 32]),
                kind: v2::OperationKind::Run,
                view: v2::OperationView::Unknown {
                    reason: v2::UnknownReason::HelperRestartedWithUnfinishedOperation,
                },
            });
        }
        assert!(c.begin(key(1)).is_ok());
    }

    #[test]
    fn new_generation_preserves_previous_durable_terminal() {
        let mut c = Cache::new();
        let first = key(1);
        c.begin(first).unwrap();
        terminal(&mut c, first);
        c.begin(key(2)).unwrap();
        assert!(matches!(
            c.operation(first.lease, first.operation),
            v2::Reply::Operation {
                state: v2::OperationView::Finished { .. },
                ..
            }
        ));
    }
}
