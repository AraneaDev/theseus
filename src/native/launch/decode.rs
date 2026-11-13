//! Explicit launch configuration decoding (catalog and integration schemas).
use anyhow::{Result, ensure};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use theseus_host::process::{ChildEnvironment, NativeCommand};
use theseus_native_helper_store::StoreBinding;
use theseus_native_protocol::v2;

use super::{
    IntegrationCeiling, LaunchAuthority, LaunchView, SshInput, SshMaterialPaths,
    openssh_subsystem_argv,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchInput {
    kind: String,
    schema: u32,
    os: String,
    target_id: String,
    profile: String,
    ssh_program: String,
    openssh_version: String,
    host: String,
    port: u16,
    user: String,
    subsystem: String,
    private_key: String,
    host_keys: String,
    runtime_directory: String,
    binding: String,
    manifest_digest: String,
    workspace_marker: String,
    provider_epoch: String,
    provider: ProviderInput,
    baseline_label: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderInput {
    name: String,
    program: String,
    args: Vec<String>,
    #[serde(default)]
    environment: Vec<ProviderEnv>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderEnv {
    name: String,
    value: String,
}
impl LaunchAuthority {
    /// Parses explicit local runtime configuration, never a recipe field.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() <= 65536, "launch configuration exceeds bound");
        let mut value: serde_json::Value = crate::native::model::decode(bytes)?;
        match value.get("schema").and_then(serde_json::Value::as_u64) {
            Some(1) => Self::decode_catalog(bytes),
            Some(2) => {
                let object = value
                    .as_object_mut()
                    .ok_or_else(|| anyhow::anyhow!("expected launch object"))?;
                ensure!(
                    object
                        .remove("helper_profile")
                        .as_ref()
                        .and_then(serde_json::Value::as_str)
                        == Some("native_integration_v1"),
                    "unsupported integration helper profile"
                );
                let digest = |value: Option<serde_json::Value>| -> Result<v2::Digest> {
                    v2::Digest::parse(
                        value
                            .as_ref()
                            .and_then(serde_json::Value::as_str)
                            .ok_or_else(|| {
                                anyhow::anyhow!("required integration manifest component")
                            })?,
                    )
                    .map_err(Into::into)
                };
                let core = digest(object.remove("core_manifest_digest"))?;
                let integration = digest(object.remove("integration_manifest_digest"))?;
                let service: Option<v2::ServiceAuthority> = serde_json::from_value(
                    object
                        .remove("service_authority")
                        .ok_or_else(|| anyhow::anyhow!("explicit service authority required"))?,
                )?;
                if let Some(service) = &service {
                    service.validate()?;
                }
                let desktop_value = object
                    .remove("desktop_authority")
                    .ok_or_else(|| anyhow::anyhow!("explicit desktop authority field required"))?;
                ensure!(
                    desktop_value
                        .get("display")
                        .and_then(|d| d.get("kind"))
                        .and_then(serde_json::Value::as_str)
                        != Some("source_owned_headless"),
                    "source headless requires explicit compiled fixture authority"
                );
                let desktop: Option<v2::broker::DesktopAuthority> =
                    serde_json::from_value(desktop_value)?;
                if let Some(desktop) = &desktop {
                    desktop.validate()?;
                }
                ensure!(
                    service.is_some() || desktop.is_some(),
                    "integration launch has no declared authority"
                );
                object.insert("schema".into(), 1.into());
                let mut authority = Self::decode_catalog(&serde_json::to_vec(&value)?)?;
                ensure!(
                    authority.binding.manifest_digest
                        == v2::integration_manifest_digest(core, integration),
                    "integration manifest components differ from full binding"
                );
                ensure!(
                    matches!(
                        (&service, authority.binding.os),
                        (None, _)
                            | (Some(v2::ServiceAuthority::Systemd { .. }), v2::Os::Linux)
                            | (Some(v2::ServiceAuthority::Launchd { .. }), v2::Os::Macos)
                            | (Some(v2::ServiceAuthority::Scm { .. }), v2::Os::Windows)
                    ),
                    "service authority differs from target OS"
                );
                if let Some(desktop) = &desktop {
                    ensure!(
                        desktop.target_id == authority.view.target_id
                            && matches!(
                                (authority.binding.os, desktop.os.as_str()),
                                (v2::Os::Linux, "linux")
                                    | (v2::Os::Macos, "macos")
                                    | (v2::Os::Windows, "windows")
                            ),
                        "desktop authority differs from explicit target"
                    );
                }
                authority.view.fingerprint = format!("{:x}", Sha256::digest(bytes));
                authority.integration = Some(IntegrationCeiling {
                    core_manifest_digest: core,
                    integration_manifest_digest: integration,
                    service,
                    desktop,
                    context: None,
                    linux_gio: None,
                });
                Ok(authority)
            }
            _ => anyhow::bail!("unsupported launch schema"),
        }
    }
    fn decode_catalog(bytes: &[u8]) -> Result<Self> {
        let input: LaunchInput = crate::native::model::decode(bytes)?;
        ensure!(
            input.kind == "theseus_native_launch" && input.schema == 1,
            "unsupported launch namespace"
        );
        crate::native::model::text(&input.target_id, 128)?;
        crate::native::model::text(&input.provider.name, 256)?;
        crate::native::model::text(&input.baseline_label, 256)?;
        let mut provider_command = NativeCommand::new(&input.provider.program);
        provider_command.args(&input.provider.args);
        provider_command.environment = ChildEnvironment::Replace(
            input
                .provider
                .environment
                .iter()
                .map(|e| (e.name.clone().into(), e.value.clone().into()))
                .collect(),
        );
        crate::native::provider::CommandProvider::new(
            input.provider.name.clone(),
            provider_command.clone(),
        )?;

        ensure!(
            input.profile == "openssh_subsystem_v1",
            "unsupported native transport profile"
        );
        ensure!(
            input.openssh_version.starts_with("OpenSSH_10.")
                && input.openssh_version.len() <= 512
                && !input.openssh_version.contains('\n'),
            "explicit OpenSSH10.x build receipt required"
        );
        let ssh = SshInput {
            program: input.ssh_program,
            version: input.openssh_version,
            host: input.host,
            port: input.port,
            user: input.user,
            subsystem: input.subsystem,
            private_key: input.private_key,
            host_keys: input.host_keys,
            runtime_directory: input.runtime_directory,
        };
        let runtime = std::path::Path::new(&ssh.runtime_directory);
        openssh_subsystem_argv(
            std::path::Path::new(&ssh.program),
            &ssh.host,
            ssh.port,
            &ssh.user,
            &ssh.subsystem,
            SshMaterialPaths {
                identity: &runtime.join("identity"),
                host_keys: &runtime.join("host_keys"),
                empty_global: &runtime.join("global_empty"),
            },
        )?;
        ensure!(
            [&ssh.private_key, &ssh.host_keys, &ssh.runtime_directory]
                .iter()
                .all(|p| std::path::Path::new(p).is_absolute()),
            "explicit absolute SSH material paths required"
        );
        let os = match input.os.as_str() {
            "linux" => v2::Os::Linux,
            "macos" => v2::Os::Macos,
            "windows" => v2::Os::Windows,
            _ => anyhow::bail!("unsupported native OS"),
        };
        let binding = StoreBinding {
            os,
            binding: v2::BindingId::parse(&input.binding)?,
            manifest_digest: v2::Digest::parse(&input.manifest_digest)?,
            workspace_marker: v2::WorkspaceMarker::parse(&input.workspace_marker)?,
        };
        let fingerprint = Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Ok(Self {
            integration: None,
            #[cfg(feature = "native-integration-test-support")]
            fixture_executables: None,
            command: None,
            provider_name: input.provider.name,
            baseline_label: input.baseline_label,
            provider_command: Some(provider_command),
            ssh: Some(ssh),
            binding,
            epoch: v2::Digest::parse(&input.provider_epoch)?,
            view: LaunchView {
                target_id: input.target_id,
                fingerprint,
                profile: "openssh_subsystem_v1",
            },
        })
    }
}
