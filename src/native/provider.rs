//! Provider effects require the existing retained RunProjector publication token.
use super::{launch::LaunchAuthority, model::*};
use crate::engine::projector::PublishedProvider;
use anyhow::{Result, ensure};
pub struct ValidatedBaseline {
    receipt: ProviderReceipt,
    launch_fingerprint: String,
    linux_gio: Option<ValidatedLinuxGioPrepare>,
}
/// Original owned provider completion, not a deserializable qualification claim.
#[derive(Clone)]
pub struct ValidatedLinuxGioPrepare {
    reference: theseus_native_protocol::v2::LinuxGioQualificationRef,
    context_digest: theseus_native_protocol::v2::Digest,
    ownership_id: String,
}
impl ValidatedLinuxGioPrepare {
    pub fn reference(&self) -> &theseus_native_protocol::v2::LinuxGioQualificationRef {
        &self.reference
    }
    pub(crate) fn validate_context(
        &self,
        context: &theseus_native_protocol::v2::IntegrationRuntimeContext,
    ) -> Result<()> {
        ensure!(
            context.linux_gio_qualification.as_ref() == Some(&self.reference)
                && context.digest()? == self.context_digest
                && context.ownership_id == self.ownership_id,
            "qualification differs from original owned Prepare context"
        );
        Ok(())
    }
}
impl ValidatedBaseline {
    pub fn launch_fingerprint(&self) -> &str {
        &self.launch_fingerprint
    }
    pub fn receipt(&self) -> &ProviderReceipt {
        &self.receipt
    }
    pub fn linux_gio_qualification(&self) -> Option<&ValidatedLinuxGioPrepare> {
        self.linux_gio.as_ref()
    }
}
pub trait NativeProvider {
    /// Implementations reserve the owner before prepare and retain partial native owners.
    fn invoke(&mut self, saved: PublishedProvider) -> Result<ProviderReceipt>;
    /// Query only the exact original identity after an unknown response.
    fn query(&mut self, original: &ProviderInvocation) -> Result<Option<ProviderReceipt>>;
}
pub fn validate_baseline(
    launch: &LaunchAuthority,
    invocation: &ProviderInvocation,
    receipt: ProviderReceipt,
) -> Result<ValidatedBaseline> {
    receipt.validate()?;
    ensure!(
        receipt.invocation_digest == invocation.digest()?
            && invocation.target_id == launch.view().target_id
            && invocation.baseline_label == launch.baseline_label()
            && invocation.launch_fingerprint == launch.view().fingerprint,
        "provider receipt differs from full pinned invocation context"
    );
    let b = launch.binding();
    ensure!(
        invocation.action == ProviderAction::Prepare
            && receipt.provider == invocation.provider
            && receipt.invocation_id == invocation.invocation_id,
        "provider preparation identity mismatch"
    );
    ensure!(
        receipt.provider == launch.provider_name()
            && receipt.manifest_digest == b.manifest_digest.to_hex(),
        "provider baseline differs from pinned launch catalog"
    );
    Ok(ValidatedBaseline {
        receipt,
        launch_fingerprint: launch.view().fingerprint.clone(),
        linux_gio: None,
    })
}

