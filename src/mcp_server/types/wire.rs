//! Wire protocol request, reply and event types.
use super::*;

#[derive(Debug)]
pub(crate) enum Method {
    Initialize,
    Discover,
    Ping,
    ToolsList,
    ToolCall(ToolCall),
    RejectedTool(ArgumentError),
}
#[derive(Debug)]
pub(crate) struct Request {
    pub id: RpcId,
    pub era: Era,
    pub method: Method,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Notification {
    Initialized,
    Cancel(RpcId),
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReplyId {
    Id(RpcId),
    Null,
    Omit,
}
#[derive(Debug)]
pub(crate) struct VersionData {
    pub requested: String,
    pub supported: Vec<String>,
}
#[derive(Debug)]
pub(crate) struct ProtocolError {
    pub code: i32,
    pub message: TextPreview,
    pub data: Option<VersionData>,
    pub id: ReplyId,
    pub close_after_send: bool,
}
#[derive(Debug)]
pub(crate) enum WireEvent {
    Request(Request),
    Notification(Notification),
    ProtocolError(ProtocolError),
    Ignore,
}
