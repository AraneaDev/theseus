//! Pollable owned local transport. Remote settlement is deliberately separate.
use super::launch::{LaunchAuthority, LaunchPreparation};
use std::time::{Duration, Instant};
use theseus_native_client::{
    Action, Client, ClientError, ClientMode, Event, Poll, RequestId, StartFailure,
};
pub struct NativeSession {
    client: Client,
    preparation: LaunchPreparation,
    prepared_intent: Option<theseus_native_helper_store::PreparedAcquire>,
    linux_gio_admission: Option<QualifiedLinuxGioAdmission>,
}
/// Created only by an authenticated helper session matching its private
/// provider bootstrap and the original owned Prepare completion.
pub struct QualifiedLinuxGioAdmission {
    reference: theseus_native_protocol::v2::LinuxGioQualificationRef,
    manifest_digest: theseus_native_protocol::v2::Digest,
}
impl QualifiedLinuxGioAdmission {
    pub fn reference(&self) -> &theseus_native_protocol::v2::LinuxGioQualificationRef {
        &self.reference
    }
    pub fn manifest_digest(&self) -> theseus_native_protocol::v2::Digest {
        self.manifest_digest
    }
}
fn validate_linux_admission_binding(
    proof: Option<&QualifiedLinuxGioAdmission>,
    manifest_digest: theseus_native_protocol::v2::Digest,
    reference: Option<&theseus_native_protocol::v2::LinuxGioQualificationRef>,
) -> anyhow::Result<()> {
    let proof = proof.ok_or_else(|| {
        anyhow::anyhow!(
            "Linux native activation requires original authenticated qualification admission"
        )
    })?;
    anyhow::ensure!(
        proof.manifest_digest == manifest_digest && reference == Some(&proof.reference),
        "Linux qualification differs from original admitted manifest or broker reference"
    );
    Ok(())
}
impl NativeSession {
    /// Retain authority from this original ready helper and its owned Prepare;
    /// a serialized reference or a new session cannot populate this field.
    pub(crate) fn admit_linux_gio_qualification(
        &mut self,
        launch: &LaunchAuthority,
        ownership_id: &str,
    ) -> anyhow::Result<()> {
        let proof = self.validate_linux_gio_qualification(launch, ownership_id)?;
        self.linux_gio_admission = Some(proof);
        Ok(())
    }
    pub(crate) fn require_linux_gio_admission(
        &self,
        manifest: &theseus_native_protocol::v2::IntegrationManifest,
        header: &theseus_native_protocol::v2::broker::IntegrationBrokerHeader,
    ) -> anyhow::Result<()> {
        use theseus_native_protocol::v2;
        if matches!(
            manifest.desktop_native,
            Some(v2::NativeDesktopDefinition::LinuxGio { .. })
        ) {
            let full_m = v2::integration_manifest_digest(
                v2::Digest::parse(&manifest.core_manifest_digest)?,
                manifest.digest()?,
            );
            anyhow::ensure!(
                header.manifest_digest == full_m.to_hex(),
                "Linux broker manifest changed after authenticated admission"
            );
            validate_linux_admission_binding(
                self.linux_gio_admission.as_ref(),
                full_m,
                header.linux_gio_qualification.as_ref(),
            )?;
        }
        Ok(())
    }
    pub fn validate_linux_gio_qualification(
        &self,
        launch: &LaunchAuthority,
        ownership_id: &str,
    ) -> anyhow::Result<QualifiedLinuxGioAdmission> {
        self.validate_integration_context(launch, ownership_id)?;
        let qualified = launch.qualified_linux_gio_prepare().ok_or_else(|| {
            anyhow::anyhow!("Linux GIO requires original owned provider qualification")
        })?;
        let context = launch.integration_context().expect("validated context");
        qualified.validate_context(context)?;
        let description = self
            .integration_description()
            .expect("validated description");
        let header = context
            .desktop_broker_header
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("qualified broker header missing"))?;
        anyhow::ensure!(
            matches!(
                description.manifest.desktop_native,
                Some(theseus_native_protocol::v2::NativeDesktopDefinition::LinuxGio { .. })
            ) && context.linux_gio_qualification.as_ref() == Some(qualified.reference())
                && header.linux_gio_qualification.as_ref() == Some(qualified.reference()),
            "helper qualification differs from original provider and broker authority"
        );
        Ok(QualifiedLinuxGioAdmission {
            reference: qualified.reference().clone(),
            manifest_digest: launch.binding().manifest_digest,
        })
    }
    pub fn integration_description(
        &self,
    ) -> Option<&theseus_native_protocol::v2::IntegrationDescription> {
        self.client.integration_description()
    }
    /// Bind provider-reported per-instance manager context to the observed helper
    /// before any acquisition or integration action.
    pub fn validate_integration_context(
        &self,
        launch: &LaunchAuthority,
        ownership_id: &str,
    ) -> anyhow::Result<()> {
        let context = launch
            .integration_context()
            .ok_or_else(|| anyhow::anyhow!("original integration context missing"))?;
        let description = self
            .client
            .integration_description()
            .ok_or_else(|| anyhow::anyhow!("integration description not observed"))?;
        description
            .manifest
            .validate_context(context, launch.binding().os)?;
        let (core, integration) = launch
            .integration_components()
            .ok_or_else(|| anyhow::anyhow!("integration ceiling missing"))?;
        anyhow::ensure!(
            context.ownership_id == ownership_id
                && description.ownership_id == ownership_id
                && description.context_digest == context.digest()?
                && description.namespace_receipt_digest == context.authority_receipt_digest()?
                && description.core_manifest_digest == core
                && description.integration_manifest_digest == integration
                && launch.integration_service_authority()
                    == description.manifest.service_authority.as_ref()
                && launch.integration_desktop_authority().is_some()
                    == description.manifest.desktop.is_some()
                && match (
                    launch.integration_desktop_authority(),
                    context.desktop_broker_header.as_ref()
                ) {
                    (Some(ceiling), Some(header)) => ceiling.validate_header(header).is_ok(),
                    (None, None) => true,
                    _ => false,
                },
            "helper integration context differs from original provider ownership"
        );
        Ok(())
    }
    /// Refuse recipe artifact facts that differ from the authenticated catalog
    /// before acquiring a lease or executing any lifecycle commands.
    pub fn validate_artifacts(
        &self,
        recipe: &super::recipe::ValidatedNativeRecipe,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.client.artifact_commitment() == Some(recipe.artifact_commitment()?),
            "recipe artifacts differ from native helper catalog commitment"
        );
        Ok(())
    }
    pub fn start(launch: &LaunchAuthority, mode: ClientMode) -> Result<Self, NativeStartFailure> {
        Self::start_authorized(launch, mode)
    }
    pub fn start_authorized(
        launch: &LaunchAuthority,
        mode: ClientMode,
    ) -> Result<Self, NativeStartFailure> {
        Self::start_prepared(launch, mode, launch.prepare())
    }
    pub fn start_prepared(
        launch: &LaunchAuthority,
        mode: ClientMode,
        mut preparation: LaunchPreparation,
    ) -> Result<Self, NativeStartFailure> {
        if preparation.launch_fingerprint() != launch.view().fingerprint
            && preparation.error.is_none()
        {
            preparation.error = Some(anyhow::anyhow!(
                "prepared transport belongs to another immutable launch ceiling"
            ));
        }
        if preparation.error().is_some() {
            return Err(NativeStartFailure {
                preparation: Box::new(preparation),
                client: None,
            });
        }
        let command = preparation
            .command
            .as_ref()
            .expect("successful preparation command")
            .clone();
        match Client::start_with(command, launch.expected(), mode) {
            Ok(client) => Ok(Self {
                client,
                preparation,
                prepared_intent: None,
                linux_gio_admission: None,
            }),
            Err(client) => Err(NativeStartFailure {
                preparation: Box::new(preparation),
                client: Some(client),
            }),
        }
    }
    pub fn submit(&mut self, action: Action<'_>) -> Result<RequestId, ClientError> {
        self.client.submit(action)
    }
    pub fn poll(&mut self) -> Result<Poll, ClientError> {
        self.client.poll()
    }
    pub fn wait_ready(&mut self) -> Result<(), ClientError> {
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            if Instant::now() >= end {
                self.client.request_stop();
                return Err(ClientError::PreflightTimeout);
            }
            match self.client.poll()? {
                Poll::Event(Event::Ready) => return Ok(()),
                Poll::Event(Event::ConnectionLost | Event::PeerClosed(_))
                | Poll::TransportSettled => return Err(ClientError::RecoveryRequired),
                _ => std::thread::yield_now(),
            }
        }
    }
    pub fn request_stop(&mut self) {
        self.client.request_stop()
    }
    pub fn poll_settle(&mut self) -> Result<bool, ClientError> {
        if !self.client.poll_settle()? {
            return Ok(false);
        }
        self.preparation
            .poll_settle()
            .map_err(|_| ClientError::Native)
    }
    pub fn preparation(&self) -> &LaunchPreparation {
        &self.preparation
    }
    pub fn is_settled(&self) -> bool {
        self.client.is_settled() && self.preparation.is_settled()
    }
    pub fn client(&self) -> &Client {
        &self.client
    }
}

