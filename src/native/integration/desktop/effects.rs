//! Desktop action dispatch, activation, queries, readiness and original close.
use super::*;

impl<'a, 'b, 's> DesktopDriver<'a, 'b, 's> {
    fn dispatch_action(&mut self, request: broker::DesktopRequest) -> Result<broker::BrokerState> {
        self.core.reserve_operation()?;
        let wire = v2::Request::DesktopAction {
            request: Box::new(request.clone()),
        };
        let identity = request.identity()?;
        let record = IntegrationActionRecord {
            ownership_id: self.core.resource.resource().ownership_id.clone(),
            lease: request.helper_lease.clone(),
            slot: self.hooks.requirement.resource.clone(),
            operation: request.helper_operation.clone(),
            request: IntegrationRequest::Desktop {
                lease: request.helper_lease.clone(),
                operation: request.helper_operation.clone(),
                slot: self.hooks.requirement.resource.clone(),
                prior_binding_digest: request.prior_binding_digest.clone(),
                mode: if self.hooks.mode == broker::DesktopMode::NativeActivation {
                    DesktopMode::NativeActivation
                } else {
                    DesktopMode::OwnedExecutable
                },
                request: Box::new(request.clone()),
            },
            request_digest: v2::request_digest(&wire)?.to_hex(),
            prior_binding_digest: request.prior_binding_digest.clone(),
            broker_identity: Some(serde_json::from_value(serde_json::to_value(&identity)?)?),
            completion: Disposition::Unknown {
                reason: "exact original desktop action published; effect unproved".into(),
            },
            terminal: None,
        };
        let token = self
            .core
            .projector
            .publish_integration_action(&self.core.resource, record)?;
        let saved = token.into_action();
        let id = self.core.session.submit(Action::DesktopAction {
            request: request.clone(),
        })?;
        let v2::Reply::DesktopState {
            identity: observed,
            state,
        } = self.core.wait_reply(id)?
        else {
            bail!("original desktop action unresolved")
        };
        ensure!(
            *observed == identity,
            "desktop original action identity changed"
        );
        self.validate_state(&state)?;
        let terminal = match &state.effect {
            broker::BrokerEffect::Receipt { receipt } if receipt.acknowledged => {
                IntegrationOutcome::Complete {
                    receipt: receipt.receipt_digest.clone(),
                }
            }
            broker::BrokerEffect::Receipt { .. } => IntegrationOutcome::Failed {
                reason: "original desktop effect was not acknowledged".into(),
            },
            broker::BrokerEffect::Unknown => IntegrationOutcome::Unknown {
                reason: "original broker effect unproved".into(),
            },
        };
        let completion = match &terminal {
            IntegrationOutcome::Complete { receipt } => Disposition::Complete {
                receipt: receipt.clone(),
            },
            IntegrationOutcome::Failed { reason } => Disposition::Failed {
                reason: reason.clone(),
            },
            IntegrationOutcome::Unknown { reason } => Disposition::Unknown {
                reason: reason.clone(),
            },
        };
        if !matches!(terminal, IntegrationOutcome::Unknown { .. }) {
            self.core.projector.record_integration_completion(
                &request.helper_lease,
                &request.helper_operation,
                completion,
                terminal,
            )?;
        }
        self.evidence()?.desktops[0].actions.push(ActionReference {
            operation: saved.operation,
            request_digest: saved.request_digest,
        });
        self.core.projector.save_report()?;
        Ok(*state)
    }
    pub(super) fn activate(&mut self) -> Result<()> {
        let transition = self.hooks.entry.as_ref().and_then(|e| e.generation.clone());
        if !self.hooks.manifest.desktop_definitions.is_empty() {
            ensure!(
                transition.is_some(),
                "desktop generation transition missing"
            );
        }
        if let Some(previous) = &self.hooks.activation {
            ensure!(
                self.hooks
                    .state
                    .as_ref()
                    .is_some_and(settled_original_broker_state),
                "desktop generation cannot replace unsettled original owner"
            );
            ensure!(
                transition
                    .as_ref()
                    .and_then(|t| t.previous_activation.as_ref())
                    == Some(&previous.identity()?),
                "desktop generation changed retired original identity"
            );
        } else if let Some(transition) = &transition {
            ensure!(
                transition.previous_activation.is_none(),
                "initial desktop generation borrowed a prior activation"
            );
        }
        let desktop = self.desktop_manifest();
        let mut request = broker::DesktopRequest {
            helper_lease: self.core.lease()?.to_hex(),
            helper_operation: operation()?.to_hex(),
            broker_lease: self.hooks.receipt.as_ref().unwrap().broker_lease.clone(),
            binding_digest: self.hooks.header.digest()?.to_hex(),
            activation_nonce: nonce()?.to_hex(),
            slot: desktop.slot.clone(),
            prior_binding_digest: self.hooks.entry.as_ref().unwrap().binding_digest.clone(),
            action: broker::DesktopAction::Activate {
                owned_lifetime: self.hooks.manifest.desktop_owned_lifetime,
                transition,
                mode: self.hooks.mode,
                entry_digest: desktop.entry_digest.clone(),
                expectation_digest: desktop.expectation_digest.clone(),
                close_binding: broker::CooperativeCloseBinding {
                    operation: operation()?.to_hex(),
                    action_digest: "00".repeat(32),
                    nonce: nonce()?.to_hex(),
                },
            },
        };
        let digest = request.close_request()?.digest()?.to_hex();
        if let broker::DesktopAction::Activate { close_binding, .. } = &mut request.action {
            close_binding.action_digest = digest;
        }
        // Complete original close bytes are committed inside this exact Activate.
        self.hooks.close = Some(request.close_request()?);
        self.hooks.activation = Some(request.clone());
        if self.hooks.mode == broker::DesktopMode::OwnedExecutable {
            let explicit_owned = self.hooks.manifest.desktop_owned_lifetime.is_some();
            let evidence = self.evidence()?;
            evidence.dispositions.primary = Disposition::Unknown {
                reason: "current original primary unproved".into(),
            };
            if explicit_owned {
                ensure!(
                    matches!(
                        evidence.dispositions.descendant_scope,
                        Disposition::NotStarted
                    ),
                    "owned unavailable tree cannot clear historical uncertainty"
                );
            } else {
                evidence.dispositions.descendant_scope = Disposition::Unknown {
                    reason: "current original descendant scope unproved".into(),
                };
            }
        } else if self.hooks.requirement.lifetime == LifetimeRequirement::OwnedPrimaryExited {
            self.evidence()?.dispositions.primary = Disposition::Unknown {
                reason: "current original native primary unproved".into(),
            };
        }
        self.hooks.state = Some(self.dispatch_action(request)?);
        Ok(())
    }
    fn validate_state(&self, state: &broker::BrokerState) -> Result<()> {
        state.validate()?;
        ensure!(
            state.owned_lifetime.as_ref().map(|s| s.definition)
                == self.hooks.manifest.desktop_owned_lifetime,
            "original owned lifetime differs from committed manifest declaration"
        );
        if let Some(native) = &self.hooks.manifest.desktop_native {
            let observed = state
                .native_lifetime
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("native original lifetime projection missing"))?;
            native.validate_state(observed)?;
            if let broker::BrokerEffect::Receipt { receipt } = &state.effect {
                ensure!(
                    receipt.evidence == broker::DesktopEvidence::NativeActivation,
                    "native action borrowed owned executable evidence"
                );
            }
        } else {
            ensure!(
                state.native_lifetime.is_none(),
                "owned action acquired undeclared native authority"
            );
            if let broker::BrokerEffect::Receipt { receipt } = &state.effect {
                ensure!(
                    receipt.evidence != broker::DesktopEvidence::NativeActivation,
                    "owned action borrowed native activation evidence"
                );
            }
        }
        Ok(())
    }
    fn query(&mut self, request: &broker::DesktopRequest) -> Result<broker::BrokerState> {
        let identity = request.identity()?;
        let id = self.core.session.submit(Action::QueryDesktop {
            identity: identity.clone(),
        })?;
        let v2::Reply::DesktopState {
            identity: observed,
            state,
        } = self.core.wait_reply(id)?
        else {
            bail!("original desktop query unavailable")
        };
        ensure!(
            *observed == identity,
            "desktop query original identity changed"
        );
        self.validate_state(&state)?;
        Ok(*state)
    }
    pub(super) fn readiness(&mut self, phase: IntegrationPhase, first: bool) -> Result<()> {
        let activation = self
            .hooks
            .activation
            .clone()
            .ok_or_else(|| anyhow::anyhow!("original activation missing"))?;
        let identity = activation.identity()?;
        let expected = self.readiness_manifest().clone();
        let nonce = if first {
            v2::BindingId::parse(&activation.activation_nonce)?
        } else {
            nonce()?
        };
        let expectation = desktop_readiness_expectation(&expected)?;
        let challenge = ReadinessChallenge {
            resource_binding_digest: activation.prior_binding_digest.clone(),
            action_digest: identity.request_digest.clone(),
            expectation_digest: expected.digest()?.to_hex(),
            ownership_id: self.core.resource.resource().ownership_id.clone(),
            lease: identity.broker_lease.clone(),
            slot: self.hooks.requirement.resource.clone(),
            operation: identity.helper_operation.clone(),
            nonce: nonce.to_hex(),
            deadline_ms: self.hooks.requirement.readiness.deadline_ms,
        };
        self.core
            .projector
            .publish_readiness_probe(ReadinessProbeRecord {
                challenge: challenge.clone(),
                expectation,
                expectation_digest: challenge.expectation_digest.clone(),
                observation_ids: vec![],
                completion: Disposition::Unknown {
                    reason: "fresh original readiness probe published".into(),
                },
            })?;
        let id = self.core.session.submit(Action::ProbeDesktopReadiness {
            identity: identity.clone(),
            nonce,
            deadline_ms: challenge.deadline_ms,
        })?;
        let v2::Reply::DesktopReadiness {
            identity: observed,
            observation,
        } = self.core.wait_reply(id)?
        else {
            bail!("original desktop readiness unavailable")
        };
        ensure!(
            *observed == identity
                && observation.challenge_digest == challenge.digest()?
                && observation.expectation_digest == challenge.expectation_digest,
            "desktop readiness changed original probe"
        );
        observation.validate_desktop(&identity)?;
        observation.validate_desktop_peer(&self.hooks.header.broker_principal)?;
        self.hooks.state = Some(self.query(&activation)?);
        self.hooks.sequence += 1;
        let state = self.hooks.state.clone().unwrap();
        let evidence = match &state.effect {
            broker::BrokerEffect::Receipt { receipt } => Some(receipt.evidence),
            _ => None,
        };
        let binding = self.binding(
            phase,
            activation.prior_binding_digest.clone(),
            observation.state != v2::ReadinessState::Incomplete,
            Some(activation.helper_operation.clone()),
        )?;
        let activated =
            matches!(&state.effect,broker::BrokerEffect::Receipt {receipt} if receipt.acknowledged);
        let bound = BoundDesktopObservation {
            generation: activation.transition().map(desktop_generation_evidence),
            entry_valid: None,
            binding: binding.clone(),
            broker_state: Some(state),
            readiness: Some(*observation),
            evidence,
            entry_present: IntegrationProvenance::Unavailable {
                reason: "readiness does not inspect current entry presence".into(),
            },
            activated: if activated {
                IntegrationProvenance::Observed {
                    value: true,
                    source: "original authenticated broker activation receipt".into(),
                }
            } else {
                IntegrationProvenance::Unavailable {
                    reason: "original activation effect receipt unproved".into(),
                }
            },
            primary_exited: IntegrationProvenance::Unavailable {
                reason: "readiness proves no primary exit".into(),
            },
            descendant_scope_settled: IntegrationProvenance::Unavailable {
                reason: "readiness proves no descendant join".into(),
            },
        };
        let completion = desktop_readiness_completion(&bound);
        self.core.projector.record_readiness_probe_completion(
            &challenge.ownership_id,
            &challenge.operation,
            &challenge.nonce,
            &challenge.expectation_digest,
            vec![binding.id.clone()],
            completion,
        )?;
        self.record_observation(bound)
    }
    pub(super) fn close_original(&mut self, phase: IntegrationPhase) -> Result<()> {
        let Some(close) = self.hooks.close.clone() else {
            return Ok(());
        };
        let activation = self.hooks.activation.clone().unwrap();
        let published = matches!(self.core.projector.journal(),StoredJournal::NativeIntegration(j) if j.integration_actions.iter().any(|a|a.lease==close.helper_lease && a.operation==close.helper_operation));
        self.hooks.state = Some(if published {
            self.query(&close)?
        } else {
            self.dispatch_action(close.clone())?
        });
        let cancel_id = self.core.session.submit(Action::CancelDesktop {
            identity: activation.identity()?,
        })?;
        let v2::Reply::DesktopState {
            identity: cancelled,
            state,
        } = self.core.wait_reply(cancel_id)?
        else {
            bail!("original desktop cleanup unavailable")
        };
        ensure!(
            *cancelled == activation.identity()?,
            "cleanup changed original desktop identity"
        );
        self.hooks.state = Some(*state);
        let end = Instant::now()
            + Duration::from_millis(self.hooks.requirement.readiness.deadline_ms.into());
        loop {
            let id = self.core.session.submit(Action::CancelDesktop {
                identity: activation.identity()?,
            })?;
            let _ = self.core.wait_reply(id)?;
            let state = self.query(&activation)?;
            let done = settled_original_broker_state(&state);
            self.hooks.state = Some(state);
            let close_state = self.query(&close)?;
            if done && settled_original_broker_state(&close_state) {
                break;
            }
            if Instant::now() >= end {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let state = self.hooks.state.clone().unwrap();
        self.hooks.sequence += 1;
        let evidence = match &state.effect {
            broker::BrokerEffect::Receipt { receipt } => Some(receipt.evidence),
            _ => None,
        };
        let binding = self.binding(
            phase,
            activation.prior_binding_digest.clone(),
            true,
            Some(activation.helper_operation.clone()),
        )?;
        let activated =
            matches!(&state.effect,broker::BrokerEffect::Receipt {receipt} if receipt.acknowledged);
        let bound = BoundDesktopObservation {
            generation: activation.transition().map(desktop_generation_evidence),
            entry_valid: None,
            binding,
            broker_state: Some(state.clone()),
            readiness: None,
            evidence,
            entry_present: IntegrationProvenance::Unavailable {
                reason: "lifetime query does not inspect current entry presence".into(),
            },
            activated: if activated {
                IntegrationProvenance::Observed {
                    value: true,
                    source: "original broker activation receipt".into(),
                }
            } else {
                IntegrationProvenance::Unavailable {
                    reason: "original activation effect unproved".into(),
                }
            },
            primary_exited: if state
                .native_lifetime
                .as_ref()
                .is_some_and(|n| n.primary == broker::NativePrimaryState::Exited)
                || (state.native_lifetime.is_none()
                    && state.primary == broker::BrokerPrimary::Exited)
            {
                IntegrationProvenance::Observed {
                    value: true,
                    source: if state.native_lifetime.is_some() {
                        "original native child/HANDLE observed exit"
                    } else {
                        "original Host process owner observed exit"
                    }
                    .into(),
                }
            } else {
                IntegrationProvenance::Unavailable {
                    reason: "original primary exit unproved".into(),
                }
            },
            descendant_scope_settled: if state.descendants == broker::BrokerDescendants::Joined {
                IntegrationProvenance::Observed {
                    value: true,
                    source: "original Host group/job joined".into(),
                }
            } else {
                IntegrationProvenance::Unavailable {
                    reason: if state.owned_lifetime.as_ref().is_some_and(|s| s.descendants == broker::OwnedTreeState::Unavailable) {
                        "original backend has no descendant-tree capability; primary/local retirement is separate"
                    } else { "original descendant scope unproved" }.into(),
                }
            },
        };
        for previous in self.evidence()?.desktops[0]
            .observations
            .iter()
            .filter(|o| o.binding.action == bound.binding.action)
            .filter_map(|o| o.broker_state.as_ref())
        {
            validate_broker_progress(previous, &state)?;
        }
        let original = ActionReference {
            operation: activation.helper_operation.clone(),
            request_digest: v2::request_digest(&v2::Request::DesktopAction {
                request: Box::new(activation.clone()),
            })?
            .to_hex(),
        };
        let evidence = self.evidence()?;
        if let Some(owned) = &state.owned_lifetime {
            ensure!(
                activation.owned_lifetime_definition() == Some(owned.definition)
                    && evidence.desktops[0].actions.contains(&original),
                "owned lifetime changed original activation declaration/digest"
            );
            evidence.owned_lifetime = Some(OwnedDesktopLifetimeProjection {
                original,
                state: owned.clone(),
            });
            evidence.dispositions.primary = if owned.primary == broker::OwnedPrimaryState::Exited {
                Disposition::Complete {
                    receipt: "original owned primary exited".into(),
                }
            } else {
                Disposition::Unknown {
                    reason: "original owned primary exit unproved".into(),
                }
            };
            if owned.descendants == broker::OwnedTreeState::Unavailable {
                ensure!(
                    matches!(
                        evidence.dispositions.descendant_scope,
                        Disposition::NotStarted
                    ),
                    "owned unavailable tree cannot clear historical uncertainty"
                );
            } else {
                evidence.dispositions.descendant_scope = Disposition::Unknown {
                    reason: "original owned descendant uncertainty retained".into(),
                };
            }
            return self.record_observation(bound);
        }
        if let Some(native) = &state.native_lifetime {
            ensure!(
                evidence.desktops[0].actions.contains(&original),
                "native lifetime changed original activation digest"
            );
            evidence.native_lifetime = Some(NativeDesktopLifetimeProjection {
                original,
                state: native.clone(),
            });
            use broker::{NativeLifetimeAuthority, NativePrimaryState, NativeTreeState};
            evidence.dispositions.primary = if native.authority
                == NativeLifetimeAuthority::ManagerInstance
                && native.primary == NativePrimaryState::Unavailable
            {
                ensure!(
                    matches!(evidence.dispositions.primary, Disposition::NotStarted),
                    "unavailable native primary cannot clear historical uncertainty"
                );
                Disposition::NotStarted
            } else if native.primary == NativePrimaryState::Exited {
                Disposition::Complete {
                    receipt: "original native child/HANDLE exited".into(),
                }
            } else {
                Disposition::Unknown {
                    reason: "required original native primary exit unproved".into(),
                }
            };
            evidence.dispositions.descendant_scope =
                if native.descendants == NativeTreeState::Unavailable {
                    ensure!(
                        matches!(
                            evidence.dispositions.descendant_scope,
                            Disposition::NotStarted
                        ),
                        "unavailable native tree cannot clear historical uncertainty"
                    );
                    Disposition::NotStarted
                } else {
                    Disposition::Unknown {
                        reason: "historical native descendant scope unknown".into(),
                    }
                };
            return self.record_observation(bound);
        }
        evidence.dispositions.primary = if state.primary == broker::BrokerPrimary::Exited {
            Disposition::Complete {
                receipt: "original primary exited".into(),
            }
        } else {
            Disposition::Unknown {
                reason: "original primary exit unproved".into(),
            }
        };
        evidence.dispositions.descendant_scope =
            if state.descendants == broker::BrokerDescendants::Joined && state.cleanup_complete {
                Disposition::Complete {
                    receipt: "original descendant scope and IPC joined".into(),
                }
            } else {
                Disposition::Unknown {
                    reason: "original descendant scope/IPC settlement unproved".into(),
                }
            };
        self.record_observation(bound)
    }
}
