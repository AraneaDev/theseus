//! systemd uses an original retained D-Bus manager and qualified unit registration.
use super::*;
impl ServiceRuntime {
    fn manager(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<&mut theseus_host::services::systemd::SystemdManager> {
        use theseus_host::services::systemd::*;
        if self.manager.is_none() {
            let v2::ServiceAuthority::Systemd {
                scope,
                bus_endpoint,
                expected_peer_uid,
                expected_manager_uid,
                qualification,
                namespace,
                definition_root,
                unit,
                allowed_actions,
                ..
            } = self.config.authority()
            else {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "service platform requires its native OS",
                ));
            };
            let v2::LocalEndpoint::UnixPath { path } = bus_endpoint else {
                return Err(io::Error::other("systemd requires UnixPath"));
            };
            let v2::RegistrationNamespace::ExclusiveOwnedRegistration {
                unit_load_roots, ..
            } = namespace;
            self.manager = Some(SystemdManager::connect(
                SystemdAuthority {
                    endpoint: UnixPath::new(path.clone())?,
                    scope: match scope {
                        v2::ServiceScope::User => SystemdScope::User,
                        v2::ServiceScope::System => SystemdScope::System,
                    },
                    bus_uid: *expected_peer_uid,
                    manager_uid: *expected_manager_uid,
                    manager_unique_name: self
                        .config
                        .context
                        .manager_unique_name
                        .clone()
                        .expect("validated service context"),
                    manager_version: qualification.version.clone(),
                    unit: unit.clone(),
                    definition_root: definition_root.into(),
                    unit_load_paths: unit_load_roots.clone(),
                    ownership_id: self.config.context.ownership_id.clone(),
                    context_receipt: self
                        .config
                        .context
                        .namespace_receipt
                        .clone()
                        .expect("validated service context"),
                    allowed_start: allowed_actions.iter().any(|s| s == "start"),
                    allowed_stop: allowed_actions.iter().any(|s| s == "stop"),
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
        use theseus_host::services::systemd::SystemdDefinition;
        use v2::InspectFailure as F;
        if slot != self.config.authority().slot() {
            return classify(
                F::SlotUndeclared,
                Err(io::Error::other("undeclared service slot")),
            );
        }
        let readiness_step = if self.readiness.is_none() {
            F::ReadinessBind
        } else {
            F::ReadinessRenewal
        };
        classify(readiness_step, self.ensure_readiness_channel())?;
        classify(
            F::ManagerConnect,
            self.manager(deadline, cancel).map(|_| ()),
        )?;
        let observation = classify(
            F::ManagerQuery,
            self.manager.as_mut().unwrap().inspect(deadline, cancel),
        )?;
        let actual_definition_digest = observation
            .definition_bytes
            .as_ref()
            .map(|bytes| format!("{:x}", Sha256::digest(bytes)));
        let definition = observation
            .loaded
            .as_ref()
            .and_then(|loaded| {
                self.config.manifest.definitions.iter().find(|d| {
                    actual_definition_digest.as_ref() == Some(&d.definition_sha256)
                        && loaded.definition
                            == Some(SystemdDefinition {
                                executable: d.executable.clone(),
                                arguments: d.arguments.clone(),
                                environment: d.environment.clone(),
                                working_directory: d.working_directory.clone(),
                            })
                })
            })
            .cloned();
        if let Some((program, receipt)) = self.sut_transition.clone()
            && program != "uninstall"
        {
            let expected = classify(
                F::TransitionDefinition,
                self.transition_definition(&program),
            )?;
            if definition.as_ref() == Some(&expected) {
                let raw = SystemdDefinition {
                    executable: expected.executable.clone(),
                    arguments: expected.arguments.clone(),
                    environment: expected.environment.clone(),
                    working_directory: expected.working_directory.clone(),
                };
                if let Some(owner) = self.owner.as_mut() {
                    let bytes = classify(
                        F::TransitionAdmission,
                        observation
                            .definition_bytes
                            .as_deref()
                            .ok_or_else(|| io::Error::other("SUT definition bytes missing")),
                    )?;
                    classify(
                        F::TransitionAdmission,
                        self.manager
                            .as_mut()
                            .unwrap()
                            .admit_configuration(owner, &raw, bytes, receipt, deadline, cancel),
                    )?;
                } else {
                    self.owner = Some(classify(
                        F::RegistrationQualification,
                        self.manager
                            .as_mut()
                            .unwrap()
                            .qualify_registration(&raw, receipt, deadline, cancel),
                    )?);
                }
            } else if self.owner.is_some() {
                // Preserve the complete known SUT mismatch as an observation,
                // while permanently revoking control of the original owner.
                self.uncertain = true;
                self.manager.as_mut().unwrap().revoke_context();
            }
            self.sut_transition = None;
        }
        let mut result = v2::ServiceObservation {
            platform: None,
            instance: observation
                .loaded
                .as_ref()
                .filter(|l| !l.invocation_id.is_empty())
                .map(|l| l.invocation_id.iter().map(|b| format!("{b:02x}")).collect()),
            slot: slot.clone(),
            registration_present: observation.definition_present
                || observation
                    .loaded
                    .as_ref()
                    .is_some_and(|l| l.load_state != "not-found"),
            definition_digest: definition.as_ref().map(|d| d.definition_sha256.clone()),
            configured: definition.is_some(),
            running: observation
                .loaded
                .as_ref()
                .is_some_and(|l| l.active_state == "active" && l.sub_state == "running"),
            origin: observation.loaded.as_ref().map(|l| l.fragment_path.clone()),
            manager_generation: self
                .config
                .context
                .manager_unique_name
                .clone()
                .expect("validated service context"),
            complete: true,
            binding_digest: String::new(),
        };
        if let Some(owner) = self.owner.as_mut()
            && !self.uncertain
            && !self
                .sut_transition
                .as_ref()
                .is_some_and(|(p, _)| p == "uninstall")
        {
            classify(
                F::RegistrationQualification,
                self.manager
                    .as_mut()
                    .unwrap()
                    .verify_registration(owner, deadline, cancel),
            )?;
        }
        if self.activation.from_sut
            && result.running
            && result.configured
            && result.instance.is_some()
            && !self.uncertain
        {
            self.activation.complete = true;
        }
        self.observed_definition = definition;
        self.instance = result.instance.clone();
        let mut hash = Sha256::new();
        hash.update(b"theseus.native_integration_v1.service_binding\0");
        hash.update(
            classify(
                F::ObservationBinding,
                self.config.context.digest().map_err(io::Error::other),
            )?
            .to_hex(),
        );
        hash.update(lease.to_hex());
        hash.update(classify(
            F::ObservationBinding,
            serde_json::to_vec(&result).map_err(io::Error::other),
        )?);
        let digest = v2::Digest::from_bytes(hash.finalize().into());
        result.binding_digest = digest.to_hex();
        self.binding = Some(digest);
        Ok(result)
    }
    fn ensure_readiness_channel(&mut self) -> io::Result<()> {
        if self.readiness.is_none()
            && let Some(config) = &self.config.manifest.readiness
        {
            let v2::LocalEndpoint::UnixPath { path } = &config.endpoint else {
                return Err(io::Error::other("Unix readiness endpoint required"));
            };
            self.readiness = Some(theseus_host::services::readiness::LocalChallenge::bind(
                theseus_host::services::systemd::UnixPath::new(path.clone())?,
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
        use theseus_host::services::systemd::{ServiceAction, SystemdDefinition};
        if slot != self.config.authority().slot() || self.binding != Some(prior) || self.uncertain {
            return Err(io::Error::other("service prior authority unproved"));
        }
        if self.owner.is_none() {
            let definition = self
                .observed_definition
                .clone()
                .ok_or_else(|| io::Error::other("service exact observed definition missing"))?;
            let receipt = self
                .install_receipt
                .clone()
                .ok_or_else(|| io::Error::other("original SUT install receipt missing"))?;
            let raw = SystemdDefinition {
                executable: definition.executable,
                arguments: definition.arguments,
                environment: definition.environment,
                working_directory: definition.working_directory,
            };
            self.owner = Some(
                self.manager(deadline, cancel)?
                    .qualify_registration(&raw, receipt, deadline, cancel)?,
            );
            use sha2::{Digest as _, Sha256};
            let expected = self
                .observed_definition
                .as_ref()
                .ok_or_else(|| io::Error::other("observed definition missing"))?;
            if format!(
                "{:x}",
                Sha256::digest(self.owner.as_ref().unwrap().definition_bytes())
            ) != expected.definition_sha256
            {
                self.uncertain = true;
                return Err(io::Error::other(
                    "original definition bytes differ from admitted manifest",
                ));
            }
        }
        if action == v2::ServiceAction::Start {
            self.ensure_readiness_channel()?;
        }
        let owner = self
            .owner
            .as_mut()
            .ok_or_else(|| io::Error::other("service original install ownership missing"))?;
        let manager = self
            .manager
            .as_mut()
            .ok_or_else(|| io::Error::other("service manager missing"))?;
        let progress = manager.action(
            owner,
            match action {
                v2::ServiceAction::Start => ServiceAction::Start,
                v2::ServiceAction::Stop => ServiceAction::Stop,
            },
            deadline,
            cancel,
        )?;
        Ok(self.progress(progress))
    }
    fn progress(
        &mut self,
        progress: theseus_host::services::systemd::OwnedActionProgress,
    ) -> Option<v2::ServiceOutcome> {
        use sha2::{Digest as _, Sha256};
        use theseus_host::services::systemd::OwnedActionProgress;
        match progress {
            OwnedActionProgress::Pending { .. } => None,
            OwnedActionProgress::Terminal { job, result } => {
                if result == "done" {
                    self.activation.complete = true;
                    Some(v2::ServiceOutcome::Complete {
                        receipt: format!("{:x}", Sha256::digest(format!("{job}\0{result}"))),
                    })
                } else {
                    Some(v2::ServiceOutcome::Failed { reason: result })
                }
            }
            OwnedActionProgress::Unknown { reason, .. } => {
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
            .ok_or_else(|| io::Error::other("missing original manager"))?
            .poll_action(deadline, cancel)?;
        Ok(self.progress(p))
    }
    pub(crate) fn retire(&mut self) -> io::Result<()> {
        if let Some(manager) = &mut self.manager {
            manager.take_terminal_action()?;
        }
        Ok(())
    }
}
