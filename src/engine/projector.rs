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

mod core;
mod integration;
mod recovery;

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
