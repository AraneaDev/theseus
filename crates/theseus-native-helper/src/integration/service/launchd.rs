//! launchd uses an original retained plist and qualified exact-domain commands.
use super::*;
use theseus_host::services::launchd as host;
impl ServiceRuntime {
    fn manager(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<&mut host::LaunchdManager> {
        if self.manager.is_none() {
            let v2::ServiceAuthority::Launchd {
                domain,
                label,
                definition_root,
                exclusive_load_roots,
                launchctl,
                qualification: q,
                allowed_actions,
                ..
            } = self.config.authority()
            else {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "launchd authority required on macOS",
                ));
            };
            let v2::LaunchdDomain::User { uid } = domain else {
                return Err(io::Error::other(
                    "only exact supplied launchd User domain supported",
                ));
            };
            self.manager = Some(host::LaunchdManager::connect(
                host::LaunchdAuthority {
                    domain: host::LaunchdDomain::User(*uid),
                    label: label.clone(),
                    definition_root: definition_root.into(),
                    exclusive_load_roots: exclusive_load_roots.iter().map(Into::into).collect(),
                    launchctl: launchctl.into(),
                    qualification: host::PrintQualification {
                        absence_profile: q.absence_profile.as_ref().map(|a| {
                            host::AbsenceQualification {
                                profile: a.profile.clone(),
                                exit_code: a.exit_code,
                                stderr: a.stderr.as_bytes().to_vec(),
                                receipt: a.receipt_sha256.clone(),
                            }
                        }),
                        os_version: q.os_version.clone(),
                        os_build: q.os_build.clone(),
                        api_profile: q.api_profile.clone(),
                        receipt: q.receipt_sha256.clone(),
                        launchctl_fingerprint: q.launchctl_fingerprint.clone(),
                    },
                    ownership_id: self.config.context.ownership_id.clone(),
                    context_receipt: self
                        .config
                        .context
                        .namespace_receipt
                        .clone()
                        .expect("validated service context"),
                    allowed_start: allowed_actions.iter().any(|a| a == "start"),
                    allowed_stop: allowed_actions.iter().any(|a| a == "stop"),
                },
                deadline,
                cancel,
            )?);
        }
        Ok(self.manager.as_mut().unwrap())
    }
    pub(crate) fn inspect(
        &mut self,
        lease: v2::LeaseId,
        slot: &v2::ResourceSlot,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<v2::ServiceObservation> {
        use sha2::{Digest as _, Sha256};
        if slot != self.config.authority().slot() {
            return Err(io::Error::other("undeclared service slot"));
        }
        if self.inspection_evidence.len() >= 64 {
            return Err(io::Error::other(
                "bounded original launchd inspection evidence capacity exhausted",
            ));
        }
        self.ensure_readiness_channel()?;
        let expected_transition = self
            .sut_transition
            .as_ref()
            .filter(|(program, _)| program != "uninstall")
            .map(|(program, _)| self.transition_definition(program))
            .transpose()?;
        self.manager(deadline, cancel)?;
        if self.owner.is_some()
            && let Some((program, receipt)) = self.sut_transition.clone()
            && program != "uninstall"
        {
            let expected = self.transition_definition(&program)?;
            let observed = self.manager.as_mut().unwrap().inspect(deadline, cancel)?;
            let bytes = observed
                .definition_bytes
                .as_deref()
                .ok_or_else(|| io::Error::other("SUT plist missing"))?;
            if format!("{:x}", Sha256::digest(bytes)) != expected.definition_sha256 {
                self.uncertain = true;
                self.manager.as_mut().unwrap().revoke_context();
                return Err(io::Error::other(
                    "SUT replacement differs from admitted plist",
                ));
            }
            self.manager.as_mut().unwrap().admit_configuration(
                self.owner.as_mut().unwrap(),
                &raw_definition(&expected),
                bytes,
                receipt,
                deadline,
                cancel,
            )?;
            self.sut_transition = None;
        }
        let manager = self.manager.as_mut().unwrap();
        let observed = if let Some(owner) = self.owner.as_mut() {
            let uninstall_receipt = self
                .sut_transition
                .as_ref()
                .filter(|(program, _)| program == "uninstall")
                .map(|(_, receipt)| receipt.as_str());
            match manager.inspect_registration(owner, uninstall_receipt, deadline, cancel) {
                Ok(observed) => observed,
                Err(error) => {
                    if owner.is_revoked() || manager.is_revoked() {
                        self.uncertain = true;
                    }
                    return Err(error);
                }
            }
        } else if manager.pending_inspection().is_some() {
            manager.poll_inspect(deadline, cancel)?
        } else {
            manager.inspect(deadline, cancel)?
        };
        let digest = observed
            .definition_bytes
            .as_ref()
            .map(|b| format!("{:x}", Sha256::digest(b)));
        if let Some(owner) = &self.owner {
            let uninstall = self
                .sut_transition
                .as_ref()
                .is_some_and(|(program, _)| program == "uninstall");
            let original_digest = format!("{:x}", Sha256::digest(owner.definition_bytes()));
            if !uninstall
                && (owner.is_revoked()
                    || digest.as_ref() != Some(&original_digest)
                    || !matches!(observed.registration, host::Fact::Known(true)))
            {
                self.uncertain = true;
                self.manager.as_mut().unwrap().revoke_context();
            }
        }
        let loaded = match &observed.loaded {
            host::Fact::Known(l) => Some(l),
            _ => None,
        };
        // launchd can have byte-identical admitted definitions for different
        // reader roles. Preserve the accepted SUT key instead of inferring the
        // role from the first equal digest in the manifest.
        let definition = loaded.and_then(|loaded| {
            select_exact_definition(
                expected_transition
                    .as_ref()
                    .or(self.observed_definition.as_ref()),
                &self.config.manifest.definitions,
                |definition| {
                    Some(&definition.definition_sha256) == digest.as_ref()
                        && loaded.definition == raw_definition(definition)
                },
            )
        });
        if self.owner.is_none()
            && let (Some(d), Some(receipt)) = (&definition, &self.install_receipt)
        {
            self.owner = Some(self.manager.as_mut().unwrap().qualify_registration(
                &raw_definition(d),
                receipt.clone(),
                deadline,
                cancel,
            )?);
            if format!(
                "{:x}",
                Sha256::digest(self.owner.as_ref().unwrap().definition_bytes())
            ) != d.definition_sha256
            {
                self.uncertain = true;
                self.manager.as_mut().unwrap().revoke_context();
                return Err(io::Error::other(
                    "original installed plist differs from manifest",
                ));
            }
            self.sut_transition = None;
        }
        let mut evidence_hash = Sha256::new();
        evidence_hash.update(b"theseus.launchd.command_evidence\0");
        evidence_hash.update((observed.evidence.stdout.len() as u64).to_be_bytes());
        evidence_hash.update(&observed.evidence.stdout);
        evidence_hash.update(&observed.evidence.stderr);
        let platform = Some(v2::ServicePlatformFacts::Launchd {
            definition_present: observed.definition_present,
            loaded_registration_present: match observed.registration {
                host::Fact::Known(present) => Some(present),
                _ => None,
            },
            runtime_state: loaded.map(|l| match l.state {
                host::RuntimeState::Running => v2::LaunchdRuntimeState::Running,
                host::RuntimeState::NotRunning => v2::LaunchdRuntimeState::NotRunning,
                host::RuntimeState::Waiting => v2::LaunchdRuntimeState::Waiting,
            }),
            registration_generation_available: false,
            command_evidence_sha256: format!("{:x}", evidence_hash.finalize()),
            command_exit_code: observed.evidence.exit_code,
            output_truncated: observed.evidence.truncated,
        });
        if self.activation.from_sut
            && definition.is_some()
            && self.owner.as_ref().is_some_and(|o| !o.is_revoked())
            && !self.uncertain
        {
            self.activation.complete =
                loaded.is_some_and(|l| l.state == host::RuntimeState::Running);
        }
        if self.activation.complete
            && definition.is_some()
            && let Some(loaded) = loaded
            && loaded.state == host::RuntimeState::Running
            && let host::Fact::Known(pid) = loaded.pid
        {
            if pid == 0
                || self
                    .original_start_pid
                    .is_some_and(|original| original != pid)
            {
                self.uncertain = true;
                self.manager.as_mut().unwrap().revoke_context();
            } else if self.original_start_pid.is_none() {
                self.original_start_pid = Some(pid);
                self.instance = self
                    .config
                    .manifest
                    .readiness
                    .as_ref()
                    .and_then(|r| r.cooperative_instance.clone());
            }
        }
        if self.owner.is_some()
            && definition.is_none()
            && !self
                .sut_transition
                .as_ref()
                .is_some_and(|(program, _)| program == "uninstall")
        {
            self.uncertain = true;
            self.manager.as_mut().unwrap().revoke_context();
        }
        self.inspection_evidence.push(observed.evidence.clone());
        let complete =
            matches!(observed.registration, host::Fact::Known(false)) || loaded.is_some(); // Only separately qualified absence can complete without loaded facts.
        self.bind_observation(
            lease,
            v2::ServiceObservation {
                platform,
                instance: self.instance.clone(),
                slot: slot.clone(),
                registration_present: observed.definition_present || loaded.is_some(),
                definition_digest: definition.as_ref().map(|d| d.definition_sha256.clone()),
                configured: definition.is_some(),
                running: loaded.is_some_and(|l| l.state == host::RuntimeState::Running),
                origin: loaded.map(|l| l.origin.clone()),
                manager_generation: self
                    .config
                    .context
                    .manager_unique_name
                    .clone()
                    .expect("validated service context"),
                complete,
                binding_digest: String::new(),
            },
            definition,
        )
    }
    fn ensure_readiness_channel(&mut self) -> io::Result<()> {
        if self.readiness.is_none() {
            let config =
                self.config.manifest.readiness.as_ref().ok_or_else(|| {
                    io::Error::other("native launchd readiness prerequisite missing")
                })?;
            if config.cooperative_instance.is_none()
                || config.peer_uid.is_none()
                || config.peer_gid.is_none()
            {
                return Err(io::Error::other(
                    "native launchd readiness identity prerequisite missing",
                ));
            }
            let v2::LocalEndpoint::UnixPath { path } = &config.endpoint else {
                return Err(io::Error::other(
                    "launchd readiness requires exact UnixPath",
                ));
            };
            self.readiness = Some(theseus_host::services::readiness::LocalChallenge::bind(
                theseus_host::services::readiness::UnixPath::new(path.clone())?,
            )?);
        }
        self.renew_readiness_if_needed()
    }
    pub(crate) fn action(
        &mut self,
        slot: &v2::ResourceSlot,
        prior: v2::Digest,
        action: v2::ServiceAction,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Option<v2::ServiceOutcome>> {
        if slot != self.config.authority().slot() || self.binding != Some(prior) || self.uncertain {
            return Err(io::Error::other("service prior authority unproved"));
        }
        if action == v2::ServiceAction::Start {
            self.ensure_readiness_channel()?;
        }
        if self.owner.is_none() {
            let d = self
                .observed_definition
                .clone()
                .ok_or_else(|| io::Error::other("exact observed definition missing"))?;
            let receipt = self
                .install_receipt
                .clone()
                .ok_or_else(|| io::Error::other("original SUT install receipt missing"))?;
            self.owner = Some(self.manager(deadline, cancel)?.qualify_registration(
                &raw_definition(&d),
                receipt,
                deadline,
                cancel,
            )?);
            use sha2::{Digest as _, Sha256};
            if format!(
                "{:x}",
                Sha256::digest(self.owner.as_ref().unwrap().definition_bytes())
            ) != d.definition_sha256
            {
                self.uncertain = true;
                return Err(io::Error::other(
                    "original plist bytes differ from admitted manifest",
                ));
            }
        }
        let progress = self.manager.as_mut().unwrap().action(
            self.owner.as_mut().unwrap(),
            match action {
                v2::ServiceAction::Start => host::ServiceAction::Start,
                v2::ServiceAction::Stop => host::ServiceAction::Stop,
            },
            deadline,
            cancel,
        )?;
        Ok(self.progress(progress))
    }
    fn progress(&mut self, p: host::OwnedActionProgress) -> Option<v2::ServiceOutcome> {
        match p {
            host::OwnedActionProgress::Pending { .. } => None,
            host::OwnedActionProgress::Terminal {
                job,
                action,
                completion,
                result,
            } => Some(terminal_outcome(
                &mut self.activation.complete,
                action,
                completion,
                job,
                result,
            )),
            host::OwnedActionProgress::Unknown { reason, .. } => {
                self.uncertain = true;
                Some(v2::ServiceOutcome::Unknown { reason })
            }
        }
    }
    pub(crate) fn poll(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Option<v2::ServiceOutcome>> {
        let p = self
            .manager
            .as_mut()
            .ok_or_else(|| io::Error::other("original manager missing"))?
            .poll_action(deadline, cancel)?;
        Ok(self.progress(p))
    }
    pub(super) fn verify_readiness_owner(&mut self) -> io::Result<()> {
        let result = self
            .manager
            .as_mut()
            .ok_or_else(|| io::Error::other("original launchd manager missing"))?
            .verify_registration(
                self.owner
                    .as_mut()
                    .ok_or_else(|| io::Error::other("original launchd registration missing"))?,
            );
        if result.is_err() {
            self.uncertain = true;
        }
        result
    }
    pub(crate) fn retire(&mut self) -> io::Result<()> {
        if let Some(m) = &mut self.manager {
            m.take_terminal_action()?;
        }
        Ok(())
    }
    pub(crate) fn unsettled(&self) -> bool {
        self.uncertain
            || self.manager.as_ref().is_some_and(|m| {
                m.pending_inspection().is_some()
                    || m.pending_action()
                        .is_some_and(|p| !matches!(p, host::OwnedActionProgress::Terminal { .. }))
            })
    }
}
fn raw_definition(d: &v2::ServiceDefinition) -> host::LaunchdDefinition {
    host::LaunchdDefinition {
        executable: d.executable.clone(),
        arguments: d.arguments.clone(),
        working_directory: d.working_directory.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launchd_terminal_command_maps_typed_native_completion_and_only_start_authority() {
        for (completion, succeeds) in [
            (host::CommandCompletion::Exited(0), true),
            (host::CommandCompletion::Exited(7), false),
            (host::CommandCompletion::NoNormalExit, false),
            (host::CommandCompletion::SpawnFailed, false),
        ] {
            for action in [host::ServiceAction::Start, host::ServiceAction::Stop] {
                let mut started = false;
                let outcome = terminal_outcome(
                    &mut started,
                    action,
                    completion,
                    "original-command".into(),
                    "native diagnostic".into(),
                );
                assert_eq!(
                    matches!(outcome, v2::ServiceOutcome::Complete { .. }),
                    succeeds
                );
                assert_eq!(started, succeeds && action == host::ServiceAction::Start);
                if succeeds && action == host::ServiceAction::Stop {
                    let mut prior_start = true;
                    terminal_outcome(
                        &mut prior_start,
                        action,
                        completion,
                        "original-stop".into(),
                        "launchctl exit 0".into(),
                    );
                    assert!(!prior_start);
                }
            }
        }
    }
}

fn terminal_outcome(
    start_complete: &mut bool,
    action: host::ServiceAction,
    completion: host::CommandCompletion,
    job: String,
    result: String,
) -> v2::ServiceOutcome {
    use sha2::{Digest as _, Sha256};
    if completion.succeeded() {
        *start_complete = action == host::ServiceAction::Start;
        v2::ServiceOutcome::Complete {
            receipt: format!(
                "{:x}",
                Sha256::digest(format!("{job}\0{action:?}\0{completion:?}\0{result}"))
            ),
        }
    } else {
        v2::ServiceOutcome::Failed { reason: result }
    }
}
