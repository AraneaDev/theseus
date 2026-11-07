//! Original direct-owned lifetime knowledge; serialized facts never create owners.
use super::{
    BrokerDescendants, BrokerEffect, BrokerOwner, BrokerPrimary, BrokerState, DesktopEvidence,
    DesktopMode,
};
use crate::v2::Error;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OwnedDesktopLifetimeDefinition {
    MacosOriginalPrimaryV1,
}
impl OwnedDesktopLifetimeDefinition {
    pub fn validate_platform(self, os: &str) -> Result<(), Error> {
        if os == "macos" {
            Ok(())
        } else {
            Err(Error::InvalidValue)
        }
    }
    pub fn validate_mode(self, modes: &[DesktopMode]) -> Result<(), Error> {
        if modes == [DesktopMode::OwnedExecutable] {
            Ok(())
        } else {
            Err(Error::InvalidValue)
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnedPrimaryState {
    Pending,
    Running,
    Exited,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnedTreeState {
    Unavailable,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnedLocalState {
    Retained,
    Retired,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedLifetimeState {
    pub definition: OwnedDesktopLifetimeDefinition,
    pub primary: OwnedPrimaryState,
    pub descendants: OwnedTreeState,
    pub local_resources: OwnedLocalState,
}
impl OwnedLifetimeState {
    pub fn initial(definition: OwnedDesktopLifetimeDefinition) -> Self {
        Self {
            definition,
            primary: OwnedPrimaryState::Pending,
            descendants: OwnedTreeState::Unavailable,
            local_resources: OwnedLocalState::Retained,
        }
    }
    pub fn validate(&self) -> Result<(), Error> {
        if self.local_resources == OwnedLocalState::Retired
            && matches!(
                self.primary,
                OwnedPrimaryState::Pending | OwnedPrimaryState::Running
            )
        {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
    pub fn validate_platform(&self, os: &str) -> Result<(), Error> {
        self.validate()?;
        self.definition.validate_platform(os)
    }
    pub fn validate_progress_from(&self, old: &Self) -> Result<(), Error> {
        self.validate()?;
        old.validate()?;
        if self.definition != old.definition
            || (old.primary == OwnedPrimaryState::Running
                && self.primary == OwnedPrimaryState::Pending)
            || (matches!(
                old.primary,
                OwnedPrimaryState::Exited | OwnedPrimaryState::Unknown
            ) && self.primary != old.primary)
            || (old.descendants == OwnedTreeState::Unknown && self.descendants != old.descendants)
            || (matches!(
                old.local_resources,
                OwnedLocalState::Retired | OwnedLocalState::Unknown
            ) && self.local_resources != old.local_resources)
        {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
    pub fn required_lifetime_satisfied(&self) -> bool {
        self.validate().is_ok() && self.primary == OwnedPrimaryState::Exited
    }
    pub fn local_retired(&self) -> bool {
        self.validate().is_ok() && self.local_resources == OwnedLocalState::Retired
    }
    fn mirrored_primary(&self) -> BrokerPrimary {
        match self.primary {
            OwnedPrimaryState::Pending | OwnedPrimaryState::Unknown => BrokerPrimary::Unknown,
            OwnedPrimaryState::Running => BrokerPrimary::Running,
            OwnedPrimaryState::Exited => BrokerPrimary::Exited,
        }
    }
}
impl BrokerState {
    /// Used only when projecting the explicit declaration from original Accepted.
    pub(super) fn accepted_owned(definition: OwnedDesktopLifetimeDefinition) -> Self {
        let state = OwnedLifetimeState::initial(definition);
        Self {
            native_lifetime: None,
            owned_lifetime: Some(state),
            effect: BrokerEffect::Unknown,
            owner: BrokerOwner::Retained,
            primary: BrokerPrimary::Unknown,
            descendants: BrokerDescendants::Unknown,
            cleanup_complete: false,
            quarantined: false,
        }
    }
    /// Progress an already accepted original axis and atomically mirror legacy facts.
    /// This cannot attach a declaration to a legacy/lost original operation.
    pub fn record_owned_lifetime(&mut self, state: OwnedLifetimeState) -> Result<(), Error> {
        self.validate()?;
        state.validate_progress_from(self.owned_lifetime.as_ref().ok_or(Error::InvalidValue)?)?;
        if self.owner == BrokerOwner::Lost {
            return Err(Error::InvalidValue);
        }
        self.primary = state.mirrored_primary();
        self.descendants = BrokerDescendants::Unknown;
        self.cleanup_complete = false;
        self.owner = if state.local_retired() {
            BrokerOwner::Settled
        } else {
            BrokerOwner::Retained
        };
        self.owned_lifetime = Some(state);
        Ok(())
    }
    pub(super) fn validate_owned_lifetime(&self) -> Result<(), Error> {
        if let Some(state) = &self.owned_lifetime {
            state.validate()?;
            if self.native_lifetime.is_some()
                || self.cleanup_complete
                || self.primary != state.mirrored_primary()
                || self.descendants != BrokerDescendants::Unknown
                || (self.owner != BrokerOwner::Lost
                    && (self.owner == BrokerOwner::Settled) != state.local_retired())
                || matches!(&self.effect, BrokerEffect::Receipt {receipt} if receipt.evidence == DesktopEvidence::NativeActivation)
            {
                return Err(Error::InvalidValue);
            }
        }
        Ok(())
    }
}

impl super::DesktopRequest {
    /// The declaration on this original Activate; Close inherits via the ledger.
    pub fn owned_lifetime_definition(&self) -> Option<OwnedDesktopLifetimeDefinition> {
        match &self.action {
            super::DesktopAction::Activate { owned_lifetime, .. } => *owned_lifetime,
            super::DesktopAction::RequestClose { .. } => None,
        }
    }
}
