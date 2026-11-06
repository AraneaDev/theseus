//! Original accepted SUT Run identity for private entry-proof settlement.
use super::*;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryPreparation {
    pub helper_lease: String,
    pub broker_lease: String,
    pub operation: String,
    pub request_digest: String,
    #[serde(with = "super::generation::program_wire")]
    pub program: super::super::ProgramId,
    pub slot: ResourceSlot,
    pub header_digest: String,
    pub binding_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<DesktopActivationTransition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_sut: Option<DesktopSutRunWitness>,
}
impl EntryPreparation {
    pub fn mutating(program: &super::super::ProgramId) -> bool {
        matches!(
            program.as_str(),
            "install_base" | "install_target" | "configure" | "upgrade" | "reinstall" | "uninstall"
        )
    }
    pub fn run(&self) -> Result<super::super::Request, Error> {
        Ok(super::super::Request::Run {
            lease: LeaseId::parse(&self.helper_lease)?,
            operation: OperationId::parse(&self.operation)?,
            program: self.program.clone(),
        })
    }
    pub fn validate(&self) -> Result<(), Error> {
        LeaseId::parse(&self.broker_lease)?;
        Digest::parse(&self.header_digest)?;
        Digest::parse(&self.binding_digest)?;
        if !Self::mutating(&self.program)
            || super::super::request_digest(&self.run()?)?.to_hex() != self.request_digest
        {
            return Err(Error::InvalidValue);
        }
        if let Some(prior) = &self.prior_sut {
            prior.validate()?;
            if prior.lease != self.helper_lease || prior.operation == self.operation {
                return Err(Error::InvalidValue);
            }
        }
        if let Some(generation) = &self.generation {
            generation.validate()?;
            if Some(&generation.sut) != self.prior_sut.as_ref() {
                return Err(Error::InvalidValue);
            }
        }
        Ok(())
    }
}
