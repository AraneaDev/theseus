//! Provider, intent, command and release publication and their completions.
use super::*;

impl RunProjector<'_> {
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
