//! Revision-bound report and raw-log evidence, through bound store authority only.
use super::{
    budget::{PageCandidate, fit_page},
    cursor::CursorRecord,
    evidence_records::{self, Catalog},
    range_writer::RangeWriter,
    types::*,
};
use crate::{model::RunReport, report::ReportStore};
use sha2::{Digest as _, Sha256};

#[derive(Debug)]
pub(crate) struct ReportSnapshot {
    pub revision: String,
    pub report: RunReport,
    pub sampled_hint: u64,
}
pub(super) fn error(code: ErrorCode) -> QueryError {
    QueryError::new(code, anyhow::anyhow!("report evidence request rejected"))
}
pub(crate) fn load_snapshot(
    store: &ReportStore,
    sampled_hint: u64,
) -> Result<ReportSnapshot, QueryError> {
    let bytes = store
        .read_bytes("report.json")
        .map_err(|e| QueryError::new(ErrorCode::IoError, e))?;
    let report = crate::report::decode_report(&bytes)
        .map_err(|e| QueryError::new(ErrorCode::EvidenceInvalid, e))?;
    Ok(ReportSnapshot {
        revision: format!("{:x}", Sha256::digest(&bytes)),
        report,
        sampled_hint,
    })
}
fn uint(n: u64) -> Result<UInt, QueryError> {
    UInt::new(n).map_err(|_| error(ErrorCode::ResponseTooLarge))
}
fn length(n: usize) -> Result<u64, QueryError> {
    let n = u64::try_from(n).map_err(|_| error(ErrorCode::ResponseTooLarge))?;
    uint(n)?;
    Ok(n)
}
/// The caller must resolve the known admission handle to this retained store.
/// This function accepts neither a caller path nor registry authority, and never
/// mutates the run/cache. A failed refresh therefore cannot replace saved facts.
pub(crate) fn evidence(
    store: &ReportStore,
    snapshot: &ReportSnapshot,
    args: &EvidenceArgs,
    reply: &ReplyContext,
) -> Result<EvidenceData, QueryError> {
    ToolCall::RunEvidence(args.clone())
        .validate()
        .map_err(|_| error(ErrorCode::InvalidArguments))?;
    if args.server_instance != reply.instance {
        return Err(error(ErrorCode::StaleInstance));
    }
    let decoded = args
        .cursor
        .as_ref()
        .map(|c| CursorRecord::decode(c, &reply.instance))
        .transpose()
        .map_err(error)?;
    if args
        .revision
        .as_ref()
        .is_some_and(|r| r.as_str() != snapshot.revision)
    {
        return Err(error(ErrorCode::RevisionChanged));
    }
    let revision =
        Revision::parse(&snapshot.revision).map_err(|_| error(ErrorCode::InternalError))?;
    let mut cursor = CursorRecord::evidence(args, &revision).map_err(error)?;
    if let Some(c) = &decoded {
        // Validate identity/selector/limit before resolving any selected model
        // position. Actual source size and current log identity follow below.
        let mut binding = cursor.clone();
        binding.log_change_digest = c.log_change_digest;
        c.validate(&binding, UINT_MAX).map_err(error)?;
    }
    let data = match &args.selector {
        EvidenceSelector::Record(selector) => {
            let record = evidence_records::record(&snapshot.report, &selector.record_id)?;
            let serialize = |writer: &mut RangeWriter| {
                serde_json::to_writer(writer, &record.detail)
                    .map_err(|e| QueryError::new(ErrorCode::EvidenceInvalid, e.into()))
            };
            let mut counter = RangeWriter::new(0, 0);
            serialize(&mut counter)?;
            let total = counter.total_bytes();
            uint(total)?;
            if let Some(c) = decoded {
                c.validate(&cursor, total).map_err(error)?;
                cursor = c;
            }
            let mut writer =
                RangeWriter::new(cursor.next_position, usize::from(cursor.requested_limit));
            serialize(&mut writer)?;
            let more = cursor.next_position + length(writer.bytes().len())? < total;
            PageCandidate {
                data: ToolData::RunEvidence(EvidenceData::Record(RecordPage {
                    kind: RecordSelectorKind::Record,
                    revision,
                    record_id: selector.record_id.clone(),
                    record_kind: record.detail.kind(),
                    encoding: Encoding::JsonUtf8,
                    offset: uint(cursor.next_position)?,
                    bytes: writer.bytes().to_vec(),
                    total_bytes: uint(total)?,
                    next_cursor: None,
                })),
                cursor,
                more_after_candidate: more,
            }
        }
        EvidenceSelector::Log(selector) => {
            let command = evidence_records::command(&snapshot.report, &selector.command_id)?;
            let reference = match selector.stream {
                Stream::Stdout => command.stdout.as_ref(),
                Stream::Stderr => command.stderr.as_ref(),
            }
            .ok_or_else(|| error(ErrorCode::UnknownRecord))?;
            // Probe at zero before any cursor-position read: a shortened/replaced
            // file must be classified as changed, never used at a stale offset.
            let probe = store
                .read_log_page(reference, 0, 1)
                .map_err(|e| log_error(e, decoded.is_some()))?;
            cursor = cursor
                .with_log_fingerprint(&probe.fingerprint.canonical_bytes())
                .map_err(error)?;
            uint(probe.readable_total)?;
            uint(probe.current_bytes)?;
            uint(probe.recorded_bytes)?;
            if let Some(c) = decoded {
                c.validate(&cursor, probe.readable_total).map_err(error)?;
                cursor = c;
            }
            let page = store
                .read_log_page(
                    reference,
                    cursor.next_position,
                    usize::from(cursor.requested_limit),
                )
                .map_err(|e| log_error(e, true))?;
            if page.fingerprint != probe.fingerprint {
                return Err(error(ErrorCode::LogChanged));
            }
            let more = page.next_offset.is_some();
            PageCandidate {
                data: ToolData::RunEvidence(EvidenceData::Log(LogPage {
                    kind: LogKind::Log,
                    revision,
                    command_id: command.id.clone(),
                    stream: selector.stream,
                    offset: uint(page.offset)?,
                    bytes: page.bytes,
                    recorded_bytes: uint(page.recorded_bytes)?,
                    current_bytes: uint(page.current_bytes)?,
                    readable_total: uint(page.readable_total)?,
                    capture_truncated: reference.truncated,
                    change_token: cursor.change_token(),
                    next_cursor: None,
                })),
                cursor,
                more_after_candidate: more,
            }
        }
        _ => {
            let catalog = Catalog::select(&snapshot.report, &args.selector)?;
            let total = length(catalog.len())?;
            if let Some(c) = decoded {
                c.validate(&cursor, total).map_err(error)?;
                cursor = c;
            }
            let start = usize::try_from(cursor.next_position)
                .map_err(|_| error(ErrorCode::InvalidCursor))?;
            let end = start
                .checked_add(usize::from(cursor.requested_limit))
                .ok_or_else(|| error(ErrorCode::InvalidCursor))?
                .min(catalog.len());
            let records = (start..end)
                .map(|i| catalog.record(i)?.summary())
                .collect::<Result<Vec<_>, _>>()?;
            PageCandidate {
                data: ToolData::RunEvidence(EvidenceData::Catalog(CatalogPage {
                    kind: CatalogKind::Catalog,
                    revision,
                    records,
                    next_cursor: None,
                })),
                cursor,
                more_after_candidate: end < catalog.len(),
            }
        }
    };
    match fit_page(reply, data).map_err(|f| error(f.error.code))? {
        ToolData::RunEvidence(data) => Ok(data),
        _ => Err(error(ErrorCode::InternalError)),
    }
}
fn log_error(source: anyhow::Error, previously_observed: bool) -> QueryError {
    // The existing contained reader emits this static typed message when its
    // before/after/reopen fingerprint check fails. Preserve the original owner.
    let changed = source.downcast_ref::<&'static str>() == Some(&"log changed while reading page")
        || source.chain().any(|e| {
            e.downcast_ref::<std::io::Error>().is_some_and(|e| {
                e.kind() == std::io::ErrorKind::UnexpectedEof
                    || previously_observed && e.kind() == std::io::ErrorKind::NotFound
            })
        });
    QueryError::new(
        if changed {
            ErrorCode::LogChanged
        } else {
            ErrorCode::IoError
        },
        source,
    )
}
#[cfg(test)]
#[path = "evidence_tests.rs"]
mod tests;