/// Compare static launch, validated baseline and recipe before any transport effect.
pub fn validate_preparation(
    launch: &LaunchAuthority,
    baseline: &super::provider::ValidatedBaseline,
    recipe: &super::recipe::ValidatedNativeRecipe,
) -> anyhow::Result<()> {
    recipe.recipe.validate()?;
    let expected_os = match recipe.recipe.os {
        super::recipe::NativeOs::Linux => theseus_native_protocol::v2::Os::Linux,
        super::recipe::NativeOs::Macos => theseus_native_protocol::v2::Os::Macos,
        super::recipe::NativeOs::Windows => theseus_native_protocol::v2::Os::Windows,
    };
    anyhow::ensure!(
        expected_os == launch.binding().os,
        "native recipe OS differs from launch"
    );
    anyhow::ensure!(
        recipe.recipe.manifest_digest == launch.binding().manifest_digest.to_hex()
            && baseline.receipt().reset_epoch == launch.provider_epoch().to_hex(),
        "native baseline/recipe differs from launch"
    );
    Ok(())
}

/// Schema2 commits core and integration separately; the provider binds fullM.
pub fn validate_integration_preparation(
    launch: &LaunchAuthority,
    baseline: &super::provider::ValidatedBaseline,
    recipe: &super::integration::ValidatedIntegrationRecipe,
) -> anyhow::Result<()> {
    recipe.recipe.validate()?;
    let (core, integration) = launch
        .integration_components()
        .ok_or_else(|| anyhow::anyhow!("integration preparation requires explicit authority"))?;
    let expected_os = match recipe.core.recipe.os {
        super::recipe::NativeOs::Linux => theseus_native_protocol::v2::Os::Linux,
        super::recipe::NativeOs::Macos => theseus_native_protocol::v2::Os::Macos,
        super::recipe::NativeOs::Windows => theseus_native_protocol::v2::Os::Windows,
    };
    anyhow::ensure!(
        expected_os == launch.binding().os
            && core.to_hex() == recipe.core.recipe.manifest_digest
            && integration == recipe.integration_manifest_digest
            && launch.binding().manifest_digest.to_hex() == recipe.full_manifest_digest()?
            && baseline.receipt().manifest_digest == recipe.full_manifest_digest()?
            && baseline.receipt().reset_epoch == launch.provider_epoch().to_hex(),
        "integration preparation components, epoch or target OS changed"
    );
    let context = launch
        .integration_context()
        .ok_or_else(|| anyhow::anyhow!("provider integration context missing"))?;
    anyhow::ensure!(
        context.manifest_digest == baseline.receipt().manifest_digest
            && context.workspace_marker == baseline.receipt().workspace_marker
            && context.binding == baseline.receipt().binding,
        "provider integration context differs from baseline"
    );
    Ok(())
}