/// Explicit runtime provider command. It must implement exclusive ownership and
/// idempotent query of the full invocation identity; no user provider is executed
/// by source validation or by parsing configuration.
pub struct CommandProvider {
    name: String,
    command: theseus_host::process::NativeCommand,
    owner: Option<theseus_host::process::AcquiredChild>,
    spawn_failure: Option<theseus_host::process::SpawnFailure>,
    failures: Vec<std::io::Error>,
    original: Option<ProviderInvocation>,
    active_owner: Option<String>,
    observed_receipt: Option<ProviderReceipt>,
}
impl CommandProvider {
    pub fn new(name: String, command: theseus_host::process::NativeCommand) -> Result<Self> {
        super::model::text(&name, 256)?;
        ensure!(
            std::path::Path::new(&command.program).is_absolute()
                && matches!(
                    command.environment,
                    theseus_host::process::ChildEnvironment::Replace(_)
                ),
            "provider requires explicit absolute command and replacement environment"
        );
        let bounded =
            |value: &std::ffi::OsStr| value.len() <= 4096 && !value.as_encoded_bytes().contains(&0);
        ensure!(
            bounded(&command.program)
                && command.args.len() <= 62
                && command.args.iter().all(|a| bounded(a))
                && command
                    .cwd
                    .as_ref()
                    .is_none_or(|p| p.is_absolute() && bounded(p.as_os_str())),
            "provider command exceeds native bounds"
        );
        if let theseus_host::process::ChildEnvironment::Replace(entries) = &command.environment {
            let mut names = std::collections::BTreeSet::new();
            ensure!(
                entries.len() <= 64
                    && entries.iter().all(|(k, v)| !k.is_empty()
                        && bounded(k)
                        && bounded(v)
                        && !k.as_encoded_bytes().contains(&b'=')
                        && names.insert(k)),
                "invalid provider replacement environment"
            );
        }
        Ok(Self {
            name,
            command,
            owner: None,
            spawn_failure: None,
            failures: vec![],
            original: None,
            active_owner: None,
            observed_receipt: None,
        })
    }
    fn require_owned_receipt(&self, receipt: &ProviderReceipt) -> Result<()> {
        ensure!(
            self.settled() && self.observed_receipt.as_ref() == Some(receipt),
            "qualification requires the original owned provider completion"
        );
        Ok(())
    }
    /// The qualifier is minted only from bytes captured and joined by this
    /// original provider command. Generic validate_baseline never mints it.
    pub fn validate_original_baseline(
        &self,
        launch: &LaunchAuthority,
        invocation: &ProviderInvocation,
        receipt: ProviderReceipt,
    ) -> Result<ValidatedBaseline> {
        let mut baseline = validate_baseline(launch, invocation, receipt)?;
        if launch.integration_components().is_none() {
            return Ok(baseline);
        }
        let context: theseus_native_protocol::v2::IntegrationRuntimeContext =
            super::model::decode(baseline.receipt.receipt.as_bytes())?;
        let Some(reference) = context.linux_gio_qualification.as_ref() else {
            return Ok(baseline);
        };
        self.require_owned_receipt(&baseline.receipt)?;
        let expected = launch.provider_command()?;
        let environment_matches = match (&self.command.environment, &expected.environment) {
            (
                theseus_host::process::ChildEnvironment::Inherit,
                theseus_host::process::ChildEnvironment::Inherit,
            ) => true,
            (
                theseus_host::process::ChildEnvironment::Replace(a),
                theseus_host::process::ChildEnvironment::Replace(b),
            ) => a == b,
            _ => false,
        };
        ensure!(
            self.name == launch.provider_name()
                && self.command.program == expected.program
                && self.command.args == expected.args
                && self.command.cwd == expected.cwd
                && environment_matches,
            "qualification provider differs from pinned original command"
        );
        context.validate()?;
        reference.validate()?;
        ensure!(
            reference.prepare_invocation_id == invocation.invocation_id
                && reference.prepare_invocation_digest == invocation.digest()?
                && context.ownership_id == invocation.ownership_id
                && context.binding == baseline.receipt.binding
                && context.manifest_digest == baseline.receipt.manifest_digest
                && context.workspace_marker == baseline.receipt.workspace_marker
                && context.provider_epoch == baseline.receipt.reset_epoch,
            "qualification reference differs from original Prepare identity"
        );
        baseline.linux_gio = Some(ValidatedLinuxGioPrepare {
            reference: reference.clone(),
            context_digest: context.digest()?,
            ownership_id: context.ownership_id,
        });
        Ok(baseline)
    }
    /// Retained owners/errors are accessible after any partial failure.
    pub fn spawn_failure(&self) -> Option<&theseus_host::process::SpawnFailure> {
        self.spawn_failure.as_ref()
    }
    pub fn settled(&self) -> bool {
        self.owner.is_none()
            && self
                .spawn_failure
                .as_ref()
                .is_none_or(|f| matches!(f.cleanup, theseus_host::process::HostCleanup::Complete))
    }
    pub fn original(&self) -> Option<&ProviderInvocation> {
        self.original.as_ref()
    }
    pub fn poll_settle(&mut self) -> Result<bool> {
        if let Some(f) = &mut self.spawn_failure
            && let theseus_host::process::HostCleanup::Pending(cleanup) = &mut f.cleanup
        {
            match cleanup.try_complete() {
                Ok(true) => f.cleanup = theseus_host::process::HostCleanup::Complete,
                Ok(false) => return Ok(false),
                Err(e) => {
                    self.failures.push(e);
                    anyhow::bail!("provider partial spawn cleanup remains unresolved")
                }
            }
        }
        if let Some(owner) = &mut self.owner {
            if let Err(e) = owner.child.terminate_owned() {
                self.failures.push(e);
                return Ok(false);
            }
            let mut complete = true;
            if let Some(stdin) = &mut owner.stdin
                && let Err(e) = stdin.close()
            {
                self.failures.push(e);
                complete = false;
            }
            for reader in [&mut owner.stdout, &mut owner.stderr] {
                if let Err(e) = reader.cancel_and_complete() {
                    self.failures.push(e);
                    complete = false
                }
            }
            match owner.child.try_reap_and_release() {
                Ok(Some(_)) if complete => self.owner = None,
                Ok(_) => return Ok(false),
                Err(e) => {
                    self.failures.push(e);
                    return Ok(false);
                }
            }
        }
        Ok(self.settled())
    }
    pub(crate) fn capture_command(
        &mut self,
        command: theseus_host::process::NativeCommand,
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        ensure!(self.settled(), "previous native command remains owned");
        self.owner = Some(
            match theseus_host::process::OwnedChild::spawn(
                &command,
                theseus_host::process::InputMode::Null,
            ) {
                Ok(o) => o,
                Err(f) => {
                    self.spawn_failure = Some(f);
                    anyhow::bail!("provider spawn failed; original native failure retained")
                }
            },
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut eof = [false; 2];
        let mut status = None;
        loop {
            ensure!(
                std::time::Instant::now() < deadline,
                "provider deadline; original owner retained"
            );
            let owner = self.owner.as_mut().expect("owned until joined");
            let mut scratch = [0u8; 4096];
            for (index, reader, bytes) in [
                (0, &mut owner.stdout, &mut stdout),
                (1, &mut owner.stderr, &mut stderr),
            ] {
                if !eof[index] {
                    match reader.try_read(&mut scratch) {
                        Ok(0) => eof[index] = true,
                        Ok(n) => {
                            ensure!(
                                bytes.len() + n <= 16384,
                                "provider output exceeded bound; owner retained"
                            );
                            bytes.extend_from_slice(&scratch[..n])
                        }
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                            ) => {}
                        Err(e) => {
                            self.failures.push(e);
                            anyhow::bail!("provider I/O failed; source retained")
                        }
                    }
                }
            }
            if status.is_none()
                && owner.child.exited_reserved().map_err(|e| {
                    self.failures.push(e);
                    anyhow::anyhow!("provider exit observation failed; source retained")
                })?
            {
                match owner.child.try_reap_and_release() {
                    Ok(s) => status = s,
                    Err(e) => {
                        self.failures.push(e);
                        anyhow::bail!("provider join failed; original owner retained")
                    }
                }
            }
            if eof == [true, true] && status.is_some() {
                break;
            }
            std::thread::yield_now();
        }
        // Both native EOFs and the actual child join were observed.
        self.owner = None;
        ensure!(
            status.expect("joined").success(),
            "native command returned failure"
        );
        Ok((stdout, stderr))
    }

