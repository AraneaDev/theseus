use super::*;
use std::{collections::VecDeque, sync::Mutex};

#[derive(Default)]
struct Schedule {
    prefix: Option<Vec<u8>>,
    complete: bool,
    submissions: Vec<Vec<u8>>,
    replies: VecDeque<Vec<u8>>,
    reads: usize,
    cancel_on_first_read: bool,
    never_complete: bool,
    elapsed: Duration,
    closed_at: Option<Duration>,
    closed: bool,
}
struct DelayedWrite(Arc<Mutex<Schedule>>);
struct ScheduledRead(Arc<Mutex<Schedule>>, Arc<Signals>);
impl ExchangeWrite for DelayedWrite {
    fn retain(self) -> theseus_host::process::OwnedSpawnCleanup {
        theseus_host::process::OwnedSpawnCleanup::from_endpoints(vec![], vec![])
    }
    fn try_write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let mut state = self.0.lock().unwrap();
        if let Some(prefix) = &state.prefix {
            if prefix != bytes {
                return Err(std::io::Error::other("retry replaced in-flight prefix"));
            }
            if !state.complete {
                return Err(std::io::ErrorKind::WouldBlock.into());
            }
            let count = prefix.len();
            state.prefix = None;
            state.complete = false;
            return Ok(count);
        }
        state.prefix = Some(bytes.to_vec());
        state.submissions.push(bytes.to_vec());
        Err(std::io::ErrorKind::WouldBlock.into())
    }
    fn close(&mut self) -> std::io::Result<()> {
        let mut state = self.0.lock().unwrap();
        state.prefix = None;
        state.closed = true;
        state.closed_at = Some(state.elapsed);
        Ok(())
    }
}
impl ExchangeRead for ScheduledRead {
    fn cancel_and_complete(&mut self) -> std::io::Result<()> {
        Ok(())
    }
    fn retain(self) -> theseus_host::process::OwnedSpawnCleanup {
        theseus_host::process::OwnedSpawnCleanup::from_endpoints(vec![], vec![])
    }
    fn try_read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let mut state = self.0.lock().unwrap();
        state.reads += 1;
        state.elapsed += Duration::from_millis(20);
        assert!(state.reads < 20, "exchange failed to make bounded progress");
        if state.cancel_on_first_read && state.reads == 1 {
            self.1.cancel.store(true, Ordering::SeqCst);
        }
        if state.prefix.is_some() {
            // The peer has consumed the native bytes and can already reply,
            // but the exchange has not acknowledged this write to its driver.
            state.complete = !state.never_complete;
            if let Some(reply) = state.replies.pop_front() {
                out[..reply.len()].copy_from_slice(&reply);
                return Ok(reply.len());
            }
            return Err(std::io::ErrorKind::WouldBlock.into());
        }
        if state.closed || state.replies.is_empty() {
            Ok(0)
        } else {
            Err(std::io::ErrorKind::WouldBlock.into())
        }
    }
}
fn scheduled<D: DuplexDriver>(driver: D, schedule: Schedule) -> (Acquired<D>, Vec<Vec<u8>>) {
    let state = Arc::new(Mutex::new(schedule));
    let signals = Arc::new(Signals::default());
    let acquired = exchange(
        DelayedWrite(state.clone()),
        ScheduledRead(state.clone(), signals.clone()),
        driver,
        None,
        4096,
        Arc::new(AtomicBool::new(false)),
        signals,
    );
    let writes = state.lock().unwrap().submissions.clone();
    (acquired, writes)
}
#[test]
fn abandoned_write_and_cancel_frame_share_one_request_clock() {
    let state = Arc::new(Mutex::new(Schedule {
        cancel_on_first_read: true,
        never_complete: true,
        ..Default::default()
    }));
    let signals = Arc::new(Signals::default());
    let start = Instant::now();
    let acquired = exchange_with_clock(
        DelayedWrite(state.clone()),
        ScheduledRead(state.clone(), signals.clone()),
        ReplaceFrame::new(false),
        None,
        8,
        Arc::new(AtomicBool::new(false)),
        signals,
        || start + state.lock().unwrap().elapsed,
    );
    let state = state.lock().unwrap();
    assert_eq!(
        state.closed_at,
        Some(Duration::from_millis(120)),
        "request observed at 20ms; old write settlement and cancel flush share 100ms"
    );
    assert_eq!(state.submissions, vec![b"old\n".to_vec()]);
    assert!(
        acquired.driver.acknowledgements.is_empty(),
        "abandoned bytes must not be acknowledged as cancellation"
    );
    assert!(acquired.stdout_eof);
}

#[test]
fn pending_write_keeps_read_progress_and_bounded_capture() {
    let state = Arc::new(Mutex::new(Schedule {
        never_complete: true,
        replies: VecDeque::from([b"12345".to_vec(), b"67890".to_vec()]),
        ..Default::default()
    }));
    let signals = Arc::new(Signals::default());
    let acquired = exchange(
        DelayedWrite(state.clone()),
        ScheduledRead(state.clone(), signals.clone()),
        ReplaceFrame::new(true),
        None,
        8,
        Arc::new(AtomicBool::new(false)),
        signals,
    );
    assert_eq!(acquired.output.bytes, b"12345678");
    assert!(acquired.output.truncated);
    assert_eq!(acquired.stdout_bytes, 10);
    assert!(acquired.driver.acknowledgements.is_empty());
    assert!(
        state.lock().unwrap().closed,
        "bounded capture closes an unresponsive writer"
    );
}

