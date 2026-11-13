use crate::{Action, ClientError};
use sha2::{Digest as _, Sha256};
use theseus_host::fs::CommitScope;
use theseus_native_helper_store::IntentRecord;
use theseus_native_protocol::v2;

/// Full canonical mutation identity; a prior publication scope is provenance only.
#[derive(Clone)]
pub struct RecoveredOperation {
    pub request: v2::Request,
    pub digest: v2::Digest,
}
#[derive(Clone)]
pub struct RecoveryImage {
    pub binding: IntentRecord,
    pub profile: v2::HelperProfile,
    pub operations: Vec<RecoveredOperation>,
    pub lease: Option<v2::LeaseId>,
}
pub enum ClientMode {
    Fresh,
    Recover {
        image: Box<RecoveryImage>,
        original: theseus_native_helper_store::VerifiedOriginalIntent,
    },
}
#[derive(Default)]
pub(crate) struct Recovery {
    intents: Vec<(IntentRecord, CommitScope, Option<v2::LeaseId>)>,
    operations: Vec<(v2::LeaseId, v2::OperationId, Option<v2::OperationView>)>,
    quarantined: Vec<v2::LeaseId>,
    identities: Vec<MutationIdentity>,
    captures: Vec<StreamEvidence>,
    original: Option<(IntentRecord, v2::HelperProfile, Option<v2::LeaseId>)>,
}
// Owned before encoding; retained across transport epochs in this live process.
struct MutationIdentity {
    request: v2::Request,
    digest: v2::Digest,
}
impl MutationIdentity {
    fn ids(&self) -> (v2::LeaseId, v2::OperationId) {
        match &self.request {
            v2::Request::Run {
                lease, operation, ..
            }
            | v2::Request::ServiceAction {
                lease, operation, ..
            }
            | v2::Request::CreateFile {
                lease, operation, ..
            } => (*lease, *operation),
            v2::Request::BrokerBind { original } => (
                v2::LeaseId::parse(&original.helper_lease).expect("validated bind lease"),
                v2::OperationId::parse(&original.helper_operation)
                    .expect("validated bind operation"),
            ),
            v2::Request::DesktopAction { request } => (
                v2::LeaseId::parse(&request.helper_lease).expect("validated desktop lease"),
                v2::OperationId::parse(&request.helper_operation)
                    .expect("validated desktop operation"),
            ),
            _ => unreachable!("mutation identity"),
        }
    }
    fn validate(&self, state: &v2::OperationView) -> Result<(), ClientError> {
        let create = matches!(self.request, v2::Request::CreateFile { .. });
        let service = matches!(self.request, v2::Request::ServiceAction { .. });
        let valid = match state {
            v2::OperationView::Accepted { operation_kind } => {
                *operation_kind
                    == if service {
                        v2::OperationKind::ServiceAction
                    } else if create {
                        v2::OperationKind::CreateFile
                    } else {
                        v2::OperationKind::Run
                    }
            }
            v2::OperationView::Running => !create && !service,
            v2::OperationView::Finished { outcome } => match outcome {
                v2::Outcome::ServiceActionCompleted { .. } => service,
                v2::Outcome::FileCreated { length, digest } => match &self.request {
                    v2::Request::CreateFile { bytes, .. } => {
                        usize::from(*length) == bytes.as_slice().len()
                            && *digest
                                == v2::Digest::from_bytes(Sha256::digest(bytes.as_slice()).into())
                    }
                    _ => false,
                },
                v2::Outcome::FileCreateFailed { .. } => create,
                v2::Outcome::RunCompleted { .. } | v2::Outcome::LaunchFailed { .. } => {
                    !create && !service
                }
                v2::Outcome::NotStarted { .. } => true,
            },
            _ => true,
        };
        // Keep the canonical digest alongside the full request, not a derived
        // replacement identity that could authorize a second mutation.
        debug_assert_eq!(v2::request_digest(&self.request).ok(), Some(self.digest));
        if valid {
            Ok(())
        } else {
            Err(ClientError::Protocol)
        }
    }
}
struct StreamEvidence {
    lease: v2::LeaseId,
    operation: v2::OperationId,
    observed: usize,
    ends: [Option<u64>; 2],
    gap: bool,
    truncated: bool,
}
impl StreamEvidence {
    fn finish(&self, outcome: &v2::Outcome) -> Result<(), ClientError> {
        match outcome {
            v2::Outcome::RunCompleted {
                stdout_bytes,
                stderr_bytes,
                truncated,
                ..
            } if !self.ends[0].is_some_and(|end| end > *stdout_bytes)
                && !self.ends[1].is_some_and(|end| end > *stderr_bytes)
                && (!(self.gap || self.truncated) || *truncated) =>
            {
                Ok(())
            }
            v2::Outcome::NotStarted { .. } | v2::Outcome::LaunchFailed { .. }
                if self.observed == 0 && !self.gap && !self.truncated =>
            {
                Ok(())
            }
            _ => Err(ClientError::Protocol),
        }
    }
}
impl Recovery {
    pub(crate) fn from_mode(
        mode: ClientMode,
        expected: &crate::ExpectedHelper,
    ) -> Result<(Self, bool), ClientError> {
        match mode {
            ClientMode::Fresh => Ok((Self::default(), false)),
            ClientMode::Recover { image, original } => {
                let image = *image;
                let b = &expected.binding;
                if image.binding != *original.record()
                    || image.binding.binding != b.binding
                    || image.binding.manifest_digest != b.manifest_digest
                    || image.binding.workspace_marker != b.workspace_marker
                    || image.profile != expected.profile
                    || image.operations.len() > v2::MAX_OPERATIONS
                {
                    return Err(ClientError::Preflight);
                }
                let mut r = Self::default();
                for op in image.operations {
                    if v2::request_digest(&op.request).map_err(|_| ClientError::Protocol)?
                        != op.digest
                    {
                        return Err(ClientError::Protocol);
                    }
                    let (lease, operation, run) = match &op.request {
                        v2::Request::Run {
                            lease,
                            operation,
                            program,
                        } if expected.programs.as_slice().contains(program) => {
                            (*lease, *operation, true)
                        }
                        v2::Request::ServiceAction {
                            lease, operation, ..
                        } if matches!(expected.profile, v2::HelperProfile::Integration { .. }) => {
                            (*lease, *operation, false)
                        }
                        v2::Request::CreateFile {
                            lease, operation, ..
                        } => (*lease, *operation, false),
                        v2::Request::BrokerBind { original }
                            if matches!(
                                expected.profile,
                                v2::HelperProfile::Integration { .. }
                            ) =>
                        {
                            (
                                v2::LeaseId::parse(&original.helper_lease)
                                    .map_err(|_| ClientError::Protocol)?,
                                v2::OperationId::parse(&original.helper_operation)
                                    .map_err(|_| ClientError::Protocol)?,
                                false,
                            )
                        }
                        v2::Request::DesktopAction { request }
                            if matches!(
                                expected.profile,
                                v2::HelperProfile::Integration { .. }
                            ) =>
                        {
                            (
                                v2::LeaseId::parse(&request.helper_lease)
                                    .map_err(|_| ClientError::Protocol)?,
                                v2::OperationId::parse(&request.helper_operation)
                                    .map_err(|_| ClientError::Protocol)?,
                                false,
                            )
                        }
                        _ => return Err(ClientError::Protocol),
                    };
                    if Some(lease) != image.lease
                        || r.identities.iter().any(|i| i.ids() == (lease, operation))
                    {
                        return Err(ClientError::Protocol);
                    }
                    let desktop = matches!(
                        op.request,
                        v2::Request::BrokerBind { .. } | v2::Request::DesktopAction { .. }
                    );
                    r.identities.push(MutationIdentity {
                        request: op.request,
                        digest: op.digest,
                    });
                    if !desktop {
                        r.operations.push((lease, operation, None));
                    }
                    if run {
                        r.stream(lease, operation, false)?;
                    }
                }
                r.original = Some((image.binding, image.profile, image.lease));
                Ok((r, true))
            }
        }
    }
    pub(crate) fn export(
        &self,
        expected: &crate::ExpectedHelper,
    ) -> Result<RecoveryImage, ClientError> {
        if self.intents.len() > 1 {
            return Err(ClientError::Capacity);
        }
        let (binding, profile, lease) = match self.original.clone() {
            Some(v) => v,
            None => {
                let (record, _, lease) =
                    self.intents.first().ok_or(ClientError::RecoveryRequired)?;
                (record.clone(), expected.profile, *lease)
            }
        };
        Ok(RecoveryImage {
            binding,
            profile,
            lease,
            operations: self
                .identities
                .iter()
                .map(|i| RecoveredOperation {
                    request: i.request.clone(),
                    digest: i.digest,
                })
                .collect(),
        })
    }

