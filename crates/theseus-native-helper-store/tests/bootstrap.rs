mod common;
use common::*;
use theseus_native_helper_store::*;
use theseus_native_protocol::v2::ledger::{Entry, encode};
#[test]
fn exclusive_bootstrap_binds_exact_marker_header_and_refuses_reuse() {
    let f = Fixture::new();
    let r = f.bootstrap();
    let marker = std::fs::read(f.tmp.path().join("workspace/.theseus-workspace-marker")).unwrap();
    assert_eq!(marker, r.workspace_marker().to_hex().as_bytes());
    assert_eq!(marker.len(), 32);
    assert!(
        matches!(r.header().entry(),Entry::Header { workspace_marker, .. } if *workspace_marker==r.workspace_marker())
    );
    assert_eq!(f.bytes(), encode(r.header()).unwrap());
    assert!(matches!(
        bootstrap(&f.ledger, &f.workspace, request()),
        Err(StoreError::BootstrapConflict)
    ));
    assert_eq!(f.bytes(), encode(r.header()).unwrap());
}
#[test]
fn aliased_roots_refuse_without_artifacts() {
    let f = Fixture::new();
    assert!(matches!(
        bootstrap(&f.ledger, &f.ledger, request()),
        Err(StoreError::AliasedRoots)
    ));
    assert_eq!(
        std::fs::read_dir(f.tmp.path().join("ledger"))
            .unwrap()
            .count(),
        0
    );
}
#[test]
fn either_existing_name_prevents_all_mutation() {
    for (dir, name) in [
        ("ledger", "ledger.bin"),
        ("workspace", ".theseus-workspace-marker"),
    ] {
        let f = Fixture::new();
        let path = f.tmp.path().join(dir).join(name);
        std::fs::write(&path, b"keep").unwrap();
        assert!(matches!(
            bootstrap(&f.ledger, &f.workspace, request()),
            Err(StoreError::BootstrapConflict)
        ));
        assert_eq!(std::fs::read(path).unwrap(), b"keep");
        assert_eq!(
            std::fs::read_dir(f.tmp.path().join(if dir == "ledger" {
                "workspace"
            } else {
                "ledger"
            }))
            .unwrap()
            .count(),
            0
        );
    }
}
