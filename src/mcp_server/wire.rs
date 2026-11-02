//! Pure per-process era adapter. Slot ownership and bounded framing live above it.
use super::types::*;
use serde_json::{Map, Value, json};

const VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
const CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";

#[derive(Debug, Default)]
pub(crate) struct WireState {
    pub selected: Option<Era>,
    pub legacy_initialized: bool,
}
impl WireState {
    fn error(&self, code: i32, id: Option<RpcId>, close: bool) -> WireEvent {
        let message = match code {
            -32700 => "Parse error",
            -32600 => "Invalid request",
            -32601 => "Method not found",
            -32022 => "Unsupported protocol version",
            _ => "Invalid params",
        };
        WireEvent::ProtocolError(ProtocolError {
            code,
            message: TextPreview::new(message, false),
            data: None,
            id: id
                .map(ReplyId::Id)
                .unwrap_or(if self.selected == Some(Era::Modern) {
                    ReplyId::Omit
                } else {
                    ReplyId::Null
                }),
            close_after_send: close,
        })
    }
    pub fn decode(&mut self, frame: &[u8]) -> WireEvent {
        let value = match std::str::from_utf8(frame)
            .ok()
            .and_then(|s| crate::strict_json::parse(s).ok())
        {
            Some(v) => v,
            None => return self.error(-32700, None, true),
        };
        let Some(object) = value.as_object() else {
            return self.error(-32600, None, true);
        };
        // Invalid notification-shaped messages never produce responses or mutate state.
        let notification = !object.contains_key("id") && object.contains_key("method");
        if object.get("jsonrpc") != Some(&json!("2.0"))
            || !object.get("method").is_some_and(Value::is_string)
            || object.contains_key("result")
            || object.contains_key("error")
            || object.get("params").is_some_and(|p| !p.is_object())
        {
            return if notification {
                WireEvent::Ignore
            } else {
                self.error(
                    -32600,
                    self.selected
                        .and_then(|_| object.get("id").and_then(rpc_id)),
                    true,
                )
            };
        }
        let empty = Map::new();
        let params = object
            .get("params")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        let method = object["method"].as_str().expect("checked method");
        if notification {
            return self.notification(method, params);
        }
        let Some(id) = object.get("id").and_then(rpc_id) else {
            return self.error(-32600, None, true);
        };
        let has_modern = params
            .get("_meta")
            .and_then(Value::as_object)
            .is_some_and(|m| {
                m.contains_key(VERSION)
                    || m.contains_key(CAPABILITIES)
                    || m.contains_key(CLIENT_INFO)
            });
        if method == "initialize" && self.selected == Some(Era::Modern) {
            return self.error(-32601, Some(id), false);
        }
        if has_modern && self.selected == Some(Era::Legacy) {
            return self.error(-32602, Some(id), false);
        }
        if params.get("_meta").is_some_and(|m| !m.is_object()) {
            return self.error(-32602, Some(id), false);
        }
        let era = if has_modern || self.selected == Some(Era::Modern) {
            let Some(meta) = params.get("_meta").and_then(Value::as_object) else {
                return self.error(-32602, Some(id), false);
            };
            let Some(version) = meta.get(VERSION).and_then(Value::as_str) else {
                return self.error(-32602, Some(id), false);
            };
            if !meta.get(CAPABILITIES).is_some_and(valid_capabilities)
                || meta.get(CLIENT_INFO).is_some_and(|v| !valid_identity(v))
                || meta
                    .get("io.modelcontextprotocol/logLevel")
                    .is_some_and(|v| {
                        !matches!(
                            v.as_str(),
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
                        )
                    })
            {
                return self.error(-32602, Some(id), false);
            }
            self.selected = Some(Era::Modern);
            if version != MODERN_VERSION {
                let WireEvent::ProtocolError(mut error) = self.error(-32022, Some(id), false)
                else {
                    unreachable!()
                };
                error.data = Some(VersionData {
                    requested: version.into(),
                    supported: vec![MODERN_VERSION.into()],
                });
                return WireEvent::ProtocolError(error);
            }
            Era::Modern
        } else if method == "initialize" {
            if self.selected.is_some()
                || !params.get("protocolVersion").is_some_and(Value::is_string)
                || !params.get("capabilities").is_some_and(valid_capabilities)
                || !params.get("clientInfo").is_some_and(valid_identity)
            {
                return self.error(-32602, Some(id), false);
            }
            self.selected = Some(Era::Legacy);
            Era::Legacy
        } else {
            Era::Legacy
        };
        if era == Era::Legacy
            && !self.legacy_initialized
            && !matches!(method, "initialize" | "ping")
        {
            return self.error(
                if matches!(method, "tools/list" | "tools/call" | "server/discover") {
                    -32602
                } else {
                    -32601
                },
                Some(id),
                false,
            );
        }
        let dispatch = match method {
            "initialize" if era == Era::Legacy => Method::Initialize,
            "server/discover" if era == Era::Modern => Method::Discover,
            "ping" => Method::Ping,
            "tools/list" => {
                if params.contains_key("cursor") {
                    return self.error(-32602, Some(id), false);
                }
                Method::ToolsList
            }
            "tools/call" => {
                let Some(name) = params.get("name").and_then(Value::as_str) else {
                    return self.error(-32602, Some(id), false);
                };
                let arguments = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match tool_call(name, arguments) {
                    None => return self.error(-32602, Some(id), false),
                    Some(Ok(call)) => Method::ToolCall(call),
                    Some(Err(e)) => Method::RejectedTool(e),
                }
            }
            _ => return self.error(-32601, Some(id), false),
        };
        WireEvent::Request(Request {
            id,
            era,
            method: dispatch,
        })
    }
    fn notification(&mut self, method: &str, params: &Map<String, Value>) -> WireEvent {
        if params.get("_meta").is_some_and(|m| !m.is_object()) {
            return WireEvent::Ignore;
        }
        let meta = params.get("_meta").and_then(Value::as_object);
        let modern_meta = meta.is_some_and(|m| {
            m.contains_key(VERSION) || m.contains_key(CAPABILITIES) || m.contains_key(CLIENT_INFO)
        });
        match self.selected {
            // NotificationMetaObject is optional and is not RequestMetaObject.
            Some(Era::Modern) => {}
            Some(Era::Legacy) if !modern_meta => {}
            _ => return WireEvent::Ignore,
        }
        match method {
            "notifications/initialized"
                if self.selected == Some(Era::Legacy) && !self.legacy_initialized =>
            {
                self.legacy_initialized = true;
                WireEvent::Notification(Notification::Initialized)
            }
            "notifications/cancelled" if !params.get("reason").is_some_and(|v| !v.is_string()) => {
                params
                    .get("requestId")
                    .and_then(rpc_id)
                    .map(|id| WireEvent::Notification(Notification::Cancel(id)))
                    .unwrap_or(WireEvent::Ignore)
            }
            _ => WireEvent::Ignore,
        }
    }
}
fn rpc_id(value: &Value) -> Option<RpcId> {
    match value {
        Value::String(s) if s.len() <= 128 => Some(RpcId::String(s.clone())),
        Value::Number(n) => n.as_i64().map(RpcId::Integer),
        _ => None,
    }
}
fn valid_identity(value: &Value) -> bool {
    value.is_object()
        && value.get("name").is_some_and(Value::is_string)
        && value.get("version").is_some_and(Value::is_string)
        && ["title", "description", "websiteUrl"]
            .iter()
            .all(|k| value.get(*k).is_none_or(Value::is_string))
        && value.get("icons").is_none_or(|v| {
            v.as_array().is_some_and(|a| {
                a.iter().all(|i| {
                    i.is_object()
                        && i.get("src").is_some_and(Value::is_string)
                        && i.get("mimeType").is_none_or(Value::is_string)
                        && i.get("sizes").is_none_or(|v| {
                            v.as_array().is_some_and(|a| a.iter().all(Value::is_string))
                        })
                        && i.get("theme")
                            .is_none_or(|v| matches!(v.as_str(), Some("light" | "dark")))
                })
            })
        })
}
fn valid_capabilities(value: &Value) -> bool {
    let Some(map) = value.as_object() else {
        return false;
    };
    [
        "roots",
        "sampling",
        "elicitation",
        "tasks",
        "experimental",
        "extensions",
    ]
    .iter()
    .all(|k| map.get(*k).is_none_or(Value::is_object))
        && value
            .get("roots")
            .and_then(|r| r.get("listChanged"))
            .is_none_or(Value::is_boolean)
        && ["experimental", "extensions"].iter().all(|k| {
            map.get(*k)
                .and_then(Value::as_object)
                .is_none_or(|m| m.values().all(Value::is_object))
        })
        && [
            ("sampling", "context"),
            ("sampling", "tools"),
            ("elicitation", "form"),
            ("elicitation", "url"),
            ("tasks", "list"),
            ("tasks", "cancel"),
            ("tasks", "requests"),
        ]
        .iter()
        .all(|(outer, inner)| {
            value
                .get(*outer)
                .and_then(|v| v.get(*inner))
                .is_none_or(Value::is_object)
        })
}

