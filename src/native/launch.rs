//! Explicit runtime authority; recipe and report data never reconstruct transport authority.
use anyhow::{Result, ensure};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use theseus_host::process::{ChildEnvironment, NativeCommand};
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
}
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
        let input: LaunchInput = super::model::decode(bytes)?;
        ensure!(
            input.kind == "theseus_native_launch" && input.schema == 1,
            "unsupported launch namespace"
        );
        super::model::text(&input.target_id, 128)?;
        super::model::text(&input.provider.name, 256)?;
        super::model::text(&input.baseline_label, 256)?;
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
        super::provider::CommandProvider::new(
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
    pub fn view(&self) -> &LaunchView {
        &self.view
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
            profile: v2::HelperProfile::Native {
                provider_epoch: self.epoch,
            },
        }
    }
    #[cfg(feature = "native-catalog-test-support")]
    pub fn fixture(command: NativeCommand, binding: StoreBinding, epoch: v2::Digest) -> Self {
        Self {
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
    pub fn baseline_label(&self) -> &str {
        &self.baseline_label
    }
    pub fn provider_name(&self) -> &str {
        &self.provider_name
    }
    pub fn provider_command(&self) -> Result<NativeCommand> {
        self.provider_command
            .clone()
            .ok_or_else(|| anyhow::anyhow!("fixture provider not configured"))
    }
    pub fn provider_factory(&self) -> Result<super::provider::CommandProvider> {
        super::provider::CommandProvider::new(self.provider_name.clone(), self.provider_command()?)
    }
    #[cfg(feature = "native-catalog-test-support")]
    pub fn with_fixture_provider(mut self, name: String, command: NativeCommand) -> Result<Self> {
        super::provider::CommandProvider::new(name.clone(), command.clone())?;
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
    #[cfg(feature = "native-catalog-test-support")]
    pub fn with_fixture_baseline(mut self, label: String) -> Result<Self> {
        super::model::text(&label, 256)?;
        self.baseline_label = label;
        self.view.fingerprint = format!(
            "{:x}",
            Sha256::digest(format!("{}:{}", self.view.fingerprint, self.baseline_label))
        );
        Ok(self)
    }
    pub fn with_baseline(&self, baseline: &super::provider::ValidatedBaseline) -> Result<Self> {
        ensure!(
            baseline.launch_fingerprint() == self.view.fingerprint,
            "provider baseline belongs to another launch"
        );
        let receipt = baseline.receipt();
        self.with_identity(
            &receipt.binding,
            &receipt.manifest_digest,
            &receipt.workspace_marker,
            &receipt.reset_epoch,
        )
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
        })
    }
    pub fn provider_epoch(&self) -> v2::Digest {
        self.epoch
    }
    pub fn binding(&self) -> &StoreBinding {
        &self.binding
    }
}

