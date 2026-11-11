//! Manager ownership retained by the existing helper worker, never reconstructed from a PID.
use std::{io, sync::atomic::AtomicBool, time::Instant};
use theseus_native_protocol::v2;
#[cfg(target_os = "macos")]
#[path = "service/launchd.rs"]
mod launchd;
#[cfg(windows)]
#[path = "service/scm.rs"]
mod scm;
/// A failed inspection step with its closed classification. Display and the
/// retained error stay the original error; only `failure`, a fixed protocol
/// name, may leave the helper (in the OwnershipPending refusal).
#[derive(Debug)]
/// Wraps the original inspect error: `Display`, `kind()` and `source()` are
/// preserved, but `raw_os_error()` and downcasts apply to the inner error, so
/// evidence formatting must reach the native code through `source()`.
struct ClassifiedInspect {
    failure: v2::InspectFailure,
    source: io::Error,
}
impl std::fmt::Display for ClassifiedInspect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(f)
    }
}
impl std::error::Error for ClassifiedInspect {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}
/// Tag a failed inspection step at its call site. The innermost (first)
/// classification wins; the error kind is preserved.
pub(crate) fn classify<T>(failure: v2::InspectFailure, result: io::Result<T>) -> io::Result<T> {
    result.map_err(|source| {
        if source
            .get_ref()
            .is_some_and(|inner| inner.is::<ClassifiedInspect>())
        {
            return source;
        }
        io::Error::new(source.kind(), ClassifiedInspect { failure, source })
    })
}
/// The closed classification of a refused inspection; untagged errors are
/// `Unclassified`. No error text is ever derived from or carried by it.
pub(crate) fn inspect_failure(error: &io::Error) -> v2::InspectFailure {
    error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<ClassifiedInspect>())
        .map_or(v2::InspectFailure::Unclassified, |c| c.failure)
}
/// A cause interval contains accepted identities, never a reconstructed manager
/// name or PID. A successful SUT mutation closes the previous Start interval.
#[derive(Default)]
struct ActivationInterval {
    original: Option<(v2::LeaseId, v2::OperationId, v2::Digest)>,
    complete: bool,
    from_sut: bool,
}
impl ActivationInterval {
    fn accept_sut(&mut self, lease: v2::LeaseId, operation: v2::OperationId, digest: v2::Digest) {
        self.original = Some((lease, operation, digest));
        self.from_sut = true;
        self.complete = false;
    }
    fn accept_start(
        &mut self,
        lease: v2::LeaseId,
        operation: v2::OperationId,
        digest: v2::Digest,
    ) -> io::Result<()> {
        if self.original.is_some() && !self.from_sut {
            return Err(io::Error::other(
                "new Start requires completed original SUT transition",
            ));
        }
        self.original = Some((lease, operation, digest));
        self.from_sut = false;
        self.complete = false;
        Ok(())
    }
}
pub(crate) fn admitted_sut_transition(program: &v2::ProgramId, outcome: &v2::Outcome) -> bool {
    matches!(
        outcome,
        v2::Outcome::RunCompleted {
            reason: v2::RunReason::Natural,
            status: v2::NativeStatus::PosixExit { code: 0 },
            ..
        } | v2::Outcome::RunCompleted {
            reason: v2::RunReason::Natural,
            status: v2::NativeStatus::WindowsExit { code: 0 },
            ..
        }
    ) && ["configure", "install_target", "install_base", "uninstall"].contains(&program.as_str())
}
#[cfg(any(target_os = "macos", test))]
fn select_exact_definition(
    preferred: Option<&v2::ServiceDefinition>,
    definitions: &[v2::ServiceDefinition],
    matches: impl Fn(&v2::ServiceDefinition) -> bool,
) -> Option<v2::ServiceDefinition> {
    if let Some(preferred) = preferred {
        return matches(preferred).then(|| preferred.clone());
    }
    definitions
        .iter()
        .find(|definition| matches(definition))
        .cloned()
}
#[derive(Clone)]
pub(crate) struct ServiceConfig {
    pub(crate) manifest: v2::IntegrationManifest,
    pub(crate) context: v2::IntegrationRuntimeContext,
}
impl ServiceConfig {
    pub(crate) fn authority(&self) -> &v2::ServiceAuthority {
        self.manifest
            .service_authority
            .as_ref()
            .expect("validated service authority")
    }
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
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    sut_transition: Option<(String, String)>,
    binding: Option<v2::Digest>,
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    readiness: Option<theseus_host::services::readiness::LocalChallenge>,
    #[cfg(any(target_os = "macos", windows))]
    original_start_pid: Option<u32>,
    activation: ActivationInterval,
    readiness_renewal_pending: bool,
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
            #[cfg(any(target_os = "linux", target_os = "macos", windows))]
            sut_transition: None,
            binding: None,
            #[cfg(any(target_os = "linux", target_os = "macos", windows))]
            readiness: None,
            #[cfg(any(target_os = "macos", windows))]
            original_start_pid: None,
            activation: ActivationInterval::default(),
            readiness_renewal_pending: false,
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
        lease: v2::LeaseId,
        operation: v2::OperationId,
        request_digest: v2::Digest,
        outcome: &v2::Outcome,
    ) {
        if !admitted_sut_transition(program, outcome) {
            return;
        }
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        {
            self.sut_transition = Some((program.as_str().into(), operation.to_hex()));
        }
        if ["install_base", "install_target"].contains(&program.as_str()) {
            self.install_receipt = Some(operation.to_hex());
        }
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        if program.as_str() != "uninstall"
            && self.activation.original.is_some()
            && self.readiness.is_some()
        {
            self.readiness_renewal_pending = true;
        }
        // A completed SUT mutation closes the previous activation interval.
        // This never revives an uncertain/revoked owner or attaches to a PID.
        self.activation.accept_sut(lease, operation, request_digest);
        self.instance = None;
        #[cfg(any(target_os = "macos", windows))]
        {
            self.original_start_pid = None;
        }
    }
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    fn renew_readiness_if_needed(&mut self) -> io::Result<()> {
        if !self.readiness_renewal_pending {
            return Ok(());
        }
        let channel = self.readiness.as_mut().ok_or_else(|| {
            io::Error::other("original readiness channel missing during SUT transition")
        })?;
        if let Err(error) = channel.renew_after_sut_transition() {
            self.uncertain = true;
            return Err(error);
        }
        self.readiness_renewal_pending = false;
        Ok(())
    }
    fn transition_definition(&self, program: &str) -> io::Result<v2::ServiceDefinition> {
        let previous = self.observed_definition.as_ref();
        let (role, artifact) = match program {
            "configure" => (
                "configured",
                previous.map(|d| d.artifact.as_str()).unwrap_or("target"),
            ),
            "install_target" => (
                if previous.is_some_and(|d| d.role == "configured") {
                    "configured"
                } else {
                    "target"
                },
                "target",
            ),
            "install_base" => ("base", "base"),
            _ => return Err(io::Error::other("unsupported SUT configuration transition")),
        };
        self.config
            .manifest
            .definitions
            .iter()
            .find(|d| d.role == role && d.artifact == artifact)
            .cloned()
            .ok_or_else(|| io::Error::other("exact admitted SUT transition definition missing"))
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
        if matches!(
            request,
            v2::Request::ServiceAction {
                action: v2::ServiceAction::Stop,
                ..
            }
        ) {
            self.activation.complete = false;
        }
        if let v2::Request::ServiceAction {
            lease,
            operation,
            action: v2::ServiceAction::Start,
            ..
        } = request
        {
            self.activation.accept_start(
                *lease,
                *operation,
                v2::request_digest(request).map_err(io::Error::other)?,
            )?;
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
        if self.activation.original != Some((*lease, *operation, *action_digest))
            || !self.activation.complete
            || self.binding != Some(*prior_binding_digest)
            || self.uncertain
            || slot != self.config.authority().slot()
        {
            return Err(io::Error::other(
                "readiness original Start binding unproved",
            ));
        }
        #[cfg(target_os = "macos")]
        self.verify_readiness_owner()?;
        #[cfg(target_os = "linux")]
        self.manager
            .as_mut()
            .ok_or_else(|| io::Error::other("original systemd manager missing"))?
            .verify_registration(
                self.owner
                    .as_mut()
                    .ok_or_else(|| io::Error::other("original systemd registration missing"))?,
                deadline,
                cancel,
            )?;
        #[cfg(windows)]
        {
            let observed = self
                .manager
                .as_mut()
                .ok_or_else(|| io::Error::other("original SCM manager missing"))?
                .inspect(deadline, cancel)?;
            if !observed.status.is_some_and(|s| {
                s.state() == theseus_host::services::scm::RuntimeState::Running
                    && Some(s.process_id) == self.original_start_pid
            }) {
                self.uncertain = true;
                return Err(io::Error::other(
                    "original SCM runtime changed before readiness",
                ));
            }
        }

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

// Shared policy seams used by the native SCM adapter and host-independent
// sequence regressions; the actual observations still come from its SC_HANDLE.
#[cfg(any(windows, test))]
fn pin_scm_activation_pid(original: &mut Option<u32>, observed: u32) -> io::Result<()> {
    if observed == 0 || original.is_some_and(|pid| pid != observed) {
        return Err(io::Error::other("original SCM activation PID changed"));
    }
    if original.is_none() {
        *original = Some(observed);
    }
    Ok(())
}
#[cfg(any(windows, test))]
fn scm_uninstall_allows_handle_close(state: Option<u32>) -> bool {
    state == Some(1)
}

#[cfg(test)]
mod scm_transition_policy_tests {
    use super::*;
    #[test]
    fn scm_first_running_pid_is_pinned_until_fresh_committed_sut_interval() {
        let mut original = None;
        pin_scm_activation_pid(&mut original, 10).unwrap();
        pin_scm_activation_pid(&mut original, 10).unwrap();
        assert!(pin_scm_activation_pid(&mut original, 20).is_err());
        assert_eq!(original, Some(10));
        // note_sut resets this field only after successful committed mutation.
        original = None;
        pin_scm_activation_pid(&mut original, 20).unwrap();
        assert_eq!(original, Some(20));
    }
    #[test]
    fn scm_running_leak_is_observed_before_any_original_handle_closure() {
        assert!(
            !scm_uninstall_allows_handle_close(Some(4)),
            "Running leak must remain observable"
        );
        for state in [None, Some(2), Some(3), Some(5), Some(7)] {
            assert!(!scm_uninstall_allows_handle_close(state));
        }
        assert!(scm_uninstall_allows_handle_close(Some(1)));
    }
}
// This initial profile admits only a settled native stopped/running observation.
// Paused can retain a live process and cannot prove the shared Stopped predicate.
#[cfg(any(windows, test))]
fn scm_runtime_complete(state: u32) -> bool {
    matches!(state, 1 | 4)
}

#[cfg(test)]
mod activation_interval_tests {
    use super::*;
    #[test]
    fn sut_transition_opens_fresh_exact_readiness_cause_without_start_or_pid_attachment() {
        let lease = v2::LeaseId::from_bytes([1; 16]);
        let configured = v2::OperationId::from_bytes([2; 16]);
        let upgraded = v2::OperationId::from_bytes([3; 16]);
        let reinstalled = v2::OperationId::from_bytes([4; 16]);
        let mut interval = ActivationInterval::default();
        for operation in [configured, upgraded, reinstalled] {
            interval.complete = true;
            let digest = v2::Digest::from_bytes([operation.as_bytes()[0]; 32]);
            interval.accept_sut(lease, operation, digest);
            assert_eq!(interval.original, Some((lease, operation, digest)));
            assert!(interval.from_sut);
            assert!(
                !interval.complete,
                "accepted SUT Run alone cannot prove Running/Ready"
            );
        }
    }
    #[test]
    fn failed_cancelled_or_reader_runs_cannot_authorize_sut_transition() {
        let mut outcome = v2::Outcome::RunCompleted {
            reason: v2::RunReason::Natural,
            status: v2::NativeStatus::PosixExit { code: 0 },
            cleanup: v2::Cleanup::PosixLeaderReapedGroupTerminationRequested,
            stdout_bytes: 0,
            stderr_bytes: 0,
            truncated: false,
        };
        for program in ["configure", "install_target", "install_base", "uninstall"] {
            assert!(admitted_sut_transition(
                &v2::ProgramId::new(program).unwrap(),
                &outcome
            ));
        }
        assert!(!admitted_sut_transition(
            &v2::ProgramId::new("check_data").unwrap(),
            &outcome
        ));
        if let v2::Outcome::RunCompleted { reason, .. } = &mut outcome {
            *reason = v2::RunReason::CancelRequested;
        }
        assert!(!admitted_sut_transition(
            &v2::ProgramId::new("install_target").unwrap(),
            &outcome
        ));
        if let v2::Outcome::RunCompleted { reason, status, .. } = &mut outcome {
            *reason = v2::RunReason::Natural;
            *status = v2::NativeStatus::PosixExit { code: 1 };
        }
        assert!(!admitted_sut_transition(
            &v2::ProgramId::new("configure").unwrap(),
            &outcome
        ));
    }
    #[test]
    fn byte_identical_launchd_definitions_keep_exact_accepted_sut_role_and_reader() {
        let definition = |role: &str, artifact: &str| v2::ServiceDefinition {
            scm: None,
            role: role.into(),
            artifact: artifact.into(),
            executable: "/fixture/reader".into(),
            arguments: vec![],
            environment: vec![],
            working_directory: "/fixture".into(),
            definition_sha256: "11".repeat(32),
        };
        let definitions = vec![
            definition("base", "base"),
            definition("configured", "base"),
            definition("target", "target"),
            definition("configured", "target"),
        ];
        let exact = &definitions[3];
        let observed = select_exact_definition(Some(exact), &definitions, |d| {
            d.definition_sha256 == exact.definition_sha256
        })
        .unwrap();
        assert_eq!(observed.role, "configured");
        assert_eq!(observed.artifact, "target");
        assert!(
            select_exact_definition(Some(exact), &definitions, |_| false).is_none(),
            "mismatched original key cannot adopt another manifest definition"
        );
    }
    #[test]
    fn rehearsal_start_requires_a_new_completed_sut_interval() {
        let lease = v2::LeaseId::from_bytes([1; 16]);
        let op = v2::OperationId::from_bytes([2; 16]);
        let digest = v2::Digest::from_bytes([3; 32]);
        let mut interval = ActivationInterval::default();
        interval.accept_start(lease, op, digest).unwrap();
        let next = v2::OperationId::from_bytes([4; 16]);
        assert!(interval.accept_start(lease, next, digest).is_err());
        assert_eq!(interval.original, Some((lease, op, digest)));
        interval.accept_sut(lease, next, digest);
        interval
            .accept_start(lease, v2::OperationId::from_bytes([5; 16]), digest)
            .unwrap();
        assert!(!interval.from_sut);
        assert!(!interval.complete);
    }
}

#[cfg(test)]
mod inspect_classification_tests {
    use super::*;
    use v2::InspectFailure as F;
    #[test]
    fn each_tagged_step_maps_to_its_closed_variant_and_keeps_the_original_error() {
        for failure in F::ALL {
            let error = classify::<()>(
                failure,
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "native detail",
                )),
            )
            .unwrap_err();
            assert_eq!(inspect_failure(&error), failure);
            // The retained helper error is unchanged; nothing derives from it.
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(error.to_string(), "native detail");
        }
        assert_eq!(classify(F::ManagerQuery, Ok(7)).unwrap(), 7);
    }
    #[test]
    fn innermost_step_wins_and_untagged_errors_are_unclassified() {
        // SCM nesting: a transition admission failure inside a later step.
        let inner = classify::<()>(F::TransitionAdmission, Err(io::Error::other("x")));
        let outer = classify(F::ObservationBinding, inner).unwrap_err();
        assert_eq!(inspect_failure(&outer), F::TransitionAdmission);
        assert_eq!(
            inspect_failure(&io::Error::other("untagged")),
            F::Unclassified
        );
    }
    #[test]
    fn scm_inspect_steps_use_distinct_closed_names() {
        let scm = [
            F::SlotUndeclared,
            F::ReadinessBind,
            F::ReadinessRenewal,
            F::ManagerConnect,
            F::UninstallTransition,
            F::TransitionDefinition,
            F::TransitionAdmission,
            F::DeletionProgress,
            F::ManagerQuery,
            F::RegistrationQualification,
            F::ActivationPid,
            F::ObservationBinding,
        ];
        let names: std::collections::BTreeSet<_> = scm.iter().map(|f| f.name()).collect();
        assert_eq!(names.len(), scm.len());
        assert!(!scm.contains(&F::Unclassified));
    }
}