/// Owns all failed probe/material/client startup authority until actual joins.
pub struct NativeStartFailure {
    preparation: Box<LaunchPreparation>,
    client: Option<StartFailure>,
}
impl NativeStartFailure {
    pub fn preparation(&self) -> &LaunchPreparation {
        &self.preparation
    }
    pub fn client_failure(&self) -> Option<&StartFailure> {
        self.client.as_ref()
    }
    pub fn request_stop(&mut self) {
        if let Some(c) = &mut self.client {
            c.request_stop()
        }
        self.preparation.request_stop()
    }
    pub fn poll_settle(&mut self) -> anyhow::Result<bool> {
        let local = match &mut self.client {
            Some(c) => c.poll_settle()?,
            None => true,
        };
        if !local {
            return Ok(false);
        }
        self.preparation.poll_settle()
    }
    pub fn is_settled(&self) -> bool {
        self.client.as_ref().is_none_or(|c| c.is_settled()) && self.preparation.is_settled()
    }
}
impl std::fmt::Display for NativeStartFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("native startup failed; original preparation/client authority retained")
    }
}

pub struct NativeAcquired {
    pub ownership_id: String,
    pub prepared: super::model::NativeIntent,
    pub lease: theseus_native_protocol::v2::LeaseId,
}
impl NativeSession {
    pub fn acquire(
        &mut self,
        saved: crate::engine::projector::PublishedIntent,
        baseline: &super::provider::ValidatedBaseline,
    ) -> anyhow::Result<NativeAcquired> {
        anyhow::ensure!(
            self.prepared_intent.is_none(),
            "native session already retains original acquisition"
        );
        let ownership_id = saved.ownership_id().to_owned();
        self.prepared_intent = Some(saved.into_prepared());
        let original = self
            .prepared_intent
            .as_ref()
            .expect("original receipt retained");
        let record = original.record();
        let receipt = baseline.receipt();
        anyhow::ensure!(
            record.binding.to_hex() == receipt.binding
                && record.manifest_digest.to_hex() == receipt.manifest_digest
                && record.workspace_marker.to_hex() == receipt.workspace_marker,
            "acquisition differs from validated baseline"
        );
        let prepared = super::model::NativeIntent {
            intent: record.intent.to_hex(),
            binding: record.binding.to_hex(),
            manifest_digest: record.manifest_digest.to_hex(),
            workspace_marker: record.workspace_marker.to_hex(),
            reset_epoch: receipt.reset_epoch.clone(),
            publication_scope: match original.commit_scope() {
                theseus_host::fs::CommitScope::FileAndDirectorySynced => {
                    "file_and_directory_synced"
                }
                theseus_host::fs::CommitScope::FileFlushedAtomicReplace => {
                    "file_flushed_atomic_replace"
                }
            }
            .into(),
        };
        let id = self.client.submit(Action::Acquire(original))?;
        match self.wait_response(id)? {
            theseus_native_protocol::v2::Reply::Acquired {
                state:
                    theseus_native_protocol::v2::AcquireView::Lease {
                        lease,
                        state: theseus_native_protocol::v2::LeaseView::Held { .. },
                    },
                ..
            } => Ok(NativeAcquired {
                ownership_id,
                prepared,
                lease,
            }),
            _ => anyhow::bail!("native Acquire unproved; original intent retained"),
        }
    }
    pub fn release(
        &mut self,
        saved: crate::engine::projector::PublishedRelease,
    ) -> anyhow::Result<theseus_native_protocol::v2::LeaseView> {
        let release = saved.into_release();
        let lease = theseus_native_protocol::v2::LeaseId::parse(&release.lease)?;
        let id = self.client.submit(Action::Release(lease))?;
        match self.wait_response(id)? {
            theseus_native_protocol::v2::Reply::Lease { lease: l, state } if l == lease => {
                Ok(state)
            }
            _ => anyhow::bail!("native Release unproved; query original lease"),
        }
    }
    fn wait_response(
        &mut self,
        id: RequestId,
    ) -> anyhow::Result<theseus_native_protocol::v2::Reply> {
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            anyhow::ensure!(
                Instant::now() < end,
                "native response deadline; original session retained"
            );
            match self.client.poll()? {
                Poll::Event(Event::Reply { request, reply }) if request == id => return Ok(reply),
                Poll::Event(Event::ConnectionLost | Event::PeerClosed(_))
                | Poll::TransportSettled => {
                    anyhow::bail!("native response lost; original identity retained")
                }
                _ => std::thread::yield_now(),
            }
        }
    }
}
impl std::fmt::Debug for NativeStartFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeStartFailure")
            .field("settled", &self.is_settled())
            .finish_non_exhaustive()
    }
}
impl std::error::Error for NativeStartFailure {}

