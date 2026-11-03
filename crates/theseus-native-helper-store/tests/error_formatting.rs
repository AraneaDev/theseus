use std::{
    error::Error,
    fmt, io,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use theseus_native_helper_store::{IntentError, StoreError};
use theseus_native_protocol::v2::ledger::LedgerError;
struct Retained {
    calls: Arc<AtomicUsize>,
    identity: Arc<()>,
}
impl fmt::Debug for Retained {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.calls.fetch_add(1, Ordering::SeqCst);
        f.write_str("SECRET\n\x1b[31m")?;
        f.write_str(&"x".repeat(8192))
    }
}
impl fmt::Display for Retained {
    fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
        panic!("retained source Display must not run")
    }
}
impl Error for Retained {}
#[test]
fn public_error_formatting_is_fixed_without_touching_original_source() {
    for which in 0..3 {
        let calls = Arc::new(AtomicUsize::new(0));
        let identity = Arc::new(());
        let io = io::Error::other(Retained {
            calls: calls.clone(),
            identity: identity.clone(),
        });
        let (error, expected): (Box<dyn Error>, &str) = match which {
            0 => (Box::new(StoreError::Io(io)), "store I/O failure"),
            1 => (Box::new(IntentError::Io(io)), "intent I/O failure"),
            _ => (
                Box::new(IntentError::Publication(io)),
                "intent publication failure",
            ),
        };
        assert_eq!(format!("{error}"), expected);
        assert_eq!(format!("{error:?}"), expected);
        assert_eq!(format!("{error:#?}"), expected);
        assert_eq!(format!("{error:60000?}"), expected);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let original = error
            .source()
            .unwrap()
            .downcast_ref::<io::Error>()
            .unwrap()
            .get_ref()
            .unwrap()
            .downcast_ref::<Retained>()
            .unwrap();
        assert!(Arc::ptr_eq(&original.identity, &identity));
        assert!(Arc::ptr_eq(&original.calls, &calls));
    }
}
#[test]
fn every_error_variant_has_bounded_classification_and_typed_protocol_source() {
    let protocol = StoreError::Protocol(LedgerError::Truncated);
    assert_eq!(
        protocol.source().unwrap().downcast_ref::<LedgerError>(),
        Some(&LedgerError::Truncated)
    );
    let errors: Vec<(Box<dyn Error>, &str)> = vec![
        (Box::new(protocol), "store protocol failure"),
        (
            Box::new(StoreError::BindingMismatch),
            "store binding mismatch",
        ),
        (
            Box::new(StoreError::MarkerMismatch),
            "workspace marker mismatch",
        ),
        (Box::new(StoreError::AliasedRoots), "aliased store roots"),
        (
            Box::new(StoreError::BootstrapConflict),
            "bootstrap conflict",
        ),
        (
            Box::new(StoreError::RecoveredUnfinished),
            "recovered unfinished history",
        ),
        (Box::new(StoreError::WritePoisoned), "store write poisoned"),
        (Box::new(IntentError::Conflict), "intent conflict"),
        (
            Box::new(IntentError::InvalidExisting),
            "invalid existing intent",
        ),
    ];
    for (error, expected) in errors {
        assert_eq!(format!("{error}"), expected);
        assert_eq!(format!("{error:?}"), expected);
        assert_eq!(format!("{error:#?}"), expected);
    }
}
