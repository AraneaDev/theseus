//! Windows filesystem FFI stays in these small, platform-local modules.
mod identity;
mod open;
mod publish;
pub(super) use identity::same_directory;
pub use identity::{FileFingerprint, fingerprint};
pub use open::read_exact_at;
pub(super) use open::{
    create_dir, entries, open_creation_parent, open_dir, open_file, open_leaf_classified,
    open_ledger, open_root, validate_native_name,
};

pub(in crate::fs) use publish::{flush_file, remove_temporary, replace};
