use super::*;
use crate::framing::{CappedWriter, FramingError, RawDecoder};
use crate::v2::{self, *};
use serde::{Serialize, Serializer, ser::SerializeMap};
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};
struct RecordRef<'a>(&'a Record);
struct EntryRef<'a>(&'a Entry);
struct DispositionRef<'a>(&'a Disposition);
fn reason(v: UnknownReason) -> &'static str {
    match v {
        UnknownReason::HelperRestartedWithUnfinishedOperation => {
            "helper_restarted_with_unfinished_operation"
        }
        UnknownReason::LedgerWriteUncertain => "ledger_write_uncertain",
        UnknownReason::ProcessStateUnproved => "process_state_unproved",
        UnknownReason::FileEffectUncertain => "file_effect_uncertain",
    }
}
impl Serialize for RecordRef<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(3))?;
        m.serialize_entry("schema", &1u8)?;
        m.serialize_entry("index", &self.0.index())?;
        m.serialize_entry("record", &EntryRef(self.0.entry()))?;
        m.end()
    }
}
impl Serialize for DispositionRef<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        match self.0 {
            Disposition::AwaitingExternalReset => {
                m.serialize_entry("kind", "awaiting_external_reset")?
            }
            Disposition::Quarantined { reason: r } => {
                m.serialize_entry("kind", "quarantined")?;
                m.serialize_entry("reason", reason(*r))?;
            }
        }
        m.end()
    }
}
impl Serialize for EntryRef<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        match self.0 {
            Entry::IntegrationHeader {
                artifact_commitment,
                binding,
                manifest_digest,
                workspace_marker,
                os,
                provider_epoch,
                core_manifest_digest,
                integration_manifest_digest,
                context_digest,
                ownership_id,
            } => {
                m.serialize_entry("kind", "integration_header")?;
                m.serialize_entry("artifact_commitment", &artifact_commitment.to_hex())?;
                m.serialize_entry("profile", "native_integration_v1")?;
                m.serialize_entry("binding", &binding.to_hex())?;
                m.serialize_entry("manifest_digest", &manifest_digest.to_hex())?;
                m.serialize_entry("workspace_marker", &workspace_marker.to_hex())?;
                m.serialize_entry(
                    "os",
                    match os {
                        Os::Linux => "linux",
                        Os::Macos => "macos",
                        Os::Windows => "windows",
                    },
                )?;
                m.serialize_entry("provider_epoch", &provider_epoch.to_hex())?;
                m.serialize_entry("core_manifest_digest", &core_manifest_digest.to_hex())?;
                m.serialize_entry(
                    "integration_manifest_digest",
                    &integration_manifest_digest.to_hex(),
                )?;
                m.serialize_entry("context_digest", &context_digest.to_hex())?;
                m.serialize_entry("ownership_id", ownership_id)?;
                m.serialize_entry("capabilities", &IntegrationCapabilities::NAMES)?;
                m.serialize_entry("durability", "file_flushed_process_crash")?;
            }
            Entry::NativeAuthorityRevoked { lease, reason } => {
                m.serialize_entry("kind", "native_authority_revoked")?;
                m.serialize_entry("lease", &lease.to_hex())?;
                m.serialize_entry(
                    "reason",
                    match reason {
                        PrefixAuthorityFailure::Missing => "missing",
                        PrefixAuthorityFailure::Replaced => "replaced",
                        PrefixAuthorityFailure::Renamed => "renamed",
                        PrefixAuthorityFailure::MappingUnproved => "mapping_unproved",
                    },
                )?;
            }
            Entry::NativeHeader {
                binding,
                manifest_digest,
                workspace_marker,
                os,
                capabilities: _,
                provider_epoch,
            } => {
                m.serialize_entry("kind", "native_header")?;
                m.serialize_entry("profile", "native_catalog_v1")?;
                m.serialize_entry("capabilities", &NativeCapabilities::NAMES)?;
                m.serialize_entry("provider_epoch", &provider_epoch.to_hex())?;
                m.serialize_entry("binding", &binding.to_hex())?;
                m.serialize_entry("manifest_digest", &manifest_digest.to_hex())?;
                m.serialize_entry("workspace_marker", &workspace_marker.to_hex())?;
                m.serialize_entry(
                    "os",
                    match os {
                        Os::Linux => "linux",
                        Os::Macos => "macos",
                        Os::Windows => "windows",
                    },
                )?;
                m.serialize_entry("durability", "file_flushed_process_crash")?;
            }
            Entry::Header {
                binding,
                manifest_digest,
                workspace_marker,
                os,
            } => {
                m.serialize_entry("kind", "header")?;
                m.serialize_entry("profile", "trusted_fixture_v1")?;
                m.serialize_entry("binding", &binding.to_hex())?;
                m.serialize_entry("manifest_digest", &manifest_digest.to_hex())?;
                m.serialize_entry("workspace_marker", &workspace_marker.to_hex())?;
                m.serialize_entry(
                    "os",
                    match os {
                        Os::Linux => "linux",
                        Os::Macos => "macos",
                        Os::Windows => "windows",
                    },
                )?;
                m.serialize_entry("durability", "file_flushed_process_crash")?;
            }
            Entry::LeaseReserved { intent, lease } => {
                m.serialize_entry("kind", "lease_reserved")?;
                m.serialize_entry("intent", &intent.to_hex())?;
                m.serialize_entry("lease", &lease.to_hex())?;
            }
            Entry::OperationAccepted {
                lease,
                operation,
                request_digest,
                operation_kind,
            } => {
                m.serialize_entry("kind", "operation_accepted")?;
                m.serialize_entry("lease", &lease.to_hex())?;
                m.serialize_entry("operation", &operation.to_hex())?;
                m.serialize_entry("request_digest", &request_digest.to_hex())?;
                m.serialize_entry(
                    "operation_kind",
                    match operation_kind {
                        OperationKind::CreateFile => "create_file",
                        OperationKind::Run => "run",
                        OperationKind::ServiceAction => "service_action",
                    },
                )?;
            }
            Entry::OperationFinished {
                lease,
                operation,
                outcome,
            } => {
                m.serialize_entry("kind", "operation_finished")?;
                m.serialize_entry("lease", &lease.to_hex())?;
                m.serialize_entry("operation", &operation.to_hex())?;
                m.serialize_entry("outcome", &v2::json::OutcomeRef(outcome))?;
            }
            Entry::LeaseClosed { lease, disposition } => {
                m.serialize_entry("kind", "lease_closed")?;
                m.serialize_entry("lease", &lease.to_hex())?;
                m.serialize_entry("disposition", &DispositionRef(disposition))?;
            }
        }
        m.end()
    }
}
fn body(r: &Record) -> Result<Vec<u8>, LedgerError> {
    let mut w = CappedWriter::<MAX_RECORD_BODY_BYTES>::new();
    if serde_json::to_writer(&mut w, &RecordRef(r)).is_err() {
        return Err(if w.overflowed() {
            LedgerError::InvalidLength
        } else {
            LedgerError::InvalidRecord
        });
    }
    w.into_body().map_err(map_frame)
}
pub fn encode(r: &Record) -> Result<Vec<u8>, LedgerError> {
    let b = body(r)?;
    let hash: [u8; 32] = Sha256::digest(&b).into();
    crate::framing::encode_frame::<MAX_RECORD_BODY_BYTES, 32>(&b, &hash).map_err(map_frame)
}
/// Domain separated digest of a canonical mutating request. No authorization is implied.
pub fn request_digest(v: &Request) -> Result<Digest, Error> {
    if !matches!(
        v,
        Request::CreateFile { .. } | Request::Run { .. } | Request::ServiceAction { .. }
    ) {
        return Err(Error::InvalidValue);
    }
    let mut w = CappedWriter::<{ v2::MAX_BODY_BYTES }>::new();
    serde_json::to_writer(&mut w, &v2::json::RequestRef(v)).map_err(|_| Error::LimitExceeded)?;
    let b = w.into_body().map_err(|_| Error::LimitExceeded)?;
    let mut h = Sha256::new();
    h.update(b"theseus-native-request-v2\0");
    h.update(b);
    Ok(Digest::from_bytes(h.finalize().into()))
}
fn map_frame(e: FramingError) -> LedgerError {
    match e {
        FramingError::InvalidLength => LedgerError::InvalidLength,
        FramingError::Truncated => LedgerError::Truncated,
        FramingError::Poisoned => LedgerError::Poisoned,
    }
}
struct Object(Map<String, Value>);
impl Object {
    fn new(v: Value) -> Result<Self, LedgerError> {
        match v {
            Value::Object(v) => Ok(Self(v)),
            _ => Err(LedgerError::InvalidRecord),
        }
    }
    fn take(&mut self, k: &str) -> Result<Value, LedgerError> {
        self.0.remove(k).ok_or(LedgerError::InvalidRecord)
    }
    fn text(&mut self, k: &str) -> Result<String, LedgerError> {
        match self.take(k)? {
            Value::String(v) => Ok(v),
            _ => Err(LedgerError::InvalidRecord),
        }
    }
    fn literal(&mut self, k: &str, v: &str) -> Result<(), LedgerError> {
        if self.text(k)? == v {
            Ok(())
        } else {
            Err(LedgerError::InvalidRecord)
        }
    }
    fn end(self) -> Result<(), LedgerError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(LedgerError::InvalidRecord)
        }
    }
}
fn scalar<T>(v: Result<T, Error>) -> Result<T, LedgerError> {
    v.map_err(|_| LedgerError::InvalidRecord)
}
fn decode_reason(s: &str) -> Result<UnknownReason, LedgerError> {
    Ok(match s {
        "helper_restarted_with_unfinished_operation" => {
            UnknownReason::HelperRestartedWithUnfinishedOperation
        }
        "ledger_write_uncertain" => UnknownReason::LedgerWriteUncertain,
        "process_state_unproved" => UnknownReason::ProcessStateUnproved,
        "file_effect_uncertain" => UnknownReason::FileEffectUncertain,
        _ => return Err(LedgerError::InvalidRecord),
    })
}
fn decode(b: &[u8], hash: &[u8; 32]) -> Result<Record, LedgerError> {
    let text = std::str::from_utf8(b).map_err(|_| LedgerError::InvalidUtf8)?;
    let value = crate::strict::parse(text).map_err(|_| LedgerError::InvalidJson)?;
    let mut o = Object::new(value)?;
    let schema = o.take("schema")?;
    if schema.as_u64() != Some(1) {
        return Err(LedgerError::UnsupportedSchema);
    }
    let index = o
        .take("index")?
        .as_u64()
        .and_then(|v| u16::try_from(v).ok())
        .ok_or(LedgerError::InvalidRecord)?;
    let mut e = Object::new(o.take("record")?)?;
    o.end()?;
    let entry = match e.text("kind")?.as_str() {
        "native_authority_revoked" => Entry::NativeAuthorityRevoked {
            lease: scalar(LeaseId::parse(&e.text("lease")?))?,
            reason: match e.text("reason")?.as_str() {
                "missing" => PrefixAuthorityFailure::Missing,
                "replaced" => PrefixAuthorityFailure::Replaced,
                "renamed" => PrefixAuthorityFailure::Renamed,
                "mapping_unproved" => PrefixAuthorityFailure::MappingUnproved,
                _ => return Err(LedgerError::InvalidRecord),
            },
        },
        "integration_header" => {
            e.literal("profile", "native_integration_v1")?;
            if e.take("capabilities")? != serde_json::json!(IntegrationCapabilities::NAMES) {
                return Err(LedgerError::InvalidRecord);
            }
            let binding = scalar(BindingId::parse(&e.text("binding")?))?;
            let manifest_digest = scalar(Digest::parse(&e.text("manifest_digest")?))?;
            let workspace_marker = scalar(WorkspaceMarker::parse(&e.text("workspace_marker")?))?;
            let provider_epoch = scalar(Digest::parse(&e.text("provider_epoch")?))?;
            let core_manifest_digest = scalar(Digest::parse(&e.text("core_manifest_digest")?))?;
            let integration_manifest_digest =
                scalar(Digest::parse(&e.text("integration_manifest_digest")?))?;
            let context_digest = scalar(Digest::parse(&e.text("context_digest")?))?;
            let ownership_id = e.text("ownership_id")?;
            if v2::integration_manifest_digest(core_manifest_digest, integration_manifest_digest)
                != manifest_digest
                || ownership_id.is_empty()
                || ownership_id.len() > 128
            {
                return Err(LedgerError::InvalidRecord);
            }
            let os = match e.text("os")?.as_str() {
                "linux" => Os::Linux,
                "macos" => Os::Macos,
                "windows" => Os::Windows,
                _ => return Err(LedgerError::InvalidRecord),
            };
            e.literal("durability", "file_flushed_process_crash")?;
            Entry::IntegrationHeader {
                artifact_commitment: scalar(Digest::parse(&e.text("artifact_commitment")?))?,
                binding,
                manifest_digest,
                workspace_marker,
                os,
                provider_epoch,
                core_manifest_digest,
                integration_manifest_digest,
                context_digest,
                ownership_id,
            }
        }
        "native_header" => {
            e.literal("profile", "native_catalog_v1")?;
            if e.take("capabilities")? != serde_json::json!(NativeCapabilities::NAMES) {
                return Err(LedgerError::InvalidRecord);
            }
            let provider_epoch = scalar(Digest::parse(&e.text("provider_epoch")?))?;
            let binding = scalar(BindingId::parse(&e.text("binding")?))?;
            let manifest_digest = scalar(Digest::parse(&e.text("manifest_digest")?))?;
            let workspace_marker = scalar(WorkspaceMarker::parse(&e.text("workspace_marker")?))?;
            let os = match e.text("os")?.as_str() {
                "linux" => Os::Linux,
                "macos" => Os::Macos,
                "windows" => Os::Windows,
                _ => return Err(LedgerError::InvalidRecord),
            };
            e.literal("durability", "file_flushed_process_crash")?;
            Entry::NativeHeader {
                binding,
                manifest_digest,
                workspace_marker,
                os,
                capabilities: NativeCapabilities,
                provider_epoch,
            }
        }
        "header" => {
            e.literal("profile", "trusted_fixture_v1")?;
            let binding = scalar(BindingId::parse(&e.text("binding")?))?;
            let manifest_digest = scalar(Digest::parse(&e.text("manifest_digest")?))?;
            let workspace_marker = scalar(WorkspaceMarker::parse(&e.text("workspace_marker")?))?;
            let os = match e.text("os")?.as_str() {
                "linux" => Os::Linux,
                "macos" => Os::Macos,
                "windows" => Os::Windows,
                _ => return Err(LedgerError::InvalidRecord),
            };
            e.literal("durability", "file_flushed_process_crash")?;
            Entry::Header {
                binding,
                manifest_digest,
                workspace_marker,
                os,
            }
        }
        "lease_reserved" => Entry::LeaseReserved {
            intent: scalar(IntentId::parse(&e.text("intent")?))?,
            lease: scalar(LeaseId::parse(&e.text("lease")?))?,
        },
        "operation_accepted" => Entry::OperationAccepted {
            lease: scalar(LeaseId::parse(&e.text("lease")?))?,
            operation: scalar(OperationId::parse(&e.text("operation")?))?,
            request_digest: scalar(Digest::parse(&e.text("request_digest")?))?,
            operation_kind: match e.text("operation_kind")?.as_str() {
                "create_file" => OperationKind::CreateFile,
                "run" => OperationKind::Run,
                "service_action" => OperationKind::ServiceAction,
                _ => return Err(LedgerError::InvalidRecord),
            },
        },
        "operation_finished" => Entry::OperationFinished {
            lease: scalar(LeaseId::parse(&e.text("lease")?))?,
            operation: scalar(OperationId::parse(&e.text("operation")?))?,
            outcome: scalar(v2::json::decode_outcome(e.take("outcome")?))?,
        },
        "lease_closed" => {
            let lease = scalar(LeaseId::parse(&e.text("lease")?))?;
            let mut d = Object::new(e.take("disposition")?)?;
            let disposition = match d.text("kind")?.as_str() {
                "awaiting_external_reset" => Disposition::AwaitingExternalReset,
                "quarantined" => Disposition::Quarantined {
                    reason: decode_reason(&d.text("reason")?)?,
                },
                _ => return Err(LedgerError::InvalidRecord),
            };
            d.end()?;
            Entry::LeaseClosed { lease, disposition }
        }
        _ => return Err(LedgerError::InvalidRecord),
    };
    e.end()?;
    let r = Record::new(index, entry)?;
    if body(&r)? != b {
        return Err(LedgerError::NonCanonical);
    }
    let expected: [u8; 32] = Sha256::digest(b).into();
    if &expected != hash {
        return Err(LedgerError::HashMismatch);
    }
    Ok(r)
}
pub struct Decoder {
    raw: RawDecoder<MAX_RECORD_BODY_BYTES, 32>,
}
pub struct DecodeProgress {
    pub consumed: usize,
    pub record: Option<Record>,
}
impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}
impl Decoder {
    pub fn new() -> Self {
        Self {
            raw: RawDecoder::new(),
        }
    }
    pub fn feed(&mut self, input: &[u8]) -> Result<DecodeProgress, LedgerError> {
        let p = self.raw.feed(input).map_err(map_frame)?;
        let record = match p.frame {
            Some(f) => match decode(&f.body, &f.footer) {
                Ok(r) => Some(r),
                Err(e) => {
                    self.raw.poison();
                    return Err(e);
                }
            },
            None => None,
        };
        Ok(DecodeProgress {
            consumed: p.consumed,
            record,
        })
    }
    pub fn finish(&mut self) -> Result<(), LedgerError> {
        self.raw.finish().map_err(map_frame)
    }
}