fn tool_call(name: &str, arguments: Value) -> Option<Result<ToolCall, ArgumentError>> {
    let result = match name {
        "recipe_inspect" => serde_json::from_value(arguments).map(ToolCall::RecipeInspect),
        "run_start" => serde_json::from_value(arguments).map(ToolCall::RunStart),
        "run_list" => serde_json::from_value(arguments).map(ToolCall::RunList),
        "run_status" => serde_json::from_value(arguments).map(ToolCall::RunStatus),
        "run_cancel" => serde_json::from_value(arguments).map(ToolCall::RunCancel),
        "run_evidence" => serde_json::from_value(arguments).map(ToolCall::RunEvidence),
        _ => return None,
    }
    .map_err(|_| ArgumentError)
    .and_then(|call| {
        call.validate()?;
        Ok(call)
    });
    Some(result)
}

/// Protocol result payload only; the bounded Task 3 encoder owns the RPC envelope.
pub(crate) fn protocol_result(era: Era, method: &Method) -> Option<Value> {
    let mut result = match method {
        Method::Initialize if era == Era::Legacy => {
            json!({"protocolVersion":LEGACY_VERSION,"capabilities":{"tools":{}},"serverInfo":server_info()})
        }
        Method::Discover if era == Era::Modern => {
            json!({"supportedVersions":[MODERN_VERSION],"capabilities":{"tools":{}}})
        }
        Method::Ping => json!({}),
        Method::ToolsList => super::schema::schemas(),
        _ => return None,
    };
    if era == Era::Modern {
        result["resultType"] = json!("complete");
        result["_meta"] = json!({"io.modelcontextprotocol/serverInfo":server_info()});
        if matches!(method, Method::Discover | Method::ToolsList) {
            result["ttlMs"] = json!(0);
            result["cacheScope"] = json!("private");
        }
    }
    Some(result)
}
fn server_info() -> Value {
    json!({"name":"theseus","version":env!("CARGO_PKG_VERSION")})
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod tests;
