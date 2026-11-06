#![allow(dead_code)]
use theseus_host::fs::HostDir;
use theseus_native_helper_store::*;
use theseus_native_protocol::v2::*;
pub struct Fixture {
    pub tmp: tempfile::TempDir,
    pub ledger: HostDir,
    pub workspace: HostDir,
}
impl Fixture {
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = HostDir::create_private(&tmp.path().join("ledger")).unwrap();
        let workspace = HostDir::create_private(&tmp.path().join("workspace")).unwrap();
        Self {
            tmp,
            ledger,
            workspace,
        }
    }
    pub fn bootstrap(&self) -> BootstrapReceipt {
        bootstrap(&self.ledger, &self.workspace, request()).unwrap()
    }
    pub fn binding(&self, marker: WorkspaceMarker) -> StoreBinding {
        StoreBinding {
            binding: BindingId::from_bytes([1; 16]),
            manifest_digest: Digest::from_bytes([2; 32]),
            workspace_marker: marker,
            os: Os::Linux,
        }
    }
    /// Reads through a separate handle, so only while no `LedgerStore` is open:
    /// Windows exclusive byte-range locks refuse reads through other handles.
    /// Use `LedgerStore::read_physical` while a store owns the ledger.
    pub fn bytes(&self) -> Vec<u8> {
        std::fs::read(self.tmp.path().join("ledger/ledger.bin")).unwrap()
    }
}
pub fn request() -> BootstrapRequest {
    BootstrapRequest {
        binding: BindingId::from_bytes([1; 16]),
        manifest_digest: Digest::from_bytes([2; 32]),
        os: Os::Linux,
    }
}
pub fn reserved() -> ledger::Record {
    ledger::Record::new(
        1,
        ledger::Entry::LeaseReserved {
            intent: IntentId::from_bytes([3; 16]),
            lease: LeaseId::from_bytes([4; 16]),
        },
    )
    .unwrap()
}
pub fn accepted() -> ledger::Record {
    ledger::Record::new(
        2,
        ledger::Entry::OperationAccepted {
            lease: LeaseId::from_bytes([4; 16]),
            operation: OperationId::from_bytes([5; 16]),
            request_digest: Digest::from_bytes([6; 32]),
            operation_kind: OperationKind::Run,
        },
    )
    .unwrap()
}
