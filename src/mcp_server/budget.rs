//! Complete-envelope bounded response staging and largest-prefix page fitting.
use super::{
    cursor::{CursorRecord, Family},
    types::*,
};
use serde::{Serialize, Serializer, ser::SerializeMap};
use std::io::{self, Write};

const CAP: usize = 65_536;
#[derive(Debug)]
pub(crate) struct OutboundFrame {
    pub id: Option<RpcId>,
    pub bytes: Vec<u8>,
    pub offset: usize,
}
impl OutboundFrame {
    pub fn remaining(&self) -> &[u8] {
        &self.bytes[self.offset..]
    }
}
#[derive(Debug)]
pub(crate) enum EncodeFailure {
    Capacity(OutboundFrame),
    Fatal,
}

/// This sink rejects before growth, including for a single huge serializer write.
struct CappedWriter {
    bytes: Vec<u8>,
    overflow: bool,
}
impl CappedWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::with_capacity(CAP),
            overflow: false,
        }
    }
}
impl Write for CappedWriter {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if input.len() > CAP - self.bytes.len() {
            self.overflow = true;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "response capacity",
            ));
        }
        self.bytes.extend_from_slice(input);
        Ok(input.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[derive(Clone, Copy)]
enum StageError {
    Capacity,
    Fatal,
}
fn serialized(value: &impl Serialize, newline: bool) -> Result<Vec<u8>, StageError> {
    let mut writer = CappedWriter::new();
    if serde_json::to_writer(&mut writer, value).is_err() {
        return Err(if writer.overflow {
            StageError::Capacity
        } else {
            StageError::Fatal
        });
    }
    if newline && writer.write_all(b"\n").is_err() {
        return Err(StageError::Capacity);
    }
    Ok(writer.bytes)
}
struct Application<'a> {
    instance: &'a Instance,
    value: &'a ToolResult,
}
impl Serialize for Application<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.value {
            Err(f) => f.serialize(s),
            Ok(data) => {
                let mut map = s.serialize_map(Some(4))?;
                map.serialize_entry("format_version", &1u8)?;
                map.serialize_entry("server_instance", self.instance)?;
                map.serialize_entry("ok", &true)?;
                map.serialize_entry("data", data)?;
                map.end()
            }
        }
    }
}
#[derive(Serialize)]
struct Content<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    text: &'a str,
}
struct ToolBody<'a> {
    era: Era,
    application: Application<'a>,
    text: &'a str,
    is_error: bool,
}
impl Serialize for ToolBody<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(None)?;
        map.serialize_entry(
            "content",
            &[Content {
                kind: "text",
                text: self.text,
            }],
        )?;
        map.serialize_entry("structuredContent", &self.application)?;
        map.serialize_entry("isError", &self.is_error)?;
        if self.era == Era::Modern {
            map.serialize_entry("resultType", "complete")?;
            map.serialize_entry("_meta",&serde_json::json!({"io.modelcontextprotocol/serverInfo":{"name":"theseus","version":env!("CARGO_PKG_VERSION")}}))?;
        }
        map.end()
    }
}
struct ResultEnvelope<'a, T> {
    id: &'a RpcId,
    body: T,
}
impl<T: Serialize> Serialize for ResultEnvelope<'_, T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(3))?;
        map.serialize_entry("jsonrpc", "2.0")?;
        map.serialize_entry("id", self.id)?;
        map.serialize_entry("result", &self.body)?;
        map.end()
    }
}
fn tool_bytes(
    era: Era,
    id: &RpcId,
    instance: &Instance,
    value: &ToolResult,
) -> Result<Vec<u8>, StageError> {
    let application = Application { instance, value };
    // The first bounded serialization is borrowed as text; escaping is streamed by
    // serde into the second bounded sink, rather than formatting another String.
    let bytes = serialized(&application, false)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| StageError::Fatal)?;
    serialized(
        &ResultEnvelope {
            id,
            body: ToolBody {
                era,
                application,
                text,
                is_error: value.is_err(),
            },
        },
        true,
    )
}
fn outbound(id: &RpcId, bytes: Vec<u8>) -> OutboundFrame {
    OutboundFrame {
        id: Some(id.clone()),
        bytes,
        offset: 0,
    }
}
fn too_large(instance: &Instance, handle: Option<&Handle>) -> Failure {
    Failure::new(
        instance.clone(),
        ErrorCode::ResponseTooLarge,
        TextPreview::new("Response exceeds the frame limit", false),
        handle.cloned(),
    )
}
/// Capacity returns an already staged replacement for the same request slot.
/// Fatal means even that replacement could not be encoded; begin shutdown.
pub(crate) fn encode(
    era: Era,
    id: &RpcId,
    instance: &Instance,
    handle: Option<&Handle>,
    value: &ToolResult,
) -> Result<OutboundFrame, EncodeFailure> {
    match tool_bytes(era, id, instance, value) {
        Ok(bytes) => Ok(outbound(id, bytes)),
        Err(StageError::Fatal) => Err(EncodeFailure::Fatal),
        Err(StageError::Capacity) => {
            let bound_handle = match value {
                Err(failure) => failure.error.handle.as_ref().or(handle),
                Ok(ToolData::RunStart(data)) => Some(&data.handle),
                Ok(ToolData::RunStatus(data)) => Some(&data.run.handle),
                Ok(ToolData::RunCancel(data)) => Some(&data.run.handle),
                _ => handle,
            };
            let failure = Err(too_large(instance, bound_handle));
            let bytes =
                tool_bytes(era, id, instance, &failure).map_err(|_| EncodeFailure::Fatal)?;
            Err(EncodeFailure::Capacity(outbound(id, bytes)))
        }
    }
}
pub(crate) fn encode_protocol_result(
    era: Era,
    id: &RpcId,
    method: &Method,
) -> Result<OutboundFrame, EncodeFailure> {
    let body = super::wire::protocol_result(era, method).ok_or(EncodeFailure::Fatal)?;
    serialized(&ResultEnvelope { id, body }, true)
        .map(|bytes| outbound(id, bytes))
        .map_err(|_| EncodeFailure::Fatal)
}
struct ErrorEnvelope<'a>(&'a ProtocolError);
#[derive(Serialize)]
struct VersionBody<'a> {
    requested: &'a str,
    supported: &'a [String],
}
struct ErrorBody<'a>(&'a ProtocolError);
impl Serialize for ErrorBody<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let e = self.0;
        let mut map = s.serialize_map(None)?;
        map.serialize_entry("code", &e.code)?;
        map.serialize_entry("message", &e.message.text)?;
        if let Some(data) = &e.data {
            map.serialize_entry(
                "data",
                &VersionBody {
                    requested: &data.requested,
                    supported: &data.supported,
                },
            )?;
        }
        map.end()
    }
}
impl Serialize for ErrorEnvelope<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let e = self.0;
        let mut map = s.serialize_map(None)?;
        map.serialize_entry("jsonrpc", "2.0")?;
        match &e.id {
            ReplyId::Id(id) => map.serialize_entry("id", id)?,
            ReplyId::Null => map.serialize_entry("id", &Option::<u8>::None)?,
            ReplyId::Omit => {}
        }
        map.serialize_entry("error", &ErrorBody(e))?;
        map.end()
    }
}
pub(crate) fn encode_protocol_error(error: &ProtocolError) -> Result<OutboundFrame, EncodeFailure> {
    let bytes = serialized(&ErrorEnvelope(error), true).map_err(|_| EncodeFailure::Fatal)?;
    // Null and omitted IDs correlate with an anonymous slot in the same pool.
    let id = match &error.id {
        ReplyId::Id(id) => Some(id.clone()),
        _ => None,
    };
    Ok(OutboundFrame {
        id,
        bytes,
        offset: 0,
    })
}
pub(crate) struct PageCandidate {
    pub data: ToolData,
    pub cursor: CursorRecord,
    pub more_after_candidate: bool,
}
fn page_len(data: &ToolData) -> Option<(usize, Family)> {
    match data {
        ToolData::RecipeInspect(InspectData::Catalog(p)) => {
            Some((p.records.len(), Family::RecipeCatalog))
        }
        ToolData::RecipeInspect(InspectData::Record(p)) => {
            Some((p.bytes.len(), Family::RecipeRecord))
        }
        ToolData::RunEvidence(EvidenceData::Catalog(p)) => {
            Some((p.records.len(), Family::EvidenceCatalog))
        }
        ToolData::RunEvidence(EvidenceData::Record(p)) => {
            Some((p.bytes.len(), Family::EvidenceRecord))
        }
        ToolData::RunEvidence(EvidenceData::Log(p)) => Some((p.bytes.len(), Family::Log)),
        ToolData::RunList(p) => Some((p.runs.len(), Family::RunList)),
        _ => None,
    }
}
fn prefix(data: &ToolData, n: usize, next: Option<Cursor>) -> ToolData {
    let mut result = data.clone();
    match &mut result {
        ToolData::RecipeInspect(InspectData::Catalog(p))
        | ToolData::RunEvidence(EvidenceData::Catalog(p)) => {
            p.records.truncate(n);
            p.next_cursor = next;
        }
        ToolData::RecipeInspect(InspectData::Record(p))
        | ToolData::RunEvidence(EvidenceData::Record(p)) => {
            p.bytes.truncate(n);
            p.next_cursor = next;
        }
        ToolData::RunEvidence(EvidenceData::Log(p)) => {
            p.bytes.truncate(n);
            p.next_cursor = next;
        }
        ToolData::RunList(p) => {
            p.runs.truncate(n);
            p.next_cursor = next;
        }
        _ => unreachable!("validated paged variant"),
    }
    result
}
pub(crate) fn fit_page(reply: &ReplyContext, page: PageCandidate) -> Result<ToolData, Failure> {
    let fail = |code| {
        Failure::new(
            reply.instance.clone(),
            code,
            TextPreview::new("Invalid page candidate", false),
            None,
        )
    };
    let (len, family) = page_len(&page.data).ok_or_else(|| fail(ErrorCode::InternalError))?;
    if family != page.cursor.family
        || len > usize::from(page.cursor.requested_limit)
        || len > usize::from(family.maximum())
    {
        return Err(fail(ErrorCode::InternalError));
    }
    let encoded = page.cursor.encode().map_err(fail)?;
    CursorRecord::decode(&encoded, &reply.instance).map_err(fail)?;
    let bound_handle = if matches!(
        family,
        Family::EvidenceCatalog | Family::EvidenceRecord | Family::Log
    ) {
        Some(
            Handle::parse(&format!(
                "{}:{}",
                reply.instance.as_str(),
                page.cursor.admission
            ))
            .map_err(|_| fail(ErrorCode::InvalidCursor))?,
        )
    } else {
        None
    };
    let fits = |data: ToolData| -> Result<Option<ToolData>, Failure> {
        match tool_bytes(reply.era, &reply.id, &reply.instance, &Ok(data.clone())) {
            Ok(_) => Ok(Some(data)),
            Err(StageError::Capacity) => Ok(None),
            Err(StageError::Fatal) => Err(fail(ErrorCode::InternalError)),
        }
    };
    // Omitting a final cursor can make the endpoint fit even when the previous
    // prefix does not, so test the complete final candidate before binary search.
    if !page.more_after_candidate
        && let Some(data) = fits(prefix(&page.data, len, None))?
    {
        return Ok(data);
    }
    if len == 0 {
        return Err(too_large(&reply.instance, bound_handle.as_ref()));
    }
    let candidate = |n: usize| -> Result<ToolData, Failure> {
        let mut cursor = page.cursor.clone();
        cursor.next_position = cursor
            .next_position
            .checked_add(n as u64)
            .filter(|n| *n <= UINT_MAX)
            .ok_or_else(|| fail(ErrorCode::InvalidCursor))?;
        let next = cursor.encode().map_err(fail)?;
        Ok(prefix(&page.data, n, Some(next)))
    };
    let mut low = 1;
    let mut high = len;
    let mut best = None;
    while low <= high {
        let n = low + (high - low) / 2;
        if let Some(data) = fits(candidate(n)?)? {
            best = Some(data);
            low = n + 1;
        } else {
            high = n - 1;
        }
    }
    best.ok_or_else(|| too_large(&reply.instance, bound_handle.as_ref()))
}
#[cfg(test)]
#[path = "budget_tests.rs"]
mod tests;
