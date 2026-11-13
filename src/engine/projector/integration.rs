//! Integration resource, broker binding, action, completion and readiness publication.
use super::*;

impl RunProjector<'_> {
    pub fn publish_integration_resource(
        &mut self,
        record: IntegrationResourceRecord,
    ) -> Result<()> {
        let StoredJournal::NativeIntegration(j) = &mut self.journal else {
            bail!("integration publication requires schema2 journal")
        };
        ensure!(
            j.ownership
                .iter()
                .any(|owner| owner.ownership_id == record.key.ownership_id
                    && owner.scenario_id == record.key.scenario_id
                    && matches!(owner.ownership, NativeOwnership::Acquired { .. })),
            "integration resource lacks original acquired owner"
        );
        ensure!(
            record.manifest_digest == j.manifest_digest,
            "integration resource manifest mismatch"
        );
        record.key.slot.validate()?;
        record.disposition.validate()?;
        crate::native::model::digest(&record.current_binding_digest)?;
        if let Some(old) = j
            .integration_ownership
            .iter_mut()
            .find(|old| old.key == record.key)
        {
            ensure!(
                old.kind == record.kind
                    && old.manifest_digest == record.manifest_digest
                    && old.initial_binding == record.initial_binding,
                "integration publication replaced original resource"
            );
            ensure!(
                old.revocation.is_none() || old.revocation == record.revocation,
                "integration revocation is sticky"
            );
            ensure!(
                record.binding_history.starts_with(&old.binding_history),
                "integration binding history was replaced"
            );
            *old = record;
        } else {
            j.integration_ownership.push(record);
        }
        self.save_journal()
    }
    pub fn publish_broker_binding(
        &mut self,
        resource: &PublishedResource,
        record: BrokerBindingRecord,
    ) -> Result<PublishedBrokerBind> {
        let StoredJournal::NativeIntegration(j) = &mut self.journal else {
            bail!("broker binding requires schema2 journal")
        };
        ensure!(
            resource.resource.ownership_id == record.ownership_id
                && matches!(&resource.resource.ownership,NativeOwnership::Acquired {lease,..} if lease==&record.helper_lease),
            "broker bind requires original acquired helper lease"
        );
        ensure!(
            j.requirements
                .desktop
                .as_ref()
                .is_some_and(|d| d.resource == record.slot),
            "desktop broker slot undeclared"
        );
        ensure!(
            record.broker_lease.is_none()
                && record.publication_scope.is_none()
                && matches!(record.disposition, Disposition::Unknown { .. }),
            "new broker binding cannot claim an unobserved publication barrier"
        );
        ensure!(
            !j.broker_bindings
                .iter()
                .any(|b| b.ownership_id == record.ownership_id),
            "original broker bind already published; query it"
        );
        let original = record.original.clone();
        j.broker_bindings.push(record);
        self.save_journal()?;
        Ok(PublishedBrokerBind { original })
    }
    pub fn record_broker_binding(
        &mut self,
        receipt: &theseus_native_protocol::v2::broker::BrokerBindingReceipt,
    ) -> Result<()> {
        receipt.validate()?;
        let StoredJournal::NativeIntegration(j) = &mut self.journal else {
            bail!("broker binding requires schema2 journal")
        };
        let record = j
            .broker_bindings
            .iter_mut()
            .find(|r| r.original == receipt.original)
            .ok_or_else(|| anyhow::anyhow!("original broker binding unpublished"))?;
        ensure!(
            record
                .broker_lease
                .as_ref()
                .is_none_or(|l| l == &receipt.broker_lease)
                && record
                    .publication_scope
                    .is_none_or(|s| s == receipt.publication_scope),
            "original broker mapping changed"
        );
        record.broker_lease = Some(receipt.broker_lease.clone());
        record.publication_scope = Some(receipt.publication_scope);
        record.disposition = Disposition::Complete {
            receipt: receipt.request_digest.clone(),
        };
        self.save_journal()
    }
    pub fn publish_integration_action(
        &mut self,
        resource: &PublishedResource,
        action: IntegrationActionRecord,
    ) -> Result<PublishedIntegrationAction> {
        let StoredJournal::NativeIntegration(j) = &mut self.journal else {
            bail!("integration publication requires schema2 journal")
        };
        let owner = j
            .ownership
            .iter()
            .find(|o| o.ownership_id == action.ownership_id)
            .ok_or_else(|| anyhow::anyhow!("integration action lacks original owner"))?;
        ensure!(
            owner == &resource.resource
                && matches!(&owner.ownership, NativeOwnership::Acquired{lease,..} if lease == &action.lease),
            "integration action original lease mismatch"
        );
        let registration = j
            .integration_ownership
            .iter()
            .find(|r| r.key.ownership_id == action.ownership_id && r.key.slot == action.slot)
            .ok_or_else(|| anyhow::anyhow!("integration action lacks published resource"))?;
        ensure!(
            registration.revocation.is_none()
                && registration.current_binding_digest == action.prior_binding_digest,
            "integration binding revoked or changed"
        );
        let (lease, operation, slot, prior) = match &action.request {
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
            lease == &action.lease
                && operation == &action.operation
                && slot == &action.slot
                && prior == &action.prior_binding_digest,
            "integration request identity mismatch"
        );
        crate::native::model::hex(&action.operation, 32)?;
        crate::native::model::digest(&action.request_digest)?;
        ensure!(
            theseus_native_protocol::v2::request_digest(&action.request.wire()?)?.to_hex()
                == action.request_digest,
            "integration request digest mismatch"
        );
        if matches!(action.request, IntegrationRequest::Service { .. }) {
            ensure!(
                action.broker_identity.is_none(),
                "service action cannot borrow desktop broker authority"
            );
        }
        ensure!(
            matches!(action.completion, Disposition::Unknown { .. }) && action.terminal.is_none(),
            "new integration action requires unknown disposition"
        );
        ensure!(
            !j.integration_actions
                .iter()
                .any(|a| a.lease == action.lease && a.operation == action.operation)
                && !j
                    .operations
                    .iter()
                    .any(|o| o.lease == action.lease && o.operation_id == action.operation),
            "operation already published; query original identity instead"
        );
        j.integration_actions.push(action.clone());
        self.save_journal()?;
        Ok(PublishedIntegrationAction { action })
    }
    pub fn record_integration_completion(
        &mut self,
        lease: &str,
        operation: &str,
        completion: Disposition,
        terminal: IntegrationOutcome,
    ) -> Result<()> {
        completion.validate()?;
        let StoredJournal::NativeIntegration(j) = &mut self.journal else {
            bail!("integration completion requires schema2 journal")
        };
        let position = j
            .integration_actions
            .iter()
            .position(|a| a.lease == lease && a.operation == operation)
            .ok_or_else(|| anyhow::anyhow!("integration action not published"))?;
        validate_cached_terminal(j, &j.integration_actions[position], &terminal)?;
        let action = &mut j.integration_actions[position];
        action.completion = completion;
        action.terminal = Some(terminal);
        self.save_journal()
    }
    pub fn publish_readiness_probe(
        &mut self,
        record: crate::native::integration::model::ReadinessProbeRecord,
    ) -> Result<()> {
        let StoredJournal::NativeIntegration(j) = &mut self.journal else {
            bail!("readiness publication requires schema2 journal")
        };
        let challenge = &record.challenge;
        ensure!(j.ownership.iter().any(|owner| owner.ownership_id==challenge.ownership_id && matches!(&owner.ownership,NativeOwnership::Acquired{lease,..} if lease==&challenge.lease))
            || j.broker_bindings.iter().any(|b|b.ownership_id==challenge.ownership_id && b.broker_lease.as_ref()==Some(&challenge.lease) && matches!(b.disposition,Disposition::Complete {..})), "readiness challenge lacks original lease");
        ensure!(
            j.integration_ownership
                .iter()
                .any(|r| r.key.ownership_id == challenge.ownership_id
                    && r.key.slot == challenge.slot
                    && r.revocation.is_none()),
            "readiness challenge lacks unrevoked original resource"
        );
        ensure!(
            (1..=30000).contains(&challenge.deadline_ms)
                && matches!(record.completion, Disposition::Unknown { .. })
                && record.observation_ids.is_empty(),
            "new readiness probe must retain bounded unknown result"
        );
        ensure!(
            !j.readiness_probes
                .iter()
                .any(|p| p.challenge.ownership_id == challenge.ownership_id
                    && p.challenge.operation == challenge.operation
                    && p.challenge.nonce == challenge.nonce),
            "readiness challenge already published"
        );
        j.readiness_probes.push(record);
        self.save_journal()
    }
    pub fn record_readiness_probe_completion(
        &mut self,
        ownership_id: &str,
        operation: &str,
        nonce: &str,
        expectation_digest: &str,
        observation_ids: Vec<String>,
        completion: Disposition,
    ) -> Result<()> {
        completion.validate()?;
        let StoredJournal::NativeIntegration(j) = &mut self.journal else {
            bail!("readiness completion requires schema2 journal")
        };
        let probe = j
            .readiness_probes
            .iter_mut()
            .find(|p| {
                p.challenge.ownership_id == ownership_id
                    && p.challenge.operation == operation
                    && p.challenge.nonce == nonce
                    && p.expectation_digest == expectation_digest
            })
            .ok_or_else(|| anyhow::anyhow!("readiness challenge not published"))?;
        ensure!(
            !matches!(completion, Disposition::Complete { .. }) || !observation_ids.is_empty(),
            "readiness completion lacks observations"
        );
        ensure!(
            matches!(probe.completion, Disposition::Unknown { .. })
                || (probe.completion == completion && probe.observation_ids == observation_ids),
            "readiness terminal result changed"
        );
        probe.observation_ids = observation_ids;
        probe.completion = completion;
        self.save_journal()
    }
}
