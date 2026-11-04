//! Installer-only launchd effects on one explicitly reserved User-domain job.
#![cfg(target_os = "macos")]
use super::creator_state::CreatedFiles;
use super::definition;
use anyhow::{Result, ensure};
use std::{
    ffi::OsStr,
    fs::File,
    io,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use theseus_host::{
    fs::{HostDir, RootPolicy},
    process::{AcquiredChild, ChildEnvironment, InputMode, NativeCommand, OwnedChild},
    services::launchd as host,
};
use theseus_native_protocol::v2;

pub struct NativeServiceSut {
    authority: host::LaunchdAuthority,
    manager: host::LaunchdManager,
    root: HostDir,
    definitions: Vec<(v2::ServiceDefinition, Vec<u8>)>,
    uncertain: bool,
    transition_stopped: bool,
    retained_definition: CreatedFiles,
}
enum Mutation {
    Bootstrap,
    Bootout,
    Kickstart,
}
impl NativeServiceSut {
    pub fn connect(
        manifest: &v2::IntegrationManifest,
        context: &v2::IntegrationRuntimeContext,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<Self> {
        manifest.validate_context(context, v2::Os::Macos)?;
        let v2::ServiceAuthority::Launchd {
            domain: v2::LaunchdDomain::User { uid },
            label,
            definition_root,
            exclusive_load_roots,
            launchctl,
            qualification: q,
            ..
        } = manifest
            .service_authority
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("service authority missing"))?
        else {
            anyhow::bail!("explicit launchd User authority required")
        };
        let authority = host::LaunchdAuthority {
            domain: host::LaunchdDomain::User(*uid),
            label: label.clone(),
            definition_root: definition_root.into(),
            exclusive_load_roots: exclusive_load_roots.iter().map(Into::into).collect(),
            launchctl: launchctl.into(),
            qualification: host::PrintQualification {
                absence_profile: q
                    .absence_profile
                    .as_ref()
                    .map(|p| host::AbsenceQualification {
                        profile: p.profile.clone(),
                        exit_code: p.exit_code,
                        stderr: p.stderr.as_bytes().to_vec(),
                        receipt: p.receipt_sha256.clone(),
                    }),
                os_version: q.os_version.clone(),
                os_build: q.os_build.clone(),
                api_profile: q.api_profile.clone(),
                receipt: q.receipt_sha256.clone(),
                launchctl_fingerprint: q.launchctl_fingerprint.clone(),
            },
            ownership_id: context.ownership_id.clone(),
            context_receipt: context
                .namespace_receipt
                .clone()
                .ok_or_else(|| anyhow::anyhow!("service namespace receipt missing"))?,
            allowed_start: false,
            allowed_stop: false,
        };
        let definitions = manifest
            .definitions
            .iter()
            .map(|d| {
                use sha2::{Digest as _, Sha256};
                let bytes = definition::launchd_definition_bytes(label, d);
                ensure!(
                    format!("{:x}", Sha256::digest(&bytes)) == d.definition_sha256,
                    "fixture plist commitment differs"
                );
                host::validate_plist(&bytes, &authority, &raw(d))?;
                Ok((d.clone(), bytes))
            })
            .collect::<Result<Vec<_>>>()?;
        let manager = host::LaunchdManager::connect(authority.clone(), deadline, cancel)?;
        let root = HostDir::open(
            Path::new(definition_root),
            RootPolicy::AllComponentsNoFollow,
        )?;
        Ok(Self {
            authority,
            manager,
            root,
            definitions,
            uncertain: false,
            transition_stopped: false,
            retained_definition: CreatedFiles::default(),
        })
    }
    fn leaf(&self) -> String {
        format!("{}.plist", self.authority.label)
    }
    fn current(&mut self) -> Result<Option<(File, Vec<u8>)>> {
        let leaf = self.leaf();
        self.retained_definition
            .current(&self.root, &self.authority.definition_root, &leaf)
    }
    fn verify_root(&mut self) -> Result<()> {
        self.retained_definition
            .verify_root(&self.root, &self.authority.definition_root)
    }
    fn verify_loaded(
        &mut self,
        expected: Option<&v2::ServiceDefinition>,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<()> {
        self.verify_root()?;
        let observation = self.manager.inspect(deadline, cancel)?;
        if let Some(d) = expected {
            ensure!(
                observation.registration == host::Fact::Known(true),
                "original fixture job registration unproved"
            );
            ensure!(
                matches!(observation.loaded, host::Fact::Known(ref loaded) if loaded.label == self.authority.label && loaded.domain == self.authority.domain && loaded.origin == self.authority.plist_path().to_string_lossy() && loaded.definition == raw(d)),
                "original fixture loaded configuration differs"
            );
        } else {
            ensure!(
                observation.registration == host::Fact::Known(false),
                "qualified native registration absence required"
            );
        }
        Ok(())
    }
    fn choose(&self, definition: &v2::ServiceDefinition) -> Result<Vec<u8>> {
        self.definitions
            .iter()
            .find(|(d, _)| d == definition)
            .map(|(_, b)| b.clone())
            .ok_or_else(|| anyhow::anyhow!("undeclared fixture definition"))
    }
    fn publish_definition(&mut self, bytes: &[u8]) -> Result<()> {
        let leaf = self.leaf();
        self.retained_definition.publish(&self.root, &leaf, bytes)?;
        self.current()?
            .ok_or_else(|| anyhow::anyhow!("original published plist missing"))?;
        Ok(())
    }
    pub fn has_owned_registration(&self) -> bool {
        self.retained_definition.has_owners()
    }
    pub fn creation_evidence(&self) -> serde_json::Value {
        let mut evidence = self.retained_definition.evidence();
        evidence["sut_mutation_unresolved"] = self.uncertain.into();
        evidence
    }
    fn command(
        &mut self,
        mutation: Mutation,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<()> {
        self.verify_root()?;
        // Read-only qualified observation immediately precedes the SUT effect.
        // The original launchctl file is pinned and checked by the Host manager.
        let mut command = NativeCommand::new(&self.authority.launchctl);
        command.environment = ChildEnvironment::Replace(vec![]);
        command.current_dir("/");
        match mutation {
            Mutation::Bootstrap => {
                command
                    .arg("bootstrap")
                    .arg(self.authority.domain.target())
                    .arg(self.authority.plist_path());
            }
            Mutation::Bootout => {
                command.arg("bootout").arg(self.authority.service_target());
            }
            Mutation::Kickstart => {
                command
                    .arg("kickstart")
                    .arg(self.authority.service_target());
            }
        }
        let leaf = self.leaf();
        let manager = &mut self.manager;
        self.retained_definition.dispatch(
            &self.root,
            &self.authority.definition_root,
            &leaf,
            || {
                manager.inspect(deadline, cancel)?;
                Ok(())
            },
            |_retained_dispatch_witness| {
                let mut owner = LaunchCommand::spawn(&command)?;
                owner.complete(deadline, cancel)
            },
        )?;
        self.verify_root()?;
        Ok(())
    }
    pub fn install(
        &mut self,
        definition: &v2::ServiceDefinition,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<()> {
        ensure!(
            !self.uncertain,
            "original fixture mutation unresolved; no replay"
        );
        let bytes = self.choose(definition)?;
        ensure!(
            self.current()?.is_none(),
            "preexisting fixture plist cannot be adopted"
        );
        self.verify_loaded(None, deadline, cancel)?;
        self.uncertain = true;
        self.publish_definition(&bytes)?;
        self.command(Mutation::Bootstrap, deadline, cancel)?;
        self.verify_loaded(Some(definition), deadline, cancel)?;
        self.uncertain = false;
        Ok(())
    }
    pub fn configure(
        &mut self,
        definition: &v2::ServiceDefinition,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<()> {
        ensure!(
            !self.uncertain,
            "original fixture mutation unresolved; no replay"
        );
        let bytes = self.choose(definition)?;
        let (_, original) = self
            .current()?
            .ok_or_else(|| anyhow::anyhow!("original fixture plist missing"))?;
        let prior = self
            .definitions
            .iter()
            .find(|(_, b)| b == &original)
            .unwrap()
            .0
            .clone();
        if !self.transition_stopped {
            self.verify_loaded(Some(&prior), deadline, cancel)?;
            self.uncertain = true;
            self.command(Mutation::Bootout, deadline, cancel)?;
        } else {
            self.uncertain = true;
        }
        self.verify_loaded(None, deadline, cancel)?;
        ensure!(
            self.current()?.is_some_and(|(_, now)| now == original),
            "original fixture plist changed during bootout"
        );
        self.publish_definition(&bytes)?;
        self.command(Mutation::Bootstrap, deadline, cancel)?;
        self.verify_loaded(Some(definition), deadline, cancel)?;
        self.uncertain = false;
        Ok(())
    }
    pub fn stop_for_transition(&mut self, deadline: Instant, cancel: &AtomicBool) -> Result<()> {
        ensure!(
            !self.uncertain && !self.transition_stopped,
            "original SUT transition unresolved"
        );
        let (_, original) = self
            .current()?
            .ok_or_else(|| anyhow::anyhow!("original configured plist missing"))?;
        let prior = self
            .definitions
            .iter()
            .find(|(_, bytes)| bytes == &original)
            .unwrap()
            .0
            .clone();
        self.verify_loaded(Some(&prior), deadline, cancel)?;
        self.uncertain = true;
        self.command(Mutation::Bootout, deadline, cancel)?;
        self.verify_loaded(None, deadline, cancel)?;
        ensure!(
            self.current()?.is_some_and(|(_, bytes)| bytes == original),
            "original plist changed while stopping"
        );
        self.transition_stopped = true;
        self.uncertain = false;
        Ok(())
    }
    pub fn start_after_transition(&mut self, deadline: Instant, cancel: &AtomicBool) -> Result<()> {
        ensure!(
            !self.uncertain && self.transition_stopped,
            "original stopped SUT transition required"
        );
        let (_, bytes) = self
            .current()?
            .ok_or_else(|| anyhow::anyhow!("configured replacement plist missing"))?;
        let definition = self
            .definitions
            .iter()
            .find(|(_, original)| original == &bytes)
            .unwrap()
            .0
            .clone();
        self.verify_loaded(Some(&definition), deadline, cancel)?;
        self.uncertain = true;
        self.command(Mutation::Kickstart, deadline, cancel)?;
        loop {
            ensure!(
                !cancel.load(Ordering::Acquire) && Instant::now() < deadline,
                "original SUT kickstart remains pending"
            );
            self.verify_loaded(Some(&definition), deadline, cancel)?;
            let observed = self.manager.inspect(deadline, cancel)?;
            if matches!(observed.loaded, host::Fact::Known(ref loaded) if loaded.state == host::RuntimeState::Running)
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.transition_stopped = false;
        self.uncertain = false;
        Ok(())
    }
    pub fn uninstall(&mut self, deadline: Instant, cancel: &AtomicBool) -> Result<()> {
        ensure!(
            !self.uncertain,
            "original fixture mutation unresolved; no replay"
        );
        let (file, original) = self
            .current()?
            .ok_or_else(|| anyhow::anyhow!("original fixture plist missing"))?;
        let prior = self
            .definitions
            .iter()
            .find(|(_, b)| b == &original)
            .unwrap()
            .0
            .clone();
        self.verify_loaded(Some(&prior), deadline, cancel)?;
        self.uncertain = true;
        self.command(Mutation::Bootout, deadline, cancel)?;
        self.verify_loaded(None, deadline, cancel)?;
        self.root
            .remove_created_leaf(OsStr::new(&self.leaf()), &file)?;
        self.retained_definition.clear_committed();
        self.verify_loaded(None, deadline, cancel)?;
        self.uncertain = false;
        Ok(())
    }
}
impl Drop for NativeServiceSut {
    fn drop(&mut self) {
        if self.uncertain {
            self.retained_definition.retain_unresolved();
        }
    }
}
fn raw(d: &v2::ServiceDefinition) -> host::LaunchdDefinition {
    host::LaunchdDefinition {
        executable: d.executable.clone(),
        arguments: d.arguments.clone(),
        working_directory: d.working_directory.clone(),
    }
}
struct LaunchCommand {
    child: Option<AcquiredChild>,
    eof: [bool; 2],
    output: [Vec<u8>; 2],
    truncated: bool,
}
impl LaunchCommand {
    fn spawn(command: &NativeCommand) -> Result<Self> {
        let child = match OwnedChild::spawn(command, InputMode::Null) {
            Ok(child) => child,
            Err(failure) => {
                let message = failure.to_string();
                if !matches!(
                    failure.cleanup,
                    theseus_host::process::HostCleanup::Complete
                ) {
                    std::mem::forget(failure);
                }
                return Err(io::Error::other(message).into());
            }
        };
        Ok(Self {
            child: Some(child),
            eof: [false; 2],
            output: [vec![], vec![]],
            truncated: false,
        })
    }
    fn complete(&mut self, deadline: Instant, cancel: &AtomicBool) -> Result<()> {
        loop {
            ensure!(
                !cancel.load(Ordering::Acquire) && Instant::now() < deadline,
                "original fixture launchctl operation pending after deadline/cancellation"
            );
            let child = self.child.as_mut().unwrap();
            for (index, pipe) in [(0, &mut child.stdout), (1, &mut child.stderr)] {
                if self.eof[index] {
                    continue;
                }
                for _ in 0..8 {
                    let mut buffer = [0; 4096];
                    match pipe.try_read(&mut buffer) {
                        Ok(0) => {
                            self.eof[index] = true;
                            break;
                        }
                        Ok(count) => {
                            let retained = self.output[0].len() + self.output[1].len();
                            let keep = count.min(16384usize.saturating_sub(retained));
                            self.output[index].extend_from_slice(&buffer[..keep]);
                            self.truncated |= keep != count;
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(error) => return Err(error.into()),
                    }
                }
            }
            if self.eof == [true; 2]
                && child.child.exited_reserved()?
                && let Some(status) = child.child.try_reap_and_release()?
            {
                self.child.take();
                ensure!(
                    !self.truncated && status.success(),
                    "fixture launchctl failed or exceeded output bound: {status}; stdout={:?}; stderr={:?}",
                    String::from_utf8_lossy(&self.output[0]),
                    String::from_utf8_lossy(&self.output[1])
                );
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
impl Drop for LaunchCommand {
    fn drop(&mut self) {
        if let Some(child) = self.child.take() {
            std::mem::forget(child);
        }
    }
}
