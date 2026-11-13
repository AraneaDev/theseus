//! Rehearsal service control and deferred cleanup authority.
use super::IntegrationDriver;
use crate::{
    engine::domain::StoredJournal,
    native::{integration::model::*, model::Disposition},
};
use anyhow::{Result, bail, ensure};
use std::time::{Duration, Instant};
use theseus_native_client::{Action, Event, Poll};
use theseus_native_protocol::v2;
impl IntegrationDriver<'_, '_, '_> {
    pub(super) fn control(&mut self, action: ServiceAction) -> Result<()> {
        self.core.reserve_operation()?;
        let lease = self.core.lease()?;
        let slot = self.service()?.resource.clone();
        let prior = self
            .last_observation
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("service control requires complete observed binding"))?
            .binding_digest
            .clone();
        let mut nonce = [0u8; 16];
        theseus_host::random_bytes(&mut nonce)?;
        let operation = v2::OperationId::from_bytes(nonce);
        let wire_action = match action {
            ServiceAction::Start => v2::ServiceAction::Start,
            ServiceAction::Stop => v2::ServiceAction::Stop,
        };
        let request = v2::Request::ServiceAction {
            lease,
            operation,
            slot: v2::ResourceSlot::new(slot.as_str())?,
            prior_binding_digest: v2::Digest::parse(&prior)?,
            action: wire_action,
        };
        let digest = v2::request_digest(&request)?.to_hex();
        let record = IntegrationActionRecord {
            ownership_id: self.core.resource.resource().ownership_id.clone(),
            lease: lease.to_hex(),
            slot: slot.clone(),
            operation: operation.to_hex(),
            request: IntegrationRequest::Service {
                lease: lease.to_hex(),
                operation: operation.to_hex(),
                slot,
                prior_binding_digest: prior.clone(),
                control: action,
            },
            request_digest: digest.clone(),
            prior_binding_digest: prior.clone(),
            broker_identity: None,
            completion: Disposition::Unknown {
                reason: "published; native action unproved".into(),
            },
            terminal: None,
        };
        let saved = self
            .core
            .projector
            .publish_integration_action(&self.core.resource, record)?
            .into_action();
        self.core.session.submit(Action::ServiceAction {
            lease,
            operation,
            slot: v2::ResourceSlot::new(saved.slot.as_str())?,
            prior_binding_digest: v2::Digest::parse(&prior)?,
            action: wire_action,
        })?;
        let deadline = Instant::now() + Duration::from_secs(30);
        let terminal = loop {
            ensure!(
                Instant::now() < deadline,
                "original service action remains unproved"
            );
            match self.core.session.poll()? {
                Poll::Event(Event::Reply {
                    reply:
                        v2::Reply::Operation {
                            lease: l,
                            operation: o,
                            state:
                                v2::OperationView::Finished {
                                    outcome: v2::Outcome::ServiceActionCompleted { outcome },
                                },
                        },
                    ..
                }) if l == lease && o == operation => break outcome,
                Poll::Event(Event::Reply {
                    reply: v2::Reply::Refused { code, .. },
                    ..
                }) => bail!("service control refused: {code:?}"),
                Poll::Event(Event::ConnectionLost | Event::PeerClosed(_))
                | Poll::TransportSettled => bail!("original service action transport lost"),
                _ => {}
            }
            // Query the same accepted operation; never resubmit the native effect.
            std::thread::sleep(Duration::from_millis(25));
            if let Ok(id) = self.core.session.submit(Action::Query { lease, operation })
                && let v2::Reply::Operation {
                    state:
                        v2::OperationView::Finished {
                            outcome: v2::Outcome::ServiceActionCompleted { outcome },
                        },
                    ..
                } = self.core.wait_reply(id)?
            {
                break outcome;
            }
        };
        let (completion, outcome) = match terminal {
            v2::ServiceOutcome::Complete { receipt } => (
                Disposition::Complete {
                    receipt: receipt.clone(),
                },
                IntegrationOutcome::Complete { receipt },
            ),
            v2::ServiceOutcome::Failed { reason } => (
                Disposition::Failed {
                    reason: reason.clone(),
                },
                IntegrationOutcome::Failed { reason },
            ),
            v2::ServiceOutcome::Unknown { reason } => (
                Disposition::Unknown {
                    reason: reason.clone(),
                },
                IntegrationOutcome::Unknown { reason },
            ),
        };
        self.core.projector.record_integration_completion(
            &lease.to_hex(),
            &operation.to_hex(),
            completion.clone(),
            outcome,
        )?;
        self.evidence_mut()?.services[0]
            .actions
            .push(ActionReference {
                operation: operation.to_hex(),
                request_digest: digest.clone(),
            });
        self.evidence_mut()?.dispositions.manager_action = completion.clone();
        self.core.projector.save_report()?;
        ensure!(
            matches!(completion, Disposition::Complete { .. }),
            "original service action failed or remains unknown"
        );
        self.pending_control_observation = Some(operation.to_hex());
        if action == ServiceAction::Start {
            self.activation_cause = ActivationCause::RehearsalStart;
            self.activation_action = Some(ActionReference {
                operation: operation.to_hex(),
                request_digest: digest,
            });
        }
        Ok(())
    }
    pub(super) fn inspect_deferred_cleanup_authority(&mut self) -> Result<()> {
        let owner = self.core.resource.resource();
        owner
            .catalog_authority
            .authorize(crate::native::model::CatalogProgram::Uninstall)?;
        let (revoked, owned_registration) = match self.core.projector.journal() {
            StoredJournal::NativeIntegration(journal) => {
                let records = journal
                    .integration_ownership
                    .iter()
                    .filter(|r| {
                        r.key.ownership_id == owner.ownership_id
                            && r.kind == IntegrationResourceKind::Service
                    })
                    .collect::<Vec<_>>();
                (
                    records.iter().any(|r| r.revocation.is_some()),
                    records.iter().any(|r| {
                        r.binding_history.iter().any(|b| {
                            matches!(
                                b.basis,
                                BindingBasis::ExclusiveOwnedRegistration
                                    | BindingBasis::LiveScmObject
                            )
                        })
                    }),
                )
            }
            _ => (true, false),
        };
        ensure!(
            !revoked,
            "service authority revoked; cleanup is provider-only"
        );
        let definition = self
            .evidence_mut()?
            .services
            .first()
            .and_then(|s| s.observations.last())
            .and_then(|o| o.binding.definition.clone());
        // No Running requirement and no rehearsal Stop: this is solely a fresh
        // read against the original manager owner before the retained SUT Run.
        self.inspect(IntegrationPhase::BeforeUninstall, definition)?;
        ensure!(
            self.last_observation
                .as_ref()
                .is_some_and(|o| o.complete
                    && (!o.registration_present || (owned_registration && o.configured))),
            "original service cleanup owner unproved; cleanup is provider-only"
        );
        Ok(())
    }
}