    fn stream(
        &mut self,
        lease: v2::LeaseId,
        operation: v2::OperationId,
        fresh: bool,
    ) -> Result<&mut StreamEvidence, ClientError> {
        let index = match self
            .captures
            .iter()
            .position(|s| s.lease == lease && s.operation == operation)
        {
            Some(index) => index,
            None => {
                if self.captures.len() == v2::MAX_OPERATIONS {
                    return Err(ClientError::Capacity);
                }
                self.captures.push(StreamEvidence {
                    lease,
                    operation,
                    observed: 0,
                    ends: if fresh { [Some(0); 2] } else { [None; 2] },
                    gap: false,
                    truncated: false,
                });
                self.captures.len() - 1
            }
        };
        Ok(&mut self.captures[index])
    }
    pub(crate) fn log(
        &mut self,
        lease: v2::LeaseId,
        operation: v2::OperationId,
        code: v2::LogCode,
    ) -> Result<(), ClientError> {
        if code == v2::LogCode::OutputTruncated {
            self.stream(lease, operation, false)?.truncated = true;
        }
        Ok(())
    }
    pub(crate) fn output(
        &mut self,
        lease: v2::LeaseId,
        operation: v2::OperationId,
        channel: v2::Channel,
        offset: u64,
        n: usize,
    ) -> Result<(), ClientError> {
        let stream = self.stream(lease, operation, false)?;
        let index = usize::from(channel == v2::Channel::Stderr);
        let end = offset.checked_add(n as u64).ok_or(ClientError::Protocol)?;
        let observed = stream
            .observed
            .checked_add(n)
            .ok_or(ClientError::Protocol)?;
        if observed > v2::MAX_STREAM_BYTES || stream.ends[index].is_some_and(|last| offset < last) {
            return Err(ClientError::Protocol);
        }
        stream.gap |= stream.ends[index].is_some_and(|last| offset > last);
        stream.observed = observed;
        stream.ends[index] = Some(end);
        Ok(())
    }
    pub(crate) fn request(
        &mut self,
        action: Action<'_>,
        expected: &crate::ExpectedHelper,
    ) -> Result<v2::Request, ClientError> {
        if let Some((_, _, lease)) = &self.original {
            let valid=match &action {
                Action::QueryOriginalAcquire(p)=>self.original.as_ref().is_some_and(|(record,_,_)|record==p.record()),
                Action::Query {lease:l,operation}|Action::Cancel {lease:l,operation}=>self.identities.iter().any(|i|i.ids()==(*l,*operation)),
                Action::QueryBrokerBind {original}=>self.identities.iter().any(|i|matches!(&i.request,v2::Request::BrokerBind {original:o} if o.as_ref()==original)),
                Action::QueryDesktop {identity}|Action::CancelDesktop {identity}=>self.require_desktop_identity(identity).is_ok(),
                Action::QueryLease(l)|Action::Release(l)=>Some(*l)==*lease,
                _=>false,
            };
            if !valid {
                return Err(ClientError::RecoveryRequired);
            }
        }
        Ok(match action {
            Action::BrokerBind { original } => {
                self.require_integration(expected)?;
                original.validate().map_err(|_| ClientError::Protocol)?;
                self.remember_desktop(v2::Request::BrokerBind {
                    original: Box::new(original),
                })?
            }
            Action::DesktopAction { request } => {
                self.require_integration(expected)?;
                request.validate().map_err(|_| ClientError::Protocol)?;
                self.remember_desktop(v2::Request::DesktopAction {
                    request: Box::new(request),
                })?
            }
            Action::QueryBrokerBind { original } => {
                self.require_integration(expected)?;
                if !self.identities.iter().any(|i|matches!(&i.request,v2::Request::BrokerBind {original:o} if o.as_ref()==&original)) {return Err(ClientError::RecoveryRequired);}
                v2::Request::QueryBrokerBind {
                    original: Box::new(original),
                }
            }
            Action::InspectDesktop { lease, slot } => {
                self.require_integration(expected)?;
                v2::Request::InspectDesktop {
                    lease,
                    slot,
                    generation: None,
                }
            }
            Action::QueryDesktop { identity } => {
                self.require_integration(expected)?;
                self.require_desktop_identity(&identity)?;
                v2::Request::QueryDesktop {
                    identity: Box::new(identity),
                }
            }
            Action::CancelDesktop { identity } => {
                self.require_integration(expected)?;
                self.require_desktop_identity(&identity)?;
                v2::Request::CancelDesktop {
                    identity: Box::new(identity),
                }
            }
            Action::ProbeDesktopReadiness {
                identity,
                nonce,
                deadline_ms,
            } => {
                self.require_integration(expected)?;
                self.require_desktop_identity(&identity)?;
                v2::Request::ProbeDesktopReadiness {
                    identity: Box::new(identity),
                    nonce,
                    deadline_ms,
                }
            }
            Action::QueryOriginalAcquire(p) => {
                let record = p.record();
                if record.binding != expected.binding.binding
                    || record.manifest_digest != expected.binding.manifest_digest
                    || record.workspace_marker != expected.binding.workspace_marker
                {
                    return Err(ClientError::Preflight);
                }
                v2::Request::QueryAcquire {
                    intent: record.intent,
                    binding: record.binding,
                    manifest_digest: record.manifest_digest,
                }
            }
            Action::Acquire(p) | Action::RecoverAcquire(p) => {
                let record = p.record();
                let b = &expected.binding;
                if record.binding != b.binding
                    || record.manifest_digest != b.manifest_digest
                    || record.workspace_marker != b.workspace_marker
                {
                    return Err(ClientError::Preflight);
                }
                let recover = matches!(action, Action::RecoverAcquire(_));
                if let Some((old, _, _)) = self
                    .intents
                    .iter()
                    .find(|(old, _, _)| old.intent == record.intent)
                {
                    if old != record {
                        return Err(ClientError::Preflight);
                    }
                    if !recover {
                        return Err(ClientError::RecoveryRequired);
                    }
                } else {
                    if self.intents.len() == v2::MAX_OPERATIONS {
                        return Err(ClientError::Capacity);
                    }
                    self.intents.push((record.clone(), p.commit_scope(), None));
                }
                if recover {
                    v2::Request::QueryAcquire {
                        intent: record.intent,
                        binding: record.binding,
                        manifest_digest: record.manifest_digest,
                    }
                } else {
                    p.request()
                }
            }
            Action::ProbeReadiness {
                deadline_ms,
                lease,
                slot,
                operation,
                action_digest,
                prior_binding_digest,
                nonce,
                definition_role,
                artifact_role,
            } => {
                if !matches!(expected.profile, v2::HelperProfile::Integration { .. }) {
                    return Err(ClientError::Preflight);
                }
                v2::Request::ProbeReadiness {
                    deadline_ms,
                    lease,
                    slot,
                    operation,
                    action_digest,
                    prior_binding_digest,
                    nonce,
                    definition_role,
                    artifact_role,
                }
            }
            Action::ServiceAction {
                lease,
                operation,
                slot,
                prior_binding_digest,
                action,
            } => {
                if !matches!(expected.profile, v2::HelperProfile::Integration { .. }) {
                    return Err(ClientError::Preflight);
                }
                self.mutation(
                    v2::Request::ServiceAction {
                        lease,
                        operation,
                        slot,
                        prior_binding_digest,
                        action,
                    },
                    false,
                )?
            }
            Action::InspectService { lease, slot } => {
                if !matches!(expected.profile, v2::HelperProfile::Integration { .. }) {
                    return Err(ClientError::Preflight);
                }
                v2::Request::InspectService { lease, slot }
            }
            Action::Run {
                lease,
                operation,
                program,
            } => {
                if !expected.programs.as_slice().contains(&program) {
                    return Err(ClientError::Preflight);
                }
                let request = v2::Request::Run {
                    lease,
                    operation,
                    program,
                };
                self.mutation(request, true)?
            }
            Action::CreateFile {
                lease,
                operation,
                path,
                bytes,
            } => {
                if !matches!(expected.profile, v2::HelperProfile::Fixture) {
                    return Err(ClientError::Preflight);
                }
                self.mutation(
                    v2::Request::CreateFile {
                        lease,
                        operation,
                        path,
                        bytes,
                    },
                    false,
                )?
            }
            Action::ReadFile {
                lease,
                path,
                offset,
                length,
            } => v2::Request::ReadFile {
                lease,
                path,
                offset,
                length,
            },
            Action::InspectNativeFile { lease, path } => {
                v2::Request::InspectNativeFile { lease, path }
            }
            Action::InspectFile { lease, path } => v2::Request::InspectFile { lease, path },
            Action::Query { lease, operation } => v2::Request::QueryOperation { lease, operation },
            Action::Cancel { lease, operation } => v2::Request::Cancel { lease, operation },
            Action::QueryLease(lease) => v2::Request::QueryLease { lease },
            Action::Release(lease) => v2::Request::Release { lease },
        })
    }
    fn require_desktop_identity(
        &self,
        identity: &v2::broker::BrokerOperationIdentity,
    ) -> Result<(), ClientError> {
        if !self.identities.iter().any(|i|matches!(&i.request,v2::Request::DesktopAction {request} if request.identity().as_ref()==Ok(identity))) {Err(ClientError::RecoveryRequired)}else{Ok(())}
    }
    pub(crate) fn has_desktop(&self) -> bool {
        self.identities.iter().any(|i| {
            matches!(
                i.request,
                v2::Request::BrokerBind { .. } | v2::Request::DesktopAction { .. }
            )
        })
    }
    fn require_integration(&self, expected: &crate::ExpectedHelper) -> Result<(), ClientError> {
        if !matches!(expected.profile, v2::HelperProfile::Integration { .. }) {
            Err(ClientError::Preflight)
        } else {
            Ok(())
        }
    }
    fn remember_desktop(&mut self, request: v2::Request) -> Result<v2::Request, ClientError> {
        let digest = v2::request_digest(&request).map_err(|_| ClientError::Protocol)?;
        let identity = MutationIdentity {
            request: request.clone(),
            digest,
        };
        let ids = identity.ids();
        if self.quarantined.contains(&ids.0)
            || self.identities.iter().any(|i| i.ids() == ids)
            || self.operations.iter().any(|(l, _, s)| {
                *l == ids.0 && !matches!(s, Some(v2::OperationView::Finished { .. }))
            })
        {
            return Err(ClientError::RecoveryRequired);
        }
        if self.identities.len() >= v2::MAX_OPERATIONS {
            return Err(ClientError::Capacity);
        }
        self.identities.push(identity);
        Ok(request)
    }
    fn mutation(&mut self, request: v2::Request, run: bool) -> Result<v2::Request, ClientError> {
        let digest = v2::request_digest(&request).map_err(|_| ClientError::Protocol)?;
        let identity = MutationIdentity {
            request: request.clone(),
            digest,
        };
        let (lease, operation) = identity.ids();
        if self.quarantined.contains(&lease)
            || self
                .identities
                .iter()
                .any(|i| i.ids() == (lease, operation))
            || self.operations.iter().any(|(l, o, state)| {
                *l == lease
                    && (*o == operation
                        || !matches!(state, Some(v2::OperationView::Finished { .. })))
            })
        {
            return Err(ClientError::RecoveryRequired);
        }
        if self.operations.len() == v2::MAX_OPERATIONS
            || self.identities.len() == v2::MAX_OPERATIONS
        {
            return Err(ClientError::Capacity);
        }
        if run {
            self.stream(lease, operation, true)?;
        }
        self.identities.push(identity);
        self.operations.push((lease, operation, None));
        Ok(request)
    }
    pub(crate) fn observe(&mut self, reply: &v2::Reply) -> Result<(), ClientError> {
        match reply {
            v2::Reply::PrefixAuthorityRevoked { lease, .. } => self.quarantine(*lease)?,
            v2::Reply::Operation {
                lease,
                operation,
                state,
            } => {
                if let Some(identity) = self
                    .identities
                    .iter()
                    .find(|i| i.ids() == (*lease, *operation))
                {
                    identity.validate(state)?;
                }
                if let v2::OperationView::Finished { outcome } = state
                    && let Some(stream) = self
                        .captures
                        .iter()
                        .find(|s| s.lease == *lease && s.operation == *operation)
                {
                    stream.finish(outcome)?;
                }
                if let Some((_, _, old)) = self
                    .operations
                    .iter_mut()
                    .find(|(l, o, _)| l == lease && o == operation)
                {
                    if matches!(
                        old,
                        Some(
                            v2::OperationView::Finished { .. } | v2::OperationView::Unknown { .. }
                        )
                    ) && old.as_ref() != Some(state)
                    {
                        return Err(ClientError::Protocol);
                    }
                    *old = Some(state.clone());
                } else if !matches!(state, v2::OperationView::Missing) {
                    if self.operations.len() == v2::MAX_OPERATIONS {
                        return Err(ClientError::Capacity);
                    }
                    self.operations
                        .push((*lease, *operation, Some(state.clone())));
                }
                if matches!(state, v2::OperationView::Unknown { .. }) {
                    self.quarantine(*lease)?;
                }
            }
            v2::Reply::Acquired {
                intent,
                binding,
                manifest_digest,
                state,
            } => {
                if let Some((record, _, observed)) = &mut self.original {
                    if record.intent != *intent
                        || record.binding != *binding
                        || record.manifest_digest != *manifest_digest
                    {
                        return Err(ClientError::Protocol);
                    }
                    match state {
                        v2::AcquireView::NotFound if observed.is_some() => {
                            return Err(ClientError::Protocol);
                        }
                        v2::AcquireView::Lease { lease, .. } => {
                            if observed.is_some_and(|old| old != *lease) {
                                return Err(ClientError::Protocol);
                            }
                            *observed = Some(*lease);
                        }
                        _ => {}
                    }
                    return Ok(());
                }
                let index = self
                    .intents
                    .iter()
                    .position(|(record, _, _)| {
                        record.intent == *intent
                            && record.binding == *binding
                            && record.manifest_digest == *manifest_digest
                    })
                    .ok_or(ClientError::Protocol)?;
                let observed = self.intents[index].2;
                match state {
                    v2::AcquireView::NotFound if observed.is_some() => {
                        return Err(ClientError::Protocol);
                    }
                    v2::AcquireView::Lease { lease, state } => {
                        if observed.is_some_and(|old| old != *lease) {
                            return Err(ClientError::Protocol);
                        }
                        if !matches!(state, v2::LeaseView::Held { .. }) {
                            self.quarantine(*lease)?;
                        }
                        self.intents[index].2 = Some(*lease);
                    }
                    v2::AcquireView::NotFound => {}
                }
            }
            v2::Reply::Lease { lease, state } if !matches!(state, v2::LeaseView::Held { .. }) => {
                self.quarantine(*lease)?
            }
            _ => {}
        }
        Ok(())
    }
    fn quarantine(&mut self, lease: v2::LeaseId) -> Result<(), ClientError> {
        if !self.quarantined.contains(&lease) {
            if self.quarantined.len() == v2::MAX_OPERATIONS {
                return Err(ClientError::Capacity);
            }
            self.quarantined.push(lease);
        }
        Ok(())
    }
}

