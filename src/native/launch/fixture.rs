//! Test-support launch authority fixtures.
use anyhow::{Result, ensure};
use sha2::{Digest as _, Sha256};
use theseus_host::process::{ChildEnvironment, NativeCommand};
use theseus_native_helper_store::StoreBinding;
use theseus_native_protocol::v2;

use super::{IntegrationCeiling, LaunchAuthority, LaunchView};

impl LaunchAuthority {
    #[cfg(any(
        feature = "native-catalog-test-support",
        feature = "native-integration-test-support"
    ))]
    pub fn fixture(command: NativeCommand, binding: StoreBinding, epoch: v2::Digest) -> Self {
        Self {
            integration: None,
            #[cfg(feature = "native-integration-test-support")]
            fixture_executables: None,
            command: Some(command.clone()),
            provider_name: "fixture-provider".into(),
            baseline_label: "inert-preprovisioned-fixture".into(),
            provider_command: None,
            ssh: None,
            binding,
            epoch,
            view: LaunchView {
                target_id: "inert-local-fixture".into(),
                fingerprint: fixture_command_digest(&command),
                profile: "inert_local_fixture",
            },
        }
    }
    #[cfg(feature = "native-integration-test-support")]
    pub fn fixture_integration(
        command: NativeCommand,
        binding: StoreBinding,
        epoch: v2::Digest,
        core: v2::Digest,
        integration: v2::Digest,
        service: v2::ServiceAuthority,
    ) -> Result<Self> {
        service.validate()?;
        ensure!(
            binding.manifest_digest == v2::integration_manifest_digest(core, integration),
            "fixture full manifest differs from integration components"
        );
        let mut authority = Self::fixture(command, binding, epoch);
        authority.view.fingerprint = format!(
            "{:x}",
            Sha256::digest(format!(
                "integration:{}:{}:{}:{}",
                authority.view.fingerprint,
                core.to_hex(),
                integration.to_hex(),
                serde_json::to_string(&service)?
            ))
        );
        authority.integration = Some(IntegrationCeiling {
            core_manifest_digest: core,
            integration_manifest_digest: integration,
            service: Some(service),
            desktop: None,
            context: None,
            linux_gio: None,
        });
        Ok(authority)
    }
    #[cfg(feature = "native-integration-test-support")]
    pub fn fixture_desktop(
        command: NativeCommand,
        binding: StoreBinding,
        epoch: v2::Digest,
        core: v2::Digest,
        integration: v2::Digest,
        desktop: v2::broker::DesktopAuthority,
    ) -> Result<Self> {
        desktop.validate()?;
        ensure!(
            binding.manifest_digest == v2::integration_manifest_digest(core, integration),
            "fixture full manifest differs from desktop components"
        );
        let mut authority = Self::fixture(command, binding, epoch);
        authority.view.target_id = desktop.target_id.clone();
        authority.view.profile = "source_owned_desktop_fixture";
        authority.view.fingerprint = format!(
            "{:x}",
            Sha256::digest(format!(
                "desktop:{}:{}:{}:{}",
                authority.view.fingerprint,
                core.to_hex(),
                integration.to_hex(),
                serde_json::to_string(&desktop)?
            ))
        );
        authority.integration = Some(IntegrationCeiling {
            core_manifest_digest: core,
            integration_manifest_digest: integration,
            service: None,
            desktop: Some(desktop),
            context: None,
            linux_gio: None,
        });
        Ok(authority)
    }
    /// Explicit compiled source fixture only. Shipping raw launch parsing stays closed.
    #[cfg(feature = "native-integration-test-support")]
    #[allow(clippy::too_many_arguments)]
    pub fn fixture_combined(
        command: NativeCommand,
        binding: StoreBinding,
        epoch: v2::Digest,
        core: v2::Digest,
        integration: v2::Digest,
        service: v2::ServiceAuthority,
        desktop: v2::broker::DesktopAuthority,
        manifest: &v2::IntegrationManifest,
    ) -> Result<Self> {
        manifest.validate()?;
        desktop.validate()?;
        service.validate()?;
        let entry = manifest
            .desktop
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("combined desktop tuple missing"))?;
        let os = match binding.os {
            v2::Os::Linux => "linux",
            v2::Os::Macos => "macos",
            v2::Os::Windows => "windows",
        };
        ensure!(
            desktop.os == os
                && matches!(
                    (&service, binding.os),
                    (v2::ServiceAuthority::Systemd { .. }, v2::Os::Linux)
                        | (v2::ServiceAuthority::Launchd { .. }, v2::Os::Macos)
                        | (v2::ServiceAuthority::Scm { .. }, v2::Os::Windows)
                ),
            "combined original authorities differ from target OS"
        );
        ensure!(
            matches!(
                desktop.display,
                v2::broker::SuppliedDisplay::SourceOwnedHeadless
            ) && desktop.allowed_modes == [v2::broker::DesktopMode::OwnedExecutable]
                && entry.allowed_modes == [v2::broker::DesktopMode::OwnedExecutable]
                && manifest.desktop_native.is_none()
                && manifest.service_authority.as_ref() == Some(&service)
                && service.slot() != &entry.slot
                && manifest.digest()? == integration
                && manifest.core_manifest_digest == core.to_hex()
                && v2::IntegrationCapabilities::for_manifest(manifest)?
                    == v2::IntegrationCapabilities::Combined,
            "compiled combined fixture requires exact distinct service and SourceOwned-only desktop authority"
        );
        let desktop_digest = serde_json::to_string(&desktop)?;
        let mut authority =
            Self::fixture_integration(command, binding, epoch, core, integration, service)?;
        authority.view.target_id = desktop.target_id.clone();
        authority.view.profile = "source_owned_combined_fixture";
        authority.view.fingerprint = format!(
            "{:x}",
            Sha256::digest(format!(
                "combined:{}:{}",
                authority.view.fingerprint, desktop_digest
            ))
        );
        authority.integration.as_mut().unwrap().desktop = Some(desktop);
        Ok(authority)
    }
    #[cfg(feature = "native-integration-test-support")]
    pub fn with_fixture_executable_pins(
        mut self,
        pins: std::sync::Arc<crate::target_authority::FixtureExecutablePins>,
    ) -> Result<Self> {
        pins.verify()?;
        ensure!(
            self.command
                .as_ref()
                .is_some_and(|c| pins.contains(std::path::Path::new(&c.program)))
                && self
                    .provider_command
                    .as_ref()
                    .is_some_and(|c| pins.contains(std::path::Path::new(&c.program))),
            "fixture commands are not in frozen executable authority"
        );
        self.view.fingerprint = format!(
            "{:x}",
            Sha256::digest(format!("{}:{}", self.view.fingerprint, pins.commitment()))
        );
        self.fixture_executables = Some(pins);
        Ok(self)
    }
    #[cfg(any(
        feature = "native-catalog-test-support",
        feature = "native-integration-test-support"
    ))]
    pub fn with_fixture_provider(mut self, name: String, command: NativeCommand) -> Result<Self> {
        crate::native::provider::CommandProvider::new(name.clone(), command.clone())?;
        self.provider_name = name;
        self.view.fingerprint = format!(
            "{:x}",
            Sha256::digest(format!(
                "{}:{}:{}",
                self.view.fingerprint,
                self.provider_name,
                fixture_command_digest(&command)
            ))
        );
        self.provider_command = Some(command);
        Ok(self)
    }
    #[cfg(any(
        feature = "native-catalog-test-support",
        feature = "native-integration-test-support"
    ))]
    pub fn with_fixture_baseline(mut self, label: String) -> Result<Self> {
        crate::native::model::text(&label, 256)?;
        self.baseline_label = label;
        self.view.fingerprint = format!(
            "{:x}",
            Sha256::digest(format!("{}:{}", self.view.fingerprint, self.baseline_label))
        );
        Ok(self)
    }
}

#[cfg(any(
    feature = "native-catalog-test-support",
    feature = "native-integration-test-support"
))]
fn fixture_command_digest(command: &NativeCommand) -> String {
    let mut h = Sha256::new();
    for value in std::iter::once(&command.program).chain(command.args.iter()) {
        let b = value.as_encoded_bytes();
        h.update((b.len() as u64).to_be_bytes());
        h.update(b)
    }
    if let ChildEnvironment::Replace(entries) = &command.environment {
        for (k, v) in entries {
            for value in [k, v] {
                let b = value.as_encoded_bytes();
                h.update((b.len() as u64).to_be_bytes());
                h.update(b)
            }
        }
    }
    format!("{:x}", h.finalize())
}
