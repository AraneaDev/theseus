//! Native hooks for the shared ordinary lifecycle ordering.
use super::{model::*, recipe::ValidatedNativeRecipe, transport::NativeSession};
use crate::{
    engine::{
        common::{self, LifecycleBackend, sealed},
        projector::{PublishedResource, RunProjector},
    },
    model::{CheckOutcome, Checkpoint, ScenarioKind, StageOutcome, StepName},
};
use anyhow::{Result, bail, ensure};
use std::time::{Duration, Instant};
use theseus_native_client::{Action, Event, Poll};
use theseus_native_protocol::v2;
pub struct NativeDriver<'a, 's> {
    pub session: NativeSession,
    pub projector: &'a mut RunProjector<'s>,
    pub resource: PublishedResource,
    pub recipe: &'a ValidatedNativeRecipe,
    forward: bool,
    references: Vec<NativeFileObservation>,
    configured: Vec<NativeFileObservation>,
    cancel: crate::engine::CancellationToken,
    limits: super::recipe::NativeRunLimits,
    operation_count: u32,
    scenario_kind: Option<ScenarioKind>,
}
impl<'a, 's> NativeDriver<'a, 's> {
    pub fn new(
        session: NativeSession,
        projector: &'a mut RunProjector<'s>,
        resource: PublishedResource,
        recipe: &'a ValidatedNativeRecipe,
    ) -> Self {
        Self {
            session,
            projector,
            resource,
            recipe,
            forward: true,
            references: vec![],
            configured: vec![],
            cancel: crate::engine::CancellationToken::default(),
            limits: recipe.recipe.limits.clone(),
            operation_count: 0,
            scenario_kind: None,
        }
    }
    pub fn set_limits(&mut self, limits: &super::recipe::NativeRunLimits) -> Result<()> {
        limits.validate()?;
        self.limits = limits.clone();
        Ok(())
    }
    pub fn set_cancellation(&mut self, cancel: crate::engine::CancellationToken) {
        self.cancel = cancel;
    }
    pub fn run(&mut self, kind: ScenarioKind) -> Result<()> {
        self.prepare(kind)?;
        common::run_common(self, kind)
    }
    pub(crate) fn forward_active(&self) -> bool {
        self.forward
    }
    pub(crate) fn prepare(&mut self, kind: ScenarioKind) -> Result<()> {
        self.session.validate_artifacts(self.recipe)?;
        self.scenario_kind = Some(kind);
        let observations = self.capture()?;
        ensure!(
            observations
                .iter()
                .all(|o| matches!(o.state, NativeFileState::AbsentSettled)),
            "native baseline is not settled absence"
        );
        self.record_stage(NativeStage {
            name: "baseline".into(),
            step: None,
            checkpoint: None,
            outcome: StageOutcome::Passed,
            commands: vec![],
            checks: vec![],
            native_files: observations,
            failures: vec![],
        })?;
        Ok(())
    }
    pub(crate) fn lease(&self) -> Result<v2::LeaseId> {
        match &self.resource.resource().ownership {
            NativeOwnership::Acquired { lease, .. } => Ok(v2::LeaseId::parse(lease)?),
            _ => bail!("native driver requires published acquired resource"),
        }
    }
    pub(crate) fn wait_reply(&mut self, id: theseus_native_client::RequestId) -> Result<v2::Reply> {
        let end = Instant::now() + Duration::from_secs(self.limits.inspection_seconds);
        loop {
            ensure!(
                Instant::now() < end,
                "native request completion unproved at deadline"
            );
            match self.session.poll()? {
                Poll::Event(Event::Reply { request, reply }) if request == id => return Ok(reply),
                Poll::Event(Event::ConnectionLost | Event::PeerClosed(_))
                | Poll::TransportSettled => bail!("native transport lost; original owner retained"),
                _ => std::thread::yield_now(),
            }
        }
    }
    pub(crate) fn reserve_operation(&mut self) -> Result<()> {
        ensure!(
            self.operation_count < self.limits.max_operations,
            "native operation budget exhausted before effect"
        );
        self.operation_count += 1;
        Ok(())
    }
    fn execute(&mut self, command: CatalogCommand) -> Result<NativeCommandEvidence> {
        self.reserve_operation()?;
        let lease = self.lease()?;
        let mut bytes = [0; 16];
        theseus_host::random_bytes(&mut bytes)?;
        let operation = v2::OperationId::from_bytes(bytes);
        let request = v2::Request::Run {
            lease,
            operation,
            program: command.program.program_id(),
        };
        let op = NativeOperation {
            ownership_id: self.resource.resource().ownership_id.clone(),
            lease: lease.to_hex(),
            operation_id: operation.to_hex(),
            request_digest: v2::request_digest(&request)?.to_hex(),
            command: command.clone(),
            completion: Disposition::Unknown {
                reason: "published; completion unproved".into(),
            },
        };
        let saved = self
            .projector
            .publish_command(&self.resource, op)?
            .into_operation();
        self.session.submit(Action::Run {
            lease,
            operation,
            program: saved.command.program.program_id(),
        })?;
        let end = Instant::now() + Duration::from_secs(self.limits.run_seconds);
        let mut stdout = Vec::new();
        let mut stdout_end = 0u64;
        let mut stderr_end = 0u64;
        let mut stderr_contiguous = true;
        let mut complete = true;
        let mut captured_total = 0usize;
        let mut cancel_sent = false;
        let outcome = loop {
            if self.cancel.is_cancelled() && !cancel_sent {
                self.session.submit(Action::Cancel { lease, operation })?;
                cancel_sent = true;
            }
            ensure!(
                Instant::now() < end,
                "native Run completion unproved at deadline"
            );
            match self.session.poll()? {
                Poll::Event(Event::Output {
                    lease: l,
                    operation: o,
                    channel,
                    offset,
                    bytes,
                }) if l == lease && o == operation => {
                    let remaining =
                        (self.limits.output_bytes as usize).saturating_sub(captured_total);
                    let retained = remaining.min(bytes.as_slice().len());
                    captured_total += retained;
                    if retained < bytes.as_slice().len() {
                        complete = false;
                        stderr_contiguous = false;
                    }
                    if channel == v2::Channel::Stdout {
                        complete &= offset == stdout_end;
                        stdout_end = offset + bytes.as_slice().len() as u64;
                        stdout.extend_from_slice(&bytes.as_slice()[..retained]);
                    } else {
                        stderr_contiguous &= offset == stderr_end;
                        stderr_end = offset + bytes.as_slice().len() as u64;
                    }
                }
                Poll::Event(Event::Reply {
                    reply:
                        v2::Reply::Operation {
                            lease: l,
                            operation: o,
                            state: v2::OperationView::Finished { outcome },
                        },
                    ..
                }) if l == lease && o == operation => break outcome,
                Poll::Event(Event::Reply {
                    reply:
                        v2::Reply::Operation {
                            lease: l,
                            operation: o,
                            state: v2::OperationView::Accepted { .. } | v2::OperationView::Running,
                        },
                    ..
                }) if l == lease && o == operation => {
                    self.query_original(lease, operation)?;
                }
                Poll::Event(Event::Reply {
                    reply:
                        v2::Reply::Refused {
                            code: v2::RefusalCode::OwnershipPending,
                            ..
                        },
                    ..
                }) => {
                    self.query_original(lease, operation)?;
                }
                Poll::Event(Event::Reply {
                    reply:
                        v2::Reply::Operation {
                            state: v2::OperationView::Unknown { .. },
                            ..
                        },
                    ..
                }) => bail!("native Run settlement unknown"),
                Poll::Event(Event::Reply {
                    reply: v2::Reply::PrefixAuthorityRevoked { reason, .. },
                    ..
                }) => {
                    self.record_revocation(reason)?;
                    bail!("native prefix authority revoked; catalog effects permanently blocked")
                }
                Poll::Event(Event::ConnectionLost | Event::PeerClosed(_))
                | Poll::TransportSettled => {
                    bail!("native transport lost; query original operation")
                }
                _ => std::thread::yield_now(),
            }
        };
        let (status, truncated, total, stderr_total) = match outcome {
            v2::Outcome::RunCompleted {
                status,
                truncated,
                stdout_bytes,
                stderr_bytes,
                ..
            } => (Some(status.into()), truncated, stdout_bytes, stderr_bytes),
            _ => (None, true, 0, 0),
        };
        complete &= !truncated && stdout_end == total;
        let utf8 = std::str::from_utf8(&stdout).is_ok();
        let mut text = String::from_utf8_lossy(&stdout).into_owned();
        if text.len() > self.limits.output_bytes as usize {
            let mut end = self.limits.output_bytes as usize;
            while !text.is_char_boundary(end) {
                end -= 1
            }
            text.truncate(end);
            complete = false;
        }
        let evidence = NativeCommandEvidence {
            command,
            operation_id: saved.operation_id.clone(),
            request_digest: saved.request_digest,
            status,
            stdout: text,
            stdout_complete: complete,
            stdout_utf8: utf8,
            stderr_complete: !truncated && stderr_contiguous && stderr_end == stderr_total,
            stdout_log: None,
            stderr_log: None,
            failure: if total == 0 && truncated {
                Some("native Run did not complete normally".into())
            } else {
                None
            },
        };
        self.projector.record_operation_completion(
            &saved.ownership_id,
            &saved.lease,
            &saved.operation_id,
            Disposition::Complete {
                receipt: "original helper terminal operation reply".into(),
            },
        )?;
        Ok(evidence)
    }
    pub fn capture(&mut self) -> Result<Vec<NativeFileObservation>> {
        let lease = self.lease()?;
        let mut result = Vec::new();
        let leaves = self.recipe.recipe.leaves.clone();
        for leaf in [leaves.reader, leaves.app, leaves.data] {
            let id = self.session.submit(Action::InspectNativeFile {
                lease,
                path: v2::LeafName::new(&leaf)?,
            })?;
            let state = match self.wait_reply(id)? {
                v2::Reply::NativeFileInfo {
                    lease: l,
                    path,
                    length,
                    sha256,
                } if l == lease && path.as_str() == leaf => NativeFileState::Present {
                    length,
                    sha256: sha256.to_hex(),
                },
                v2::Reply::FileAbsent { lease: l, path } if l == lease && path.as_str() == leaf => {
                    NativeFileState::AbsentSettled
                }
                v2::Reply::PrefixAuthorityRevoked { reason, .. } => {
                    self.record_revocation(reason)?;
                    bail!("native prefix authority revoked")
                }
                _ => NativeFileState::Unknown {
                    reason: "inspection not settled".into(),
                },
            };
            result.push(NativeFileObservation {
                request_id: format!("{id:?}"),
                root_identity: self.root_identity(),
                leaf,
                state,
            });
        }
        Ok(result)
    }
    fn query_original(&mut self, lease: v2::LeaseId, operation: v2::OperationId) -> Result<()> {
        match self.session.submit(Action::Query { lease, operation }) {
            Ok(_) | Err(theseus_native_client::ClientError::Busy) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
    fn root_identity(&self) -> String {
        match &self.resource.resource().ownership {
            NativeOwnership::Intent { prepared } | NativeOwnership::Acquired { prepared, .. } => {
                format!("workspace_marker:{}", prepared.workspace_marker)
            }
            _ => "workspace_unproved".into(),
        }
    }
    fn record_revocation(&mut self, reason: v2::PrefixAuthorityFailure) -> Result<()> {
        self.forward = false;
        self.projector.revoke_catalog(
            &self.resource.resource().ownership_id,
            prefix_failure(reason),
        )?;
        let leaves = &self.recipe.recipe.leaves;
        let native_files = [&leaves.reader, &leaves.app, &leaves.data]
            .into_iter()
            .map(|leaf| NativeFileObservation {
                request_id: "prefix_authority_revoked".into(),
                root_identity: self.root_identity(),
                leaf: leaf.clone(),
                state: NativeFileState::Unknown {
                    reason: format!("prefix authority revoked: {reason:?}"),
                },
            })
            .collect();
        self.record_stage(NativeStage {
            name: "prefix_authority_revoked".into(),
            step: None,
            checkpoint: None,
            outcome: StageOutcome::Incomplete,
            commands: vec![],
            checks: vec![],
            native_files,
            failures: vec![format!("prefix authority revoked: {reason:?}")],
        })
    }
    fn record_stage(&mut self, stage: NativeStage) -> Result<()> {
        let owner = &self.resource.resource().ownership_id;
        let scenario = self
            .projector
            .catalog_scenarios_mut()?
            .iter_mut()
            .find(|s| &s.ownership_id == owner)
            .ok_or_else(|| anyhow::anyhow!("native report scenario missing"))?;
        scenario.stages.push(stage);
        self.projector.save_report()
    }
}
impl sealed::Sealed for NativeDriver<'_, '_> {}
impl LifecycleBackend for NativeDriver<'_, '_> {
    fn step(&mut self, step: StepName, name: &str) -> Result<()> {
        ensure!(
            !self.cancel.is_cancelled() || step == StepName::Uninstall,
            "native run cancelled before catalog effect"
        );
        if !self.forward && step != StepName::Uninstall {
            return Ok(());
        }
        let command = self
            .recipe
            .recipe
            .steps
            .get(&step)
            .ok_or_else(|| anyhow::anyhow!("missing native step"))?
            .clone();
        let evidence = self.execute(command)?;
        let pass = command_passes(&evidence);
        self.forward &= pass;
        self.record_stage(NativeStage {
            name: name.into(),
            step: Some(step),
            checkpoint: None,
            outcome: if pass {
                StageOutcome::Passed
            } else {
                StageOutcome::Failed
            },
            commands: vec![evidence],
            checks: vec![],
            native_files: vec![],
            failures: vec![],
        })
    }
    fn checkpoint(&mut self, checkpoint: Checkpoint, _configured: bool) -> Result<()> {
        if !self.forward && checkpoint != Checkpoint::Uninstalled {
            return Ok(());
        }
        let observations = self.capture()?;
        let absent = matches!(checkpoint, Checkpoint::Uninstalled);
        let mut pass = if absent {
            observations
                .iter()
                .all(|o| matches!(o.state, NativeFileState::AbsentSettled))
        } else {
            let artifact = if checkpoint == Checkpoint::BaseInstalled
                || (checkpoint == Checkpoint::Configured
                    && self.scenario_kind == Some(ScenarioKind::Upgrade))
            {
                &self.recipe.recipe.base
            } else {
                &self.recipe.recipe.target
            };
            observations.iter().find(|o|o.leaf==self.recipe.recipe.leaves.reader).is_some_and(|o|matches!(&o.state,NativeFileState::Present{length,sha256} if *length==artifact.length&&sha256==&artifact.sha256)) && observations.iter().all(|o|!matches!(o.state,NativeFileState::Unknown{..}))
        };
        if checkpoint == Checkpoint::Reinstalled && !self.references.is_empty() {
            pass &= observations.iter().all(|o| {
                self.references
                    .iter()
                    .any(|old| old.leaf == o.leaf && old.state == o.state)
            });
        }
        if checkpoint == Checkpoint::TargetInstalled && !self.configured.is_empty() {
            pass &= observations
                .iter()
                .find(|o| o.leaf == "data")
                .is_some_and(|o| {
                    self.configured
                        .iter()
                        .any(|old| old.leaf == "data" && old.state == o.state)
                });
        }
        if checkpoint == Checkpoint::Configured {
            self.configured = observations.clone();
        }
        let mut commands = Vec::new();
        let mut checks = Vec::new();
        for check in self
            .recipe
            .recipe
            .checks
            .iter()
            .filter(|c| c.checkpoint == checkpoint)
            .cloned()
            .collect::<Vec<_>>()
        {
            let evidence = self.execute(check.command)?;
            let ok = command_passes(&evidence);
            pass &= ok;
            checks.push(NativeCheckEvidence {
                id: check.id,
                checkpoint,
                outcome: if ok {
                    CheckOutcome::Passed
                } else {
                    CheckOutcome::Failed
                },
                detail: if ok {
                    "native command matched"
                } else {
                    "native command mismatch or incomplete output"
                }
                .into(),
                command_ids: vec![evidence.command.id.clone()],
            });
            commands.push(evidence);
        }
        self.forward &= pass;
        self.record_stage(NativeStage {
            name: checkpoint.to_string(),
            step: None,
            checkpoint: Some(checkpoint),
            outcome: if pass {
                StageOutcome::Passed
            } else {
                StageOutcome::Failed
            },
            commands,
            checks,
            native_files: observations,
            failures: vec![],
        })
    }
    fn before_reinstall(&mut self) -> Result<()> {
        self.references = self.capture()?;
        Ok(())
    }
}

fn prefix_failure(reason: v2::PrefixAuthorityFailure) -> PrefixAuthorityFailure {
    match reason {
        v2::PrefixAuthorityFailure::Missing => PrefixAuthorityFailure::Missing,
        v2::PrefixAuthorityFailure::Replaced => PrefixAuthorityFailure::Replaced,
        v2::PrefixAuthorityFailure::Renamed => PrefixAuthorityFailure::Renamed,
        v2::PrefixAuthorityFailure::MappingUnproved => PrefixAuthorityFailure::MappingUnproved,
    }
}

fn command_passes(evidence: &NativeCommandEvidence) -> bool {
    evidence.failure.is_none()
        && evidence.status.as_ref() == Some(&evidence.command.expected_status)
        && evidence
            .command
            .stdout_literal
            .as_ref()
            .is_none_or(|literal| {
                evidence.stdout_complete && evidence.stdout_utf8 && &evidence.stdout == literal
            })
}
#[cfg(test)]
mod evidence_tests {
    use super::*;
    fn evidence() -> NativeCommandEvidence {
        NativeCommandEvidence {
            command: CatalogCommand {
                id: "target_check".into(),
                program: CatalogProgram::CheckTarget,
                expected_status: NativeStatus::WindowsExit { code: 0xfedcba98 },
                stdout_literal: Some("target\n".into()),
            },
            operation_id: "01".repeat(16),
            request_digest: "02".repeat(32),
            status: Some(NativeStatus::WindowsExit { code: 0xfedcba98 }),
            stdout: "target\n".into(),
            stdout_complete: true,
            stdout_utf8: true,
            stderr_complete: true,
            stdout_log: None,
            stderr_log: None,
            failure: None,
        }
    }
    #[test]
    fn missing_truncated_nonutf8_and_cross_domain_output_never_pass_required_literal() {
        let valid = evidence();
        assert!(command_passes(&valid));
        let mut e = evidence();
        e.stdout_complete = false;
        assert!(!command_passes(&e));
        let mut e = evidence();
        e.stdout.clear();
        assert!(!command_passes(&e));
        let mut e = evidence();
        e.stdout_utf8 = false;
        assert!(!command_passes(&e));
        let mut e = evidence();
        e.status = Some(NativeStatus::PosixExit { code: 0x98 });
        assert!(!command_passes(&e));
        let mut e = evidence();
        e.status = None;
        assert!(!command_passes(&e));
    }
}