/// Inert argv construction only. Acceptance still requires actual OpenSSH10.x
/// version and effective-option qualification using dedicated runtime material.
#[derive(Clone, Copy)]
pub struct SshMaterialPaths<'a> {
    pub identity: &'a std::path::Path,
    pub host_keys: &'a std::path::Path,
    pub empty_global: &'a std::path::Path,
}
pub fn openssh_subsystem_argv(
    program: &std::path::Path,
    host: &str,
    port: u16,
    user: &str,
    subsystem: &str,
    material: SshMaterialPaths<'_>,
) -> Result<NativeCommand> {
    ensure!(
        program.is_absolute() && port > 0,
        "invalid SSH program/port"
    );
    ensure!(valid_host(host), "invalid SSH host");
    ensure!(
        !user.is_empty()
            && user.len() <= 64
            && user
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "invalid SSH account"
    );
    ensure!(
        !subsystem.is_empty()
            && subsystem.len() <= 32
            && subsystem.as_bytes()[0].is_ascii_alphabetic()
            && subsystem
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
        "invalid SSH subsystem"
    );
    let path = |p: &std::path::Path| -> Result<String> {
        let s = p
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("SSH option path must be UTF-8"))?;
        // OpenSSH option values accept forward slashes on Windows. Normalize
        // native separators before checking the same closed literal alphabet.
        #[cfg(windows)]
        let s = {
            // canonicalize() can produce a verbatim drive prefix. Removing only
            // that prefix preserves the absolute drive path; other verbatim
            // namespaces remain outside the literal option profile.
            let ordinary = s
                .strip_prefix(r"\\?\")
                .filter(|tail| {
                    let bytes = tail.as_bytes();
                    bytes.len() >= 3
                        && bytes[0].is_ascii_alphabetic()
                        && bytes[1] == b':'
                        && matches!(bytes[2], b'\\' | b'/')
                })
                .unwrap_or(s);
            ordinary.replace('\\', "/")
        };
        #[cfg(not(windows))]
        let s = s.to_owned();
        ensure!(
            p.is_absolute()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/._-:".contains(&b)),
            "SSH option path permits expansion or separators"
        );
        Ok(s)
    };
    let identity = path(material.identity)?;
    let host_keys = path(material.host_keys)?;
    let empty_global = path(material.empty_global)?;
    let mut command = NativeCommand::new(program);
    command.environment = ChildEnvironment::Replace(vec![]);
    command.args([
        "-F",
        "none",
        "-T",
        "-s",
        "-S",
        "none",
        "-p",
        &port.to_string(),
        "-l",
        user,
        "-i",
        &identity,
    ]);
    for option in [
        "BatchMode=yes",
        "IdentitiesOnly=yes",
        "IdentityAgent=none",
        "PreferredAuthentications=publickey",
        "PasswordAuthentication=no",
        "KbdInteractiveAuthentication=no",
        "StrictHostKeyChecking=yes",
        "UpdateHostKeys=no",
        "VerifyHostKeyDNS=no",
        "ProxyCommand=none",
        "ProxyJump=none",
        "ControlMaster=no",
        "ControlPersist=no",
        "ClearAllForwardings=yes",
        "ForwardAgent=no",
        "ForwardX11=no",
        "Tunnel=no",
        "PermitLocalCommand=no",
        "CanonicalizeHostname=no",
        "PKCS11Provider=none",
    ] {
        command.args(["-o", option]);
    }
    command.args([
        "-o",
        &format!("UserKnownHostsFile={host_keys}"),
        "-o",
        &format!("GlobalKnownHostsFile={empty_global}"),
        host,
        subsystem,
    ]);
    Ok(command)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ssh_argv_has_one_literal_subsystem_and_no_config_or_agent_fallback() {
        let p = std::path::Path::new(if cfg!(windows) {
            r"\\?\C:\fixture"
        } else {
            "/fixture"
        });
        let command = openssh_subsystem_argv(
            p,
            "target.example",
            22,
            "helper",
            "theseus",
            SshMaterialPaths {
                identity: p,
                host_keys: p,
                empty_global: p,
            },
        )
        .unwrap();
        let args = command
            .args
            .iter()
            .map(|s| s.to_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(&args[..6], ["-F", "none", "-T", "-s", "-S", "none"]);
        assert_eq!(&args[args.len() - 2..], ["target.example", "theseus"]);
        assert!(args.contains(&"IdentityAgent=none"));
        let expected_identity = if cfg!(windows) {
            "C:/fixture"
        } else {
            "/fixture"
        };
        let identity_option = args.iter().position(|arg| *arg == "-i").unwrap();
        assert_eq!(args[identity_option + 1], expected_identity);
        assert!(args.contains(&format!("UserKnownHostsFile={expected_identity}").as_str()));
        assert!(
            openssh_subsystem_argv(
                p,
                "-oProxyCommand=x",
                22,
                "helper",
                "theseus",
                SshMaterialPaths {
                    identity: p,
                    host_keys: p,
                    empty_global: p
                }
            )
            .is_err()
        );
        assert!(
            openssh_subsystem_argv(
                p,
                "target",
                22,
                "helper",
                "name;command",
                SshMaterialPaths {
                    identity: p,
                    host_keys: p,
                    empty_global: p
                }
            )
            .is_err()
        );
        assert!(
            openssh_subsystem_argv(
                p,
                "target",
                22,
                "helper",
                "theseus",
                SshMaterialPaths {
                    identity: std::path::Path::new("/tmp/%h"),
                    host_keys: p,
                    empty_global: p
                }
            )
            .is_err()
        );
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

impl LaunchPreparation {
    pub fn launch_fingerprint(&self) -> &str {
        &self.launch_fingerprint
    }
    pub fn material(&self) -> Option<&theseus_host::fs::HostDir> {
        self.material.as_ref()
    }
    pub fn probe(&self) -> Option<&super::provider::CommandProvider> {
        self.probe.as_ref()
    }
    pub fn error(&self) -> Option<&anyhow::Error> {
        self.error.as_ref()
    }
    pub fn request_stop(&mut self) {
        if let Some(p) = &mut self.probe {
            let _ = p.poll_settle();
        }
    }
    /// Currently blocking authority/unknown failure; settled retry history is separate.
    pub fn cleanup_error(&self) -> Option<&std::io::Error> {
        self.cleanup_error.as_ref().map(|failure| match failure {
            MaterialCleanupBlocker::Sticky(error) => error,
            MaterialCleanupBlocker::Unsettled(owner) => owner.error(),
        })
    }
    /// First original terminal deletion failure, retained even after a proved retry.
    pub fn settled_cleanup_error(&self) -> Option<&std::io::Error> {
        self.settled_cleanup_history.as_ref()
    }

    pub fn poll_settle(&mut self) -> Result<bool> {
        if self.cleanup_error.is_some() {
            anyhow::bail!("original auth material cleanup failure remains owned")
        }
        if let Some(probe) = &mut self.probe
            && !probe.poll_settle()?
        {
            return Ok(false);
        }
        if self
            .next_cleanup_retry
            .is_some_and(|next| std::time::Instant::now() < next)
        {
            return Ok(false);
        }
        if let Some(root) = &self.material {
            while let Some((name, file)) = self.material_files.last() {
                use theseus_host::fs::CreatedLeafRemovalResult as R;
                match root.remove_created_leaf_classified(name, file) {
                    R::Removed => {
                        self.material_files.pop();
                        self.next_cleanup_retry = None;
                    }
                    R::RetryableSettledDelete(error) => {
                        self.settled_cleanup_attempts =
                            self.settled_cleanup_attempts.saturating_add(1);
                        if self.settled_cleanup_history.is_none() {
                            self.settled_cleanup_history = Some(error)
                        }
                        // Later errors in this typed arm are terminal native delete
                        // results, with no unresolved owner; retain the first original.

                        self.next_cleanup_retry =
                            Some(std::time::Instant::now() + std::time::Duration::from_millis(25));
                        return Ok(false);
                    }
                    R::StickyAuthorityFailure(error) => {
                        self.cleanup_error = Some(MaterialCleanupBlocker::Sticky(error));
                        anyhow::bail!(
                            "auth material authority changed; original file and failure retained"
                        )
                    }
                    R::Unsettled(owner) => {
                        self.cleanup_error = Some(MaterialCleanupBlocker::Unsettled(owner));
                        anyhow::bail!(
                            "auth material cleanup unresolved; original native operation retained"
                        )
                    }
                }
            }
        }
        Ok(true)
    }
    pub fn is_settled(&self) -> bool {
        self.probe.as_ref().is_none_or(|p| p.settled())
            && self.material_files.is_empty()
            && self.cleanup_error.is_none()
    }
}
impl LaunchAuthority {
    pub fn prepare(&self) -> LaunchPreparation {
        let mut prepared = LaunchPreparation {
            command: None,
            probe: None,
            material: None,
            material_files: vec![],
            error: None,
            launch_fingerprint: self.view.fingerprint.clone(),
            cleanup_error: None,
            settled_cleanup_history: None,
            settled_cleanup_attempts: 0,
            next_cleanup_retry: None,
        };
        let result = (|| -> Result<()> {
            if let Some(command) = &self.command {
                prepared.command = Some(command.clone());
                return Ok(());
            }
            let ssh = self
                .ssh
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("missing SSH authority"))?;
            let ceiling = std::path::Path::new(&ssh.runtime_directory);
            let parent = match theseus_host::fs::HostDir::open(
                ceiling,
                theseus_host::fs::RootPolicy::AllComponentsNoFollow,
            ) {
                Ok(p) => p,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    theseus_host::fs::HostDir::create_private(ceiling)?
                }
                Err(e) => return Err(e.into()),
            };
            let mut nonce = [0u8; 16];
            theseus_host::random_bytes(&mut nonce)?;
            let name = format!(
                "session-{}",
                nonce.iter().map(|b| format!("{b:02x}")).collect::<String>()
            );
            let runtime = ceiling.join(&name);
            prepared.material = Some(parent.create_child(std::ffi::OsStr::new(&name))?);
            let root = prepared
                .material
                .as_ref()
                .expect("retained private material");
            let read = |path: &str| -> Result<Vec<u8>> {
                use std::io::Read;
                use theseus_host::fs::{HostDir, RelativePath, RootPolicy};
                let path = std::path::Path::new(path);
                let parent = HostDir::open(
                    path.parent()
                        .ok_or_else(|| anyhow::anyhow!("material parent missing"))?,
                    RootPolicy::AllComponentsNoFollow,
                )?;
                let name = path
                    .file_name()
                    .and_then(|p| p.to_str())
                    .ok_or_else(|| anyhow::anyhow!("invalid material basename"))?;
                let mut bytes = Vec::new();
                parent
                    .open_file(&RelativePath::parse(name)?)?
                    .take(65537)
                    .read_to_end(&mut bytes)?;
                ensure!(
                    !bytes.is_empty() && bytes.len() <= 65536,
                    "SSH material exceeds bound"
                );
                Ok(bytes)
            };
            let key = read(&ssh.private_key)?;
            ensure!(
                unencrypted_private_key(&key),
                "unencrypted raw private key required"
            );
            let pins = read(&ssh.host_keys)?;
            let pins_text = std::str::from_utf8(&pins)?;
            ensure!(
                pins_text.lines().all(|line| {
                    let parts = line.split_ascii_whitespace().collect::<Vec<_>>();
                    parts.len() == 3 && !parts[0].starts_with('@') && !parts[1].contains("cert-v01")
                }),
                "dedicated plain host-key pins required"
            );
            use std::io::Write;
            for (name, bytes) in [
                ("identity", key.as_slice()),
                ("host_keys", pins.as_slice()),
                ("global_empty", b"".as_slice()),
            ] {
                let file = root.create_file(&theseus_host::fs::RelativePath::parse(name)?)?;
                prepared.material_files.push((name.into(), file));
                prepared
                    .material_files
                    .last_mut()
                    .expect("created file owner")
                    .1
                    .write_all(bytes)?;
            }
            let command = openssh_subsystem_argv(
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
            prepared.probe = Some(super::provider::CommandProvider::new(
                "openssh-qualification".into(),
                command.clone(),
            )?);
            let probe = prepared.probe.as_mut().expect("probe owner");
            let mut version = NativeCommand::new(&ssh.program);
            version.environment = ChildEnvironment::Replace(vec![]);
            version.arg("-V");
            let (out, err) = probe.capture_command(version)?;
            ensure!(
                out.is_empty() || err.is_empty(),
                "ambiguous OpenSSH version output"
            );
            let actual = if err.is_empty() { out } else { err };
            ensure!(
                std::str::from_utf8(&actual)?.trim() == ssh.version,
                "OpenSSH version/build differs from explicit receipt"
            );
            let mut effective = command.clone();
            effective.args.insert(0, "-G".into());
            let (out, err) = probe.capture_command(effective)?;
            ensure!(
                err.is_empty(),
                "OpenSSH option qualification emitted diagnostics"
            );
            verify_effective_options(&command, std::str::from_utf8(&out)?)?;
            verify_transport_selection(&command, std::str::from_utf8(&out)?)?;
            prepared.command = Some(command);
            Ok(())
        })();
        if let Err(error) = result {
            prepared.error = Some(error)
        }
        prepared
    }
}
fn verify_effective_options(command: &NativeCommand, text: &str) -> Result<()> {
    let args = command
        .args
        .iter()
        .map(|s| {
            s.to_str()
                .ok_or_else(|| anyhow::anyhow!("nonUTF8 SSH option"))
        })
        .collect::<Result<Vec<_>>>()?;
    for pair in args.windows(2).filter(|p| p[0] == "-o") {
        let (key, value) = pair[1]
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("invalid SSH option"))?;
        let lines = text
            .lines()
            .filter_map(|line| line.split_once(' '))
            .filter(|(name, _)| name.eq_ignore_ascii_case(key))
            .collect::<Vec<_>>();
        ensure!(
            (lines.len() == 1 && equivalent_option(lines[0].1.trim(), value))
                || (lines.is_empty()
                    && value == "none"
                    && matches!(
                        key.to_ascii_lowercase().as_str(),
                        "proxycommand" | "proxyjump" | "pkcs11provider"
                    )),
            "OpenSSH effective option profile mismatch: {}",
            key
        );
    }
    Ok(())
}

fn valid_host(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    if host.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    if host.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return false;
    }
    let dns = host.strip_suffix('.').unwrap_or(host);
    dns.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label.as_bytes()[0].is_ascii_alphanumeric()
            && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

fn equivalent_option(actual: &str, expected: &str) -> bool {
    actual == expected || matches!((actual, expected), ("true", "yes") | ("false", "no"))
}
fn verify_transport_selection(command: &NativeCommand, text: &str) -> Result<()> {
    let args = command
        .args
        .iter()
        .map(|s| {
            s.to_str()
                .ok_or_else(|| anyhow::anyhow!("nonUTF8 SSH selection"))
        })
        .collect::<Result<Vec<_>>>()?;
    let option = |flag: &str| {
        args.windows(2)
            .find(|p| p[0] == flag)
            .map(|p| p[1])
            .ok_or_else(|| anyhow::anyhow!("missing SSH selection"))
    };
    let one = |name: &str, value: &str| -> Result<()> {
        let lines = text
            .lines()
            .filter_map(|l| l.split_once(' '))
            .filter(|(k, _)| *k == name)
            .collect::<Vec<_>>();
        ensure!(
            lines.len() == 1 && equivalent_option(lines[0].1.trim(), value),
            "OpenSSH effective selection differs: {}",
            name
        );
        Ok(())
    };
    one("user", option("-l")?)?;
    one("port", option("-p")?)?;
    one("identityfile", option("-i")?)?;
    one("requesttty", "no")?;
    one("sessiontype", "subsystem")?;
    one("host", args[args.len() - 2])?;
    one("hostname", args[args.len() - 2])?;
    for name in [
        "certificatefile",
        "localforward",
        "remoteforward",
        "dynamicforward",
        "remotecommand",
        "controlpath",
    ] {
        ensure!(
            !text
                .lines()
                .filter_map(|l| l.split_once(' '))
                .any(|(k, v)| k == name && v.trim() != "none"),
            "OpenSSH unwanted configured effect: {}",
            name
        );
    }
    Ok(())
}

fn unencrypted_private_key(key: &[u8]) -> bool {
    if key.windows(b"ENCRYPTED".len()).any(|s| s == b"ENCRYPTED") {
        return false;
    }
    if key.starts_with(b"-----BEGIN RSA PRIVATE KEY-----")
        || key.starts_with(b"-----BEGIN PRIVATE KEY-----")
    {
        return true;
    }
    if !key.starts_with(b"-----BEGIN OPENSSH PRIVATE KEY-----") {
        return false;
    }
    {
        // Decode only the bounded public container header, never format key bytes.
        let Some(newline) = key.iter().position(|b| *b == b'\n') else {
            return false;
        };
        let mut prefix = Vec::new();
        let mut acc = 0u32;
        let mut bits = 0;
        for c in key[newline + 1..]
            .iter()
            .copied()
            .filter(|b| !b.is_ascii_whitespace())
            .take(128)
        {
            let v = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => break,
            };
            acc = (acc << 6) | u32::from(v);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                prefix.push((acc >> bits) as u8)
            }
        }
        prefix.starts_with(b"openssh-key-v1\0\0\0\0\x04none\0\0\0\x04none")
            && !prefix.windows(3).any(|p| p == b"sk-")
            && !prefix.windows(9).any(|p| p == b"cert-v01@")
    }
}