impl RecoveryImage {
    /// Bounded canonical image bytes for caller-owned durable publication.
    /// This does not create a PreparedAcquire or grant mutation authority.
    pub fn encode(&self) -> Result<Vec<u8>, ClientError> {
        if self.operations.len() > v2::MAX_OPERATIONS {
            return Err(ClientError::Capacity);
        }
        let mut bytes = b"theseus_native_client_recovery\0\x01".to_vec();
        for field in [
            self.binding.intent.to_hex(),
            self.binding.binding.to_hex(),
            self.binding.manifest_digest.to_hex(),
            self.binding.workspace_marker.to_hex(),
        ] {
            bytes.extend_from_slice(field.as_bytes())
        }
        match self.profile {
            v2::HelperProfile::Fixture => bytes.push(0),
            v2::HelperProfile::Integration {
                provider_epoch,
                core_manifest_digest,
                integration_manifest_digest,
            } => {
                bytes.push(2);
                for d in [
                    provider_epoch,
                    core_manifest_digest,
                    integration_manifest_digest,
                ] {
                    bytes.extend_from_slice(d.to_hex().as_bytes())
                }
            }
            v2::HelperProfile::Native { provider_epoch } => {
                bytes.push(1);
                bytes.extend_from_slice(provider_epoch.to_hex().as_bytes())
            }
        }
        match self.lease {
            None => bytes.push(0),
            Some(lease) => {
                bytes.push(1);
                bytes.extend_from_slice(lease.to_hex().as_bytes())
            }
        }
        bytes.extend_from_slice(&(self.operations.len() as u16).to_be_bytes());
        let mut identities = Vec::new();
        for (index, operation) in self.operations.iter().enumerate() {
            let ids = match &operation.request {
                v2::Request::Run {
                    lease, operation, ..
                }
                | v2::Request::ServiceAction {
                    lease, operation, ..
                }
                | v2::Request::CreateFile {
                    lease, operation, ..
                } => (*lease, *operation),
                _ => return Err(ClientError::Protocol),
            };
            if Some(ids.0) != self.lease
                || identities.contains(&ids)
                || v2::request_digest(&operation.request).map_err(|_| ClientError::Protocol)?
                    != operation.digest
            {
                return Err(ClientError::Protocol);
            }
            identities.push(ids);
            let envelope = v2::Envelope::new(
                v2::SessionId::from_bytes([0x52; 16]),
                index as u64,
                v2::Message::Request {
                    request: operation.request.clone(),
                },
            )
            .map_err(|_| ClientError::Protocol)?;
            let frame = v2::encode(&envelope).map_err(|_| ClientError::Protocol)?;
            bytes.extend_from_slice(operation.digest.to_hex().as_bytes());
            bytes.extend_from_slice(&(frame.len() as u32).to_be_bytes());
            bytes.extend_from_slice(&frame);
        }
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, ClientError> {
        if bytes.len() > v2::MAX_OPERATIONS * v2::MAX_FRAME_BYTES + 512 {
            return Err(ClientError::Capacity);
        }
        struct Input<'a> {
            bytes: &'a [u8],
            at: usize,
        }
        impl<'a> Input<'a> {
            fn take(&mut self, n: usize) -> Result<&'a [u8], ClientError> {
                let end = self.at.checked_add(n).ok_or(ClientError::Protocol)?;
                let b = self.bytes.get(self.at..end).ok_or(ClientError::Protocol)?;
                self.at = end;
                Ok(b)
            }
            fn text(&mut self, n: usize) -> Result<&'a str, ClientError> {
                std::str::from_utf8(self.take(n)?).map_err(|_| ClientError::Protocol)
            }
        }
        let mut input = Input { bytes, at: 0 };
        let header = b"theseus_native_client_recovery\0\x01";
        if input.take(header.len())? != header {
            return Err(ClientError::Protocol);
        }
        let binding = IntentRecord {
            intent: v2::IntentId::parse(input.text(32)?).map_err(|_| ClientError::Protocol)?,
            binding: v2::BindingId::parse(input.text(32)?).map_err(|_| ClientError::Protocol)?,
            manifest_digest: v2::Digest::parse(input.text(64)?)
                .map_err(|_| ClientError::Protocol)?,
            workspace_marker: v2::WorkspaceMarker::parse(input.text(32)?)
                .map_err(|_| ClientError::Protocol)?,
        };
        let profile = match input.take(1)?[0] {
            2 => v2::HelperProfile::Integration {
                provider_epoch: v2::Digest::parse(input.text(64)?)
                    .map_err(|_| ClientError::Protocol)?,
                core_manifest_digest: v2::Digest::parse(input.text(64)?)
                    .map_err(|_| ClientError::Protocol)?,
                integration_manifest_digest: v2::Digest::parse(input.text(64)?)
                    .map_err(|_| ClientError::Protocol)?,
            },
            0 => v2::HelperProfile::Fixture,
            1 => v2::HelperProfile::Native {
                provider_epoch: v2::Digest::parse(input.text(64)?)
                    .map_err(|_| ClientError::Protocol)?,
            },
            _ => return Err(ClientError::Protocol),
        };
        let lease = match input.take(1)?[0] {
            0 => None,
            1 => Some(v2::LeaseId::parse(input.text(32)?).map_err(|_| ClientError::Protocol)?),
            _ => return Err(ClientError::Protocol),
        };
        let count = u16::from_be_bytes(
            input
                .take(2)?
                .try_into()
                .map_err(|_| ClientError::Protocol)?,
        ) as usize;
        if count > v2::MAX_OPERATIONS {
            return Err(ClientError::Capacity);
        }
        let mut operations = Vec::new();
        for index in 0..count {
            let digest = v2::Digest::parse(input.text(64)?).map_err(|_| ClientError::Protocol)?;
            let length = u32::from_be_bytes(
                input
                    .take(4)?
                    .try_into()
                    .map_err(|_| ClientError::Protocol)?,
            ) as usize;
            if length > v2::MAX_FRAME_BYTES {
                return Err(ClientError::Capacity);
            }
            let frame = input.take(length)?;
            let mut decoder = v2::Decoder::new();
            let progress = decoder.feed(frame).map_err(|_| ClientError::Protocol)?;
            decoder.finish().map_err(|_| ClientError::Protocol)?;
            let envelope = progress.frame.ok_or(ClientError::Protocol)?;
            if progress.consumed != length
                || envelope.sequence() != index as u64
                || *envelope.session() != v2::SessionId::from_bytes([0x52; 16])
            {
                return Err(ClientError::Protocol);
            }
            let request = match envelope.message() {
                v2::Message::Request { request } => request.clone(),
                _ => return Err(ClientError::Protocol),
            };
            operations.push(RecoveredOperation { request, digest });
        }
        if input.at != bytes.len() {
            return Err(ClientError::Protocol);
        }
        let image = Self {
            binding,
            profile,
            operations,
            lease,
        };
        if image.encode()?.as_slice() != bytes {
            return Err(ClientError::Protocol);
        }
        Ok(image)
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod stream_tests;

#[cfg(test)]
mod terminal_capture_tests;

#[cfg(test)]
mod review_regressions;

#[cfg(test)]
mod file_tests;

#[cfg(test)]
mod durable_tests;
