//! Retained publication authority. Tokens can be minted only after a successful save.
use super::domain::{StoredJournal, StoredRun};
use crate::{
    native::{
        integration::model::{
            BrokerBindingRecord, IntegrationActionRecord, IntegrationOutcome, IntegrationRequest,
            IntegrationResourceRecord, NativeIntegrationJournal, OriginalQueryView,
        },
        model::*,
    },
    report::ReportStore,
};
use anyhow::{Result, bail, ensure};
use theseus_native_helper_store::PreparedAcquire;

/// A successful publication and its exact original provider identity.
pub struct PublishedProvider {
    invocation: ProviderInvocation,
}
impl PublishedProvider {
    pub fn into_invocation(self) -> ProviderInvocation {
        self.invocation
    }
}
/// Owns the original IntentStore publication receipt, never a reconstructed nonce.
pub struct PublishedIntent {
    ownership_id: String,
    prepared: PreparedAcquire,
}
impl PublishedIntent {
    pub fn ownership_id(&self) -> &str {
        &self.ownership_id
    }
    pub fn into_prepared(self) -> PreparedAcquire {
        self.prepared
    }
}
/// Borrow this retained authority through setup and teardown; do not reacquire it.
pub struct PublishedResource {
    resource: NativeResource,
}
impl PublishedResource {
    pub fn resource(&self) -> &NativeResource {
        &self.resource
    }
}
pub struct PublishedCommand {
    operation: NativeOperation,
}
impl PublishedCommand {
    pub fn into_operation(self) -> NativeOperation {
        self.operation
    }
}

/// A saved integration action is the sole authority to submit that exact request.
pub struct PublishedIntegrationAction {
    action: IntegrationActionRecord,
}
impl PublishedIntegrationAction {
    pub fn into_action(self) -> IntegrationActionRecord {
        self.action
    }
}

pub struct PublishedBrokerBind {
    original: theseus_native_protocol::v2::broker::BrokerBind,
}
impl PublishedBrokerBind {
    pub fn into_original(self) -> theseus_native_protocol::v2::broker::BrokerBind {
        self.original
    }
}
pub struct PublishedRelease {
    release: NativeRelease,
}
impl PublishedRelease {
    pub fn into_release(self) -> NativeRelease {
        self.release
    }
}

fn validate_cached_terminal(
    journal: &NativeIntegrationJournal,
    action: &IntegrationActionRecord,
    terminal: &IntegrationOutcome,
) -> Result<()> {
    ensure!(
        action.terminal.as_ref().is_none_or(|old| old == terminal),
        "integration terminal outcome changed"
    );
    for query in journal
        .integration_recoveries
        .iter()
        .filter(|r| r.ownership_id == action.ownership_id)
        .flat_map(|r| &r.queries)
        .filter(|q| {
            q.view == OriginalQueryView::CachedTerminal
                && q.original.operation == action.operation
                && q.original.request_digest == action.request_digest
        })
    {
        ensure!(
            &query.outcome == terminal,
            "original cached recovery terminal changed"
        );
    }
    Ok(())
}