#[cfg(test)]
mod qualification_tests {
    use super::*;
    #[test]
    fn shipping_loader_refuses_arbitrary_command_and_secret_profiles() {
        let raw=br#"{"kind":"theseus_native_launch","schema":1,"program":"/fixture","args":[],"os":"linux","target_id":"fixture","binding":"01010101010101010101010101010101","manifest_digest":"0202020202020202020202020202020202020202020202020202020202020202","workspace_marker":"03030303030303030303030303030303","provider_epoch":"0808080808080808080808080808080808080808080808080808080808080808"}"#;
        assert!(LaunchAuthority::decode(raw).is_err());
        assert!(!unencrypted_private_key(
            b"-----BEGIN OPENSSH PRIVATE KEY-----\nnot-key\n"
        ));
        assert!(!unencrypted_private_key(
            b"-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\n"
        ));
    }
    #[test]
    fn missing_changed_duplicate_or_truncated_effective_options_refuse() {
        let mut command = NativeCommand::new("/inert-ssh");
        command.args(["-o", "BatchMode=yes", "-o", "IdentityAgent=none"]);
        assert!(verify_effective_options(&command, "batchmode yes\nidentityagent none\n").is_ok());
        for output in [
            "batchmode yes\n",
            "batchmode no\nidentityagent none\n",
            "batchmode yes\nbatchmode yes\nidentityagent none\n",
            "batchmode yes\nidentityagent non",
        ] {
            assert!(verify_effective_options(&command, output).is_err())
        }
    }
}

