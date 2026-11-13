//! Broker bind and binding receipt.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentPublicationScope {
    FileAndDirectorySynced,
    FileFlushedAtomicReplace,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerBind {
    pub ownership_id: String,
    pub helper_lease: String,
    pub helper_operation: String,
    pub slot: ResourceSlot,
    pub broker_intent: String,
    pub expected_header: IntegrationBrokerHeader,
}
impl BrokerBind {
    pub fn validate(&self) -> Result<(), Error> {
        BindingId::parse(&self.ownership_id)?;
        LeaseId::parse(&self.helper_lease)?;
        OperationId::parse(&self.helper_operation)?;
        ResourceSlot::new(self.slot.as_str())?;
        IntentId::parse(&self.broker_intent)?;
        self.expected_header.validate()?;
        if self.ownership_id != self.expected_header.ownership_id {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<Digest, Error> {
        self.validate()?;
        digest(b"theseus-native-broker-bind-v1\0", self)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerBindingReceipt {
    pub original: BrokerBind,
    pub request_digest: String,
    pub publication_scope: IntentPublicationScope,
    pub broker_lease: String,
}
impl BrokerBindingReceipt {
    pub fn validate(&self) -> Result<(), Error> {
        self.original.validate()?;
        LeaseId::parse(&self.broker_lease)?;
        if self.request_digest != self.original.digest()?.to_hex() {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
}
