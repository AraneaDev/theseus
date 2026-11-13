//! Broker state and effects.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopEvidence {
    OwnedExecutable,
    NativeActivation,
    SourceOwnedExecutable,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerEffectReceipt {
    pub evidence: DesktopEvidence,
    pub acknowledged: bool,
    pub receipt_digest: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrokerEffect {
    Unknown,
    Receipt { receipt: BrokerEffectReceipt },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerOwner {
    Retained,
    Settled,
    Lost,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerPrimary {
    Running,
    Exited,
    NotCreated,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerDescendants {
    Pending,
    Joined,
    NotCreated,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned_lifetime: Option<OwnedLifetimeState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_lifetime: Option<NativeLifetimeState>,
    pub effect: BrokerEffect,
    pub owner: BrokerOwner,
    pub primary: BrokerPrimary,
    pub descendants: BrokerDescendants,
    pub cleanup_complete: bool,
    pub quarantined: bool,
}
impl BrokerState {
    pub fn validate(&self) -> Result<(), Error> {
        self.validate_owned_lifetime()?;
        if let Some(native) = &self.native_lifetime {
            native.validate()?;
            if self.cleanup_complete
                || (self.primary == BrokerPrimary::Exited
                    && native.primary != NativePrimaryState::Exited)
                || self.descendants != BrokerDescendants::Unknown
                || (matches!(&self.effect,BrokerEffect::Receipt {receipt} if receipt.evidence!=DesktopEvidence::NativeActivation))
            {
                return Err(Error::InvalidValue);
            }
        }
        if let BrokerEffect::Receipt { receipt } = &self.effect {
            Digest::parse(&receipt.receipt_digest)?;
            if receipt.evidence == DesktopEvidence::NativeActivation
                && self.native_lifetime.is_none()
                && (self.cleanup_complete || self.owner == BrokerOwner::Settled)
            {
                return Err(Error::InvalidValue);
            }
        }
        if self.cleanup_complete
            && (self.owner != BrokerOwner::Settled
                || !matches!(
                    self.primary,
                    BrokerPrimary::Exited | BrokerPrimary::NotCreated
                )
                || !matches!(
                    self.descendants,
                    BrokerDescendants::Joined | BrokerDescendants::NotCreated
                ))
        {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
    pub fn unknown() -> Self {
        Self {
            owned_lifetime: None,
            native_lifetime: None,
            effect: BrokerEffect::Unknown,
            owner: BrokerOwner::Lost,
            primary: BrokerPrimary::Unknown,
            descendants: BrokerDescendants::Unknown,
            cleanup_complete: false,
            quarantined: true,
        }
    }
}
