//! Exact admitted SUT Run references; serialized witnesses never create Run authority.
use super::{BrokerOperationIdentity, DesktopAction, DesktopRequest};
use crate::v2::{
    self, DesktopArtifactRole, DesktopDefinitionKey, DesktopDefinitionRole, Digest, Error, LeaseId,
    OperationId, ProgramId,
};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopSutRunWitness {
    pub lease: String,
    pub operation: String,
    pub request_digest: String,
    #[serde(with = "program_wire")]
    pub program: ProgramId,
}
impl DesktopSutRunWitness {
    pub fn validate(&self) -> Result<(), Error> {
        let lease = LeaseId::parse(&self.lease)?;
        let operation = OperationId::parse(&self.operation)?;
        Digest::parse(&self.request_digest)?;
        if !["install_base", "install_target", "configure", "uninstall"]
            .contains(&self.program.as_str())
            || v2::request_digest(&v2::Request::Run {
                lease,
                operation,
                program: self.program.clone(),
            })?
            .to_hex()
                != self.request_digest
        {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
    pub fn definition_after(
        &self,
        previous: Option<DesktopDefinitionKey>,
    ) -> Result<Option<DesktopDefinitionKey>, Error> {
        self.validate()?;
        use DesktopArtifactRole as A;
        use DesktopDefinitionRole as R;
        Ok(match self.program.as_str() {
            "install_base" => Some(DesktopDefinitionKey {
                role: R::Base,
                artifact: A::Base,
            }),
            "install_target" => Some(DesktopDefinitionKey {
                role: if previous.is_some_and(|p| p.role == R::Configured) {
                    R::Configured
                } else {
                    R::Target
                },
                artifact: A::Target,
            }),
            "configure" => Some(DesktopDefinitionKey {
                role: R::Configured,
                artifact: previous.map_or(A::Target, |p| p.artifact),
            }),
            "uninstall" => None,
            _ => return Err(Error::InvalidValue),
        })
    }
    pub fn matches_successful_terminal(&self, records: &[v2::ledger::Record]) -> Result<(), Error> {
        self.validate()?;
        let lease = LeaseId::parse(&self.lease)?;
        let operation = OperationId::parse(&self.operation)?;
        let digest = Digest::parse(&self.request_digest)?;
        let accepted=records.iter().position(|r|matches!(r.entry(),v2::ledger::Entry::OperationAccepted {lease:l,operation:o,request_digest:d,operation_kind:v2::OperationKind::Run} if *l==lease && *o==operation && *d==digest)).ok_or(Error::InvalidValue)?;
        if !records.iter().skip(accepted+1).any(|r|matches!(r.entry(),v2::ledger::Entry::OperationFinished {lease:l,operation:o,outcome} if *l==lease && *o==operation && successful_sut_outcome(outcome))) {return Err(Error::InvalidValue);}
        Ok(())
    }
}
pub fn successful_sut_outcome(outcome: &v2::Outcome) -> bool {
    matches!(
        outcome,
        v2::Outcome::RunCompleted {
            reason: v2::RunReason::Natural,
            status: v2::NativeStatus::PosixExit { code: 0 }
                | v2::NativeStatus::WindowsExit { code: 0 },
            ..
        }
    )
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopActivationTransition {
    pub definition: DesktopDefinitionKey,
    pub tuple_digest: String,
    pub sut: DesktopSutRunWitness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_activation: Option<BrokerOperationIdentity>,
}
impl DesktopActivationTransition {
    pub fn validate(&self) -> Result<(), Error> {
        self.sut.validate()?;
        Digest::parse(&self.tuple_digest)?;
        if let Some(previous) = &self.previous_activation {
            previous.validate()?;
            if previous.helper_lease != self.sut.lease
                || previous.helper_operation == self.sut.operation
            {
                return Err(Error::InvalidValue);
            }
        }
        Ok(())
    }
    pub fn validate_request(&self, request: &DesktopRequest) -> Result<(), Error> {
        self.validate()?;
        if self.sut.lease != request.helper_lease || self.sut.operation == request.helper_operation
        {
            return Err(Error::InvalidValue);
        }
        if let Some(previous) = &self.previous_activation
            && (previous.helper_lease != request.helper_lease
                || previous.broker_lease != request.broker_lease
                || previous.binding_digest != request.binding_digest
                || previous.helper_operation == request.helper_operation)
        {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
}
impl DesktopRequest {
    pub fn transition(&self) -> Option<&DesktopActivationTransition> {
        match &self.action {
            DesktopAction::Activate { transition, .. } => transition.as_ref(),
            _ => None,
        }
    }
}

pub(super) mod program_wire {
    use super::ProgramId;
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(program: &ProgramId, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(program.as_str())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<ProgramId, D::Error> {
        let text = String::deserialize(deserializer)?;
        ProgramId::new(&text).map_err(serde::de::Error::custom)
    }
}

impl super::BrokerLedgerView {
    pub fn latest_helper_activation(
        &self,
    ) -> Option<(&DesktopRequest, Option<&super::BrokerState>)> {
        self.helper_actions
            .iter()
            .rev()
            .find(|(request, _)| matches!(request.action, DesktopAction::Activate { .. }))
            .map(|(request, state)| (request, state.as_ref()))
    }
    pub fn latest_sut(&self) -> Option<(&DesktopSutRunWitness, Option<DesktopDefinitionKey>)> {
        self.sut_runs.last().map(|(w, k)| (w, *k))
    }
    pub fn helper_transition(
        &self,
        identity: &BrokerOperationIdentity,
    ) -> Option<&DesktopActivationTransition> {
        self.transitions
            .iter()
            .find(|(old, _)| old == identity)
            .map(|(_, t)| t)
    }
    pub fn validate_helper_generation(
        &self,
        manifest: &v2::IntegrationManifest,
        request: &DesktopRequest,
    ) -> Result<(), Error> {
        let DesktopAction::Activate {
            mode,
            entry_digest,
            expectation_digest,
            owned_lifetime,
            transition,
            ..
        } = &request.action
        else {
            return Err(Error::InvalidValue);
        };
        let previous = self.helper_actions.iter().rev().find(|(r, _)| {
            r.slot == request.slot && matches!(r.action, DesktopAction::Activate { .. })
        });
        let key = transition
            .as_ref()
            .map_or(DesktopDefinitionKey::CONFIGURED_TARGET, |t| t.definition);
        let definition = manifest.desktop_definition(&key)?;
        if !definition.desktop.allowed_modes.contains(mode)
            || definition.desktop.entry_digest != *entry_digest
            || definition.desktop.expectation_digest != *expectation_digest
            || definition.owned_lifetime != *owned_lifetime
        {
            return Err(Error::InvalidValue);
        }
        match transition {
            None if manifest.desktop_definitions.is_empty() && previous.is_none() => Ok(()),
            Some(transition) => {
                transition.validate_request(request)?;
                if transition.tuple_digest != definition.digest()?.to_hex()
                    || self.latest_sut() != Some((&transition.sut, Some(transition.definition)))
                    || self
                        .transitions
                        .iter()
                        .any(|(_, old)| old.sut.operation == transition.sut.operation)
                {
                    return Err(Error::InvalidValue);
                }
                match (previous, &transition.previous_activation) {
                    (None, None) => Ok(()),
                    (Some((old, Some(state))), Some(identity))
                        if old.identity().as_ref() == Ok(identity)
                            && state.original_retired()
                            && self
                                .sut_accepted_at
                                .iter()
                                .find(|(op, _)| op == &transition.sut.operation)
                                .is_some_and(|(_, accepted)| {
                                    self.helper_retired_at
                                        .iter()
                                        .find(|(op, _)| op == &identity.helper_operation)
                                        .is_some_and(|(_, retired)| accepted > retired)
                                }) =>
                    {
                        Ok(())
                    }
                    _ => Err(Error::InvalidValue),
                }
            }
            _ => Err(Error::InvalidValue),
        }
    }
    pub fn validate_broker_generation(&self, request: &DesktopRequest) -> Result<(), Error> {
        let previous = self.accepted.iter().rev().find(|a| {
            a.request.slot == request.slot
                && matches!(a.request.action, DesktopAction::Activate { .. })
        });
        if let Some(previous) = previous
            && let (
                DesktopAction::Activate {
                    owned_lifetime: old,
                    ..
                },
                DesktopAction::Activate {
                    owned_lifetime: next,
                    ..
                },
            ) = (&previous.request.action, &request.action)
            && old != next
        {
            return Err(Error::InvalidValue);
        }
        match request.transition() {
            None if previous.is_none() => Ok(()),
            Some(transition) => {
                transition.validate_request(request)?;
                if self.accepted.iter().any(|a| {
                    a.request
                        .transition()
                        .is_some_and(|old| old.sut.operation == transition.sut.operation)
                }) {
                    return Err(Error::InvalidValue);
                }
                match (previous, &transition.previous_activation) {
                    (None, None) => Ok(()),
                    (Some(old), Some(identity))
                        if old.request.identity().as_ref() == Ok(identity)
                            && old.state.original_retired() =>
                    {
                        Ok(())
                    }
                    _ => Err(Error::InvalidValue),
                }
            }
            _ => Err(Error::InvalidValue),
        }
    }
}