#[test]
fn response_after_native_completion_before_ack_is_not_an_unsent_mcp_id() {
    use crate::mcp::{McpProbe, McpProtocol, McpSession, McpShutdown};
    let session = McpSession::new(McpProbe {
        argv: vec!["/fixture".into()],
        env: Default::default(),
        timeout_seconds: 3,
        protocol: McpProtocol::Modern20260728,
        server_name: "fixture".into(),
        server_version: "2".into(),
        tools: vec!["hello".into()],
    })
    .unwrap();
    let replies = VecDeque::from([
        br#"{"jsonrpc":"2.0","id":"theseus-discover-1","result":{"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"public","_meta":{"io.modelcontextprotocol/serverInfo":{"name":"fixture","version":"2"}}}}
"#.to_vec(),
        br#"{"jsonrpc":"2.0","id":1,"result":{"resultType":"complete","tools":[{"name":"hello","inputSchema":{"type":"object"}}],"ttlMs":0,"cacheScope":"public"}}
"#.to_vec(),
    ]);
    let (acquired, writes) = scheduled(
        session,
        Schedule {
            replies,
            ..Default::default()
        },
    );
    assert!(acquired.error.is_none(), "{:?}", acquired.error);
    let evidence = acquired.driver.finish(McpShutdown::Graceful);
    assert!(evidence.protocol_complete, "{:?}", evidence.issue);
    assert_eq!(evidence.sent_ids.len(), 2);
    assert_eq!(writes.len(), 2);
}

#[test]
fn invalid_reply_clearing_pending_does_not_erase_or_reassign_native_ack() {
    use crate::mcp::{McpIssueKind, McpProbe, McpProtocol, McpSession, McpShutdown};
    for (reply, message) in [
        (b"not JSON\n".to_vec(), "invalid JSON"),
        (
            br#"{"jsonrpc":"2.0","id":999,"result":{}}
"#
            .to_vec(),
            "unmatched or premature RPC response ID",
        ),
    ] {
        let session = McpSession::new(McpProbe {
            argv: vec!["/fixture".into()],
            env: Default::default(),
            timeout_seconds: 3,
            protocol: McpProtocol::Modern20260728,
            server_name: "fixture".into(),
            server_version: "2".into(),
            tools: vec!["hello".into()],
        })
        .unwrap();
        let (acquired, writes) = scheduled(
            session,
            Schedule {
                replies: VecDeque::from([reply]),
                ..Default::default()
            },
        );
        assert!(acquired.error.is_none(), "{:?}", acquired.error);
        let evidence = acquired.driver.finish(McpShutdown::Graceful);
        assert!(!evidence.protocol_complete);
        let issue = evidence.issue.unwrap();
        assert_eq!(issue.kind, McpIssueKind::Protocol);
        assert!(issue.message.contains(message), "{}", issue.message);
        assert_eq!(
            evidence.sent_ids.len(),
            1,
            "only the actually completed request is sent"
        );
        assert_eq!(writes.len(), 1);
    }
}

struct ReplaceFrame {
    pending: Vec<u8>,
    offset: usize,
    acknowledgements: Vec<(Vec<u8>, usize)>,
    replace_on_receive: bool,
    mutated_before_ack: bool,
}
impl ReplaceFrame {
    fn new(replace_on_receive: bool) -> Self {
        Self {
            pending: b"old\n".to_vec(),
            offset: 0,
            acknowledgements: vec![],
            replace_on_receive,
            mutated_before_ack: false,
        }
    }
    fn replace(&mut self, bytes: &[u8]) {
        self.mutated_before_ack |= self.offset != self.pending.len();
        self.pending = bytes.to_vec();
        self.offset = 0;
    }
}
impl DuplexDriver for ReplaceFrame {
    fn pending_write(&self) -> &[u8] {
        &self.pending[self.offset..]
    }
    fn written(&mut self, count: usize) -> Result<()> {
        self.acknowledgements.push((self.pending.clone(), count));
        self.offset += count;
        Ok(())
    }
    fn receive(&mut self, _: &[u8]) {
        if self.replace_on_receive {
            self.replace(b"new\n");
            self.replace_on_receive = false;
        }
    }
    fn eof(&mut self) {}
    fn cancel(&mut self) {
        self.replace(b"cancel\n");
    }
    fn wants_close(&self) -> bool {
        false
    }
}
#[test]
fn cancellation_cannot_acknowledge_old_native_bytes_as_cancel_frame() {
    let (acquired, _) = scheduled(
        ReplaceFrame::new(false),
        Schedule {
            cancel_on_first_read: true,
            ..Default::default()
        },
    );
    assert!(acquired.error.is_none(), "{:?}", acquired.error);
    assert!(!acquired.driver.mutated_before_ack);
    assert_eq!(
        acquired.driver.acknowledgements,
        vec![(b"old\n".to_vec(), 4), (b"cancel\n".to_vec(), 7)]
    );
}
#[test]
fn receive_replacing_pending_frame_preserves_exact_ack_binding() {
    let (acquired, _) = scheduled(
        ReplaceFrame::new(true),
        Schedule {
            replies: VecDeque::from([b"reply".to_vec()]),
            ..Default::default()
        },
    );
    assert!(acquired.error.is_none(), "{:?}", acquired.error);
    assert!(!acquired.driver.mutated_before_ack);
    assert_eq!(
        acquired.driver.acknowledgements,
        vec![(b"old\n".to_vec(), 4), (b"new\n".to_vec(), 4)]
    );
}
