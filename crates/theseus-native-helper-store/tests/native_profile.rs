use theseus_host::fs::HostDir;
use theseus_native_helper_store::{
    BootstrapRequest, LedgerStore, StoreBinding, StoreError, bootstrap, bootstrap_native,
};
use theseus_native_protocol::v2;
fn request() -> BootstrapRequest {
    BootstrapRequest {
        binding: v2::BindingId::from_bytes([1; 16]),
        manifest_digest: v2::Digest::from_bytes([2; 32]),
        os: v2::Os::Linux,
    }
}
fn binding(marker: v2::WorkspaceMarker) -> StoreBinding {
    StoreBinding {
        binding: request().binding,
        manifest_digest: request().manifest_digest,
        workspace_marker: marker,
        os: request().os,
    }
}
#[test]
fn store_profiles_and_current_epoch_are_exact_and_disjoint() {
    for native in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let l = HostDir::create_private(&temp.path().join("l")).unwrap();
        let w = HostDir::create_private(&temp.path().join("w")).unwrap();
        let epoch = v2::Digest::from_bytes([4; 32]);
        let receipt = if native {
            bootstrap_native(&l, &w, request(), epoch)
        } else {
            bootstrap(&l, &w, request())
        }
        .unwrap();
        let marker = receipt.workspace_marker();
        if native {
            assert!(matches!(
                LedgerStore::open(&l, &w, binding(marker)),
                Err(StoreError::BindingMismatch)
            ));
            assert!(matches!(
                LedgerStore::open_native(&l, &w, binding(marker), v2::Digest::from_bytes([5; 32])),
                Err(StoreError::BindingMismatch)
            ));
            drop(LedgerStore::open_native(&l, &w, binding(marker), epoch).unwrap());
        } else {
            assert!(matches!(
                LedgerStore::open_native(&l, &w, binding(marker), epoch),
                Err(StoreError::BindingMismatch)
            ));
            drop(LedgerStore::open(&l, &w, binding(marker)).unwrap());
        }
    }
}