    fn call(
        &mut self,
        invocation: &ProviderInvocation,
        query: bool,
    ) -> Result<Option<ProviderReceipt>> {
        ensure!(self.settled(), "previous provider transport remains owned");
        ensure!(
            invocation.provider == self.name,
            "provider name differs from publication"
        );
        let mut command = self.command.clone();
        command.args([
            if query { "query" } else { "invoke" },
            &serde_json::to_string(invocation)?,
        ]);
        self.original = Some(invocation.clone());
        self.observed_receipt = None;
        let (stdout, _) = self.capture_command(command)?;
        if query && std::str::from_utf8(&stdout)?.trim() == "null" {
            return Ok(None);
        }
        let receipt: ProviderReceipt = super::model::decode(&stdout)?;
        receipt.validate()?;
        ensure!(
            receipt.provider == invocation.provider
                && receipt.invocation_id == invocation.invocation_id
                && receipt.invocation_digest == invocation.digest()?,
            "provider response identity mismatch"
        );
        self.observed_receipt = Some(receipt.clone());
        Ok(Some(receipt))
    }
}
impl NativeProvider for CommandProvider {
    fn invoke(&mut self, saved: PublishedProvider) -> Result<ProviderReceipt> {
        let invocation = saved.into_invocation();
        match invocation.action {
            ProviderAction::Prepare => {
                ensure!(
                    self.active_owner.is_none(),
                    "provider already reserved; query original owner"
                );
                self.active_owner = Some(invocation.ownership_id.clone());
            }
            ProviderAction::Reset | ProviderAction::Dispose => ensure!(
                self.active_owner.as_ref() == Some(&invocation.ownership_id),
                "provider disposition differs from reserved owner"
            ),
        }
        let receipt = self
            .call(&invocation, false)?
            .ok_or_else(|| anyhow::anyhow!("provider omitted required receipt"))?;
        if matches!(
            invocation.action,
            ProviderAction::Reset | ProviderAction::Dispose
        ) {
            self.active_owner = None;
        }
        Ok(receipt)
    }

