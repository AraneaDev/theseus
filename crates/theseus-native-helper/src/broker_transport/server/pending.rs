use super::*;
pub(in crate::broker_transport) enum Pending {
    Activation {
        reply_to: u64,
        identity: Box<BrokerOperationIdentity>,
        deadline: Instant,
    },
    Ready {
        reply_to: u64,
        identity: Box<BrokerOperationIdentity>,
        nonce: v2::BindingId,
        deadline: Instant,
    },
    Close {
        reply_to: u64,
        request: Box<DesktopRequest>,
        deadline: Instant,
    },
}
impl Pending {
    pub(in crate::broker_transport) fn semantic_deadline(&self) -> Instant {
        match self {
            Self::Ready { deadline, .. }
            | Self::Close { deadline, .. }
            | Self::Activation { deadline, .. } => *deadline,
        }
    }
}
pub(super) struct RetainedRuntime(pub(super) Option<BrokerRuntime>);
impl std::ops::Deref for RetainedRuntime {
    type Target = BrokerRuntime;
    fn deref(&self) -> &BrokerRuntime {
        self.0.as_ref().expect("retained original runtime")
    }
}
impl std::ops::DerefMut for RetainedRuntime {
    fn deref_mut(&mut self) -> &mut BrokerRuntime {
        self.0.as_mut().expect("retained original runtime")
    }
}

impl BrokerServer {
    pub(super) fn advance_pending(&mut self) -> io::Result<()> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        let semantic_deadline = pending.semantic_deadline();
        let result = (|| match pending {
            Pending::Activation {
                reply_to,
                identity,
                deadline,
            } => {
                let Some(state) =
                    self.runtime
                        .poll_activation(&identity, semantic_deadline, &self.cancel)?
                else {
                    self.pending = Some(Pending::Activation {
                        reply_to,
                        identity,
                        deadline,
                    });
                    return Ok(());
                };
                self.reply(
                    reply_to,
                    v2::Reply::DesktopState {
                        identity,
                        state: Box::new(state),
                    },
                )
            }
            Pending::Ready {
                reply_to,
                identity,
                nonce,
                deadline,
            } => {
                let observed = self.runtime.poll_ready_with_nonce(
                    &identity,
                    nonce,
                    semantic_deadline,
                    &self.cancel,
                )?;
                let Some(observed) = observed else {
                    self.pending = Some(Pending::Ready {
                        reply_to,
                        identity,
                        nonce,
                        deadline,
                    });
                    return Ok(());
                };
                let reply =
                    readiness_reply(&self.config, &self.runtime, &identity, nonce, observed)?;
                self.reply(reply_to, reply)
            }
            Pending::Close {
                reply_to,
                request,
                deadline,
            } => {
                let state = self.runtime.poll_request_close(
                    (*request).clone(),
                    semantic_deadline,
                    &self.cancel,
                )?;
                let Some(state) = state else {
                    self.pending = Some(Pending::Close {
                        reply_to,
                        request,
                        deadline,
                    });
                    return Ok(());
                };
                self.reply(
                    reply_to,
                    v2::Reply::DesktopState {
                        identity: Box::new(request.identity().map_err(io::Error::other)?),
                        state: Box::new(state),
                    },
                )
            }
        })();
        if let Err(error) = result {
            self.remember(error);
            return Err(io::Error::other(
                "original pending broker action failed; owners retained",
            ));
        }
        Ok(())
    }
}
fn readiness_reply(
    config: &BrokerServerConfig,
    runtime: &BrokerRuntime,
    identity: &BrokerOperationIdentity,
    nonce: v2::BindingId,
    observed: host::ReadinessObservation,
) -> io::Result<v2::Reply> {
    let view = BrokerLedgerView::replay(runtime.store().records()).map_err(io::Error::other)?;
    let request = view
        .original(&identity.helper_operation)
        .ok_or_else(|| io::Error::other("original readiness activation missing"))?;
    let key = request
        .transition()
        .map_or(v2::DesktopDefinitionKey::CONFIGURED_TARGET, |t| {
            t.definition
        });
    let expectation = config
        .manifest
        .desktop_definition(&key)
        .map_err(io::Error::other)?
        .readiness;
    let action = request.digest().map_err(io::Error::other)?;
    let challenge = host::ReadinessChallenge {
        protocol: host::ReadyProtocol::TheseusFixtureReadyV1,
        lease: request.broker_lease.clone(),
        slot: request.slot.as_str().into(),
        resource_binding_digest: launch::digest(&request.prior_binding_digest)?,
        start_or_activation_operation: request.helper_operation.clone(),
        action_digest: *action.as_bytes(),
        nonce: *nonce.as_bytes(),
        expectation_digest: *expectation.digest().map_err(io::Error::other)?.as_bytes(),
    };
    let expectation_digest = expectation.digest().map_err(io::Error::other)?.to_hex();
    let response = observed.response.as_ref();
    let wire = v2::ReadinessObservation {
        peer_sid: observed.peer_sid.as_ref().map(|sid| sid.as_str().into()),
        expectation,
        slot: request.slot.clone(),
        operation: request.helper_operation.clone(),
        action_digest: action.to_hex(),
        resource_binding_digest: request.prior_binding_digest.clone(),
        challenge_digest: format!("{:x}", Sha256::digest(host::encode_challenge(&challenge)?)),
        expectation_digest,
        state: match observed.state {
            host::ReadinessState::Ready => v2::ReadinessState::Ready,
            host::ReadinessState::Failed => v2::ReadinessState::Failed,
            host::ReadinessState::Incomplete => v2::ReadinessState::Incomplete,
        },
        authenticated_instance: observed.authenticated_instance,
        instance_binding_digest: v2::Digest::from_bytes(observed.instance_binding_digest).to_hex(),
        peer_uid: observed.peer.map(|p| p.uid),
        peer_gid: observed.peer.map(|p| p.gid),
        artifact_sha256: response.map(|r| v2::Digest::from_bytes(r.artifact_sha256).to_hex()),
        version_literal: response.map(|r| r.version_literal.clone()),
        data_sha256: response.map(|r| v2::Digest::from_bytes(r.data_sha256).to_hex()),
        configuration_digest: response
            .map(|r| v2::Digest::from_bytes(r.configuration_digest).to_hex()),
        initialized: response.map(|r| r.initialized),
        response_digest: response
            .map(|r| host::encode_response(r).map(|bytes| format!("{:x}", Sha256::digest(bytes))))
            .transpose()?,
        failure: observed.failure.map(|failure| format!("{failure:?}")),
    };
    wire.validate_desktop(identity).map_err(io::Error::other)?;
    wire.validate_desktop_peer(&config.header.broker_principal)
        .map_err(io::Error::other)?;
    Ok(v2::Reply::DesktopReadiness {
        identity: Box::new(identity.clone()),
        observation: Box::new(wire),
    })
}

