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
}
impl NativeSession {
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
