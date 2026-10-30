//! Private, atomic evidence persistence and bounded offline log access.
mod bounded;
mod store;
pub(crate) use bounded::json_bytes;
pub(crate) use store::decode_report;
pub use store::{
    LogFingerprint, LogPage, LogRead, ReportStore, create, load, load_journal, log_fingerprint,
    open_existing, read_log, read_log_page,
};
