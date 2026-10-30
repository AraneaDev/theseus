use super::*;
use crate::model::CheckOutcome;

impl McpEvidence {
    /// Checks bounded internal consistency, not the authenticity of the saved report.
    /// Malformed frames and notification bodies are intentionally not retained; a
    /// diagnostic-only failure may be valid without reproducing its discarded bytes.
    pub fn valid(&self) -> bool {
        if self.probe.validate().is_err()
            || self.sent_ids.len() > PAGE_LIMIT + 1
            || self
                .sent_ids
                .iter()
                .any(|id| matches!(id,RpcId::String(s) if s.len()>128))
            || self.response_lines.len() > PAGE_LIMIT + 1
            || self.notification_count > FRAME_COUNT
            || self.response_lines.len() + self.notification_count as usize > FRAME_COUNT as usize
            || self
                .issue
                .as_ref()
                .is_some_and(|i| i.message.is_empty() || i.message.len() > 512)
            || self.protocol_complete && self.issue.is_some()
            || self.stdout_complete && self.stdout_bytes > STDOUT_LIMIT as u64
        {
            return false;
        }
        let mut response_bytes = 0usize;
        for line in &self.response_lines {
            if line.len() > FRAME_LIMIT
                || !line.ends_with('\n')
                || line[..line.len() - 1].contains('\n')
            {
                return false;
            }
            if !crate::strict_json::parse(line)
                .is_ok_and(|v| super::protocol::response_envelope(&v))
            {
                return false;
            }
            response_bytes = response_bytes.saturating_add(line.len());
        }
        // The shortest accepted notification is a logging message with level
        // info and data 0. Omitted bodies may be longer, never shorter.
        let notification_min = br#"{"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info","data":0}}"#.len() as u64 + 1;
        if response_bytes > STDOUT_LIMIT
            || (response_bytes as u64)
                .saturating_add((self.notification_count as u64).saturating_mul(notification_min))
                > self.stdout_bytes
            || self.protocol_complete
                && self.notification_count == 0
                && response_bytes as u64 != self.stdout_bytes
        {
            return false;
        }
        if !serde_json::to_vec(self).is_ok_and(|v| v.len() <= EVIDENCE_LIMIT) {
            return false;
        }
        let replay = self.replay();
        if self.protocol_complete {
            replay.protocol_complete && replay.issue.is_none() && replay.sent_ids == self.sent_ids
        } else {
            // The last queued request need not have been fully written when
            // acquisition stopped; only an exact prefix of generated IDs is legal.
            self.sent_ids.len() <= replay.sent_ids.len()
                && replay.sent_ids[..self.sent_ids.len()] == self.sent_ids
                && (!replay.protocol_complete || self.issue.is_some())
        }
    }
    fn replay(&self) -> McpEvidence {
        let mut session = McpSession::new(self.probe.clone()).expect("validated MCP probe");
        for line in &self.response_lines {
            advance(&mut session);
            session.receive(line.as_bytes());
        }
        advance(&mut session);
        session.eof();
        session.finish(McpShutdown::Graceful)
    }
    pub fn verified_pass(&self) -> bool {
        self.protocol_complete
            && self.stdout_complete
            && self.shutdown == McpShutdown::Graceful
            && self.issue.is_none()
            && self.valid()
    }
    pub fn protocol_outcome(&self) -> CheckOutcome {
        if !self.valid() || self.shutdown != McpShutdown::Graceful || !self.stdout_complete {
            return CheckOutcome::Incomplete;
        }
        match self.issue.as_ref().map(|i| i.kind) {
            Some(McpIssueKind::Protocol | McpIssueKind::Inventory) => CheckOutcome::Failed,
            Some(_) => CheckOutcome::Incomplete,
            None if self.verified_pass() => CheckOutcome::Passed,
            None => CheckOutcome::Incomplete,
        }
    }
}
fn advance(session: &mut McpSession) {
    // At most the legacy initialized notification followed by one tools request.
    for _ in 0..2 {
        let count = session.pending_write().len();
        if count == 0 {
            break;
        }
        session.written(count).expect("complete generated frame");
    }
}