    fn query(&mut self, original: &ProviderInvocation) -> Result<Option<ProviderReceipt>> {
        ensure!(
            self.original.as_ref().is_none_or(|i| i == original),
            "query must retain original provider identity"
        );
        if self.active_owner.is_none() {
            self.active_owner = Some(original.ownership_id.clone())
        }
        ensure!(
            self.active_owner.as_ref() == Some(&original.ownership_id),
            "provider query differs from reserved owner"
        );
        let receipt = self.call(original, true)?;
        if receipt.is_some()
            && matches!(
                original.action,
                ProviderAction::Reset | ProviderAction::Dispose
            )
        {
            self.active_owner = None;
        }
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_missing_probe_preserves_original_spawn_failure_until_settlement() {
        let mut command = theseus_host::process::NativeCommand::new(if cfg!(windows) {
            "C:\\theseus-inert-missing\\probe.exe"
        } else {
            "/theseus-inert-missing/probe"
        });
        command.environment = theseus_host::process::ChildEnvironment::Replace(vec![]);
        let mut provider = CommandProvider::new("inert-probe".into(), command.clone()).unwrap();
        assert!(provider.capture_command(command).is_err());
        assert!(provider.spawn_failure.is_some());
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !provider.poll_settle().unwrap() {
            assert!(std::time::Instant::now() < end);
            std::thread::yield_now()
        }
        assert!(
            provider.spawn_failure.as_ref().unwrap().error.kind() == std::io::ErrorKind::NotFound
        );
        assert!(provider.settled());
    }
}

#[cfg(test)]
mod qualification_origin_tests {
    use super::*;
    use theseus_host::process::NativeCommand;
    fn receipt() -> ProviderReceipt {
        ProviderReceipt {
            provider: "qualified-provider".into(),
            invocation_id: "original-prepare".into(),
            invocation_digest: "11".repeat(32),
            binding: "22".repeat(32),
            manifest_digest: "33".repeat(32),
            workspace_marker: "44".repeat(32),
            reset_epoch: "55".repeat(32),
            receipt: "context".into(),
        }
    }
    #[test]
    fn qualified_origin_requires_the_actual_owned_provider_completion() {
        let mut command = NativeCommand::new(if cfg!(windows) {
            "C:/fixture/provider.exe"
        } else {
            "/fixture/provider"
        });
        command.environment = theseus_host::process::ChildEnvironment::Replace(vec![]);
        let mut provider = CommandProvider::new("qualified-provider".into(), command).unwrap();
        let receipt = receipt();
        assert!(provider.require_owned_receipt(&receipt).is_err());
        provider.observed_receipt = Some(receipt.clone());
        assert!(provider.require_owned_receipt(&receipt).is_ok());
        let mut forged = receipt.clone();
        forged.invocation_id = "other-prepare".into();
        assert!(provider.require_owned_receipt(&forged).is_err());
        forged = receipt.clone();
        forged.receipt = "forged-context".into();
        assert!(provider.require_owned_receipt(&forged).is_err());
        provider.observed_receipt = None;
        assert!(provider.require_owned_receipt(&receipt).is_err());
    }
}
