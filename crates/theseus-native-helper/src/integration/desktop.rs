//! Original desktop requests serialized by the existing helper worker.
use crate::{
    broker::{self, BindDispatch},
    broker_transport::BrokerConnection,
};
use std::{
    io,
    time::{Duration, Instant},
};
use theseus_host::fs::{HostDir, RootPolicy};
use theseus_native_helper_store::{IntentStore, LedgerStore};
use theseus_native_protocol::v2::{self, broker::*};

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
fn append_event(store: &mut LedgerStore, event: BrokerEvent) -> io::Result<()> {
    let index = store.records().len().try_into().map_err(io::Error::other)?;
    let record = v2::ledger::Record::new(index, v2::ledger::Entry::Broker { event })
        .map_err(io::Error::other)?;
    store.append(record).map_err(io::Error::other)?;
    Ok(())
}
struct Pending {
    original: v2::Request,
    wire: v2::Request,
    bind: Option<BindDispatch>,
    sent: bool,
    deadline: Instant,
}
pub(crate) struct DesktopRuntime {
    header: IntegrationBrokerHeader,
    qualification: Option<theseus_native_helper_store::VerifiedLinuxGioBootstrap>,
    manifest: v2::IntegrationManifest,
    current_sut: Option<(DesktopSutRunWitness, Option<v2::DesktopDefinitionKey>)>,
    prior_definition: Option<v2::DesktopDefinitionKey>,
    slot: v2::ResourceSlot,
    intents: IntentStore,
    connection: Option<BrokerConnection>,
    failed_connection: Option<Box<BrokerConnection>>,
    pending: Option<Pending>,
    cancel: Option<v2::Request>,
    cancel_sent: bool,
    pub(crate) errors: Vec<io::Error>,
    pub(crate) unavailable: bool,
}
impl DesktopRuntime {
    pub(crate) fn new(
        config: &super::service::ServiceConfig,
        ledger_root: &std::path::Path,
    ) -> io::Result<Self> {
        let header = config
            .context
            .desktop_broker_header
            .clone()
            .ok_or_else(|| invalid("desktop session missing"))?;
        let slot = config
            .manifest
            .desktop
            .as_ref()
            .ok_or_else(|| invalid("desktop manifest missing"))?
            .slot
            .clone();
        config
            .manifest
            .validate_context(
                &config.context,
                match header.os.as_str() {
                    "linux" => v2::Os::Linux,
                    "macos" => v2::Os::Macos,
                    "windows" => v2::Os::Windows,
                    _ => return Err(invalid("desktop platform invalid")),
                },
            )
            .map_err(io::Error::other)?;
        Ok(Self {
            header,
            qualification: None,
            slot,
            manifest: config.manifest.clone(),
            current_sut: None,
            prior_definition: None,
            intents: IntentStore::new(HostDir::open(
                ledger_root,
                RootPolicy::AllComponentsNoFollow,
            )?),
            connection: None,
            failed_connection: None,
            pending: None,
            cancel: None,
            cancel_sent: false,
            errors: vec![],
            unavailable: false,
        })
    }
    /// Validate original private bootstrap and retain actual graph owners before
    /// advertising Description. Config reference bytes alone never set this cap.
    pub(crate) fn retain_provider_qualification(
        &mut self,
        store: &LedgerStore,
        context: &v2::IntegrationRuntimeContext,
    ) -> io::Result<()> {
        let actual = store
            .retain_linux_gio_qualification()
            .map_err(io::Error::other)?;
        match (&self.header.linux_gio_qualification, actual) {
            (None, None) => Ok(()),
            (Some(expected), Some(proof)) => {
                proof
                    .attestation()
                    .validate_context(&self.manifest, context)
                    .map_err(io::Error::other)?;
                if proof.attestation().reference().map_err(io::Error::other)? != *expected {
                    return Err(invalid(
                        "qualification reference differs from original provider bootstrap",
                    ));
                }
                proof.revalidate().map_err(io::Error::other)?;
                self.qualification = Some(proof);
                Ok(())
            }
            _ => Err(invalid(
                "qualification config is not backed by original private bootstrap",
            )),
        }
    }
    fn revalidate_qualification(&self) -> io::Result<()> {
        if self.header.linux_gio_qualification.is_some() {
            self.qualification
                .as_ref()
                .ok_or_else(|| invalid("original provider qualification owner unavailable"))?
                .revalidate()
                .map_err(io::Error::other)?;
        }
        Ok(())
    }
    /// Called only after this worker durably completed this exact original Run.
    /// Serialized witnesses and reopened ledger bytes never call this authority hook.
    pub(crate) fn note_sut(
        &mut self,
        store: &mut LedgerStore,
        program: &v2::ProgramId,
        lease: v2::LeaseId,
        operation: v2::OperationId,
        request_digest: v2::Digest,
        outcome: &v2::Outcome,
    ) -> io::Result<()> {
        if !super::service::admitted_sut_transition(program, outcome) {
            return Ok(());
        }
        let witness = DesktopSutRunWitness {
            lease: lease.to_hex(),
            operation: operation.to_hex(),
            program: program.clone(),
            request_digest: request_digest.to_hex(),
        };
        witness
            .matches_successful_terminal(store.records())
            .map_err(io::Error::other)?;
        let definition = witness
            .definition_after(self.prior_definition)
            .map_err(io::Error::other)?;
        append_event(
            store,
            BrokerEvent::HelperSutRunCompleted {
                witness: witness.clone(),
                definition,
            },
        )?;
        if definition.is_some() {
            self.prior_definition = definition;
        }
        self.current_sut = Some((witness, definition));
        Ok(())
    }
    fn current_generation(
        &self,
        view: &BrokerLedgerView,
        force: bool,
    ) -> io::Result<Option<DesktopActivationTransition>> {
        if self.manifest.desktop_definitions.is_empty() && !force {
            return Ok(None);
        }
        let Some((witness, definition)) = &self.current_sut else {
            return if force {
                Err(invalid("no live completed original SUT boundary"))
            } else {
                Ok(None)
            };
        };
        let Some(key) = *definition else {
            return if force {
                Err(invalid("SUT generation has no admitted definition"))
            } else {
                Ok(None)
            };
        };
        let tuple = self
            .manifest
            .desktop_definition(&key)
            .map_err(io::Error::other)?;
        let previous = view.latest_helper_activation();
        if let Some((old, _)) = previous
            && let Some(transition) = old.transition()
            && transition.sut == *witness
        {
            return Ok(Some(transition.clone()));
        }
        let previous_activation = match previous {
            Some((old, Some(state))) if state.original_retired() => {
                Some(old.identity().map_err(io::Error::other)?)
            }
            Some(_) => return Err(invalid("prior original desktop lifetime remains unsettled")),
            None => None,
        };
        Ok(Some(DesktopActivationTransition {
            definition: key,
            tuple_digest: tuple.digest().map_err(io::Error::other)?.to_hex(),
            sut: witness.clone(),
            previous_activation,
        }))
    }
    pub(crate) fn is_pending(&self) -> bool {
        self.pending.is_some() || self.cancel.is_some()
    }
    pub(crate) fn start(
        &mut self,
        store: &mut LedgerStore,
        lease: v2::LeaseId,
        request: v2::Request,
    ) -> io::Result<Option<v2::Reply>> {
        if self.unavailable {
            return Err(invalid("desktop original exchange unavailable"));
        }
        if self.is_pending() {
            if let v2::Request::CancelDesktop { identity } = &request {
                let view = BrokerLedgerView::replay(store.records()).map_err(io::Error::other)?;
                if self.cancel.is_none()
                    && identity.helper_lease == lease.to_hex()
                    && view
                        .helper_request(identity)
                        .map_err(io::Error::other)?
                        .is_some()
                {
                    self.cancel = Some(request);
                    self.cancel_sent = false;
                    return Ok(None);
                }
            }
            return Err(invalid("desktop original exchange pending"));
        }
        let view = BrokerLedgerView::replay(store.records()).map_err(io::Error::other)?;
        let original = view.binding_original().cloned();
        let receipt = original
            .as_ref()
            .map(|b| view.binding(b))
            .transpose()
            .map_err(io::Error::other)?
            .flatten()
            .cloned();
        let mut bind = None;
        let wire = match &request {
            v2::Request::BrokerBind {
                original: requested,
            }
            | v2::Request::QueryBrokerBind {
                original: requested,
            } => {
                if requested.expected_header != self.header
                    || requested.slot != self.slot
                    || requested.helper_lease != lease.to_hex()
                {
                    return Err(invalid("broker bind outside original ceiling"));
                }
                if matches!(request, v2::Request::QueryBrokerBind { .. })
                    && original.as_ref() != Some(requested.as_ref())
                {
                    return Err(invalid("original broker binding missing"));
                }
                if original.is_none() {
                    self.revalidate_qualification()?;
                }
                let dispatch = broker::begin_bind(store, &mut self.intents, *requested.clone())?;
                let wire = match &dispatch {
                    BindDispatch::Cached(receipt) => {
                        return Ok(Some(v2::Reply::BrokerBinding {
                            receipt: Box::new(receipt.clone()),
                        }));
                    }
                    BindDispatch::AcquireOnce(_) => None,
                    BindDispatch::QueryOriginal { verified, .. } => {
                        Some(broker::query_original_acquire(verified))
                    }
                };
                bind = Some(dispatch);
                wire.unwrap_or(v2::Request::Describe)
            }
            v2::Request::DesktopAction { request: action } => {
                let original = original
                    .as_ref()
                    .ok_or_else(|| invalid("original broker mapping missing"))?;
                if action.helper_lease != lease.to_hex() {
                    return Err(invalid("wrong original helper lease"));
                }
                let identity = action.identity().map_err(io::Error::other)?;
                if view
                    .helper_request(&identity)
                    .map_err(io::Error::other)?
                    .is_some()
                {
                    v2::Request::QueryDesktop {
                        identity: Box::new(identity),
                    }
                } else {
                    if matches!(action.action, DesktopAction::Activate { .. }) {
                        self.revalidate_qualification()?;
                        let expected =
                            self.current_generation(&view, action.transition().is_some())?;
                        if action.transition() != expected.as_ref() {
                            return Err(invalid(
                                "activation changed original SUT generation witness",
                            ));
                        }
                    }
                    if !broker::prepare_helper_action(store, original, action)? {
                        return Err(invalid(
                            "new original action unexpectedly became query only",
                        ));
                    }
                    if let Some(transition) = action.transition() {
                        // Acceptance and exact completed-SUT boundary are durable before transmission.
                        append_event(
                            store,
                            BrokerEvent::HelperTransitionAccepted {
                                identity,
                                transition: transition.clone(),
                            },
                        )?;
                    }
                    request.clone()
                }
            }
            v2::Request::InspectDesktop {
                lease: actual,
                slot,
                generation,
            } => {
                if *actual != lease || slot != &self.slot {
                    return Err(invalid("wrong desktop inspection lease/slot"));
                }
                let receipt = receipt
                    .as_ref()
                    .ok_or_else(|| invalid("original broker mapping missing"))?;
                let expected = self.current_generation(&view, generation.is_some())?;
                if generation
                    .as_ref()
                    .is_some_and(|supplied| Some(supplied) != expected.as_ref())
                {
                    return Err(invalid(
                        "inspection changed original SUT generation witness",
                    ));
                }
                v2::Request::InspectDesktop {
                    lease: v2::LeaseId::parse(&receipt.broker_lease).map_err(io::Error::other)?,
                    slot: slot.clone(),
                    generation: expected,
                }
            }
            v2::Request::QueryDesktop { identity }
            | v2::Request::CancelDesktop { identity }
            | v2::Request::ProbeDesktopReadiness { identity, .. } => {
                if identity.helper_lease != lease.to_hex()
                    || view
                        .helper_request(identity)
                        .map_err(io::Error::other)?
                        .is_none()
                {
                    return Err(invalid("original desktop action missing"));
                }
                request.clone()
            }
            v2::Request::Release { lease: actual } if *actual == lease => {
                let receipt = receipt
                    .as_ref()
                    .ok_or_else(|| invalid("original broker mapping missing"))?;
                v2::Request::Release {
                    lease: v2::LeaseId::parse(&receipt.broker_lease).map_err(io::Error::other)?,
                }
            }
            _ => return Err(invalid("unsupported desktop command")),
        };
        self.pending = Some(Pending {
            original: request,
            wire,
            bind,
            sent: false,
            deadline: Instant::now() + Duration::from_secs(30),
        });
        Ok(None)
    }
    pub(crate) fn poll(
        &mut self,
        store: &mut LedgerStore,
    ) -> io::Result<Option<(v2::Request, v2::Reply)>> {
        if self.pending.is_none() {
            let Some(original) = self.cancel.clone() else {
                return Ok(None);
            };
            let connection = self
                .connection
                .as_mut()
                .ok_or_else(|| invalid("original cancel connection missing"))?;
            let Some(reply) = connection.poll_reply()? else {
                return Ok(None);
            };
            let (
                v2::Request::CancelDesktop { identity: expected },
                v2::Reply::DesktopState { identity, state },
            ) = (&original, &reply)
            else {
                return Err(invalid("original cancel reply changed"));
            };
            if expected != identity {
                return Err(invalid("original cancel identity changed"));
            }
            broker::record_helper_state(store, identity, state)?;
            self.cancel = None;
            self.cancel_sent = false;
            return Ok(Some((original, reply)));
        }
        let pending = self.pending.as_mut().unwrap();
        if Instant::now() >= pending.deadline {
            return Err(invalid("original broker exchange deadline"));
        }
        if self.connection.is_none() {
            match BrokerConnection::connect(&self.header) {
                Ok(connection) => self.connection = Some(connection),
                Err(failure) => {
                    let (error, owner) = failure.into_parts();
                    self.failed_connection = owner;
                    return Err(error);
                }
            }
        }
        let connection = self.connection.as_mut().unwrap();
        if !connection.poll_ready()? {
            return Ok(None);
        }
        if !pending.sent {
            let wire = if let Some(BindDispatch::AcquireOnce(prepared)) = &mut pending.bind {
                prepared.take_request()?
            } else {
                pending.wire.clone()
            };
            // The once-token is consumed before native transmission. Any failure
            // leaves its original acceptance durable and cannot repeat Acquire.
            pending.sent = true;
            connection.start_request(wire)?;
        }
        if let Some(cancel) = &self.cancel
            && !self.cancel_sent
        {
            connection.start_request(cancel.clone())?;
            self.cancel_sent = true;
        }
        let Some(mut reply) = connection.poll_reply()? else {
            return Ok(None);
        };
        if let (
            Some(v2::Request::CancelDesktop { identity: cancel }),
            v2::Reply::DesktopState { identity, state },
        ) = (&self.cancel, &reply)
            && cancel == identity
        {
            broker::record_helper_state(store, identity, state)?;
            let original = self.cancel.take().unwrap();
            self.cancel_sent = false;
            return Ok(Some((original, reply)));
        }
        let mut pending = self.pending.take().unwrap();
        if let Some(dispatch) = pending.bind.take() {
            let v2::Reply::Acquired {
                state:
                    v2::AcquireView::Lease {
                        lease,
                        state: v2::LeaseView::Held { .. },
                    },
                ..
            } = reply
            else {
                return Err(invalid("original broker acquire unresolved"));
            };
            let receipt = match dispatch {
                BindDispatch::AcquireOnce(prepared) => broker::finish_bind(store, prepared, lease)?,
                BindDispatch::QueryOriginal { original, verified } => {
                    let scope = BrokerLedgerView::replay(store.records())
                        .map_err(io::Error::other)?
                        .binding_scope(&original)
                        .ok_or_else(|| invalid("original publication scope missing"))?;
                    broker::finish_original_bind(store, &original, &verified, scope, lease)?
                }
                BindDispatch::Cached(_) => unreachable!(),
            };
            reply = v2::Reply::BrokerBinding {
                receipt: Box::new(receipt),
            };
        } else {
            match &mut reply {
                v2::Reply::DesktopState { identity, state } => {
                    broker::record_helper_state(store, identity, state)?
                }
                v2::Reply::DesktopObservation { lease, observation } => {
                    if let v2::Request::InspectDesktop {
                        lease: original, ..
                    } = &pending.original
                    {
                        let v2::Request::InspectDesktop { generation, .. } = &pending.wire else {
                            return Err(invalid("original inspection wire changed"));
                        };
                        if observation.generation != *generation {
                            return Err(invalid(
                                "broker inspection changed completed SUT generation",
                            ));
                        }
                        *lease = *original;
                    }
                }
                v2::Reply::Lease {
                    state: v2::LeaseView::AwaitingExternalReset { .. },
                    lease: broker_lease,
                } if matches!(pending.original, v2::Request::Release { .. }) => {
                    let original = BrokerLedgerView::replay(store.records())
                        .map_err(io::Error::other)?
                        .binding_original()
                        .cloned()
                        .ok_or_else(|| invalid("original broker bind missing"))?;
                    broker::record_helper_release(store, &original, &broker_lease.to_hex())?;
                }
                _ => {}
            }
        }
        Ok(Some((pending.original, reply)))
    }
    pub(crate) fn fail(&mut self, error: io::Error) {
        self.unavailable = true;
        self.pending = None;
        self.cancel = None;
        if self.errors.len() < 16 {
            self.errors.push(error);
        }
        self.request_stop();
    }
    pub(crate) fn request_stop(&mut self) {
        if let Some(c) = &mut self.connection {
            c.request_stop();
        }
        if let Some(c) = &mut self.failed_connection {
            c.request_stop();
        }
    }
    pub(crate) fn poll_settle(&mut self) -> bool {
        self.request_stop();
        let mut settled = true;
        if let Some(c) = &mut self.connection {
            match c.poll_settle() {
                Ok(done) => settled &= done,
                Err(error) => {
                    settled = false;
                    if self.errors.len() < 16 {
                        self.errors.push(error);
                    }
                }
            }
        }
        if let Some(c) = &mut self.failed_connection {
            match c.poll_settle() {
                Ok(done) => settled &= done,
                Err(error) => {
                    settled = false;
                    if self.errors.len() < 16 {
                        self.errors.push(error);
                    }
                }
            }
        }
        settled
    }
}

