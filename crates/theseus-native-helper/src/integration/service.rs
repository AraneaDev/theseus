//! Manager ownership retained by the existing helper worker, never reconstructed from a PID.
use std::{io, sync::atomic::AtomicBool, time::Instant};
use theseus_native_protocol::v2;
#[cfg(target_os = "macos")]
#[path = "service/launchd.rs"]
mod launchd;
#[cfg(windows)]
#[path = "service/scm.rs"]
mod scm;
#[derive(Clone)]
pub(crate) struct ServiceConfig {
    pub(crate) manifest: v2::IntegrationManifest,
    pub(crate) context: v2::IntegrationRuntimeContext,
}
pub(crate) struct ServiceRuntime {
    config: ServiceConfig,
    #[cfg(target_os = "linux")]
    manager: Option<theseus_host::services::systemd::SystemdManager>,
    #[cfg(target_os = "linux")]
    owner: Option<theseus_host::services::systemd::RegistrationOwner>,
    #[cfg(target_os = "macos")]
    manager: Option<theseus_host::services::launchd::LaunchdManager>,
    #[cfg(target_os = "macos")]
    owner: Option<theseus_host::services::launchd::RegistrationOwner>,
    #[cfg(target_os = "macos")]
    inspection_evidence: Vec<theseus_host::services::launchd::CommandEvidence>,
    #[cfg(windows)]
    manager: Option<theseus_host::services::scm::ScmManager>,
    #[cfg(windows)]
    owner: Option<theseus_host::services::scm::RegistrationOwner>,
    #[cfg(windows)]
    deletion: Option<theseus_host::services::scm::DeletionOwner>,
    #[cfg(any(target_os = "macos", windows))]
    sut_transition: Option<(String, String)>,
    binding: Option<v2::Digest>,
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    readiness: Option<theseus_host::services::readiness::LocalChallenge>,
    #[cfg(any(target_os = "macos", windows))]
    original_start_pid: Option<u32>,
    last_start: Option<(v2::LeaseId, v2::OperationId, v2::Digest)>,
    start_complete: bool,
    instance: Option<String>,
    install_receipt: Option<String>,
    observed_definition: Option<v2::ServiceDefinition>,
    pub(crate) errors: Vec<io::Error>,
    pub(crate) uncertain: bool,
}
impl ServiceRuntime {
    pub(crate) fn new(config: ServiceConfig) -> Self {
        Self {
            config,
            #[cfg(any(target_os = "linux", target_os = "macos", windows))]
            manager: None,
            #[cfg(any(target_os = "linux", target_os = "macos", windows))]
            owner: None,
            #[cfg(target_os = "macos")]
            inspection_evidence: Vec::new(),
            #[cfg(windows)]
            deletion: None,
            #[cfg(any(target_os = "macos", windows))]
            sut_transition: None,
            binding: None,
            #[cfg(any(target_os = "linux", target_os = "macos", windows))]
            readiness: None,
            #[cfg(any(target_os = "macos", windows))]
            original_start_pid: None,
            last_start: None,
            start_complete: false,
            instance: None,
            install_receipt: None,
            observed_definition: None,
            errors: vec![],
            uncertain: false,
        }
    }
    pub(crate) fn note_sut(
        &mut self,
        program: &v2::ProgramId,
        operation: v2::OperationId,
        outcome: &v2::Outcome,
    ) {
        #[cfg(any(target_os = "macos", windows))]
        if matches!(
            outcome,
            v2::Outcome::RunCompleted {
                status: v2::NativeStatus::PosixExit { code: 0 },
                ..
            } | v2::Outcome::RunCompleted {
                status: v2::NativeStatus::WindowsExit { code: 0 },
                ..
            }
        ) && ["configure", "install_target", "install_base", "uninstall"]
            .contains(&program.as_str())
        {
            self.sut_transition = Some((program.as_str().into(), operation.to_hex()));
        }
        if matches!(
            outcome,
            v2::Outcome::RunCompleted {
                status: v2::NativeStatus::PosixExit { code: 0 },
                ..
            } | v2::Outcome::RunCompleted {
                status: v2::NativeStatus::WindowsExit { code: 0 },
                ..
            }
        ) && ["install_base", "install_target"].contains(&program.as_str())
        {
            self.install_receipt = Some(operation.to_hex());
        }
    }
    #[cfg(target_os = "linux")]
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
            } = &self.config.manifest.service_authority
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
                    manager_unique_name: self.config.context.manager_unique_name.clone(),
                    manager_version: qualification.version.clone(),
                    unit: unit.clone(),
                    definition_root: definition_root.into(),
                    unit_load_paths: unit_load_roots.clone(),
                    ownership_id: self.config.context.ownership_id.clone(),
                    context_receipt: self.config.context.namespace_receipt.clone(),
                    allowed_start: allowed_actions.iter().any(|s| s == "start"),
                    allowed_stop: allowed_actions.iter().any(|s| s == "stop"),
                },
                deadline,
                cancel,
            )?);
        }
        Ok(self.manager.as_mut().unwrap())
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn inspect(
        &mut self,
        lease: v2::LeaseId,
        slot: &v2::ResourceSlot,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<v2::ServiceObservation> {
        use sha2::{Digest as _, Sha256};
        use theseus_host::services::systemd::SystemdDefinition;
        if slot != self.config.manifest.service_authority.slot() {
            return Err(io::Error::other("undeclared service slot"));
        }
        let observation = self.manager(deadline, cancel)?.inspect(deadline, cancel)?;
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
            manager_generation: self.config.context.manager_unique_name.clone(),
            complete: true,
            binding_digest: String::new(),
        };
        self.observed_definition = definition;
        self.instance = result.instance.clone();
        let mut hash = Sha256::new();
        hash.update(b"theseus.native_integration_v1.service_binding\0");
        hash.update(
            self.config
                .context
                .digest()
                .map_err(io::Error::other)?
                .to_hex(),
        );
        hash.update(lease.to_hex());
        hash.update(serde_json::to_vec(&result).map_err(io::Error::other)?);
        let digest = v2::Digest::from_bytes(hash.finalize().into());
        result.binding_digest = digest.to_hex();
        self.binding = Some(digest);
        Ok(result)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    pub(crate) fn inspect(
        &mut self,
        _lease: v2::LeaseId,
        _slot: &v2::ResourceSlot,
        _deadline: Instant,
        _cancel: &AtomicBool,
    ) -> io::Result<v2::ServiceObservation> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "systemd requires Linux",
        ))
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn action(
        &mut self,
        slot: &v2::ResourceSlot,
        prior: v2::Digest,
        action: v2::ServiceAction,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Option<v2::ServiceOutcome>> {
        use theseus_host::services::systemd::{ServiceAction, SystemdDefinition};
        if slot != self.config.manifest.service_authority.slot()
            || self.binding != Some(prior)
            || self.uncertain
        {
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
        if action == v2::ServiceAction::Start
            && self.readiness.is_none()
            && let Some(config) = &self.config.manifest.readiness
        {
            let v2::LocalEndpoint::UnixPath { path } = &config.endpoint else {
                return Err(io::Error::other("Unix readiness endpoint required"));
            };
            self.readiness = Some(theseus_host::services::readiness::LocalChallenge::bind(
                theseus_host::services::systemd::UnixPath::new(path.clone())?,
            )?);
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
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    pub(crate) fn action(
        &mut self,
        _slot: &v2::ResourceSlot,
        _prior: v2::Digest,
        _action: v2::ServiceAction,
        _deadline: Instant,
        _cancel: &AtomicBool,
    ) -> io::Result<Option<v2::ServiceOutcome>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "systemd requires Linux",
        ))
    }
    #[cfg(target_os = "linux")]
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
                    self.start_complete = true;
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
    #[cfg(target_os = "linux")]
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
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    pub(crate) fn poll(
        &mut self,
        _deadline: Instant,
        _cancel: &AtomicBool,
    ) -> io::Result<Option<v2::ServiceOutcome>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "systemd requires Linux",
        ))
    }
    #[cfg(any(target_os = "macos", windows))]
    fn bind_observation(
        &mut self,
        lease: v2::LeaseId,
        mut observation: v2::ServiceObservation,
        definition: Option<v2::ServiceDefinition>,
    ) -> io::Result<v2::ServiceObservation> {
        use sha2::{Digest as _, Sha256};
        self.observed_definition = definition;
        self.instance = observation.instance.clone();
        let mut hash = Sha256::new();
        hash.update(b"theseus.native_integration_v1.service_binding\0");
        hash.update(
            self.config
                .context
                .digest()
                .map_err(io::Error::other)?
                .to_hex(),
        );
        hash.update(lease.to_hex());
        hash.update(serde_json::to_vec(&observation).map_err(io::Error::other)?);
        let digest = v2::Digest::from_bytes(hash.finalize().into());
        observation.binding_digest = digest.to_hex();
        self.binding = Some(digest);
        Ok(observation)
    }
    pub(crate) fn note_action(&mut self, request: &v2::Request) -> io::Result<()> {
        if let v2::Request::ServiceAction {
            lease,
            operation,
            action: v2::ServiceAction::Start,
            ..
        } = request
        {
            if self.config.manifest.service_authority.platform() != v2::ManagerPlatform::Systemd
                && self.last_start.is_some()
            {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "native service Clean admits only one original Start",
                ));
            }
            self.last_start = Some((
                *lease,
                *operation,
                v2::request_digest(request).map_err(io::Error::other)?,
            ));
            self.start_complete = false;
        }
        Ok(())
    }
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    pub(crate) fn probe(
        &mut self,
        request: &v2::Request,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<v2::ReadinessObservation> {
        use sha2::{Digest as _, Sha256};
        use theseus_host::services::readiness as host;
        let v2::Request::ProbeReadiness {
            deadline_ms: _,
            lease,
            slot,
            operation,
            action_digest,
            prior_binding_digest,
            nonce,
            definition_role,
            artifact_role,
        } = request
        else {
            return Err(io::Error::other("invalid readiness request"));
        };
        if self.last_start != Some((*lease, *operation, *action_digest))
            || !self.start_complete
            || self.binding != Some(*prior_binding_digest)
            || self.uncertain
            || slot != self.config.manifest.service_authority.slot()
        {
            return Err(io::Error::other(
                "readiness original Start binding unproved",
            ));
        }
        #[cfg(target_os = "macos")]
        self.verify_readiness_owner()?;
        let config = self
            .config
            .manifest
            .readiness
            .as_ref()
            .ok_or_else(|| io::Error::other("readiness authority missing"))?;
        let expected = config
            .expectations
            .iter()
            .find(|e| &e.role == definition_role && &e.artifact == artifact_role)
            .ok_or_else(|| io::Error::other("undeclared readiness expectation"))?;
        let digest = expected.digest().map_err(io::Error::other)?;
        let expectation = host::ReadinessExpectation {
            artifact: if artifact_role == "base" {
                host::ArtifactRole::Base
            } else {
                host::ArtifactRole::Target
            },
            artifact_length: expected.artifact_length,
            artifact_sha256: *v2::Digest::parse(&expected.artifact_sha256)
                .map_err(io::Error::other)?
                .as_bytes(),
            version_literal: expected.version_literal.clone(),
            data_sha256: *v2::Digest::parse(&expected.data_sha256)
                .map_err(io::Error::other)?
                .as_bytes(),
            configuration_digest: *v2::Digest::parse(&expected.configuration_digest)
                .map_err(io::Error::other)?
                .as_bytes(),
        };
        let challenge = host::ReadinessChallenge {
            protocol: host::ReadyProtocol::TheseusFixtureReadyV1,
            lease: lease.to_hex(),
            slot: slot.as_str().into(),
            resource_binding_digest: *prior_binding_digest.as_bytes(),
            start_or_activation_operation: operation.to_hex(),
            action_digest: *action_digest.as_bytes(),
            nonce: *nonce.as_bytes(),
            expectation_digest: *digest.as_bytes(),
        };
        #[cfg(target_os = "linux")]
        let cooperative_instance = self
            .instance
            .clone()
            .ok_or_else(|| io::Error::other("original manager invocation unavailable"))?;
        #[cfg(any(target_os = "macos", windows))]
        let cooperative_instance = config
            .cooperative_instance
            .clone()
            .ok_or_else(|| io::Error::other("provider-attested installation instance missing"))?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let peer = host::PeerExpectation {
            uid: config
                .peer_uid
                .ok_or_else(|| io::Error::other("Unix readiness UID required"))?,
            gid: config
                .peer_gid
                .ok_or_else(|| io::Error::other("Unix readiness GID required"))?,
            cooperative_instance,
            instance_binding_digest: *prior_binding_digest.as_bytes(),
        };
        #[cfg(windows)]
        let peer = host::PipePeerExpectation {
            sid: host::Sid::new(
                config
                    .peer_sid
                    .clone()
                    .ok_or_else(|| io::Error::other("Windows readiness SID required"))?,
            )?,
            original_pid: self.original_start_pid.ok_or_else(|| {
                io::Error::other("original SCM Start PID correlation unavailable")
            })?,
            cooperative_instance,
            instance_binding_digest: *prior_binding_digest.as_bytes(),
        };
        let channel = self
            .readiness
            .as_mut()
            .ok_or_else(|| io::Error::other("original readiness channel missing"))?;
        #[cfg(any(target_os = "linux", windows))]
        let observed = channel.probe(&challenge, &expectation, &peer, deadline, cancel)?;
        #[cfg(target_os = "macos")]
        let observed = channel.probe_original(
            &challenge,
            &expectation,
            &peer,
            self.original_start_pid.ok_or_else(|| {
                io::Error::other("original launchd Start PID correlation unavailable")
            })?,
            deadline,
            cancel,
        )?;
        let response = observed.response.as_ref();
        let result = v2::ReadinessObservation {
            peer_sid: observed.peer_sid.as_ref().map(|sid| sid.as_str().into()),
            expectation: expected.clone(),
            slot: slot.clone(),
            operation: operation.to_hex(),
            action_digest: action_digest.to_hex(),
            resource_binding_digest: prior_binding_digest.to_hex(),
            challenge_digest: format!("{:x}", Sha256::digest(host::encode_challenge(&challenge)?)),
            expectation_digest: digest.to_hex(),
            state: match observed.state {
                host::ReadinessState::Ready => v2::ReadinessState::Ready,
                host::ReadinessState::Failed => v2::ReadinessState::Failed,
                host::ReadinessState::Incomplete => v2::ReadinessState::Incomplete,
            },
            authenticated_instance: observed.authenticated_instance,
            instance_binding_digest: v2::Digest::from_bytes(observed.instance_binding_digest)
                .to_hex(),
            peer_uid: observed.peer.map(|p| p.uid),
            peer_gid: observed.peer.map(|p| p.gid),
            artifact_sha256: response.map(|r| v2::Digest::from_bytes(r.artifact_sha256).to_hex()),
            version_literal: response.map(|r| r.version_literal.clone()),
            data_sha256: response.map(|r| v2::Digest::from_bytes(r.data_sha256).to_hex()),
            configuration_digest: response
                .map(|r| v2::Digest::from_bytes(r.configuration_digest).to_hex()),
            initialized: response.map(|r| r.initialized),
            response_digest: response
                .map(|r| host::encode_response(r).map(|b| format!("{:x}", Sha256::digest(b))))
                .transpose()?,
            failure: observed.failure.map(|f| format!("{f:?}")),
        };
        if observed.state != host::ReadinessState::Incomplete {
            let _ = channel.take_terminal_probe()?;
        }
        #[cfg(target_os = "macos")]
        self.verify_readiness_owner()?;
        Ok(result)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    pub(crate) fn probe(
        &mut self,
        _request: &v2::Request,
        _deadline: Instant,
        _cancel: &AtomicBool,
    ) -> io::Result<v2::ReadinessObservation> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "service readiness requires Linux",
        ))
    }
    pub(crate) fn cleanup_readiness(&mut self) -> io::Result<()> {
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        if let Some(channel) = &mut self.readiness {
            let _ = channel.abandon_probe();
            channel.cleanup()?;
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    pub(crate) fn unsettled(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.uncertain
                || self
                    .manager
                    .as_ref()
                    .and_then(|m| m.pending_action())
                    .is_some_and(|p| {
                        !matches!(
                            p,
                            theseus_host::services::systemd::OwnedActionProgress::Terminal { .. }
                        )
                    })
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            self.uncertain
        }
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn retire(&mut self) -> io::Result<()> {
        if let Some(manager) = &mut self.manager {
            manager.take_terminal_action()?;
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    pub(crate) fn retire(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod platform_regressions {
    #[test]
    fn scm_paused_and_pending_cannot_supply_complete_stopped_evidence() {
        // This is the gate used by the SCM observation passed to checkpoints.
        assert!(super::scm_runtime_complete(1));
        assert!(super::scm_runtime_complete(4));
        for state in [0, 2, 3, 5, 6, 7, 99] {
            assert!(!super::scm_runtime_complete(state), "SCM state {state}");
        }
    }
}

// This initial profile admits only a settled native stopped/running observation.
// Paused can retain a live process and cannot prove the shared Stopped predicate.
#[cfg(any(windows, test))]
fn scm_runtime_complete(state: u32) -> bool {
    matches!(state, 1 | 4)
}
