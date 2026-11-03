//! Retained publication authority. Tokens can be minted only after a successful save.
use super::domain::{StoredJournal, StoredRun};
use crate::{native::model::*, report::ReportStore};
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

pub struct PublishedRelease {
    release: NativeRelease,
}
impl PublishedRelease {
    pub fn into_release(self) -> NativeRelease {
        self.release
    }
}

pub struct RunProjector<'a> {
    store: &'a ReportStore,
    report: StoredRun,
    journal: StoredJournal,
}
impl<'a> RunProjector<'a> {
    pub fn new(store: &'a ReportStore, report: StoredRun, journal: StoredJournal) -> Result<Self> {
        ensure!(
            matches!(
                (&report, &journal),
                (StoredRun::Docker(_), StoredJournal::Docker(_))
                    | (StoredRun::Native(_), StoredJournal::Native(_))
            ),
            "report/journal backend mismatch"
        );
        let (report_id, journal_id) = match (&report, &journal) {
            (StoredRun::Docker(r), StoredJournal::Docker(j)) => (&r.run_id, &j.run_id),
            (StoredRun::Native(r), StoredJournal::Native(j)) => (&r.run_id, &j.run_id),
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
        }
    }
    pub fn save_journal(&self) -> Result<()> {
        match &self.journal {
            StoredJournal::Docker(j) => self.store.save_journal(j),
            StoredJournal::Native(j) => {
                j.validate()?;
                self.store.atomic_json(JOURNAL_FILENAME, j)
            }
        }
    }
    fn native_journal(&mut self) -> Result<&mut NativeJournal> {
        match &mut self.journal {
            StoredJournal::Native(j) => Ok(j),
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
        j.launch_fingerprint = Some(fingerprint.to_owned());
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
        self.save_journal()?;
        Ok(PublishedCommand { operation })
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
