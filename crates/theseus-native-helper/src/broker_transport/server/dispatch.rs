use super::*;
impl BrokerServer {
    pub(super) fn dispatch(&mut self, sequence: u64, request: v2::Request) -> io::Result<()> {
        if self.pending.is_some()
            && !matches!(
                request,
                v2::Request::CancelDesktop { .. } | v2::Request::QueryDesktop { .. }
            )
        {
            return self.reply(
                sequence,
                v2::Reply::Refused {
                    code: v2::RefusalCode::Busy,
                    inspect_failure: None,
                },
            );
        }
        let request = match request {
            v2::Request::InspectOwnedEntry {
                lease,
                slot,
                generation,
                sut,
            } => {
                self.observe_owned_sut(sut.as_deref())?;
                v2::Request::InspectDesktop {
                    lease,
                    slot,
                    generation,
                }
            }
            v2::Request::InspectDesktop { .. }
                if self.config.manifest.desktop_owned_entry.is_some() =>
            {
                return Err(io::Error::other(
                    "owned entry requires original helper SUT inspection",
                ));
            }
            other => other,
        };
        match request {
            v2::Request::PrepareOwnedEntry { preparation } => {
                self.prepare_owned_entry(&preparation)?;
                self.reply(sequence, v2::Reply::OwnedEntryPrepared { preparation })
            }
            v2::Request::Describe => self.reply(
                sequence,
                v2::Reply::BrokerDescription {
                    header: Box::new(self.config.header.clone()),
                },
            ),
            v2::Request::Acquire {
                intent,
                binding,
                manifest_digest,
            } => {
                let state = self.runtime.acquire(intent, binding, manifest_digest)?;
                self.reply(
                    sequence,
                    v2::Reply::Acquired {
                        intent,
                        binding,
                        manifest_digest,
                        state,
                    },
                )
            }
            v2::Request::QueryAcquire {
                intent,
                binding,
                manifest_digest,
            } => {
                let state = self
                    .runtime
                    .query_acquire(intent, binding, manifest_digest)?;
                self.reply(
                    sequence,
                    v2::Reply::Acquired {
                        intent,
                        binding,
                        manifest_digest,
                        state,
                    },
                )
            }
            v2::Request::QueryLease { lease } => {
                let state = self
                    .runtime
                    .store()
                    .restart_view()
                    .query_lease(lease)
                    .map_err(|code| io::Error::other(OriginalRefusal(code)))?;
                self.reply(sequence, v2::Reply::Lease { lease, state })
            }
            v2::Request::Release { lease } => {
                let state = self.runtime.release_original(lease)?;
                self.definition = None;
                self.reply(sequence, v2::Reply::Lease { lease, state })
            }
            v2::Request::InspectDesktop {
                lease,
                slot,
                generation,
            } => self.dispatch_inspect_desktop(sequence, lease, slot, generation),

            v2::Request::DesktopAction { request } => match request.action {
                DesktopAction::Activate { .. } => self.dispatch_activate(sequence, request),
                DesktopAction::RequestClose { .. } => {
                    self.pending = Some(Pending::Close {
                        reply_to: sequence,
                        request,
                        deadline: Instant::now() + Duration::from_secs(30),
                    });
                    Ok(())
                }
            },
            v2::Request::QueryDesktop { identity } => {
                self.dispatch_query_desktop(sequence, identity)
            }
            v2::Request::CancelDesktop { identity } => {
                self.dispatch_cancel_desktop(sequence, identity)
            }
            v2::Request::ProbeDesktopReadiness {
                identity,
                nonce,
                deadline_ms,
            } => {
                if deadline_ms == 0 || deadline_ms > 30_000 {
                    return Err(io::Error::other("readiness deadline outside bound"));
                }
                self.pending = Some(Pending::Ready {
                    reply_to: sequence,
                    identity,
                    nonce,
                    deadline: Instant::now() + Duration::from_millis(deadline_ms as u64),
                });
                Ok(())
            }
            _ => self.reply(
                sequence,
                v2::Reply::Refused {
                    code: v2::RefusalCode::Unsupported,
                    inspect_failure: None,
                },
            ),
        }
    }
    fn dispatch_inspect_desktop(
        &mut self,
        sequence: u64,
        lease: v2::LeaseId,
        slot: v2::ResourceSlot,
        generation: Option<DesktopActivationTransition>,
    ) -> io::Result<()> {
        self.runtime
            .store()
            .restart_view()
            .query_lease(lease)
            .map_err(|code| io::Error::other(OriginalRefusal(code)))?;
        if self
            .config
            .manifest
            .desktop
            .as_ref()
            .is_none_or(|d| d.slot != slot)
        {
            return Err(io::Error::other("wrong desktop slot"));
        }
        let view =
            BrokerLedgerView::replay(self.runtime.store().records()).map_err(io::Error::other)?;
        let continuity = self.runtime.retirement_continuity()?;
        let selected = super::super::generation::inspection(
            &self.config,
            &view,
            generation.as_ref(),
            continuity.as_ref(),
        )?;
        if generation.is_some() && self.observed_generation.as_ref() != generation.as_ref() {
            self.roots.revalidate()?;
            if self.unconsumed_launch.is_some() {
                return Err(io::Error::other(
                    "original prepared generation remains retained",
                ));
            }
            if self.definition_used
                && self.definition.is_some()
                && self.retired_definitions.len() >= v2::MAX_OPERATIONS
            {
                return Err(io::Error::other("retired definition capacity"));
            }
            if let Some(old) = self.definition.take()
                && self.definition_used
            {
                self.retired_definitions.push(old);
            }
            // Unused metadata admission has no activation owner; dropping
            // it settles original COM/parser state before fresh admission.
            self.definition_used = false;
            self.generation = None;
            self.observed_generation = generation.clone();
        }
        let observed = match launch::inspect(&self.config, &self.roots, &selected) {
            Ok(observed) => observed,
            Err(error) => {
                self.definition_revoked = true;
                let _ = self.runtime.observe_session(None);
                return Err(error);
            }
        };
        let d = &selected.desktop;
        let expected = observed.entry_digest.as_ref() == Some(&d.entry_digest)
            && observed.executable_digest.as_ref() == Some(&d.executable_digest);
        let mut valid = false;
        if observed.entry_present {
            if !expected {
                self.definition_revoked = true;
            }
            if !self.definition_revoked {
                if self.definition.is_none()
                    && self.config.manifest.desktop_owned_entry.is_some()
                    && self.generation.is_some()
                {
                    return Err(io::Error::other(
                        "retired entry proof cannot be re-admitted without original SUT transition",
                    ));
                }
                if self.definition.is_none() {
                    #[cfg(windows)]
                    if (selected.native.is_some() || selected.owned_entry.is_some())
                        && self.apartment.is_none()
                    {
                        self.apartment=Some(theseus_host::desktop::windows::ComApartment::initialize_original_thread()?);
                    }
                    match RetainedDefinition::admit(
                        &self.config,
                        &self.roots,
                        &selected,
                        #[cfg(windows)]
                        &mut self.apartment,
                    ) {
                        Ok(definition) => {
                            let base =
                                definition.binding_digest(&self.config, &self.roots, lease)?;
                            self.generation = Some(super::super::generation::inspection_binding(
                                &base,
                                &selected,
                                generation.as_ref(),
                            )?);
                            self.definition = Some(definition);
                        }
                        Err(error) => {
                            self.remember(error);
                            self.definition_revoked = true;
                        }
                    }
                }
                if let Some(definition) = &self.definition {
                    match definition.revalidate() {
                        Ok(()) => valid = true,
                        Err(error) => {
                            self.remember(error);
                            self.definition_revoked = true;
                            let _ = self.runtime.observe_session(None);
                        }
                    }
                }
            }
        } else if self.definition.is_some() {
            self.definition_revoked = true;
        }
        let binding_digest = match &self.generation {
            Some(value) => value.clone(),
            None => super::super::generation::inspection_binding(
                &self.roots.vacant_binding(&self.config, lease)?,
                &selected,
                generation.as_ref(),
            )?,
        };
        let header = self
            .config
            .header
            .digest()
            .map_err(io::Error::other)?
            .to_hex();
        self.reply(
            sequence,
            v2::Reply::DesktopObservation {
                lease,
                observation: Box::new(DesktopObservation {
                    generation: generation.clone(),
                    slot,
                    entry_present: observed.entry_present,
                    entry_valid: valid,
                    entry_digest: observed.entry_digest,
                    executable_digest: observed.executable_digest,
                    session_header_digest: header,
                    binding_digest,
                    complete: true,
                }),
            },
        )
    }
    fn dispatch_activate(&mut self, sequence: u64, request: Box<DesktopRequest>) -> io::Result<()> {
        self.revalidate_session()?;
        let view =
            BrokerLedgerView::replay(self.runtime.store().records()).map_err(io::Error::other)?;
        let mut launch = None;
        let continuity = self.runtime.retirement_continuity()?;
        let selected = if view.original(&request.helper_operation).is_none() {
            Some(super::super::generation::activation(
                &self.config,
                &view,
                &request,
                continuity.as_ref(),
            )?)
        } else {
            None
        };

        if view.original(&request.helper_operation).is_none() {
            if request.transition() != self.observed_generation.as_ref()
                && (!self.config.manifest.desktop_definitions.is_empty()
                    || request.transition().is_some())
            {
                return Err(io::Error::other(
                    "activation changed original inspected SUT generation",
                ));
            }
            if self.definition_revoked
                || self.generation.as_deref() != Some(request.prior_binding_digest.as_str())
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "original observed desktop generation unavailable/replaced",
                ));
            }
            let validation = self.roots.revalidate().and_then(|()| {
                self.definition
                    .as_ref()
                    .ok_or_else(|| io::Error::other("original installed definition not retained"))?
                    .revalidate()
            });
            if let Err(error) = validation {
                self.definition_revoked = true;
                if let Err(error) = self.runtime.observe_session(None) {
                    self.remember(error);
                }
                return Err(error);
            }
            let selected = selected
                .as_ref()
                .ok_or_else(|| io::Error::other("selected tuple missing"))?;
            crate::broker::owned_lifetime::validate_definition(
                selected.owned_lifetime,
                request.owned_lifetime_definition(),
            )?;
            let action_digest = request.digest().map_err(io::Error::other)?;
            if self.unconsumed_launch.is_none() {
                #[cfg(target_os = "linux")]
                if let Some(handoff) = self.runtime.prepare_generation(&request)? {
                    match handoff {
                        crate::broker::LinuxGenerationHandoff::OwnedScope(scope) => {
                            if self.scope.is_some() {
                                return Err(io::Error::other("original scope handoff collision"));
                            }
                            self.scope = Some(scope);
                        }
                        crate::broker::LinuxGenerationHandoff::NativeBroker(original) => self
                            .native_factory
                            .as_mut()
                            .ok_or_else(|| io::Error::other("original qualified factory missing"))?
                            .restore(*original)?,
                    }
                }
                #[cfg(not(target_os = "linux"))]
                self.runtime.prepare_generation(&request)?;
                let d = &selected.desktop;
                if self.ready_channel.is_none() && self.close_channel.is_none() {
                    self.ready_channel =
                        Some(launch::bind_channel(&self.config, &d.ready_endpoint)?);
                    self.close_channel =
                        Some(launch::bind_channel(&self.config, &d.close_endpoint)?);
                }
                let session = launch::binding(&self.config, &self.measurement)?;
                launch = Some(launch::prepare(
                    &self.config,
                    &request,
                    session,
                    &mut self.ready_channel,
                    &mut self.close_channel,
                    #[cfg(target_os = "linux")]
                    &mut self.scope,
                    selected,
                    #[cfg(target_os = "linux")]
                    &mut self.native_factory,
                    self.definition
                        .as_mut()
                        .ok_or_else(|| io::Error::other("original definition missing"))?,
                )?);
                self.definition_used = true;
            } else if self
                .unconsumed_launch
                .as_ref()
                .is_none_or(|old| old.binding().ready.action_digest != *action_digest.as_bytes())
            {
                return Err(io::Error::other(
                    "different original preparation already retained",
                ));
            }
        }
        if launch.is_some() {
            if self.unconsumed_launch.is_some() {
                return Err(io::Error::other(
                    "original unconsumed launch remains retained",
                ));
            }
            self.unconsumed_launch = launch;
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        let state = self.runtime.activate_prepared(
            (*request).clone(),
            &mut self.unconsumed_launch,
            deadline,
            &self.cancel,
        )?;
        let validation = self.roots.revalidate().and_then(|()| {
            self.definition
                .as_ref()
                .ok_or_else(|| io::Error::other("original definition missing after activation"))?
                .revalidate()
        });
        let identity = request.identity().map_err(io::Error::other)?;
        let state = finish_activation_check(state, validation, |error| {
            self.definition_revoked = true;
            self.remember(error);
            if let Err(error) = self.runtime.observe_session(None) {
                self.remember(error);
            }
            self.runtime.query(&identity)
        })?;
        if matches!(
            request.action,
            DesktopAction::Activate {
                mode: DesktopMode::NativeActivation,
                ..
            }
        ) && !state.quarantined
            && !matches!(state.effect, BrokerEffect::Receipt { .. })
        {
            self.pending = Some(Pending::Activation {
                reply_to: sequence,
                identity: Box::new(request.identity().map_err(io::Error::other)?),
                deadline,
            });
            Ok(())
        } else {
            self.reply(
                sequence,
                v2::Reply::DesktopState {
                    identity: Box::new(request.identity().map_err(io::Error::other)?),
                    state: Box::new(state),
                },
            )
        }
    }
    fn dispatch_query_desktop(
        &mut self,
        sequence: u64,
        identity: Box<BrokerOperationIdentity>,
    ) -> io::Result<()> {
        self.runtime.query(&identity)?;
        let view =
            BrokerLedgerView::replay(self.runtime.store().records()).map_err(io::Error::other)?;
        let original = view.original(&identity.helper_operation).cloned();
        if let Some(request) =
            original.filter(|r| matches!(r.action, DesktopAction::RequestClose { .. }))
            && self.pending.is_none()
        {
            self.pending = Some(Pending::Close {
                reply_to: sequence,
                request: Box::new(request),
                deadline: Instant::now() + Duration::from_secs(30),
            });
            return Ok(());
        }
        let state = self.runtime.poll_original(&identity)?;
        self.reply(
            sequence,
            v2::Reply::DesktopState {
                identity,
                state: Box::new(state),
            },
        )
    }
    fn dispatch_cancel_desktop(
        &mut self,
        sequence: u64,
        identity: Box<BrokerOperationIdentity>,
    ) -> io::Result<()> {
        self.runtime.query(&identity)?;
        let view =
            BrokerLedgerView::replay(self.runtime.store().records()).map_err(io::Error::other)?;
        let original = view
            .original(&identity.helper_operation)
            .ok_or_else(|| io::Error::other("original cancellation identity missing"))?;
        validate_cancel_identity(original, self.pending.as_ref(), &identity)?;
        let cancel = Arc::clone(&self.cancel);
        let state = with_cancel_flag(&cancel, || {
            self.advance_pending()?;
            self.runtime.cancel(&identity)
        })?;
        self.reply(
            sequence,
            v2::Reply::DesktopState {
                identity,
                state: Box::new(state),
            },
        )
    }
}
