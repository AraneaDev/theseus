//! Explicit runtime authority; recipe and report data never reconstruct transport authority.
use anyhow::{Result, ensure};
use theseus_host::process::NativeCommand;
use theseus_native_client::ExpectedHelper;
use theseus_native_helper_store::StoreBinding;
use theseus_native_protocol::v2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchView {
    pub target_id: String,
    pub fingerprint: String,
    pub profile: &'static str,
}
/// Deliberately neither Debug nor Serialize: this owns explicit launch authority.
pub struct LaunchAuthority {
    command: Option<NativeCommand>,
    ssh: Option<SshInput>,
    binding: StoreBinding,
    epoch: v2::Digest,
    view: LaunchView,
    provider_name: String,
    baseline_label: String,
    provider_command: Option<NativeCommand>,
    integration: Option<IntegrationCeiling>,
    #[cfg(feature = "native-integration-test-support")]
    fixture_executables: Option<std::sync::Arc<crate::target_authority::FixtureExecutablePins>>,
}
#[derive(Clone)]
struct IntegrationCeiling {
    core_manifest_digest: v2::Digest,
    integration_manifest_digest: v2::Digest,
    service: Option<v2::ServiceAuthority>,
    desktop: Option<v2::broker::DesktopAuthority>,
    context: Option<v2::IntegrationRuntimeContext>,
    linux_gio: Option<super::provider::ValidatedLinuxGioPrepare>,
}
impl Clone for LaunchAuthority {
    fn clone(&self) -> Self {
        Self {
            command: self.command.clone(),
            ssh: self.ssh.clone(),
            binding: StoreBinding {
                binding: self.binding.binding,
                manifest_digest: self.binding.manifest_digest,
                workspace_marker: self.binding.workspace_marker,
                os: self.binding.os,
            },
            epoch: self.epoch,
            view: self.view.clone(),
            provider_name: self.provider_name.clone(),
            baseline_label: self.baseline_label.clone(),
            provider_command: self.provider_command.clone(),
            integration: self.integration.clone(),
            #[cfg(feature = "native-integration-test-support")]
            fixture_executables: self.fixture_executables.clone(),
        }
    }
}
impl LaunchAuthority {
    pub fn view(&self) -> &LaunchView {
        &self.view
    }
    pub fn integration_components(&self) -> Option<(v2::Digest, v2::Digest)> {
        self.integration.as_ref().map(|value| {
            (
                value.core_manifest_digest,
                value.integration_manifest_digest,
            )
        })
    }
    pub fn integration_service_authority(&self) -> Option<&v2::ServiceAuthority> {
        self.integration.as_ref()?.service.as_ref()
    }
    pub fn integration_desktop_authority(&self) -> Option<&v2::broker::DesktopAuthority> {
        self.integration.as_ref()?.desktop.as_ref()
    }
    pub fn integration_context(&self) -> Option<&v2::IntegrationRuntimeContext> {
        self.integration.as_ref()?.context.as_ref()
    }
    pub fn qualified_linux_gio_prepare(
        &self,
    ) -> Option<&super::provider::ValidatedLinuxGioPrepare> {
        self.integration.as_ref()?.linux_gio.as_ref()
    }
    pub fn expected(&self) -> ExpectedHelper {
        ExpectedHelper {
            binding: StoreBinding {
                binding: self.binding.binding,
                manifest_digest: self.binding.manifest_digest,
                workspace_marker: self.binding.workspace_marker,
                os: self.binding.os,
            },
            programs: v2::ProgramList::new(
                super::model::CATALOG_IDS
                    .iter()
                    .map(|p| p.program_id())
                    .collect(),
            )
            .expect("closed catalog"),
            profile: match &self.integration {
                Some(value) => v2::HelperProfile::Integration {
                    provider_epoch: self.epoch,
                    core_manifest_digest: value.core_manifest_digest,
                    integration_manifest_digest: value.integration_manifest_digest,
                },
                None => v2::HelperProfile::Native {
                    provider_epoch: self.epoch,
                },
            },
        }
    }
    pub fn baseline_label(&self) -> &str {
        &self.baseline_label
    }
    pub fn provider_name(&self) -> &str {
        &self.provider_name
    }
    pub fn provider_command(&self) -> Result<NativeCommand> {
        #[cfg(feature = "native-integration-test-support")]
        if let Some(pins) = &self.fixture_executables {
            pins.verify()?;
        }
        self.provider_command
            .clone()
            .ok_or_else(|| anyhow::anyhow!("fixture provider not configured"))
    }
    pub fn provider_factory(&self) -> Result<super::provider::CommandProvider> {
        super::provider::CommandProvider::new(self.provider_name.clone(), self.provider_command()?)
    }
    pub fn with_baseline(&self, baseline: &super::provider::ValidatedBaseline) -> Result<Self> {
        ensure!(
            baseline.launch_fingerprint() == self.view.fingerprint,
            "provider baseline belongs to another launch"
        );
        let receipt = baseline.receipt();
        let mut result = self.with_identity(
            &receipt.binding,
            &receipt.manifest_digest,
            &receipt.workspace_marker,
            &receipt.reset_epoch,
        )?;
        if result.integration.is_some() {
            result
                .integration
                .as_mut()
                .expect("declared integration")
                .linux_gio = None;
            result.bind_integration_context(&receipt.receipt)?;
            if let Some(qualified) = baseline.linux_gio_qualification() {
                let ceiling = result.integration.as_mut().expect("declared integration");
                qualified.validate_context(ceiling.context.as_ref().expect("bound context"))?;
                ceiling.linux_gio = Some(qualified.clone());
            }
        }
        Ok(result)
    }
    fn bind_integration_context(&mut self, bytes: &str) -> Result<()> {
        ensure!(
            bytes.len() <= 4096,
            "integration context receipt exceeds bound"
        );
        let context: v2::IntegrationRuntimeContext = super::model::decode(bytes.as_bytes())?;
        context.validate()?;
        let integration = self
            .integration
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("catalog launch has no integration context"))?;
        ensure!(
            context.binding == self.binding.binding.to_hex()
                && context.manifest_digest == self.binding.manifest_digest.to_hex()
                && context.workspace_marker == self.binding.workspace_marker.to_hex()
                && context.provider_epoch == self.epoch.to_hex()
                && context.integration_manifest_digest
                    == integration.integration_manifest_digest.to_hex(),
            "provider integration context differs from original binding"
        );
        match (&integration.service, context.manager_platform) {
            (Some(service), Some(_)) => service.validate_context(&context, self.binding.os)?,
            (None, None) => context.validate_os(self.binding.os)?,
            _ => anyhow::bail!("provider manager context differs from declared authority"),
        }
        match (&integration.desktop, &context.desktop_broker_header) {
            (Some(desktop), Some(header)) => desktop.validate_header(header)?,
            (None, None) => {}
            _ => anyhow::bail!("provider desktop context differs from declared authority"),
        }
        integration.context = Some(context);
        Ok(())
    }
    pub fn for_recovery(
        &self,
        prepared: &super::model::NativeIntent,
        launch_fingerprint: &str,
    ) -> Result<Self> {
        ensure!(
            launch_fingerprint == self.view.fingerprint,
            "recovery launch fingerprint changed"
        );
        self.with_identity(
            &prepared.binding,
            &prepared.manifest_digest,
            &prepared.workspace_marker,
            &prepared.reset_epoch,
        )
    }
    fn with_identity(
        &self,
        binding: &str,
        manifest: &str,
        marker: &str,
        epoch: &str,
    ) -> Result<Self> {
        ensure!(
            manifest == self.binding.manifest_digest.to_hex(),
            "native instance exceeds pinned catalog ceiling"
        );
        Ok(Self {
            command: self.command.clone(),
            ssh: self.ssh.clone(),
            binding: StoreBinding {
                binding: v2::BindingId::parse(binding)?,
                manifest_digest: v2::Digest::parse(manifest)?,
                workspace_marker: v2::WorkspaceMarker::parse(marker)?,
                os: self.binding.os,
            },
            epoch: v2::Digest::parse(epoch)?,
            view: self.view.clone(),
            provider_name: self.provider_name.clone(),
            baseline_label: self.baseline_label.clone(),
            provider_command: self.provider_command.clone(),
            integration: self.integration.clone(),
            #[cfg(feature = "native-integration-test-support")]
            fixture_executables: self.fixture_executables.clone(),
        })
    }
    pub fn provider_epoch(&self) -> v2::Digest {
        self.epoch
    }
    pub fn binding(&self) -> &StoreBinding {
        &self.binding
    }
}

