use crate::{
    StoreError,
    bootstrap::{LEDGER, MARKER},
};
use std::{ffi::OsStr, io::Read};
use theseus_host::fs::{HostDir, LockedLedgerFile, RelativePath};
use theseus_native_protocol::v2::{
    BindingId, Digest, LeaseId, LeaseView, Os, UnknownReason, WorkspaceMarker,
    ledger::{self, Disposition, Entry, LedgerError, LedgerView, Record},
};

pub struct StoreBinding {
    pub binding: BindingId,
    pub manifest_digest: Digest,
    pub workspace_marker: WorkspaceMarker,
    pub os: Os,
}
/// Owns the locked file object. The caller exclusively owns its private namespace.
pub struct LedgerStore {
    file: LockedLedgerFile,
    records: Vec<Record>,
    restart: LedgerView,
    bytes: u64,
    poisoned: bool,
    recovered: Option<(LeaseId, UnknownReason)>,
}
pub struct CommittedRecord {
    record: Record,
    ledger_bytes: u64,
}
impl CommittedRecord {
    pub fn record(&self) -> &Record {
        &self.record
    }
    pub fn ledger_bytes(&self) -> u64 {
        self.ledger_bytes
    }
}
impl LedgerStore {
    pub fn open(
        ledger_root: &HostDir,
        workspace_root: &HostDir,
        expected: StoreBinding,
    ) -> Result<Self, StoreError> {
        Self::open_profile(
            ledger_root,
            workspace_root,
            expected,
            theseus_native_protocol::v2::HelperProfile::Fixture,
        )
    }
    pub fn open_native(
        ledger_root: &HostDir,
        workspace_root: &HostDir,
        expected: StoreBinding,
        provider_epoch: Digest,
    ) -> Result<Self, StoreError> {
        Self::open_profile(
            ledger_root,
            workspace_root,
            expected,
            theseus_native_protocol::v2::HelperProfile::Native { provider_epoch },
        )
    }
    fn open_profile(
        ledger_root: &HostDir,
        workspace_root: &HostDir,
        expected: StoreBinding,
        profile: theseus_native_protocol::v2::HelperProfile,
    ) -> Result<Self, StoreError> {
        if ledger_root.same_object(workspace_root)? {
            return Err(StoreError::AliasedRoots);
        }
        let mut file = ledger_root.open_locked_ledger(OsStr::new(LEDGER))?;
        let bytes = file.read_bounded(ledger::MAX_LEDGER_BYTES)?;
        let mut decoder = ledger::Decoder::new();
        let mut offset = 0;
        let mut records = Vec::new();
        while offset < bytes.len() {
            let progress = decoder.feed(&bytes[offset..])?;
            if progress.consumed == 0 {
                return Err(LedgerError::InvalidLength.into());
            }
            offset += progress.consumed;
            if let Some(record) = progress.record {
                if records.len() >= ledger::MAX_RECORDS {
                    return Err(LedgerError::Capacity.into());
                }
                records.push(record);
            }
        }
        decoder.finish()?;
        let restart = LedgerView::replay(&records)?;
        match records[0].entry() {
            Entry::Header {
                binding,
                manifest_digest,
                workspace_marker,
                os,
            } if profile == theseus_native_protocol::v2::HelperProfile::Fixture
                && *binding == expected.binding
                && *manifest_digest == expected.manifest_digest
                && *workspace_marker == expected.workspace_marker
                && *os == expected.os => {}
            Entry::NativeHeader {
                binding,
                manifest_digest,
                workspace_marker,
                os,
                provider_epoch,
                ..
            } if profile
                == (theseus_native_protocol::v2::HelperProfile::Native {
                    provider_epoch: *provider_epoch,
                })
                && *binding == expected.binding
                && *manifest_digest == expected.manifest_digest
                && *workspace_marker == expected.workspace_marker
                && *os == expected.os => {}
            _ => return Err(StoreError::BindingMismatch),
        }
        let mut marker = Vec::new();
        workspace_root
            .open_file(&RelativePath::parse(MARKER)?)?
            .take(33)
            .read_to_end(&mut marker)?;
        if marker != expected.workspace_marker.to_hex().as_bytes() {
            return Err(StoreError::MarkerMismatch);
        }
        let recovered = if let Some(Entry::OperationAccepted { lease, .. }) =
            records.last().map(Record::entry)
        {
            match restart.query_lease(*lease) {
                Ok(LeaseView::Quarantined { reason, .. }) => Some((*lease, reason)),
                _ => return Err(LedgerError::InvalidTransition.into()),
            }
        } else {
            None
        };
        Ok(Self {
            file,
            records,
            restart,
            bytes: u64::try_from(bytes.len()).map_err(|_| LedgerError::Capacity)?,
            poisoned: false,
            recovered,
        })
    }
    pub fn records(&self) -> &[Record] {
        &self.records
    }
    /// Restart projection only: unfinished accepted operations are Unknown.
    pub fn restart_view(&self) -> &LedgerView {
        &self.restart
    }
    pub fn append(&mut self, record: Record) -> Result<CommittedRecord, StoreError> {
        self.append_with(record, |file, bytes| {
            file.append_and_sync(bytes, ledger::MAX_LEDGER_BYTES as u64)
        })
    }
    fn append_with(
        &mut self,
        record: Record,
        write: impl FnOnce(&mut LockedLedgerFile, &[u8]) -> std::io::Result<()>,
    ) -> Result<CommittedRecord, StoreError> {
        if self.poisoned {
            return Err(StoreError::WritePoisoned);
        }
        if let Some((lease, reason)) = self.recovered
            && !matches!(record.entry(), Entry::LeaseClosed { lease: actual, disposition: Disposition::Quarantined { reason: actual_reason } } if *actual == lease && *actual_reason == reason)
        {
            return Err(StoreError::RecoveredUnfinished);
        }
        let encoded = ledger::encode(&record)?;
        let mut candidate = self.records.clone();
        candidate.push(record.clone());
        let restart = LedgerView::replay(&candidate)?;
        let bytes = self
            .bytes
            .checked_add(u64::try_from(encoded.len()).map_err(|_| LedgerError::Capacity)?)
            .filter(|n| *n <= ledger::MAX_LEDGER_BYTES as u64)
            .ok_or(LedgerError::Capacity)?;
        // An I/O error may follow a partial write or completed write with failed sync.
        // Permanently retain this locked owner and refuse subsequent mutation.
        self.poisoned = true;
        write(&mut self.file, &encoded)?;
        self.records = candidate;
        self.restart = restart;
        self.bytes = bytes;
        self.poisoned = false;
        self.recovered = None;
        Ok(CommittedRecord {
            record,
            ledger_bytes: bytes,
        })
    }
    pub fn is_write_poisoned(&self) -> bool {
        self.poisoned
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BootstrapRequest, bootstrap};
    use theseus_native_protocol::v2::IntentId;
    #[test]
    fn uncertain_append_errors_keep_original_source_poison_and_cache() {
        // Model the dependency's partial-write and post-write-sync failures through
        // private closures; native LockedLedgerFile fault behavior has its own tests.
        for partial in [true, false] {
            let tmp = tempfile::tempdir().unwrap();
            let l = HostDir::create_private(&tmp.path().join("l")).unwrap();
            let w = HostDir::create_private(&tmp.path().join("w")).unwrap();
            let binding = BindingId::from_bytes([1; 16]);
            let manifest_digest = Digest::from_bytes([2; 32]);
            let receipt = bootstrap(
                &l,
                &w,
                BootstrapRequest {
                    binding,
                    manifest_digest,
                    os: Os::Linux,
                },
            )
            .unwrap();
            let mut s = LedgerStore::open(
                &l,
                &w,
                StoreBinding {
                    binding,
                    manifest_digest,
                    workspace_marker: receipt.workspace_marker(),
                    os: Os::Linux,
                },
            )
            .unwrap();
            let record = Record::new(
                1,
                Entry::LeaseReserved {
                    intent: IntentId::from_bytes([3; 16]),
                    lease: LeaseId::from_bytes([4; 16]),
                },
            )
            .unwrap();
            let result = s.append_with(record.clone(), |file, bytes| {
                file.append_and_sync(
                    if partial { &bytes[..1] } else { bytes },
                    ledger::MAX_LEDGER_BYTES as u64,
                )?;
                Err(std::io::Error::from_raw_os_error(123))
            });
            assert!(matches!(result,Err(StoreError::Io(ref e)) if e.raw_os_error()==Some(123)));
            assert!(s.is_write_poisoned());
            assert_eq!(s.records.len(), 1);
            let physical = std::fs::read(tmp.path().join("l").join(LEDGER)).unwrap();
            assert!(matches!(s.append(record), Err(StoreError::WritePoisoned)));
            assert_eq!(
                std::fs::read(tmp.path().join("l").join(LEDGER)).unwrap(),
                physical
            );
            drop(s);
            let reopened = LedgerStore::open(
                &l,
                &w,
                StoreBinding {
                    binding,
                    manifest_digest,
                    workspace_marker: receipt.workspace_marker(),
                    os: Os::Linux,
                },
            );
            if partial {
                assert!(reopened.is_err());
            } else {
                assert_eq!(reopened.unwrap().records.len(), 2);
            }
        }
    }
}
