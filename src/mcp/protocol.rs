use super::*;
use serde_json::{Value, json};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Startup,
    InitializedNotification,
    Tools,
    Closing,
}
struct Pending {
    bytes: Vec<u8>,
    offset: usize,
    id: Option<RpcId>,
}
/// Pure bounded byte-stream driver; it never launches a server or performs I/O.
pub struct McpSession {
    evidence: McpEvidence,
    phase: Phase,
    pending: Option<Pending>,
    outstanding: Option<RpcId>,
    buffer: Vec<u8>,
    frames: u32,
    pages: usize,
    outgoing_bytes: usize,
    tools: BTreeSet<String>,
    cursors: BTreeSet<String>,
    eof: bool,
    limited: bool,
}
impl McpSession {
    pub fn new(probe: McpProbe) -> Result<Self> {
        probe.validate()?;
        let mut s = Self {
            evidence: McpEvidence {
                probe,
                sent_ids: vec![],
                response_lines: vec![],
                notification_count: 0,
                stdout_bytes: 0,
                stdout_complete: false,
                protocol_complete: false,
                issue: None,
                shutdown: McpShutdown::NotStarted,
            },
            phase: Phase::Startup,
            pending: None,
            outstanding: None,
            buffer: vec![],
            frames: 0,
            pages: 0,
            outgoing_bytes: 0,
            tools: BTreeSet::new(),
            cursors: BTreeSet::new(),
            eof: false,
            limited: false,
        };
        let modern = s.modern();
        let id = RpcId::String(
            if modern {
                "theseus-discover-1"
            } else {
                "theseus-initialize-1"
            }
            .into(),
        );
        let params = if modern {
            s.modern_params(json!({}))
        } else {
            json!({"protocolVersion":s.evidence.probe.protocol.version(),"capabilities":{},"clientInfo":{"name":"theseus","version":env!("CARGO_PKG_VERSION")}})
        };
        s.queue(
            if modern {
                "server/discover"
            } else {
                "initialize"
            },
            Some(id),
            params,
        );
        Ok(s)
    }
    fn modern(&self) -> bool {
        self.evidence.probe.protocol == McpProtocol::Modern20260728
    }
    fn modern_params(&self, mut params: Value) -> Value {
        params["_meta"] = json!({"io.modelcontextprotocol/protocolVersion":self.evidence.probe.protocol.version(),"io.modelcontextprotocol/clientCapabilities":{}});
        params
    }
    fn queue(&mut self, method: &str, id: Option<RpcId>, params: Value) {
        let mut value = json!({"jsonrpc":"2.0","method":method,"params":params});
        if let Some(id) = &id {
            value["id"] = serde_json::to_value(id).expect("RPC ID serialization");
        }
        let mut bytes = serde_json::to_vec(&value).expect("RPC request serialization");
        bytes.push(b'\n');
        if self.outgoing_bytes.saturating_add(bytes.len()) > OUTGOING_LIMIT {
            self.fail(McpIssueKind::Limit, "outgoing byte budget exceeded");
            return;
        }
        self.outgoing_bytes += bytes.len();
        self.pending = Some(Pending {
            bytes,
            offset: 0,
            id,
        });
    }
    fn queue_tools(&mut self, cursor: Option<String>) {
        self.phase = Phase::Tools;
        let mut params = json!({});
        if let Some(cursor) = cursor {
            params["cursor"] = json!(cursor);
        }
        if self.modern() {
            params = self.modern_params(params);
        }
        self.queue(
            "tools/list",
            Some(RpcId::Integer((self.pages + 1) as i64)),
            params,
        );
    }
    pub fn pending_write(&self) -> &[u8] {
        self.pending.as_ref().map_or(&[], |p| &p.bytes[p.offset..])
    }
    pub fn written(&mut self, count: usize) -> Result<()> {
        let available = self.pending_write().len();
        ensure!(count <= available, "write exceeds pending MCP frame");
        if count == 0 {
            return Ok(());
        }
        let pending = self.pending.as_mut().expect("nonempty pending frame");
        pending.offset += count;
        if pending.offset == pending.bytes.len() {
            let pending = self.pending.take().expect("pending frame");
            if let Some(id) = pending.id {
                self.evidence.sent_ids.push(id.clone());
                self.outstanding = Some(id);
            }
            if self.phase == Phase::InitializedNotification {
                self.queue_tools(None);
            }
        }
        Ok(())
    }
    fn fail(&mut self, kind: McpIssueKind, message: impl Into<String>) {
        if self.evidence.issue.is_none() {
            let mut message = message.into();
            let mut end = message.len().min(512);
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
            self.evidence.issue = Some(McpIssue { kind, message });
        }
        if kind == McpIssueKind::Limit {
            self.limited = true;
        }
        self.evidence.protocol_complete = false;
        self.phase = Phase::Closing;
        self.pending = None;
        self.outstanding = None;
        self.buffer.clear();
    }
    pub fn receive(&mut self, bytes: &[u8]) {
        let previous = self.evidence.stdout_bytes;
        self.evidence.stdout_bytes = previous.saturating_add(bytes.len() as u64);
        if self.evidence.issue.is_some() {
            return;
        }
        if self.eof {
            self.fail(McpIssueKind::Protocol, "stdout bytes after EOF");
            return;
        }
        // Process the captured prefix so an earlier complete protocol error wins over a later limit.
        let allowed = (STDOUT_LIMIT as u64)
            .saturating_sub(previous)
            .min(bytes.len() as u64) as usize;
        for &byte in &bytes[..allowed] {
            if self.buffer.len() == FRAME_LIMIT {
                self.fail(McpIssueKind::Limit, "MCP frame byte budget exceeded");
                break;
            }
            self.buffer.push(byte);
            if byte == b'\n' {
                self.frames += 1;
                if self.frames > FRAME_COUNT {
                    self.fail(McpIssueKind::Limit, "MCP frame count exceeded");
                    break;
                }
                let frame = std::mem::take(&mut self.buffer);
                self.frame(&frame);
                if self.evidence.issue.is_some() {
                    break;
                }
            }
        }
        if allowed < bytes.len() {
            self.fail(McpIssueKind::Limit, "MCP stdout byte budget exceeded");
        }
    }
    fn frame(&mut self, frame: &[u8]) {
        let text = match std::str::from_utf8(frame) {
            Ok(s) => s,
            Err(e) => {
                self.bad_frame(frame, &format!("invalid UTF-8 at byte {}", e.valid_up_to()));
                return;
            }
        };
        let value = match crate::strict_json::parse(text) {
            Ok(v) => v,
            Err(e) => {
                self.bad_frame(frame, &format!("invalid JSON: {e}"));
                return;
            }
        };
        if response_envelope(&value) && self.evidence.response_lines.len() < PAGE_LIMIT + 1 {
            self.evidence.response_lines.push(text.into());
        }
        if let Err(message) = self.dispatch(&value) {
            self.fail(McpIssueKind::Protocol, message);
        }
    }
    fn bad_frame(&mut self, frame: &[u8], message: &str) {
        let prefix = frame
            .iter()
            .take(48)
            .flat_map(|b| std::ascii::escape_default(*b))
            .map(char::from)
            .collect::<String>();
        self.fail(McpIssueKind::Protocol, format!("{message}; frame {prefix}"));
    }
    fn dispatch(&mut self, value: &Value) -> std::result::Result<(), String> {
        let object = value.as_object().ok_or("RPC envelope must be an object")?;
        if value["jsonrpc"] != "2.0" {
            return Err("RPC jsonrpc must be 2.0".into());
        }
        if let Some(method) = object.get("method") {
            if object.contains_key("id")
                || object.contains_key("result")
                || object.contains_key("error")
            {
                return Err("unsupported server request/conflicting envelope".into());
            }
            validate_notification(method, value.get("params"))?;
            self.evidence.notification_count += 1;
            return Ok(());
        }
        if object.contains_key("params")
            || object.contains_key("result") == object.contains_key("error")
        {
            return Err("RPC requires exactly one result/error".into());
        }
        let id: RpcId = serde_json::from_value(value.get("id").ok_or("missing RPC ID")?.clone())
            .map_err(|_| "invalid RPC ID")?;
        if self.outstanding.as_ref() != Some(&id) {
            return Err("unmatched or premature RPC response ID".into());
        }
        self.outstanding = None;
        if object.contains_key("error") {
            return Err("server returned RPC error".into());
        }
        let result = value["result"]
            .as_object()
            .ok_or("RPC result must be object")?;
        if result.get("_meta").is_some_and(|m| !m.is_object()) {
            return Err("invalid result _meta".into());
        }
        if self.modern() {
            validate_modern(result)?;
        }
        match self.phase {
            Phase::Startup => {
                let info = if self.modern() {
                    let versions = result
                        .get("supportedVersions")
                        .and_then(Value::as_array)
                        .ok_or("missing supportedVersions")?;
                    if !versions.iter().all(Value::is_string)
                        || !versions
                            .iter()
                            .any(|v| v == self.evidence.probe.protocol.version())
                    {
                        return Err("incompatible supportedVersions".into());
                    }
                    result
                        .get("_meta")
                        .and_then(|m| m.get("io.modelcontextprotocol/serverInfo"))
                        .ok_or("missing modern serverInfo")?
                } else {
                    if result.get("protocolVersion").and_then(Value::as_str)
                        != Some(self.evidence.probe.protocol.version())
                    {
                        return Err("incompatible legacy protocolVersion".into());
                    }
                    result
                        .get("serverInfo")
                        .ok_or("missing legacy serverInfo")?
                };
                if ["name", "version"].iter().any(|key| {
                    info.get(key)
                        .and_then(Value::as_str)
                        .is_some_and(|s| s.len() > 256)
                }) {
                    self.fail(
                        McpIssueKind::Limit,
                        "MCP server identity byte budget exceeded",
                    );
                    return Ok(());
                }
                validate_identity(info, &self.evidence.probe)?;
                if result.get("instructions").is_some_and(|v| !v.is_string()) {
                    return Err("invalid server instructions".into());
                }
                validate_capabilities(
                    result
                        .get("capabilities")
                        .ok_or("missing server capabilities")?,
                )?;
                if self.modern() {
                    self.queue_tools(None);
                } else {
                    self.phase = Phase::InitializedNotification;
                    self.queue("notifications/initialized", None, json!({}));
                }
            }
            Phase::Tools => {
                self.pages += 1;
                let tools = result
                    .get("tools")
                    .and_then(Value::as_array)
                    .ok_or("missing tools array")?;
                if self.tools.len().saturating_add(tools.len()) > TOOL_LIMIT {
                    self.fail(McpIssueKind::Limit, "MCP tool count exceeded");
                    return Ok(());
                }
                for tool in tools {
                    if tool
                        .get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|s| s.len() > 128)
                    {
                        self.fail(McpIssueKind::Limit, "MCP tool name byte budget exceeded");
                        return Ok(());
                    }
                    let name = validate_tool(tool)?;
                    if !self.tools.insert(name.into()) {
                        return Err("duplicate discovered tool name".into());
                    }
                }
                if let Some(cursor) = result.get("nextCursor") {
                    if cursor.as_str().is_some_and(|s| s.len() > 256) {
                        self.fail(McpIssueKind::Limit, "MCP cursor byte budget exceeded");
                        return Ok(());
                    }
                    let cursor = cursor
                        .as_str()
                        .filter(|c| bounded_text(c, 256))
                        .ok_or("invalid tools cursor")?;
                    if !self.cursors.insert(cursor.into()) {
                        return Err("repeated tools cursor".into());
                    }
                    if self.pages >= PAGE_LIMIT {
                        self.fail(McpIssueKind::Limit, "MCP page count exceeded");
                        return Ok(());
                    }
                    self.queue_tools(Some(cursor.into()));
                } else {
                    let expected = self
                        .evidence
                        .probe
                        .tools
                        .iter()
                        .cloned()
                        .collect::<BTreeSet<_>>();
                    if self.tools != expected {
                        self.fail(
                            McpIssueKind::Inventory,
                            "discovered tool inventory differs from expected inventory",
                        );
                    } else {
                        self.evidence.protocol_complete = true;
                        self.phase = Phase::Closing;
                    }
                }
            }
            _ => return Err("unexpected RPC response phase".into()),
        }
        Ok(())
    }
    pub fn eof(&mut self) {
        if self.eof {
            return;
        }
        self.eof = true;
        self.evidence.stdout_complete =
            !self.limited && self.evidence.stdout_bytes <= STDOUT_LIMIT as u64;
        if self.evidence.issue.is_none()
            && (!self.buffer.is_empty() || !self.evidence.protocol_complete)
        {
            self.fail(
                McpIssueKind::Transport,
                "stdout EOF before complete MCP discovery",
            );
        }
    }
    pub fn cancel(&mut self) {
        if self.evidence.issue.is_some() || self.phase == Phase::Closing {
            return;
        }
        let cancellable = !(self.phase == Phase::Startup && !self.modern())
            && self.pending.as_ref().is_none_or(|p| p.offset == 0);
        let outstanding = self.outstanding.clone();
        self.fail(McpIssueKind::Transport, "MCP discovery cancelled");
        if cancellable && let Some(id) = outstanding {
            let mut params = json!({"requestId":id});
            if self.modern() {
                params = self.modern_params(params);
            }
            self.queue("notifications/cancelled", None, params);
        }
    }
    pub fn wants_close(&self) -> bool {
        self.phase == Phase::Closing && self.pending.is_none()
    }
    pub fn finish(mut self, shutdown: McpShutdown) -> McpEvidence {
        self.evidence.shutdown = shutdown;
        self.evidence
    }
}
fn optional_string(value: &Value, key: &str) -> std::result::Result<(), String> {
    if value.get(key).is_some_and(|v| !v.is_string()) {
        Err(format!("invalid {key}"))
    } else {
        Ok(())
    }
}
fn validate_identity(value: &Value, probe: &McpProbe) -> std::result::Result<(), String> {
    if !value.is_object()
        || value["name"] != probe.server_name
        || value["version"] != probe.server_version
    {
        return Err("server identity/version mismatch".into());
    }
    for key in ["title", "description", "websiteUrl"] {
        optional_string(value, key)?;
    }
    Ok(())
}
fn validate_modern(result: &serde_json::Map<String, Value>) -> std::result::Result<(), String> {
    if result.get("resultType").and_then(Value::as_str) != Some("complete")
        || !result
            .get("ttlMs")
            .and_then(Value::as_f64)
            .is_some_and(|n| n.is_finite() && n >= 0.0)
        || !matches!(
            result.get("cacheScope").and_then(Value::as_str),
            Some("public" | "private")
        )
    {
        return Err("invalid required modern result fields".into());
    }
    if result.get("_meta").is_some_and(|m| !m.is_object()) {
        return Err("invalid result _meta".into());
    }
    Ok(())
}
fn validate_capabilities(value: &Value) -> std::result::Result<(), String> {
    if !value.is_object() || !value.get("tools").is_some_and(Value::is_object) {
        return Err("missing tools capability".into());
    }
    for key in [
        "logging",
        "completions",
        "experimental",
        "resources",
        "prompts",
        "tasks",
    ] {
        if value.get(key).is_some_and(|v| !v.is_object()) {
            return Err(format!("invalid {key} capability"));
        }
    }
    for (key, field) in [
        ("resources", "subscribe"),
        ("resources", "listChanged"),
        ("prompts", "listChanged"),
    ] {
        if value
            .get(key)
            .and_then(|v| v.get(field))
            .is_some_and(|v| !v.is_boolean())
        {
            return Err("invalid capability flag".into());
        }
    }
    if value["tools"]
        .get("listChanged")
        .is_some_and(|v| !v.is_boolean())
    {
        return Err("invalid tools capability".into());
    }
    Ok(())
}
fn validate_tool(value: &Value) -> std::result::Result<&str, String> {
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| bounded_text(n, 128))
        .ok_or("invalid tool name")?;
    let schema = value
        .get("inputSchema")
        .filter(|v| v.is_object() && v["type"] == "object")
        .ok_or("invalid tool inputSchema")?;
    if schema.get("properties").is_some_and(|v| !v.is_object())
        || schema
            .get("required")
            .is_some_and(|v| !v.as_array().is_some_and(|a| a.iter().all(Value::is_string)))
    {
        return Err("invalid tool schema fields".into());
    }
    for key in ["title", "description"] {
        optional_string(value, key)?;
    }
    if value
        .get("outputSchema")
        .is_some_and(|v| !v.is_object() || v["type"] != "object")
        || value.get("_meta").is_some_and(|v| !v.is_object())
    {
        return Err("invalid optional tool fields".into());
    }
    if let Some(annotations) = value.get("annotations") {
        if !annotations.is_object() {
            return Err("invalid tool annotations".into());
        }
        optional_string(annotations, "title")?;
        for key in [
            "readOnlyHint",
            "destructiveHint",
            "idempotentHint",
            "openWorldHint",
        ] {
            if annotations.get(key).is_some_and(|v| !v.is_boolean()) {
                return Err("invalid tool annotation hint".into());
            }
        }
    }
    Ok(name)
}
fn validate_notification(
    method: &Value,
    params: Option<&Value>,
) -> std::result::Result<(), String> {
    let params = params
        .filter(|p| p.is_object())
        .ok_or("notification params must be object")?;
    if params.get("_meta").is_some_and(|v| !v.is_object()) {
        return Err("invalid notification _meta".into());
    }
    match method.as_str() {
        Some("notifications/message") => {
            if !matches!(
                params["level"].as_str(),
                Some(
                    "debug"
                        | "info"
                        | "notice"
                        | "warning"
                        | "error"
                        | "critical"
                        | "alert"
                        | "emergency"
                )
            ) || params.get("data").is_none()
            {
                return Err("invalid logging notification".into());
            }
            optional_string(params, "logger")?;
        }
        Some("notifications/progress") => {
            let token = params.get("progressToken").ok_or("missing progressToken")?;
            if !token.is_string() && token.as_i64().is_none() {
                return Err("invalid progressToken".into());
            }
            if !params
                .get("progress")
                .and_then(Value::as_f64)
                .is_some_and(|n| n.is_finite())
                || params
                    .get("total")
                    .is_some_and(|v| !v.as_f64().is_some_and(|n| n.is_finite()))
            {
                return Err("invalid progress value".into());
            }
            optional_string(params, "message")?;
        }
        _ => return Err("unsupported server notification".into()),
    }
    Ok(())
}

/// Only response envelopes enter retained evidence; unsupported server requests
/// and malformed frames remain bounded diagnostics plus the original stdout log.
pub(super) fn response_envelope(value: &Value) -> bool {
    value.is_object()
        && value.get("id").is_some()
        && value.get("method").is_none()
        && (value.get("result").is_some() || value.get("error").is_some())
}
