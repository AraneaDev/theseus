//! Private transport-independent domain and protocol boundary for the local façade.
pub(crate) mod budget;
pub(crate) mod coordinator;
pub(crate) mod cursor;
pub(crate) mod diagnostics;
pub(crate) mod evidence;
pub(crate) mod evidence_records;
pub(crate) mod framing;
pub(crate) mod inspection;
pub(crate) mod inspection_records;
pub(crate) mod owned_error;
pub(crate) mod query;
pub(crate) mod range_writer;
pub(crate) mod registry;
pub(crate) mod roots;
pub(crate) mod schema;
pub(crate) mod shutdown;
pub(crate) mod status_projection;
pub(crate) mod types;
pub(crate) mod wire;
pub(crate) mod worker;

#[cfg(test)]
mod documentation_tests;
