//! Local SCM queries and controls retain only the original service handle.
use super::*;
use theseus_host::services::scm as host;
impl ServiceRuntime {
    fn manager(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<&mut host::ScmManager> {
        if self.manager.is_none() {
            let v2::ServiceAuthority::Scm {
                service_name,
                qualification,
                allowed_access,
                allow_local_system,
                preflight: p,
                allowed_actions,
                ..
            } = self.config.authority()
            else {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "SCM authority required on Windows",
                ));
            };
            host::verify_platform_version(&qualification.version)?;
            self.manager = Some(host::ScmManager::connect(
                host::ScmAuthority {
                    context: host::ScmContext::LocalMachine,
                    service_name: service_name.clone(),
                    ownership_id: self.config.context.ownership_id.clone(),
                    context_receipt: self
                        .config
                        .context
                        .namespace_receipt
                        .clone()
                        .expect("validated service context"),
                    qualification_receipt: qualification.receipt_sha256.clone(),
                    allowed_access: *allowed_access,
                    allowed_start: allowed_actions.iter().any(|a| a == "start"),
                    allowed_stop: allowed_actions.iter().any(|a| a == "stop"),
                    allow_local_system: *allow_local_system,
                    preflight: host::ScmPreflight {
                        helper_sid: p.helper_sid.clone(),
                        configured_account_sid: p.configured_account_sid.clone(),
                        executable: p.executable.encode_utf16().collect(),
                        definition: p.definition.encode_utf16().collect(),
                        data: p.data.encode_utf16().collect(),
                        pipe: p.pipe.encode_utf16().collect(),
                        executable_and_definition_read_execute: p
                            .executable_and_definition_read_execute,
                        data_declared_modify_only: p.data_declared_modify_only,
                        pipe_system_and_helper_only: p.pipe_system_and_helper_only,
                        pipe_rejects_remote_clients: p.pipe_rejects_remote_clients,
                        no_other_acl_grants: p.no_other_acl_grants,
                        exact_access_receipt: p.exact_access_receipt.clone(),
                    },
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
        if slot != self.config.authority().slot() {
            return Err(io::Error::other("undeclared service slot"));
        }
        self.ensure_readiness_channel()?;
        self.manager(deadline, cancel)?;
        if self.owner.is_some()
            && self.deletion.is_none()
            && let Some((program, receipt)) = self.sut_transition.clone()
        {
            if program == "uninstall" {
                let status = self
                    .manager
                    .as_mut()
                    .unwrap()
                    .inspect(deadline, cancel)?
                    .status;
                if scm_uninstall_allows_handle_close(status.map(|s| s.state)) {
                    // This explicit transition follows the accepted SUT uninstaller;
                    // it is local original-handle closure, never DeleteService repair.
                    self.deletion = Some(self.manager.as_mut().unwrap().close_after_sut_delete(
                        self.owner.as_mut().unwrap(),
                        receipt,
                        deadline,
                        cancel,
                    )?);
                }
            } else {
                let d = self.transition_definition(&program)?;
                self.manager.as_mut().unwrap().admit_configuration(
                    self.owner.as_mut().unwrap(),
                    &raw_definition(
                        d.scm
                            .as_ref()
                            .ok_or_else(|| io::Error::other("raw SCM definition missing"))?,
                    ),
                    receipt,
                    deadline,
                    cancel,
                )?;
            }
            if program != "uninstall" || self.deletion.is_some() {
                self.sut_transition = None;
            }
        }
        if let Some(owner) = &mut self.deletion {
            let progress = self
                .manager
                .as_mut()
                .unwrap()
                .poll_deletion(owner, deadline, cancel)?;
            let (present, complete) = match progress {
                host::DeletionProgress::Complete => (false, true),
                host::DeletionProgress::SameNamePresent => (true, true),
                _ => (true, false),
            };
            return self.bind_observation(
                lease,
                v2::ServiceObservation {
                    platform: Some(v2::ServicePlatformFacts::Scm {
                        next_start: None,
                        registration: match progress {
                            host::DeletionProgress::Complete => v2::ScmRegistrationState::Absent,
                            host::DeletionProgress::SameNamePresent => {
                                v2::ScmRegistrationState::SameNamePresent
                            }
                            host::DeletionProgress::Pending => {
                                v2::ScmRegistrationState::DeletePending
                            }
                            host::DeletionProgress::Unknown(_) => v2::ScmRegistrationState::Unknown,
                        },
                        status: None,
                        configured_account_sid: None,
                        child_token_measured: false,
                        original_handle_retained: false,
                    }),
                    instance: self.instance.clone(),
                    slot: slot.clone(),
                    registration_present: present,
                    definition_digest: None,
                    configured: false,
                    running: false,
                    origin: None,
                    manager_generation: self
                        .config
                        .context
                        .manager_unique_name
                        .clone()
                        .expect("validated service context"),
                    complete,
                    binding_digest: String::new(),
                },
                None,
            );
        }
        let observed = self.manager.as_mut().unwrap().inspect(deadline, cancel)?;
        if self.manager.as_ref().is_some_and(|m| m.is_revoked()) {
            self.uncertain = true;
        }
        let definition = observed
            .next_start
            .as_ref()
            .and_then(|raw| {
                self.config.manifest.definitions.iter().find(|d| {
                    d.scm
                        .as_ref()
                        .is_some_and(|expected| raw_definition(expected) == *raw)
                })
            })
            .cloned();
        if self.owner.is_none()
            && let (Some(d), Some(receipt)) = (&definition, &self.install_receipt)
        {
            let raw = raw_definition(
                d.scm
                    .as_ref()
                    .ok_or_else(|| io::Error::other("raw SCM definition missing"))?,
            );
            self.owner = Some(self.manager.as_mut().unwrap().qualify_registration(
                &raw,
                receipt.clone(),
                deadline,
                cancel,
            )?);
            self.sut_transition = None;
        }
        let platform = Some(v2::ServicePlatformFacts::Scm {
            next_start: observed
                .next_start
                .as_ref()
                .map(|d| Box::new(protocol_definition(d))),
            registration: match observed.registration {
                host::RegistrationState::Absent => v2::ScmRegistrationState::Absent,
                host::RegistrationState::Present => v2::ScmRegistrationState::Present,
                host::RegistrationState::DeletePending => v2::ScmRegistrationState::DeletePending,
            },
            status: observed.status.map(|s| v2::ScmStatus {
                service_type: s.service_type,
                state: s.state,
                controls_accepted: s.controls_accepted,
                win32_exit_code: s.win32_exit_code,
                service_specific_exit_code: s.service_specific_exit_code,
                checkpoint: s.checkpoint,
                wait_hint_ms: s.wait_hint_ms,
                process_id: s.process_id,
                service_flags: s.service_flags,
            }),
            configured_account_sid: if observed.configured_account
                == host::ConfiguredAccount::LocalSystem
            {
                Some("S-1-5-18".into())
            } else {
                None
            },
            child_token_measured: false,
            original_handle_retained: self
                .owner
                .as_ref()
                .is_some_and(|o| !o.original_handle_closed()),
        });
        if self.activation.from_sut
            && definition.is_some()
            && self
                .owner
                .as_ref()
                .is_some_and(|o| !o.original_handle_closed())
            && !self.uncertain
            && let Some(status) = observed
                .status
                .filter(|s| s.state() == host::RuntimeState::Running && s.process_id != 0)
        {
            self.activation.complete = true;
            if let Err(error) =
                pin_scm_activation_pid(&mut self.original_start_pid, status.process_id)
            {
                self.uncertain = true;
                self.manager.as_mut().unwrap().revoke_context();
                return Err(error);
            }
            self.instance = self
                .config
                .manifest
                .readiness
                .as_ref()
                .and_then(|r| r.cooperative_instance.clone());
        }
        let absent = observed.registration == host::RegistrationState::Absent;
        let complete = absent
            || (observed.registration == host::RegistrationState::Present
                && observed.next_start.is_some()
                && observed
                    .status
                    .is_some_and(|s| scm_runtime_complete(s.state)));
        self.bind_observation(
            lease,
            v2::ServiceObservation {
                platform,
                instance: self.instance.clone(),
                slot: slot.clone(),
                registration_present: !absent,
                definition_digest: definition.as_ref().map(|d| d.definition_sha256.clone()),
                configured: definition.is_some(),
                running: observed
                    .status
                    .is_some_and(|s| s.state() == host::RuntimeState::Running),
                origin: None,
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
            let config = self
                .config
                .manifest
                .readiness
                .as_ref()
                .ok_or_else(|| io::Error::other("native SCM readiness prerequisite missing"))?;
            if config.cooperative_instance.is_none()
                || config.peer_sid.as_deref() != Some("S-1-5-18")
            {
                return Err(io::Error::other(
                    "native SCM readiness identity prerequisite missing",
                ));
            }
            let v2::LocalEndpoint::NamedPipe { path } = &config.endpoint else {
                return Err(io::Error::other(
                    "SCM readiness requires exact local NamedPipe",
                ));
            };
            let v2::ServiceAuthority::Scm { preflight, .. } = self.config.authority() else {
                return Err(io::Error::other("SCM authority required"));
            };
            if path != &preflight.pipe {
                return Err(io::Error::other(
                    "readiness pipe differs from admitted exact access preflight",
                ));
            }
            use theseus_host::services::readiness::{LocalChallenge, NamedPipe, Sid};
            self.readiness = Some(LocalChallenge::bind(
                NamedPipe::new(path.clone())?,
                Sid::new(preflight.helper_sid.clone())?,
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
        if slot != self.config.authority().slot()
            || self.binding != Some(prior)
            || self.uncertain
            || self.deletion.is_some()
        {
            return Err(io::Error::other("service prior authority unproved"));
        }
        if action == v2::ServiceAction::Start {
            self.ensure_readiness_channel()?;
        }
        if self.owner.is_none() {
            let d = self
                .observed_definition
                .clone()
                .ok_or_else(|| io::Error::other("exact observed SCM definition missing"))?;
            let receipt = self
                .install_receipt
                .clone()
                .ok_or_else(|| io::Error::other("original SUT install receipt missing"))?;
            self.owner = Some(
                self.manager(deadline, cancel)?.qualify_registration(
                    &raw_definition(
                        d.scm
                            .as_ref()
                            .ok_or_else(|| io::Error::other("raw SCM definition missing"))?,
                    ),
                    receipt,
                    deadline,
                    cancel,
                )?,
            );
            self.sut_transition = None;
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
        use sha2::{Digest as _, Sha256};
        match p {
            host::OwnedActionProgress::Pending { .. } => None,
            host::OwnedActionProgress::Terminal {
                action,
                status,
                succeeded,
            } => {
                if succeeded {
                    self.activation.complete = action == host::ServiceAction::Start;
                    if self.activation.complete
                        && status.state() == host::RuntimeState::Running
                        && status.process_id != 0
                    {
                        self.original_start_pid = Some(status.process_id);
                        self.instance = self
                            .config
                            .manifest
                            .readiness
                            .as_ref()
                            .and_then(|r| r.cooperative_instance.clone());
                    }
                    Some(v2::ServiceOutcome::Complete {
                        receipt: format!("{:x}", Sha256::digest(format!("{action:?}\0{status:?}"))),
                    })
                } else {
                    Some(v2::ServiceOutcome::Failed {
                        reason: format!("original SCM {action:?} failed: {status:?}"),
                    })
                }
            }
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
    pub(crate) fn retire(&mut self) -> io::Result<()> {
        if let Some(m) = &mut self.manager {
            m.take_terminal_action()?;
        }
        Ok(())
    }
    pub(crate) fn unsettled(&self) -> bool {
        self.uncertain
            || self
                .owner
                .as_ref()
                .is_some_and(|o| !o.original_handle_closed())
            || self
                .deletion
                .as_ref()
                .is_some_and(|o| o.progress() != host::DeletionProgress::Complete)
            || self.manager.as_ref().is_some_and(|m| {
                m.pending_action()
                    .is_some_and(|p| !matches!(p, host::OwnedActionProgress::Terminal { .. }))
                    || !m.retained_errors().is_empty()
            })
    }
}
fn raw_definition(d: &v2::ScmDefinition) -> host::ScmDefinition {
    host::ScmDefinition {
        command_line: d.command_line.clone(),
        display_name: d.display_name.clone(),
        account: d.account.clone(),
        service_type: d.service_type,
        start_type: d.start_type,
        error_control: d.error_control,
        load_order_group: d.load_order_group.clone(),
        dependencies: d.dependencies.clone(),
        tag_id: d.tag_id,
        delayed_auto_start: d.delayed_auto_start,
        failure_actions: d
            .failure_actions
            .iter()
            .map(|a| host::FailureAction {
                kind: a.kind,
                delay_ms: a.delay_ms,
            })
            .collect(),
        failure_reset_period: d.failure_reset_period,
        failure_reboot_message: d.failure_reboot_message.clone(),
        failure_command: d.failure_command.clone(),
        failure_actions_on_non_crash: d.failure_actions_on_non_crash,
        service_sid_type: d.service_sid_type,
        required_privileges: d.required_privileges.clone(),
        triggers_present: d.triggers_present,
        launch_protected: d.launch_protected,
    }
}

fn protocol_definition(d: &host::ScmDefinition) -> v2::ScmDefinition {
    v2::ScmDefinition {
        command_line: d.command_line.clone(),
        display_name: d.display_name.clone(),
        account: d.account.clone(),
        service_type: d.service_type,
        start_type: d.start_type,
        error_control: d.error_control,
        load_order_group: d.load_order_group.clone(),
        dependencies: d.dependencies.clone(),
        tag_id: d.tag_id,
        delayed_auto_start: d.delayed_auto_start,
        failure_actions: d
            .failure_actions
            .iter()
            .map(|a| v2::ScmFailureAction {
                kind: a.kind,
                delay_ms: a.delay_ms,
            })
            .collect(),
        failure_reset_period: d.failure_reset_period,
        failure_reboot_message: d.failure_reboot_message.clone(),
        failure_command: d.failure_command.clone(),
        failure_actions_on_non_crash: d.failure_actions_on_non_crash,
        service_sid_type: d.service_sid_type,
        required_privileges: d.required_privileges.clone(),
        triggers_present: d.triggers_present,
        launch_protected: d.launch_protected,
    }
}