pub struct RunProjector<'a> {
    store: &'a ReportStore,
    report: StoredRun,
    journal: StoredJournal,
}
/// Borrow only shared native fields. No compatibility journal is synthesized,
/// and saves still validate/write the actual versioned envelope.
pub(crate) struct NativeJournalFields<'a> {
    pub launch_fingerprint: &'a mut Option<String>,
    pub ownership: &'a mut Vec<NativeResource>,
    pub operations: &'a mut Vec<NativeOperation>,
    pub provider_calls: &'a mut Vec<NativeProviderCall>,
    pub releases: &'a mut Vec<NativeRelease>,
    pub recovery_attempts: &'a mut Vec<NativeRecoveryAttempt>,
}
impl<'a> RunProjector<'a> {
    pub fn new(store: &'a ReportStore, report: StoredRun, journal: StoredJournal) -> Result<Self> {
        ensure!(
            matches!(
                (&report, &journal),
                (StoredRun::Docker(_), StoredJournal::Docker(_))
                    | (StoredRun::Native(_), StoredJournal::Native(_))
                    | (
                        StoredRun::NativeIntegration(_),
                        StoredJournal::NativeIntegration(_)
                    )
            ),
            "report/journal backend mismatch"
        );
        let (report_id, journal_id) = match (&report, &journal) {
            (StoredRun::Docker(r), StoredJournal::Docker(j)) => (&r.run_id, &j.run_id),
            (StoredRun::Native(r), StoredJournal::Native(j)) => (&r.run_id, &j.run_id),
            (StoredRun::NativeIntegration(r), StoredJournal::NativeIntegration(j)) => {
                (&r.run_id, &j.run_id)
            }
            _ => unreachable!("backend match checked"),
        };
        ensure!(
            report_id == journal_id,
            "report/journal run identity mismatch"
        );
        let projector = Self {
            store,
            report,
            journal,
        };
        projector.save_journal()?;
        projector.save_report()?;
        Ok(projector)
    }
    pub fn report(&self) -> &StoredRun {
        &self.report
    }
    pub fn journal(&self) -> &StoredJournal {
        &self.journal
    }
    pub fn report_mut(&mut self) -> &mut StoredRun {
        &mut self.report
    }
    pub(crate) fn catalog_scenarios_mut(&mut self) -> Result<&mut Vec<NativeScenario>> {
        match &mut self.report {
            StoredRun::Native(report) => Ok(&mut report.scenarios),
            StoredRun::NativeIntegration(report) => Ok(&mut report.catalog.scenarios),
            _ => bail!("native catalog projection requires native report"),
        }
    }
    /// Lend only the envelope-free schema2 catalog and original core fields.
    /// The closure has no store, filename, schema or replacement envelope.
    pub(crate) fn with_catalog_evidence_mut<T>(
        &mut self,
        apply: impl FnOnce(
            &mut crate::native::integration::model::CatalogRunEvidence,
            NativeJournalFields<'_>,
        ) -> Result<T>,
    ) -> Result<T> {
        let (StoredRun::NativeIntegration(report), StoredJournal::NativeIntegration(journal)) =
            (&mut self.report, &mut self.journal)
        else {
            bail!("integration catalog seam requires selected schema2 envelopes")
        };
        apply(
            &mut report.catalog,
            NativeJournalFields {
                launch_fingerprint: &mut journal.launch_fingerprint,
                ownership: &mut journal.ownership,
                operations: &mut journal.operations,
                provider_calls: &mut journal.provider_calls,
                releases: &mut journal.releases,
                recovery_attempts: &mut journal.recovery_attempts,
            },
        )
    }
    pub fn into_parts(self) -> (StoredRun, StoredJournal) {
        (self.report, self.journal)
    }
    pub fn save_report(&self) -> Result<()> {
        match &self.report {
            StoredRun::Docker(r) => self.store.save(r),
            StoredRun::Native(r) => {
                r.validate()?;
                self.store.atomic_json(REPORT_FILENAME, r)
            }
            StoredRun::NativeIntegration(r) => self.store.save_native_integration_report(r),
        }
    }
    pub fn save_journal(&self) -> Result<()> {
        match &self.journal {
            StoredJournal::Docker(j) => self.store.save_journal(j),
            StoredJournal::Native(j) => {
                j.validate()?;
                self.store.atomic_json(JOURNAL_FILENAME, j)
            }
            StoredJournal::NativeIntegration(j) => self.store.save_native_integration_journal(j),
        }
    }
    fn native_journal(&mut self) -> Result<NativeJournalFields<'_>> {
        match &mut self.journal {
            StoredJournal::Native(j) => Ok(NativeJournalFields {
                launch_fingerprint: &mut j.launch_fingerprint,
                ownership: &mut j.ownership,
                operations: &mut j.operations,
                provider_calls: &mut j.provider_calls,
                releases: &mut j.releases,
                recovery_attempts: &mut j.recovery_attempts,
            }),
            StoredJournal::NativeIntegration(j) => Ok(NativeJournalFields {
                launch_fingerprint: &mut j.launch_fingerprint,
                ownership: &mut j.ownership,
                operations: &mut j.operations,
                provider_calls: &mut j.provider_calls,
                releases: &mut j.releases,
                recovery_attempts: &mut j.recovery_attempts,
            }),
            _ => bail!("native publication requires native journal"),
        }
    }
    pub fn bind_launch(&mut self, fingerprint: &str) -> Result<()> {
        crate::native::model::digest(fingerprint)?;
        let j = self.native_journal()?;
        ensure!(
            j.launch_fingerprint.is_none() && j.ownership.is_empty() && j.provider_calls.is_empty(),
            "launch already bound or effects started"
        );
        *j.launch_fingerprint = Some(fingerprint.to_owned());
        self.save_journal()
    }
    /// Retain ownership before even invoking the provider. Unknown replies keep this record.
    pub fn publish_provider(
        &mut self,
        resource: NativeResource,
        invocation: ProviderInvocation,
    ) -> Result<PublishedProvider> {
        resource.validate()?;
        invocation.validate()?;
        ensure!(
            resource == invocation.prior_resource,
            "provider prior resource mismatch"
        );
        ensure!(
            resource.ownership_id == invocation.ownership_id,
            "provider owner mismatch"
        );
        if invocation.action == ProviderAction::Prepare {
            match &resource.ownership {
                NativeOwnership::Preparing {
                    provider,
                    invocation_id,
                    receipt: None,
                } => ensure!(
                    provider == &invocation.provider && invocation_id == &invocation.invocation_id,
                    "provider invocation mismatch"
                ),
                _ => bail!("prepare requires original preparing ownership"),
            }
        }
        let j = self.native_journal()?;
        ensure!(
            j.launch_fingerprint.as_deref() == Some(invocation.launch_fingerprint.as_str()),
            "provider launch is not bound"
        );
        ensure!(
            !j.provider_calls
                .iter()
                .any(|p| p.invocation.provider == invocation.provider
                    && p.invocation.invocation_id == invocation.invocation_id),
            "provider invocation already published; query original identity instead"
        );
        match j
            .ownership
            .iter()
            .position(|r| r.ownership_id == resource.ownership_id)
        {
            Some(index) => {
                ensure!(
                    j.ownership[index] == resource,
                    "provider publication cannot replace original ownership"
                );
            }
            None => {
                ensure!(
                    invocation.action == ProviderAction::Prepare,
                    "disposition requires original owner"
                );
                j.ownership.push(resource);
            }
        }
        j.provider_calls.push(NativeProviderCall {
            invocation: invocation.clone(),
            completion: Disposition::Unknown {
                reason: "published; provider completion not yet proven".into(),
            },
            receipt: None,
        });
        self.save_journal()?;
        Ok(PublishedProvider { invocation })
    }
    pub fn publish_intent(
        &mut self,
        resource: NativeResource,
        prepared: PreparedAcquire,
    ) -> Result<PublishedIntent> {
        resource.validate()?;
        let NativeOwnership::Intent { prepared: identity } = &resource.ownership else {
            bail!("intent publication requires original prepared identity")
        };
        let original = prepared.record();
        ensure!(
            identity.intent == original.intent.to_hex()
                && identity.binding == original.binding.to_hex()
                && identity.manifest_digest == original.manifest_digest.to_hex()
                && identity.workspace_marker == original.workspace_marker.to_hex(),
            "intent differs from retained original receipt"
        );
        let scope = match prepared.commit_scope() {
            theseus_host::fs::CommitScope::FileAndDirectorySynced => "file_and_directory_synced",
            theseus_host::fs::CommitScope::FileFlushedAtomicReplace => {
                "file_flushed_atomic_replace"
            }
        };
        ensure!(
            identity.publication_scope == scope,
            "intent publication scope mismatch"
        );
        let id = resource.ownership_id.clone();
        let j = self.native_journal()?;
        let old = j
            .ownership
            .iter_mut()
            .find(|r| r.ownership_id == id)
            .ok_or_else(|| anyhow::anyhow!("intent has no original preparing owner"))?;
        ensure!(
            old.scenario_id == resource.scenario_id
                && old.catalog_authority == resource.catalog_authority
                && matches!(
                    old.ownership,
                    NativeOwnership::Preparing {
                        receipt: Some(_),
                        ..
                    }
                ),
            "intent requires validated provider preparation"
        );
        if let NativeOwnership::Preparing {
            receipt: Some(receipt),
            ..
        } = &old.ownership
        {
            ensure!(
                identity.binding == receipt.binding
                    && identity.manifest_digest == receipt.manifest_digest
                    && identity.workspace_marker == receipt.workspace_marker
                    && identity.reset_epoch == receipt.reset_epoch,
                "prepared intent differs from original provider baseline"
            );
        }
        *old = resource;
        self.save_journal()?;
        Ok(PublishedIntent {
            ownership_id: id,
            prepared,
        })
    }
    pub fn publish_acquired(&mut self, resource: NativeResource) -> Result<PublishedResource> {
        resource.validate()?;
        let NativeOwnership::Acquired { prepared, .. } = &resource.ownership else {
            bail!("acquired publication requires lease")
        };
        let j = self.native_journal()?;
        let old = j
            .ownership
            .iter_mut()
            .find(|r| r.ownership_id == resource.ownership_id)
            .ok_or_else(|| anyhow::anyhow!("lease has no original owner"))?;
        ensure!(
            old.scenario_id == resource.scenario_id
                && old.catalog_authority == resource.catalog_authority
                && matches!(&old.ownership,NativeOwnership::Intent{prepared:original} if original==prepared),
            "acquisition differs from original published intent"
        );
        *old = resource.clone();
        self.save_journal()?;
        Ok(PublishedResource { resource })
    }
    pub fn publish_command(
        &mut self,
        resource: &PublishedResource,
        operation: NativeOperation,
    ) -> Result<PublishedCommand> {
        operation.validate()?;
        let j = self.native_journal()?;
        let owner = j
            .ownership
            .iter()
            .find(|r| r.ownership_id == operation.ownership_id)
            .ok_or_else(|| anyhow::anyhow!("command lacks original owner"))?;
        ensure!(
            owner.ownership_id == resource.resource.ownership_id
                && owner.scenario_id == resource.resource.scenario_id,
            "command resource authority mismatch"
        );
        owner
            .catalog_authority
            .authorize(operation.command.program)?;
        ensure!(
            matches!(&owner.ownership,NativeOwnership::Acquired{lease,..} if lease==&operation.lease),
            "command lease mismatch"
        );
        ensure!(
            !j.operations
                .iter()
                .any(|op| op.lease == operation.lease && op.operation_id == operation.operation_id),
            "operation already published; query original identity instead"
        );
        ensure!(
            matches!(operation.completion, Disposition::Unknown { .. }),
            "new command must retain unknown disposition"
        );
        j.operations.push(operation.clone());
        if let StoredJournal::NativeIntegration(j) = &mut self.journal {
            // Link the original SUT install publication to the prior observed
            // vacancy. Preserve the immutable first snapshot and append history.
            if matches!(
                operation.command.program,
                CatalogProgram::InstallBase | CatalogProgram::InstallTarget
            ) {
                for resource in j
                    .integration_ownership
                    .iter_mut()
                    .filter(|r| r.key.ownership_id == operation.ownership_id)
                {
                    let prior = resource
                        .binding_history
                        .last()
                        .unwrap_or(&resource.initial_binding);
                    if prior.basis
                        == crate::native::integration::model::BindingBasis::VacantRegistration
                        && prior.original_action.is_none()
                    {
                        let mut linked = prior.clone();
                        linked.original_action = Some(operation.operation_id.clone());
                        resource.binding_history.push(linked);
                    }
                }
            }
        }
        self.save_journal()?;
        Ok(PublishedCommand { operation })
    }
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
    pub(crate) fn publish_integration_recovery(
        &mut self,
        record: crate::native::integration::model::IntegrationRecoveryRecord,
    ) -> Result<()> {
        let StoredJournal::NativeIntegration(j) = &mut self.journal else {
            bail!("integration recovery requires selected schema2 journal")
        };
        ensure!(
            j.ownership
                .iter()
                .any(|o| o.ownership_id == record.ownership_id),
            "integration recovery lacks original owner"
        );
        ensure!(
            !j.integration_recoveries
                .iter()
                .any(|r| r.recovery_id == record.recovery_id),
            "integration recovery already published"
        );
        let originals = j
            .integration_actions
            .iter()
            .filter(|a| a.ownership_id == record.ownership_id)
            .map(|a| crate::native::integration::model::ActionReference {
                operation: a.operation.clone(),
                request_digest: a.request_digest.clone(),
            })
            .collect::<Vec<_>>();
        ensure!(
            record.broker_effect_baselines
                == crate::native::integration::model::original_broker_effect_baselines(
                    j,
                    &record.ownership_id
                )?,
            "recovery desktop terminal baseline differs from original publication knowledge"
        );
        ensure!(
            record.original_actions == originals
                && record.queries.is_empty()
                && record.broker_observations.is_empty()
                && record.broker_release.is_none(),
            "integration recovery original action set changed"
        );
        j.integration_recoveries.push(record);
        self.save_journal()
    }
    pub(crate) fn record_integration_query(
        &mut self,
        recovery_id: &str,
        query: crate::native::integration::model::OriginalQueryOutcome,
    ) -> Result<()> {
        let StoredJournal::NativeIntegration(j) = &mut self.journal else {
            bail!("integration recovery requires selected schema2 journal")
        };
        let record = j
            .integration_recoveries
            .iter()
            .find(|r| r.recovery_id == recovery_id)
            .ok_or_else(|| anyhow::anyhow!("integration recovery not published"))?;
        ensure!(
            record.original_actions.contains(&query.original)
                && !record.queries.iter().any(|q| q.original == query.original),
            "integration recovery query identity changed or duplicated"
        );
        let action = j
            .integration_actions
            .iter()
            .find(|a| {
                a.ownership_id == record.ownership_id
                    && a.operation == query.original.operation
                    && a.request_digest == query.original.request_digest
            })
            .ok_or_else(|| {
                anyhow::anyhow!("integration recovery query lacks exact original action")
            })?;
        match query.view {
            OriginalQueryView::CachedTerminal => {
                validate_cached_terminal(j, action, &query.outcome)?;
                crate::native::integration::model::validate_original_query_broker_receipts(
                    j,
                    &record.ownership_id,
                    &query,
                )?;
            }
            OriginalQueryView::NonterminalUnknown => ensure!(
                matches!(query.outcome, IntegrationOutcome::Unknown { .. }),
                "nonterminal recovery query cannot claim a terminal outcome"
            ),
        }
        j.integration_recoveries
            .iter_mut()
            .find(|r| r.recovery_id == recovery_id)
            .expect("published recovery checked")
            .queries
            .push(query);
        self.save_journal()
    }
    pub(crate) fn record_broker_recovery_observation(
        &mut self,
        recovery_id: &str,
        observation: crate::native::integration::model::RecoveredBrokerObservation,
    ) -> Result<()> {
        use crate::native::integration::model::*;
        let StoredJournal::NativeIntegration(j) = &self.journal else {
            bail!("broker recovery requires schema2 journal")
        };
        let record = j
            .integration_recoveries
            .iter()
            .find(|r| r.recovery_id == recovery_id)
            .ok_or_else(|| anyhow::anyhow!("broker recovery not published"))?;
        let owner = record.ownership_id.clone();
        let action = j
            .integration_actions
            .iter()
            .find(|a| {
                a.ownership_id == owner
                    && a.operation == observation.original.operation
                    && a.request_digest == observation.original.request_digest
            })
            .ok_or_else(|| anyhow::anyhow!("broker state lacks original action"))?;
        let IntegrationRequest::Desktop { request, .. } = &action.request else {
            bail!("broker state borrowed service authority")
        };
        ensure!(
            request.identity()? == observation.identity,
            "broker recovery identity changed"
        );
        validate_original_broker_effect(action, &observation.state)?;
        if let Some(baseline) = record
            .broker_effect_baselines
            .iter()
            .find(|b| b.original == observation.original)
        {
            validate_broker_effect_terminal(baseline.terminal.as_ref(), &observation.state)?;
        }
        let StoredRun::NativeIntegration(report) = &self.report else {
            bail!("broker recovery requires original integration report")
        };
        validate_original_broker_report_progress(report, action, &observation.state)?;
        if observation.source == BrokerObservationSource::Cancel {
            ensure!(
                matches!(
                    request.action,
                    theseus_native_protocol::v2::broker::DesktopAction::Activate { .. }
                ),
                "Cancel may control only original activation"
            );
        }
        if let Some(previous) = j
            .integration_recoveries
            .iter()
            .rev()
            .filter(|r| r.ownership_id == owner)
            .flat_map(|r| r.broker_observations.iter().rev())
            .find(|o| o.original == observation.original)
        {
            validate_broker_progress(&previous.state, &observation.state)?;
        }
        if record
            .broker_observations
            .iter()
            .rev()
            .find(|o| o.original == observation.original)
            .is_some_and(|o| o.state == observation.state)
        {
            return Ok(());
        }
        ensure!(
            record.broker_observations.len() < 64 * record.original_actions.len().max(1),
            "broker recovery state budget exceeded"
        );
        let state = observation.state.clone();
        let StoredJournal::NativeIntegration(j) = &mut self.journal else {
            unreachable!()
        };
        j.integration_recoveries
            .iter_mut()
            .find(|r| r.recovery_id == recovery_id)
            .unwrap()
            .broker_observations
            .push(observation);
        self.save_journal()?;
        if let StoredRun::NativeIntegration(report) = &mut self.report
            && let Some(evidence) = report
                .integrations
                .iter_mut()
                .find(|s| s.ownership_id == owner)
        {
            use theseus_native_protocol::v2::broker::*;
            evidence.dispositions.primary = if matches!(
                state.primary,
                BrokerPrimary::Exited | BrokerPrimary::NotCreated
            ) {
                Disposition::Complete {receipt:"original broker observed original primary exited or never created during recovery".into()}
            } else {
                Disposition::Unknown {
                    reason: "original desktop primary exit remains unproved".into(),
                }
            };
            evidence.dispositions.descendant_scope = if matches!(
                state.descendants,
                BrokerDescendants::Joined | BrokerDescendants::NotCreated
            ) && state.cleanup_complete
            {
                Disposition::Complete {receipt:"original broker observed original scope joined or never created and IPC settled during recovery".into()}
            } else {
                Disposition::Unknown {
                    reason: "original desktop scope/IPC settlement remains unproved".into(),
                }
            };
            if !settled_original_broker_state(&state) {
                evidence.dispositions.broker = Disposition::Unknown {
                    reason: "original broker lifetime, effect or quarantine remains unresolved"
                        .into(),
                };
            }
        }
        self.save_report()
    }
    pub(crate) fn record_broker_recovery_release(
        &mut self,
        recovery_id: &str,
        release: crate::native::integration::model::RecoveredBrokerRelease,
    ) -> Result<()> {
        use crate::native::integration::model::*;
        let StoredJournal::NativeIntegration(j) = &self.journal else {
            bail!("broker release recovery requires schema2 journal")
        };
        let record = j
            .integration_recoveries
            .iter()
            .find(|r| r.recovery_id == recovery_id)
            .ok_or_else(|| anyhow::anyhow!("broker recovery not published"))?;
        let owner = record.ownership_id.clone();
        let binding = j
            .broker_bindings
            .iter()
            .find(|b| {
                b.ownership_id == owner
                    && b.helper_lease == release.helper_lease
                    && b.broker_lease.as_ref() == Some(&release.broker_lease)
            })
            .ok_or_else(|| anyhow::anyhow!("release lacks original broker mapping"))?;
        ensure!(j.ownership.iter().any(|o|o.ownership_id==owner && matches!(&o.ownership,NativeOwnership::Acquired {prepared,lease} if lease==&release.helper_lease && prepared.intent==release.helper_intent)),"release changed original helper intent");
        if let Some(old) = &record.broker_release {
            ensure!(
                old.helper_lease == release.helper_lease
                    && old.broker_lease == release.broker_lease
                    && old.helper_intent == release.helper_intent,
                "original broker release receipt changed"
            );
            return Ok(());
        }
        let settled = original_desktop_scope_settled(j, &owner, &binding.slot);
        let StoredJournal::NativeIntegration(j) = &mut self.journal else {
            unreachable!()
        };
        let record = j
            .integration_recoveries
            .iter_mut()
            .find(|r| r.recovery_id == recovery_id)
            .unwrap();
        record.broker_release = Some(release);
        if settled {
            record.broker=Disposition::Complete {receipt:"original helper AwaitingExternalReset proves its prior original broker release barrier".into()};
        }
        let broker = record.broker.clone();
        self.save_journal()?;
        if let StoredRun::NativeIntegration(report) = &mut self.report
            && let Some(evidence) = report
                .integrations
                .iter_mut()
                .find(|s| s.ownership_id == owner)
        {
            evidence.dispositions.broker = broker;
        }
        self.save_report()
    }
    pub(crate) fn finish_integration_recovery(
        &mut self,
        recovery_id: &str,
        local: Disposition,
        broker: Disposition,
        provider: Disposition,
    ) -> Result<()> {
        local.validate()?;
        broker.validate()?;
        provider.validate()?;
        let StoredJournal::NativeIntegration(j) = &mut self.journal else {
            bail!("integration recovery requires selected schema2 journal")
        };
        let record = j
            .integration_recoveries
            .iter_mut()
            .find(|r| r.recovery_id == recovery_id)
            .ok_or_else(|| anyhow::anyhow!("integration recovery not published"))?;
        record.local = local;
        record.broker = broker;
        record.provider = provider;
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
    pub fn revoke_catalog(
        &mut self,
        ownership_id: &str,
        failure: PrefixAuthorityFailure,
    ) -> Result<()> {
        let j = self.native_journal()?;
        let resource = j
            .ownership
            .iter_mut()
            .find(|r| r.ownership_id == ownership_id)
            .ok_or_else(|| anyhow::anyhow!("unknown native owner"))?;
        resource.catalog_authority.revoke(failure);
        self.save_journal()
    }
    /// Record the original receipt; a successful provider reply is not a launch authority.
    pub fn record_provider_receipt(
        &mut self,
        invocation: &ProviderInvocation,
        receipt: ProviderReceipt,
    ) -> Result<()> {
        receipt.validate()?;
        ensure!(
            receipt.provider == invocation.provider
                && receipt.invocation_id == invocation.invocation_id
                && receipt.invocation_digest == invocation.digest()?,
            "provider receipt mismatch"
        );
        let j = self.native_journal()?;
        let call = j
            .provider_calls
            .iter_mut()
            .find(|p| &p.invocation == invocation)
            .ok_or_else(|| anyhow::anyhow!("provider invocation not published"))?;
        ensure!(call.receipt.is_none(), "provider receipt already recorded");
        call.completion = Disposition::Complete {
            receipt: receipt.receipt.clone(),
        };
        call.receipt = Some(receipt.clone());
        if invocation.action == ProviderAction::Prepare {
            let resource = j
                .ownership
                .iter_mut()
                .find(|r| r.ownership_id == invocation.ownership_id)
                .ok_or_else(|| anyhow::anyhow!("provider owner missing"))?;
            let NativeOwnership::Preparing { receipt: saved, .. } = &mut resource.ownership else {
                bail!("provider receipt phase mismatch")
            };
            *saved = Some(receipt);
        }
        self.save_journal()
    }
    pub fn record_operation_completion(
        &mut self,
        ownership_id: &str,
        lease: &str,
        operation_id: &str,
        completion: Disposition,
    ) -> Result<()> {
        completion.validate()?;
        let j = self.native_journal()?;
        let operation = j
            .operations
            .iter_mut()
            .find(|op| {
                op.ownership_id == ownership_id
                    && op.lease == lease
                    && op.operation_id == operation_id
            })
            .ok_or_else(|| anyhow::anyhow!("completion lacks original published operation"))?;
        ensure!(
            matches!(operation.completion, Disposition::Unknown { .. }),
            "operation already has terminal disposition"
        );
        operation.completion = completion;
        self.save_journal()
    }
    /// Release uses retained lease authority and remains available after prefix revocation.
    pub fn publish_release(&mut self, resource: &PublishedResource) -> Result<PublishedRelease> {
        let j = self.native_journal()?;
        let owner = j
            .ownership
            .iter()
            .find(|r| r.ownership_id == resource.resource.ownership_id)
            .ok_or_else(|| anyhow::anyhow!("release lacks original owner"))?;
        ensure!(
            owner.scenario_id == resource.resource.scenario_id
                && owner.ownership == resource.resource.ownership,
            "release resource identity mismatch"
        );
        let NativeOwnership::Acquired { lease, .. } = &owner.ownership else {
            bail!("release requires original acquired lease")
        };
        ensure!(
            !j.releases
                .iter()
                .any(|r| r.ownership_id == owner.ownership_id && &r.lease == lease),
            "release already published; query original lease disposition"
        );
        let release = NativeRelease {
            ownership_id: owner.ownership_id.clone(),
            lease: lease.clone(),
            completion: Disposition::Unknown {
                reason: "published; release completion not yet proven".into(),
            },
        };
        j.releases.push(release.clone());
        self.save_journal()?;
        Ok(PublishedRelease { release })
    }
    /// A query-only recovery may learn the lease of its exact original intent.
    /// This publishes knowledge only, never a command-capable PublishedResource.
    pub(crate) fn record_recovered_lease(&mut self, ownership_id: &str, lease: &str) -> Result<()> {
        theseus_native_protocol::v2::LeaseId::parse(lease)?;
        let journal = self.native_journal()?;
        let owner = journal
            .ownership
            .iter_mut()
            .find(|owner| owner.ownership_id == ownership_id)
            .ok_or_else(|| anyhow::anyhow!("recovered owner missing"))?;
        match &owner.ownership {
            NativeOwnership::Intent { prepared } => {
                owner.ownership = NativeOwnership::Acquired {
                    prepared: prepared.clone(),
                    lease: lease.to_owned(),
                }
            }
            NativeOwnership::Acquired {
                lease: original, ..
            } => ensure!(original == lease, "recovered lease differs from original"),
            _ => bail!("recovered lease lacks original intent"),
        }
        self.save_journal()
    }
    /// A fresh query proved the original lease Held. Republish only that same
    /// release identity; no new command/Acquire capability can be constructed.
    pub(crate) fn publish_recovery_release(
        &mut self,
        ownership_id: &str,
    ) -> Result<PublishedRelease> {
        let journal = self.native_journal()?;
        let owner = journal
            .ownership
            .iter()
            .find(|owner| owner.ownership_id == ownership_id)
            .ok_or_else(|| anyhow::anyhow!("recovery owner missing"))?;
        let NativeOwnership::Acquired { lease, .. } = &owner.ownership else {
            bail!("recovery release requires original lease")
        };
        let release = if let Some(original) = journal
            .releases
            .iter()
            .find(|release| release.ownership_id == ownership_id && &release.lease == lease)
        {
            ensure!(
                matches!(original.completion, Disposition::Unknown { .. }),
                "terminal release conflicts with Held query"
            );
            original.clone()
        } else {
            let release = NativeRelease {
                ownership_id: ownership_id.to_owned(),
                lease: lease.clone(),
                completion: Disposition::Unknown {
                    reason: "recovery release published; settlement unproved".into(),
                },
            };
            journal.releases.push(release.clone());
            release
        };
        self.save_journal()?;
        Ok(PublishedRelease { release })
    }
    pub(crate) fn record_recovery_attempt(&mut self, attempt: NativeRecoveryAttempt) -> Result<()> {
        let journal = self.native_journal()?;
        ensure!(
            !journal
                .recovery_attempts
                .iter()
                .any(|old| old.recovery_id == attempt.recovery_id),
            "recovery attempt already published"
        );
        journal.recovery_attempts.push(attempt);
        self.save_journal()
    }
    pub(crate) fn finish_recovery_attempt(
        &mut self,
        recovery_id: &str,
        local: Disposition,
        remote: Disposition,
    ) -> Result<()> {
        let attempt = self
            .native_journal()?
            .recovery_attempts
            .iter_mut()
            .find(|attempt| attempt.recovery_id == recovery_id)
            .ok_or_else(|| anyhow::anyhow!("recovery attempt missing"))?;
        attempt.local = local;
        attempt.remote = remote;
        self.save_journal()
    }
    pub fn record_release_completion(
        &mut self,
        ownership_id: &str,
        lease: &str,
        completion: Disposition,
    ) -> Result<()> {
        completion.validate()?;
        let j = self.native_journal()?;
        let release = j
            .releases
            .iter_mut()
            .find(|r| r.ownership_id == ownership_id && r.lease == lease)
            .ok_or_else(|| anyhow::anyhow!("completion lacks original published release"))?;
        ensure!(
            matches!(release.completion, Disposition::Unknown { .. }),
            "release already has terminal disposition"
        );
        release.completion = completion;
        self.save_journal()
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::{
        model::ScenarioKind,
        native::integration::{ValidatedIntegrationRecipe, model::*},
    };
    fn setup(
        store: &ReportStore,
    ) -> (
        RunProjector<'_>,
        PublishedResource,
        IntegrationResourceRecord,
    ) {
        let recipe = ValidatedIntegrationRecipe::decode(
            include_bytes!("../../examples/native/service-recipe.example.json"),
            "recipe.json".into(),
        )
        .unwrap();
        let report =
            NativeIntegrationReport::new("run".into(), &recipe, vec![ScenarioKind::Clean]).unwrap();
        let mut journal =
            NativeIntegrationJournal::new("run".into(), &recipe, vec![ScenarioKind::Clean])
                .unwrap();
        journal.launch_fingerprint = Some("01".repeat(32));
        let resource = NativeResource {
            scenario_id: "clean".into(),
            ownership_id: "owner".into(),
            catalog_authority: NativeCatalogAuthority::Valid,
            ownership: NativeOwnership::Acquired {
                prepared: NativeIntent {
                    intent: "01".repeat(16),
                    binding: "02".repeat(16),
                    manifest_digest: recipe.full_manifest_digest().unwrap(),
                    workspace_marker: "03".repeat(16),
                    reset_epoch: "04".repeat(32),
                    publication_scope: "file_and_directory_synced".into(),
                },
                lease: "05".repeat(16),
            },
        };
        journal.ownership.push(resource.clone());
        let binding = ResourceBindingSnapshot {
            digest: "06".repeat(32),
            basis: BindingBasis::VacantRegistration,
            evidence_ids: vec!["absence-observation".into()],
            definition: None,
            artifact_commitment: recipe.core.artifact_commitment().unwrap().to_hex(),
            namespace_or_session_receipt_digest: "07".repeat(32),
            original_action: None,
        };
        let record = IntegrationResourceRecord {
            key: IntegrationResourceKey {
                scenario_id: "clean".into(),
                ownership_id: "owner".into(),
                slot: ResourceSlot::new("service").unwrap(),
            },
            kind: IntegrationResourceKind::Service,
            manifest_digest: recipe.full_manifest_digest().unwrap(),
            initial_binding: binding.clone(),
            binding_history: vec![binding.clone()],
            current_binding_digest: binding.digest,
            revocation: None,
            disposition: Disposition::Unknown {
                reason: "registration pending".into(),
            },
        };
        (
            RunProjector::new(
                store,
                StoredRun::NativeIntegration(report),
                StoredJournal::NativeIntegration(journal),
            )
            .unwrap(),
            PublishedResource { resource },
            record,
        )
    }
    fn action(
        resource: &PublishedResource,
        registration: &IntegrationResourceRecord,
    ) -> IntegrationActionRecord {
        let NativeOwnership::Acquired { lease, .. } = &resource.resource.ownership else {
            unreachable!()
        };
        let request = IntegrationRequest::Service {
            lease: lease.clone(),
            operation: "08".repeat(16),
            slot: registration.key.slot.clone(),
            prior_binding_digest: registration.current_binding_digest.clone(),
            control: ServiceAction::Start,
        };
        IntegrationActionRecord {
            ownership_id: resource.resource.ownership_id.clone(),
            lease: lease.clone(),
            slot: registration.key.slot.clone(),
            operation: "08".repeat(16),
            request_digest: theseus_native_protocol::v2::request_digest(&request.wire().unwrap())
                .unwrap()
                .to_hex(),
            request,
            prior_binding_digest: registration.current_binding_digest.clone(),
            broker_identity: None,
            completion: Disposition::Unknown {
                reason: "action pending".into(),
            },
            terminal: None,
        }
    }
    #[test]
    fn action_requires_saved_original_binding_and_cannot_change_digest_or_replay() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::report::create(&root.path().join("run")).unwrap();
        let (mut p, owner, registration) = setup(&store);
        let command = action(&owner, &registration);
        assert!(
            p.publish_integration_action(&owner, command.clone())
                .is_err()
        );
        p.publish_integration_resource(registration.clone())
            .unwrap();
        let mut wrong = command.clone();
        wrong.request_digest = "09".repeat(32);
        assert!(p.publish_integration_action(&owner, wrong).is_err());
        let saved = p
            .publish_integration_action(&owner, command.clone())
            .unwrap()
            .into_action();
        assert_eq!(saved, command);
        assert!(p.publish_integration_action(&owner, command).is_err());
        let StoredJournal::NativeIntegration(journal) = store.load_selected_journal().unwrap()
        else {
            unreachable!()
        };
        assert_eq!(journal.integration_actions.len(), 1);
    }
    #[test]
    fn save_failure_mints_no_action_authority_and_revocation_is_sticky() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("run");
        let store = crate::report::create(&path).unwrap();
        let (mut p, owner, mut registration) = setup(&store);
        p.publish_integration_resource(registration.clone())
            .unwrap();
        registration.revocation = Some(AuthorityRevocation::WrongObject);
        p.publish_integration_resource(registration.clone())
            .unwrap();
        registration.revocation = None;
        assert!(
            p.publish_integration_resource(registration.clone())
                .is_err()
        );
        assert!(
            p.publish_integration_action(&owner, action(&owner, &registration))
                .is_err()
        );
        let root2 = tempfile::tempdir().unwrap();
        let path2 = root2.path().join("run");
        let store2 = crate::report::create(&path2).unwrap();
        let (mut p2, owner2, registration2) = setup(&store2);
        p2.publish_integration_resource(registration2.clone())
            .unwrap();
        std::fs::rename(
            path2.join(JOURNAL_FILENAME),
            path2.join("retained-journal.json"),
        )
        .unwrap();
        std::fs::create_dir(path2.join(JOURNAL_FILENAME)).unwrap();
        assert!(
            p2.publish_integration_action(&owner2, action(&owner2, &registration2))
                .is_err(),
            "failed publication must never yield submission token"
        );
    }
}
