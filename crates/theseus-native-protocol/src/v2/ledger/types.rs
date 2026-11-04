use crate::v2::*;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LedgerError {
    InvalidLength,
    InvalidUtf8,
    InvalidJson,
    InvalidRecord,
    NonCanonical,
    HashMismatch,
    UnsupportedSchema,
    Truncated,
    Poisoned,
    InvalidTransition,
    Capacity,
}
impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidLength => "ledger invalid length",
            Self::InvalidUtf8 => "ledger invalid UTF-8",
            Self::InvalidJson => "ledger invalid JSON",
            Self::InvalidRecord => "ledger invalid record",
            Self::NonCanonical => "ledger noncanonical record",
            Self::HashMismatch => "ledger hash mismatch",
            Self::UnsupportedSchema => "ledger unsupported schema",
            Self::Truncated => "ledger truncated record",
            Self::Poisoned => "ledger decoder poisoned",
            Self::InvalidTransition => "ledger invalid transition",
            Self::Capacity => "ledger capacity exceeded",
        })
    }
}
impl std::error::Error for LedgerError {}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Disposition {
    AwaitingExternalReset,
    Quarantined { reason: UnknownReason },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    IntegrationHeader {
        artifact_commitment: Digest,
        binding: BindingId,
        manifest_digest: Digest,
        workspace_marker: WorkspaceMarker,
        os: Os,
        provider_epoch: Digest,
        core_manifest_digest: Digest,
        integration_manifest_digest: Digest,
        context_digest: Digest,
        ownership_id: String,
    },
    NativeAuthorityRevoked {
        lease: LeaseId,
        reason: PrefixAuthorityFailure,
    },
    NativeHeader {
        binding: BindingId,
        manifest_digest: Digest,
        workspace_marker: WorkspaceMarker,
        os: Os,
        capabilities: NativeCapabilities,
        provider_epoch: Digest,
    },
    Header {
        binding: BindingId,
        manifest_digest: Digest,
        workspace_marker: WorkspaceMarker,
        os: Os,
    },
    LeaseReserved {
        intent: IntentId,
        lease: LeaseId,
    },
    OperationAccepted {
        lease: LeaseId,
        operation: OperationId,
        request_digest: Digest,
        operation_kind: OperationKind,
    },
    OperationFinished {
        lease: LeaseId,
        operation: OperationId,
        outcome: Outcome,
    },
    LeaseClosed {
        lease: LeaseId,
        disposition: Disposition,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    index: u16,
    entry: Entry,
}
impl Record {
    pub fn new(index: u16, entry: Entry) -> Result<Self, LedgerError> {
        if usize::from(index) >= super::MAX_RECORDS
            || (index != 0
                && matches!(
                    entry,
                    Entry::Header { .. }
                        | Entry::NativeHeader { .. }
                        | Entry::IntegrationHeader { .. }
                ))
        {
            return Err(LedgerError::InvalidRecord);
        }
        if let Entry::OperationFinished { outcome, .. } = &entry {
            super::super::types::validate_outcome(outcome)
                .map_err(|_| LedgerError::InvalidRecord)?;
        }
        Ok(Self { index, entry })
    }
    pub fn index(&self) -> u16 {
        self.index
    }
    pub fn entry(&self) -> &Entry {
        &self.entry
    }
}