#[derive(Clone)]
struct SshInput {
    program: String,
    version: String,
    host: String,
    port: u16,
    user: String,
    subsystem: String,
    private_key: String,
    host_keys: String,
    runtime_directory: String,
}
/// Owns staged material and all probe owners, including partial failures.
pub struct LaunchPreparation {
    pub(crate) command: Option<NativeCommand>,
    probe: Option<super::provider::CommandProvider>,
    material: Option<theseus_host::fs::HostDir>,
    material_files: Vec<(std::ffi::OsString, std::fs::File)>,
    pub(crate) error: Option<anyhow::Error>,
    launch_fingerprint: String,
    cleanup_error: Option<MaterialCleanupBlocker>,
    settled_cleanup_history: Option<std::io::Error>,
    settled_cleanup_attempts: u64,
    next_cleanup_retry: Option<std::time::Instant>,
}
enum MaterialCleanupBlocker {
    Sticky(std::io::Error),
    Unsettled(theseus_host::fs::LeafOpenUncertainty),
}

mod decode;
#[cfg(any(
    feature = "native-catalog-test-support",
    feature = "native-integration-test-support"
))]
mod fixture;
mod preparation;
mod ssh;

#[cfg(all(test, unix))]
use sha2::{Digest as _, Sha256};
pub use ssh::{SshMaterialPaths, openssh_subsystem_argv};
#[cfg(test)]
use ssh::{unencrypted_private_key, verify_effective_options};
#[cfg(all(test, unix, feature = "native-integration-test-support"))]
use theseus_host::process::ChildEnvironment;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod qualification_tests;

#[cfg(all(test, unix))]
mod cleanup_retry_tests;