#[derive(Debug)]
pub(super) struct OriginalRefusal(pub(super) v2::RefusalCode);
impl std::fmt::Display for OriginalRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "original broker refusal: {:?}", self.0)
    }
}
impl std::error::Error for OriginalRefusal {}

/// A failed post-effect validation must return the new quarantined observation,
/// never the successful pre-validation snapshot.
pub(in crate::broker_transport) fn finish_activation_check(
    state: BrokerState,
    validation: io::Result<()>,
    quarantine: impl FnOnce(io::Error) -> io::Result<BrokerState>,
) -> io::Result<BrokerState> {
    match validation {
        Ok(()) => Ok(state),
        Err(error) => quarantine(error),
    }
}
pub(in crate::broker_transport) fn validate_cancel_identity(
    original: &DesktopRequest,
    pending: Option<&Pending>,
    identity: &BrokerOperationIdentity,
) -> io::Result<()> {
    if original.identity().map_err(io::Error::other)? != *identity
        || !matches!(original.action, DesktopAction::Activate { .. })
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "cancel does not name the original activation owner",
        ));
    }
    let related = match pending {
        None => true,
        Some(
            Pending::Ready {
                identity: ready, ..
            }
            | Pending::Activation {
                identity: ready, ..
            },
        ) => ready.as_ref() == identity,
        Some(Pending::Close { request, .. }) => {
            original.close_request().map_err(io::Error::other)? == **request
        }
    };
    if !related {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "cancel is unrelated to the original pending action",
        ));
    }
    Ok(())
}
pub(in crate::broker_transport) fn with_cancel_flag<T>(
    flag: &Arc<AtomicBool>,
    operation: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    struct Reset(Arc<AtomicBool>);
    impl Drop for Reset {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    let _reset = Reset(Arc::clone(flag));
    flag.store(true, Ordering::Release);
    operation()
}
