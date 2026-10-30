//! Private, atomic evidence persistence and bounded offline log access.
mod store;
pub(crate) use store::decode_report;
pub use store::{LogRead, ReportStore, create, load, load_journal, open_existing, read_log};