#[cfg(test)]
mod generation_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn uninstalled_sut_does_not_reuse_prior_configured_generation() {
        let ready = json!({"role":"configured","artifact":"target","artifact_length":123,"artifact_sha256":"33".repeat(32),"version_literal":"target","data_sha256":"44".repeat(32),"configuration_digest":"55".repeat(32)});
        let expectation: v2::ReadinessDefinitionExpectation =
            serde_json::from_value(ready.clone()).unwrap();
        let mut manifest: v2::IntegrationManifest = serde_json::from_value(json!({"schema":1,"core_manifest_digest":"11".repeat(32),"desktop":{"schema":1,"core_manifest_digest":"11".repeat(32),"slot":"desktop","entry_root":"/fixture/entries","namespace_policy":"exclusive_quiescent","entry":"fixture.desktop","entry_digest":"22".repeat(32),"executable_digest":"33".repeat(32),"allowed_modes":["owned_executable"],"ready_endpoint":{"kind":"unix_path","path":"/fixture/ready.sock"},"close_endpoint":{"kind":"unix_path","path":"/fixture/close.sock"},"expectation_digest":expectation.digest().unwrap().to_hex()},"desktop_executable":{"root":"/fixture","relative_path":"app","length":123,"sha256":"33".repeat(32),"arguments":["/fixture/app"],"environment":[],"working_directory":"/fixture"},"desktop_readiness":ready,"definitions":[],"artifact_slots":[{"slot":"desktop","relative_path":"fixture.desktop","length":128,"sha256":"22".repeat(32)}],"readiness_protocol":"theseus_fixture_ready_v1","desktop_capabilities":["inspect_desktop","owned_executable","request_close"]})).unwrap();
        let mut base = manifest
            .desktop_definition(&v2::DesktopDefinitionKey::CONFIGURED_TARGET)
            .unwrap();
        base.key = v2::DesktopDefinitionKey {
            role: v2::DesktopDefinitionRole::Base,
            artifact: v2::DesktopArtifactRole::Base,
        };
        base.readiness.role = "base".into();
        base.readiness.artifact = "base".into();
        base.desktop.expectation_digest = base.readiness.digest().unwrap().to_hex();
        manifest.desktop_definitions.push(base);
        manifest.validate().unwrap();
        let integration = manifest.digest().unwrap();
        let full = v2::integration_manifest_digest(
            v2::Digest::parse(&manifest.core_manifest_digest).unwrap(),
            integration,
        );
        let header: IntegrationBrokerHeader = serde_json::from_value(json!({"profile":"native_integration_broker_v1","binding":"01".repeat(16),"manifest_digest":full.to_hex(),"workspace_marker":"02".repeat(16),"os":"linux","target_id":"fixture","ownership_id":"03".repeat(16),"provider_epoch":"04".repeat(32),"session_id":"supplied","session_epoch":"05".repeat(32),"broker_endpoint":{"kind":"unix_path","path":"/fixture/broker.sock"},"broker_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"helper_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"display":{"kind":"x11","display":":92","authority_file":"/fixture/auth","bus":"/fixture/bus.sock"},"allowed_modes":["owned_executable"]})).unwrap();
        let context: v2::IntegrationRuntimeContext = serde_json::from_value(json!({"kind":"theseus_native_integration_context","schema":1,"ownership_id":header.ownership_id,"binding":header.binding,"manifest_digest":header.manifest_digest,"workspace_marker":header.workspace_marker,"provider_epoch":header.provider_epoch,"integration_manifest_digest":integration.to_hex(),"desktop_broker_header":header})).unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut runtime = DesktopRuntime::new(
            &super::super::service::ServiceConfig { manifest, context },
            root.path(),
        )
        .unwrap();
        let program = v2::ProgramId::new("uninstall").unwrap();
        let lease = v2::LeaseId::from_bytes([7; 16]);
        let operation = v2::OperationId::from_bytes([8; 16]);
        let request = v2::Request::Run {
            lease,
            operation,
            program: program.clone(),
        };
        let uninstall = DesktopSutRunWitness {
            lease: lease.to_hex(),
            operation: operation.to_hex(),
            request_digest: v2::request_digest(&request).unwrap().to_hex(),
            program,
        };
        let prior = v2::DesktopDefinitionKey::CONFIGURED_TARGET;
        runtime.prior_definition = Some(prior);
        runtime.current_sut = Some((
            uninstall.clone(),
            uninstall.definition_after(Some(prior)).unwrap(),
        ));
        let view = BrokerLedgerView::default();
        assert_eq!(runtime.current_generation(&view, false).unwrap(), None);
        assert!(runtime.current_generation(&view, true).is_err());
        assert_eq!(runtime.prior_definition, Some(prior));
        let program = v2::ProgramId::new("install_target").unwrap();
        let install = DesktopSutRunWitness {
            request_digest: v2::request_digest(&v2::Request::Run {
                lease,
                operation,
                program: program.clone(),
            })
            .unwrap()
            .to_hex(),
            program,
            ..uninstall
        };
        assert_eq!(
            install.definition_after(runtime.prior_definition).unwrap(),
            Some(prior)
        );
    }
}