#[cfg(test)]
mod linux_admission_gate_tests {
    use super::*;
    use theseus_native_protocol::v2::{Digest, LinuxGioQualificationRef};
    #[test]
    fn serialized_reference_cannot_replace_original_admission_and_binding() {
        let reference = LinuxGioQualificationRef {
            schema: 1,
            attestation_digest: "11".repeat(32),
            prepare_invocation_id: "original-prepare-2".into(),
            prepare_invocation_digest: "33".repeat(32),
        };
        let full_m = Digest::from_bytes([4; 32]);
        assert!(validate_linux_admission_binding(None, full_m, Some(&reference)).is_err());
        // Pure comparison only: this test does not authenticate a provider or
        // create a usable session, SDK token, lease, or native activation.
        let original = QualifiedLinuxGioAdmission {
            reference: reference.clone(),
            manifest_digest: full_m,
        };
        assert!(
            validate_linux_admission_binding(Some(&original), full_m, Some(&reference)).is_ok()
        );
        assert!(
            validate_linux_admission_binding(
                Some(&original),
                Digest::from_bytes([5; 32]),
                Some(&reference)
            )
            .is_err()
        );
        assert!(validate_linux_admission_binding(Some(&original), full_m, None).is_err());
        let mut changed = reference.clone();
        changed.attestation_digest = "66".repeat(32);
        assert!(validate_linux_admission_binding(Some(&original), full_m, Some(&changed)).is_err());
    }
}
