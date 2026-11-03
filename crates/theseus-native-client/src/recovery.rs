use crate::{Action, ClientError};
use theseus_host::fs::CommitScope;
use theseus_native_helper_store::IntentRecord;
use theseus_native_protocol::v2;

#[derive(Default)]
pub(crate) struct Recovery {
    intents: Vec<(IntentRecord, CommitScope, Option<v2::LeaseId>)>,
    operations: Vec<(v2::LeaseId, v2::OperationId, Option<v2::OperationView>)>,
    quarantined: Vec<v2::LeaseId>,
    captures: Vec<StreamEvidence>,
}
struct StreamEvidence {
    lease: v2::LeaseId,
    operation: v2::OperationId,
    observed: usize,
    ends: [Option<u64>; 2],
    gap: bool,
    truncated: bool,
}
impl StreamEvidence {
    fn finish(&self, outcome: &v2::Outcome) -> Result<(), ClientError> {
        match outcome {
            v2::Outcome::RunCompleted {
                stdout_bytes,
                stderr_bytes,
                truncated,
                ..
            } if !self.ends[0].is_some_and(|end| end > *stdout_bytes)
                && !self.ends[1].is_some_and(|end| end > *stderr_bytes)
                && (!(self.gap || self.truncated) || *truncated) =>
            {
                Ok(())
            }
            v2::Outcome::NotStarted { .. } | v2::Outcome::LaunchFailed { .. }
                if self.observed == 0 && !self.gap && !self.truncated =>
            {
                Ok(())
            }
            _ => Err(ClientError::Protocol),
        }
    }
}
impl Recovery {
    fn stream(
        &mut self,
        lease: v2::LeaseId,
        operation: v2::OperationId,
        fresh: bool,
    ) -> Result<&mut StreamEvidence, ClientError> {
        let index = match self
            .captures
            .iter()
            .position(|s| s.lease == lease && s.operation == operation)
        {
            Some(index) => index,
            None => {
                if self.captures.len() == v2::MAX_OPERATIONS {
                    return Err(ClientError::Capacity);
                }
                self.captures.push(StreamEvidence {
                    lease,
                    operation,
                    observed: 0,
                    ends: if fresh { [Some(0); 2] } else { [None; 2] },
                    gap: false,
                    truncated: false,
                });
                self.captures.len() - 1
            }
        };
        Ok(&mut self.captures[index])
    }
    pub(crate) fn log(
        &mut self,
        lease: v2::LeaseId,
        operation: v2::OperationId,
        code: v2::LogCode,
    ) -> Result<(), ClientError> {
        if code == v2::LogCode::OutputTruncated {
            self.stream(lease, operation, false)?.truncated = true;
        }
        Ok(())
    }
    pub(crate) fn output(
        &mut self,
        lease: v2::LeaseId,
        operation: v2::OperationId,
        channel: v2::Channel,
        offset: u64,
        n: usize,
    ) -> Result<(), ClientError> {
        let stream = self.stream(lease, operation, false)?;
        let index = usize::from(channel == v2::Channel::Stderr);
        let end = offset.checked_add(n as u64).ok_or(ClientError::Protocol)?;
        let observed = stream
            .observed
            .checked_add(n)
            .ok_or(ClientError::Protocol)?;
        if observed > v2::MAX_STREAM_BYTES || stream.ends[index].is_some_and(|last| offset < last) {
            return Err(ClientError::Protocol);
        }
        stream.gap |= stream.ends[index].is_some_and(|last| offset > last);
        stream.observed = observed;
        stream.ends[index] = Some(end);
        Ok(())
    }
    pub(crate) fn request(
        &mut self,
        action: Action<'_>,
        expected: &crate::ExpectedHelper,
    ) -> Result<v2::Request, ClientError> {
        Ok(match action {
            Action::Acquire(p) | Action::RecoverAcquire(p) => {
                let record = p.record();
                let b = &expected.binding;
                if record.binding != b.binding
                    || record.manifest_digest != b.manifest_digest
                    || record.workspace_marker != b.workspace_marker
                {
                    return Err(ClientError::Preflight);
                }
                let recover = matches!(action, Action::RecoverAcquire(_));
                if let Some((old, _, _)) = self
                    .intents
                    .iter()
                    .find(|(old, _, _)| old.intent == record.intent)
                {
                    if old != record {
                        return Err(ClientError::Preflight);
                    }
                    if !recover {
                        return Err(ClientError::RecoveryRequired);
                    }
                } else {
                    if self.intents.len() == v2::MAX_OPERATIONS {
                        return Err(ClientError::Capacity);
                    }
                    self.intents.push((record.clone(), p.commit_scope(), None));
                }
                if recover {
                    v2::Request::QueryAcquire {
                        intent: record.intent,
                        binding: record.binding,
                        manifest_digest: record.manifest_digest,
                    }
                } else {
                    p.request()
                }
            }
            Action::Run {
                lease,
                operation,
                program,
            } => {
                if !expected.programs.as_slice().contains(&program) {
                    return Err(ClientError::Preflight);
                }
                if self.quarantined.contains(&lease)
                    || self.operations.iter().any(|(l, o, state)| {
                        *l == lease
                            && (*o == operation
                                || !matches!(state, Some(v2::OperationView::Finished { .. })))
                    })
                {
                    return Err(ClientError::RecoveryRequired);
                }
                if self.operations.len() == v2::MAX_OPERATIONS {
                    return Err(ClientError::Capacity);
                }
                self.stream(lease, operation, true)?;
                self.operations.push((lease, operation, None));
                v2::Request::Run {
                    lease,
                    operation,
                    program,
                }
            }
            Action::Query { lease, operation } => v2::Request::QueryOperation { lease, operation },
            Action::Cancel { lease, operation } => v2::Request::Cancel { lease, operation },
            Action::QueryLease(lease) => v2::Request::QueryLease { lease },
            Action::Release(lease) => v2::Request::Release { lease },
        })
    }
    pub(crate) fn observe(&mut self, reply: &v2::Reply) -> Result<(), ClientError> {
        match reply {
            v2::Reply::Operation {
                lease,
                operation,
                state,
            } => {
                if let v2::OperationView::Finished { outcome } = state
                    && let Some(stream) = self
                        .captures
                        .iter()
                        .find(|s| s.lease == *lease && s.operation == *operation)
                {
                    stream.finish(outcome)?;
                }
                if let Some((_, _, old)) = self
                    .operations
                    .iter_mut()
                    .find(|(l, o, _)| l == lease && o == operation)
                {
                    if matches!(
                        old,
                        Some(
                            v2::OperationView::Finished { .. } | v2::OperationView::Unknown { .. }
                        )
                    ) && old.as_ref() != Some(state)
                    {
                        return Err(ClientError::Protocol);
                    }
                    *old = Some(state.clone());
                } else if !matches!(state, v2::OperationView::Missing) {
                    if self.operations.len() == v2::MAX_OPERATIONS {
                        return Err(ClientError::Capacity);
                    }
                    self.operations
                        .push((*lease, *operation, Some(state.clone())));
                }
                if matches!(state, v2::OperationView::Unknown { .. }) {
                    self.quarantine(*lease)?;
                }
            }
            v2::Reply::Acquired {
                intent,
                binding,
                manifest_digest,
                state,
            } => {
                let index = self
                    .intents
                    .iter()
                    .position(|(record, _, _)| {
                        record.intent == *intent
                            && record.binding == *binding
                            && record.manifest_digest == *manifest_digest
                    })
                    .ok_or(ClientError::Protocol)?;
                let observed = self.intents[index].2;
                match state {
                    v2::AcquireView::NotFound if observed.is_some() => {
                        return Err(ClientError::Protocol);
                    }
                    v2::AcquireView::Lease { lease, state } => {
                        if observed.is_some_and(|old| old != *lease) {
                            return Err(ClientError::Protocol);
                        }
                        if !matches!(state, v2::LeaseView::Held { .. }) {
                            self.quarantine(*lease)?;
                        }
                        self.intents[index].2 = Some(*lease);
                    }
                    v2::AcquireView::NotFound => {}
                }
            }
            v2::Reply::Lease { lease, state } if !matches!(state, v2::LeaseView::Held { .. }) => {
                self.quarantine(*lease)?
            }
            _ => {}
        }
        Ok(())
    }
    fn quarantine(&mut self, lease: v2::LeaseId) -> Result<(), ClientError> {
        if !self.quarantined.contains(&lease) {
            if self.quarantined.len() == v2::MAX_OPERATIONS {
                return Err(ClientError::Capacity);
            }
            self.quarantined.push(lease);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ids() -> (v2::LeaseId, v2::OperationId) {
        (
            v2::LeaseId::from_bytes([1; 16]),
            v2::OperationId::from_bytes([2; 16]),
        )
    }
    #[test]
    fn loss_requires_explicit_original_query_and_never_another_run() {
        let mut r = Recovery::default();
        let e = crate::pump::tests::expected();
        let (lease, operation) = ids();
        assert!(matches!(
            r.request(
                Action::Run {
                    lease,
                    operation,
                    program: v2::ProgramId::new("probe").unwrap()
                },
                &e
            )
            .unwrap(),
            v2::Request::Run { .. }
        ));
        assert_eq!(
            r.request(
                Action::Run {
                    lease,
                    operation: v2::OperationId::from_bytes([3; 16]),
                    program: v2::ProgramId::new("probe").unwrap()
                },
                &e
            ),
            Err(ClientError::RecoveryRequired)
        );
        assert_eq!(
            r.request(Action::Query { lease, operation }, &e).unwrap(),
            v2::Request::QueryOperation { lease, operation }
        );
    }
    #[test]
    fn unknown_quarantines_new_identity_and_finished_never_changes() {
        let mut r = Recovery::default();
        let e = crate::pump::tests::expected();
        let (lease, operation) = ids();
        let finished = v2::Reply::Operation {
            lease,
            operation,
            state: v2::OperationView::Finished {
                outcome: v2::Outcome::NotStarted {
                    reason: v2::NotStartedReason::CancelRequested,
                },
            },
        };
        r.observe(&finished).unwrap();
        r.observe(&finished).unwrap();
        assert_eq!(
            r.observe(&v2::Reply::Operation {
                lease,
                operation,
                state: v2::OperationView::Missing
            }),
            Err(ClientError::Protocol)
        );
        assert!(
            r.request(
                Action::Run {
                    lease,
                    operation: v2::OperationId::from_bytes([3; 16]),
                    program: v2::ProgramId::new("probe").unwrap()
                },
                &e
            )
            .is_ok()
        );
        r.observe(&v2::Reply::Operation {
            lease,
            operation: v2::OperationId::from_bytes([3; 16]),
            state: v2::OperationView::Unknown {
                reason: v2::UnknownReason::ProcessStateUnproved,
            },
        })
        .unwrap();
        assert_eq!(
            r.request(
                Action::Run {
                    lease,
                    operation: v2::OperationId::from_bytes([4; 16]),
                    program: v2::ProgramId::new("probe").unwrap()
                },
                &e
            ),
            Err(ClientError::RecoveryRequired)
        );
    }
    #[test]
    fn prepared_acquire_copies_receipt_and_recovery_only_queries_same_intent() {
        use theseus_host::fs::HostDir;
        use theseus_native_helper_store::IntentStore;
        let temp = tempfile::tempdir().unwrap();
        let root = HostDir::create_private(&temp.path().join("intent")).unwrap();
        let mut store = IntentStore::new(root);
        let record = IntentRecord {
            intent: v2::IntentId::from_bytes([8; 16]),
            binding: v2::BindingId::from_bytes([1; 16]),
            manifest_digest: v2::Digest::from_bytes([2; 32]),
            workspace_marker: v2::WorkspaceMarker::from_bytes([3; 16]),
        };
        let prepared = store.prepare(record.clone()).unwrap();
        let e = crate::pump::tests::expected();
        let mut r = Recovery::default();
        assert_eq!(
            r.request(Action::Acquire(&prepared), &e).unwrap(),
            v2::Request::Acquire {
                intent: record.intent,
                binding: record.binding,
                manifest_digest: record.manifest_digest
            }
        );
        assert_eq!(
            r.request(Action::Acquire(&prepared), &e),
            Err(ClientError::RecoveryRequired)
        );
        assert_eq!(
            r.request(Action::RecoverAcquire(&prepared), &e).unwrap(),
            v2::Request::QueryAcquire {
                intent: record.intent,
                binding: record.binding,
                manifest_digest: record.manifest_digest
            }
        );
        assert_eq!(r.intents[0].0, record);
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;
    #[test]
    fn output_budget_survives_connection_epochs_and_covers_both_channels() {
        let mut recovery = Recovery::default();
        let lease = v2::LeaseId::from_bytes([1; 16]);
        let operation = v2::OperationId::from_bytes([2; 16]);
        for (channel, offset) in [
            (v2::Channel::Stdout, 0),
            (v2::Channel::Stderr, 0),
            (v2::Channel::Stdout, 4096),
            (v2::Channel::Stderr, 4096),
        ] {
            recovery
                .output(lease, operation, channel, offset, 4096)
                .unwrap();
        }
        // A new SessionGuard after reconnect must not replenish the live run's
        // shared raw-byte budget or accept an overlapping capture segment.
        assert_eq!(
            recovery.output(lease, operation, v2::Channel::Stdout, 8192, 1),
            Err(ClientError::Protocol)
        );
    }
}

#[cfg(test)]
mod terminal_capture_tests {
    use super::*;
    #[test]
    fn terminal_counters_on_new_connection_cannot_erase_pre_loss_bytes() {
        let mut recovery = Recovery::default();
        let lease = v2::LeaseId::from_bytes([1; 16]);
        let operation = v2::OperationId::from_bytes([2; 16]);
        recovery
            .output(lease, operation, v2::Channel::Stdout, 100, 12)
            .unwrap();
        let reply = v2::Reply::Operation {
            lease,
            operation,
            state: v2::OperationView::Finished {
                outcome: v2::Outcome::RunCompleted {
                    reason: v2::RunReason::Natural,
                    status: v2::NativeStatus::PosixExit { code: 0 },
                    cleanup: v2::Cleanup::PosixLeaderReapedGroupTerminationRequested,
                    stdout_bytes: 111,
                    stderr_bytes: 0,
                    truncated: false,
                },
            },
        };
        assert_eq!(recovery.observe(&reply), Err(ClientError::Protocol));
    }
}

#[cfg(test)]
mod review_regressions {
    use super::*;
    fn ids() -> (v2::LeaseId, v2::OperationId) {
        (
            v2::LeaseId::from_bytes([1; 16]),
            v2::OperationId::from_bytes([2; 16]),
        )
    }
    fn run(r: &mut Recovery) {
        let (lease, operation) = ids();
        r.request(
            Action::Run {
                lease,
                operation,
                program: v2::ProgramId::new("probe").unwrap(),
            },
            &crate::pump::tests::expected(),
        )
        .unwrap();
        r.observe(&v2::Reply::Operation {
            lease,
            operation,
            state: v2::OperationView::Running,
        })
        .unwrap();
    }
    fn completed(truncated: bool) -> v2::Outcome {
        v2::Outcome::RunCompleted {
            reason: v2::RunReason::Natural,
            status: v2::NativeStatus::PosixExit { code: 0 },
            cleanup: v2::Cleanup::PosixLeaderReapedGroupTerminationRequested,
            stdout_bytes: 30,
            stderr_bytes: 20,
            truncated,
        }
    }
    fn terminal(outcome: v2::Outcome) -> v2::Reply {
        let (lease, operation) = ids();
        v2::Reply::Operation {
            lease,
            operation,
            state: v2::OperationView::Finished { outcome },
        }
    }
    #[test]
    fn r1_output_before_loss_rejects_not_started_and_launch_failed_without_mutation() {
        let (lease, operation) = ids();
        for outcome in [
            v2::Outcome::NotStarted {
                reason: v2::NotStartedReason::CancelRequested,
            },
            v2::Outcome::LaunchFailed {
                phase: v2::LaunchPhase::NeverCreated,
                code: v2::FailureCode::Io,
                cleanup: v2::SpawnCleanup::NoChildCreated,
            },
        ] {
            let mut r = Recovery::default();
            run(&mut r);
            r.output(lease, operation, v2::Channel::Stdout, 0, 4)
                .unwrap();
            assert_eq!(r.observe(&terminal(outcome)), Err(ClientError::Protocol));
            assert!(matches!(
                r.operations[0].2,
                Some(v2::OperationView::Running)
            ));
            assert_eq!(
                r.request(
                    Action::Run {
                        lease,
                        operation: v2::OperationId::from_bytes([4; 16]),
                        program: v2::ProgramId::new("probe").unwrap()
                    },
                    &crate::pump::tests::expected()
                ),
                Err(ClientError::RecoveryRequired)
            );
            r.observe(&terminal(completed(false))).unwrap();
        }
    }
    #[test]
    fn r1_gap_and_truncation_log_survive_loss_and_require_truncated_terminal() {
        let (lease, operation) = ids();
        for log_only in [false, true] {
            let mut r = Recovery::default();
            run(&mut r);
            if log_only {
                r.log(lease, operation, v2::LogCode::OutputTruncated)
                    .unwrap();
            } else {
                r.output(lease, operation, v2::Channel::Stdout, 0, 1)
                    .unwrap();
                r.output(lease, operation, v2::Channel::Stdout, 3, 1)
                    .unwrap();
            }
            assert_eq!(
                r.observe(&terminal(completed(false))),
                Err(ClientError::Protocol)
            );
            assert!(matches!(
                r.operations[0].2,
                Some(v2::OperationView::Running)
            ));
            assert_eq!(
                r.observe(&terminal(v2::Outcome::NotStarted {
                    reason: v2::NotStartedReason::Deadline
                })),
                Err(ClientError::Protocol)
            );
            r.observe(&terminal(completed(true))).unwrap();
        }
    }
    #[test]
    fn r1_no_output_evidence_still_allows_valid_recovered_not_started() {
        let mut r = Recovery::default();
        run(&mut r);
        r.observe(&terminal(v2::Outcome::NotStarted {
            reason: v2::NotStartedReason::Deadline,
        }))
        .unwrap();
    }
    #[test]
    fn r2_observed_lease_cannot_change_or_disappear_but_can_progress() {
        use theseus_native_helper_store::IntentStore;
        let temp = tempfile::tempdir().unwrap();
        let mut store = IntentStore::new(
            theseus_host::fs::HostDir::create_private(&temp.path().join("intent")).unwrap(),
        );
        let record = IntentRecord {
            intent: v2::IntentId::from_bytes([8; 16]),
            binding: v2::BindingId::from_bytes([1; 16]),
            manifest_digest: v2::Digest::from_bytes([2; 32]),
            workspace_marker: v2::WorkspaceMarker::from_bytes([3; 16]),
        };
        let prepared = store.prepare(record.clone()).unwrap();
        let mut r = Recovery::default();
        let e = crate::pump::tests::expected();
        r.request(Action::RecoverAcquire(&prepared), &e).unwrap();
        let acquired = |state| v2::Reply::Acquired {
            intent: record.intent,
            binding: record.binding,
            manifest_digest: record.manifest_digest,
            state,
        };
        r.observe(&acquired(v2::AcquireView::NotFound)).unwrap();
        let lease = v2::LeaseId::from_bytes([5; 16]);
        r.observe(&acquired(v2::AcquireView::Lease {
            lease,
            state: v2::LeaseView::Held {
                intent: record.intent,
            },
        }))
        .unwrap();
        assert_eq!(
            r.observe(&acquired(v2::AcquireView::NotFound)),
            Err(ClientError::Protocol)
        );
        assert_eq!(
            r.observe(&acquired(v2::AcquireView::Lease {
                lease: v2::LeaseId::from_bytes([6; 16]),
                state: v2::LeaseView::Held {
                    intent: record.intent
                }
            })),
            Err(ClientError::Protocol)
        );
        r.observe(&acquired(v2::AcquireView::Lease {
            lease,
            state: v2::LeaseView::AwaitingExternalReset {
                intent: record.intent,
            },
        }))
        .unwrap();
        assert!(r.quarantined.contains(&lease));
    }
}