#[cfg(feature = "native-catalog-test-support")]
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

#[cfg(all(test, unix))]
mod cleanup_retry_tests {
    use super::*;
    use std::{io::Write, os::unix::fs::PermissionsExt, path::Path};
    fn material(path: &Path) -> LaunchPreparation {
        let root = theseus_host::fs::HostDir::create_private(path).unwrap();
        let mut file = root
            .create_file(&theseus_host::fs::RelativePath::parse("identity").unwrap())
            .unwrap();
        file.write_all(b"synthetic key bytes").unwrap();
        file.sync_all().unwrap();
        LaunchPreparation {
            command: None,
            probe: None,
            material: Some(root),
            material_files: vec![("identity".into(), file)],
            error: None,
            launch_fingerprint: "00".repeat(32),
            cleanup_error: None,
            settled_cleanup_history: None,
            settled_cleanup_attempts: 0,
            next_cleanup_retry: None,
        }
    }
    fn deny_delete(preparation: &mut LaunchPreparation, path: &Path) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o500)).unwrap();
        let result = preparation.poll_settle();
        // Restore immediately even if the expected permission denial fails.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            !result.unwrap(),
            "actual unlink must fail under unprivileged directory0500"
        );
        assert_eq!(
            preparation.settled_cleanup_attempts, 1,
            "one deletion attempt per poll"
        );
        assert_eq!(
            preparation.settled_cleanup_error().unwrap().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert!(preparation.cleanup_error().is_none());
        assert!(!preparation.is_settled());
    }
    #[test]
    fn settled_auth_delete_retries_same_original_leaf_and_keeps_first_error_history() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("auth");
        let mut preparation = material(&path);
        deny_delete(&mut preparation, &path);
        let first = preparation.settled_cleanup_error().unwrap().raw_os_error();
        assert!(path.join("identity").exists());
        assert_eq!(preparation.material_files.len(), 1);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !preparation.poll_settle().unwrap() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(preparation.is_settled());
        assert!(!path.join("identity").exists());
        assert!(preparation.cleanup_error().is_none());
        assert_eq!(
            preparation.settled_cleanup_error().unwrap().raw_os_error(),
            first
        );
        assert_eq!(preparation.settled_cleanup_attempts, 1);
    }
    #[test]
    fn transient_auth_delete_followed_by_replacement_stays_revoked_after_original_returns() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("auth");
        let mut preparation = material(&path);
        deny_delete(&mut preparation, &path);
        std::fs::rename(path.join("identity"), path.join("original")).unwrap();
        std::fs::write(path.join("identity"), b"synthetic key bytes").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert!(preparation.poll_settle().is_err());
        assert!(preparation.cleanup_error().is_some());
        std::fs::remove_file(path.join("identity")).unwrap();
        std::fs::rename(path.join("original"), path.join("identity")).unwrap();
        assert!(preparation.poll_settle().is_err());
        assert!(!preparation.is_settled());
        assert!(path.join("identity").exists());
        assert_eq!(preparation.material_files.len(), 1);
        assert!(preparation.settled_cleanup_error().is_some());
    }
}
