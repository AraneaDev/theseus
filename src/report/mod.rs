//! Private, atomic evidence persistence and bounded offline log access.
mod store;
pub use store::{LogRead, ReportStore, create, load, load_journal, open_existing, read_log};
