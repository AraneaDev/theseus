//! Serializable original-operation knowledge. None of these facts creates an owner.
use super::{BrokerEffect, BrokerOwner, BrokerState, DesktopEvidence};
use crate::v2::Error;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeLifetimeRequirement {
    ActivationInstanceTerminated,
    OwnedPrimaryExited,
    OwnedProcessTreeSettled,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeLifetimeAuthority {
    OriginalChild,
    OriginalPrimaryHandle,
    ManagerInstance,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeInstanceState {
    Pending,
    Live,
    Terminated,
    NotCreated,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativePrimaryState {
    Pending,
    Running,
    Exited,
    NotCreated,
    Unavailable,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeTreeState {
    Unavailable,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeLocalState {
    Retained,
    Retired,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeLifetimeState {
    pub authority: NativeLifetimeAuthority,
    pub required: NativeLifetimeRequirement,
    pub instance: NativeInstanceState,
    pub primary: NativePrimaryState,
    pub descendants: NativeTreeState,
    pub local_resources: NativeLocalState,
}
impl NativeLifetimeState {
    pub fn validate(&self) -> Result<(), Error> {
        let supported = matches!(
            (self.authority, self.required),
            (
                NativeLifetimeAuthority::ManagerInstance,
                NativeLifetimeRequirement::ActivationInstanceTerminated
            ) | (
                NativeLifetimeAuthority::OriginalChild
                    | NativeLifetimeAuthority::OriginalPrimaryHandle,
                NativeLifetimeRequirement::OwnedPrimaryExited
            )
        );
        if !supported
            || (self.authority == NativeLifetimeAuthority::ManagerInstance
                && !matches!(
                    self.primary,
                    NativePrimaryState::Unavailable | NativePrimaryState::Unknown
                ))
            || (self.authority == NativeLifetimeAuthority::OriginalChild
                && self.primary == NativePrimaryState::Unavailable)
            || (self.primary == NativePrimaryState::NotCreated
                && self.instance != NativeInstanceState::NotCreated)
            || (self.instance == NativeInstanceState::NotCreated
                && !matches!(
                    self.primary,
                    NativePrimaryState::NotCreated
                        | NativePrimaryState::Unavailable
                        | NativePrimaryState::Unknown
                ))
        {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
    pub fn validate_platform(&self, os: &str) -> Result<(), Error> {
        self.validate()?;
        if matches!(
            (os, self.authority),
            ("linux", NativeLifetimeAuthority::OriginalChild)
                | ("macos", NativeLifetimeAuthority::ManagerInstance)
                | ("windows", NativeLifetimeAuthority::OriginalPrimaryHandle)
        ) {
            Ok(())
        } else {
            Err(Error::InvalidValue)
        }
    }
    /// Unavailable is an admission ceiling. It never repairs lost/unknown authority.
    pub fn validate_progress_from(&self, old: &Self) -> Result<(), Error> {
        self.validate()?;
        old.validate()?;
        if self.authority != old.authority
            || self.required != old.required
            || (old.instance == NativeInstanceState::Live
                && matches!(
                    self.instance,
                    NativeInstanceState::Pending | NativeInstanceState::NotCreated
                ))
            || (old.primary == NativePrimaryState::Running
                && matches!(
                    self.primary,
                    NativePrimaryState::Pending
                        | NativePrimaryState::NotCreated
                        | NativePrimaryState::Unavailable
                ))
            || (matches!(
                old.instance,
                NativeInstanceState::Terminated
                    | NativeInstanceState::NotCreated
                    | NativeInstanceState::Unknown
            ) && self.instance != old.instance)
            || (matches!(
                old.primary,
                NativePrimaryState::Exited
                    | NativePrimaryState::NotCreated
                    | NativePrimaryState::Unavailable
                    | NativePrimaryState::Unknown
            ) && self.primary != old.primary)
            || self.descendants != old.descendants
            || (matches!(
                old.local_resources,
                NativeLocalState::Retired | NativeLocalState::Unknown
            ) && self.local_resources != old.local_resources)
        {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
    pub fn required_lifetime_satisfied(&self) -> bool {
        self.validate().is_ok()
            && match self.required {
                NativeLifetimeRequirement::ActivationInstanceTerminated => {
                    self.instance == NativeInstanceState::Terminated
                }
                NativeLifetimeRequirement::OwnedPrimaryExited => {
                    self.primary == NativePrimaryState::Exited
                }
                NativeLifetimeRequirement::OwnedProcessTreeSettled => false,
            }
    }
    pub fn local_retired(&self) -> bool {
        self.validate().is_ok() && self.local_resources == NativeLocalState::Retired
    }
}
impl BrokerState {
    /// Original local retirement and the admitted mandatory lifetime, never a tree claim.
    pub fn original_retired(&self) -> bool {
        if self.validate().is_err() || self.owner != BrokerOwner::Settled || self.quarantined {
            return false;
        }
        match &self.native_lifetime {
            None => {
                self.cleanup_complete
                    && matches!(&self.effect,BrokerEffect::Receipt {receipt} if receipt.evidence!=DesktopEvidence::NativeActivation)
            }
            Some(native) => {
                matches!(&self.effect,BrokerEffect::Receipt {receipt} if receipt.evidence == DesktopEvidence::NativeActivation && receipt.acknowledged)
                    && native.required_lifetime_satisfied()
                    && native.local_retired()
                    && native.descendants != NativeTreeState::Unknown
                    && native.primary != NativePrimaryState::Unknown
                    && !matches!(
                        native.instance,
                        NativeInstanceState::Unknown | NativeInstanceState::Pending
                    )
            }
        }
    }
}
