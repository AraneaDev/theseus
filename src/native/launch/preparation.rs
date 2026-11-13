//! Launch preparation: staged SSH material, qualification probes, and cleanup retry.
use anyhow::{Result, ensure};
use theseus_host::process::{ChildEnvironment, NativeCommand};

use super::ssh::{unencrypted_private_key, verify_effective_options, verify_transport_selection};
use super::{
    LaunchAuthority, LaunchPreparation, MaterialCleanupBlocker, SshMaterialPaths,
    openssh_subsystem_argv,
};

impl LaunchPreparation {
    pub fn launch_fingerprint(&self) -> &str {
        &self.launch_fingerprint
    }
    pub fn probe(&self) -> Option<&crate::native::provider::CommandProvider> {
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
    /// Only the unit tests below observe it.
    #[cfg(test)]
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
            #[cfg(feature = "native-integration-test-support")]
            if let Some(pins) = &self.fixture_executables {
                pins.verify()?;
            }
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
            prepared.probe = Some(crate::native::provider::CommandProvider::new(
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
