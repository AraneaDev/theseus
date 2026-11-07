//! Explicit connected broker conversation using the original bounded client pump.
//! Native endpoint/peer owners remain with the caller; no child is fabricated.
use crate::{ClientError, Event, RequestId, pump::Pump};
use theseus_native_protocol::v2;

pub struct BrokerSession {
    pump: Pump,
    next_id: u64,
}
impl BrokerSession {
    pub fn new(expected: v2::broker::IntegrationBrokerHeader) -> std::io::Result<Self> {
        let mut random = [0; 16];
        theseus_host::random_bytes(&mut random)?;
        Self::with_session(expected, v2::SessionId::from_bytes(random))
            .map_err(std::io::Error::other)
    }
    pub fn with_session(
        expected: v2::broker::IntegrationBrokerHeader,
        session: v2::SessionId,
    ) -> Result<Self, ClientError> {
        Ok(Self {
            pump: Pump::new_broker(session, expected).map_err(|_| ClientError::Preflight)?,
            next_id: 0,
        })
    }
    pub fn is_ready(&self) -> bool {
        self.pump.ready
    }
    pub fn timed_out(&self) -> bool {
        self.pump.timeout()
    }
    pub fn pending_output(&self) -> Option<&[u8]> {
        let frame = self.pump.frames.front()?;
        Some(&frame.bytes[frame.offset..(frame.offset + 4096).min(frame.bytes.len())])
    }
    pub fn advance_output(&mut self, n: usize) -> Result<(), ClientError> {
        let frame = self.pump.frames.front_mut().ok_or(ClientError::Protocol)?;
        if n == 0 || n > (frame.offset + 4096).min(frame.bytes.len()) - frame.offset {
            return Err(ClientError::Protocol);
        }
        if frame
            .advance(|_| Ok(n))
            .map_err(|_| ClientError::Protocol)?
        {
            self.pump.frames.pop_front();
        }
        Ok(())
    }
    /// Returns consumed bytes; the caller retains any unconsumed suffix exactly.
    pub fn feed_input(&mut self, bytes: &[u8]) -> Result<(usize, Option<Event>), ClientError> {
        self.pump.decode_broker(bytes)
    }
    pub fn eof(&mut self) -> Result<(), ClientError> {
        self.pump.eof().map_err(|_| ClientError::Protocol)
    }
    pub fn submit(&mut self, request: v2::Request) -> Result<RequestId, ClientError> {
        if !self.is_ready() {
            return Err(ClientError::NotReady);
        }
        if !matches!(
            request,
            v2::Request::InspectOwnedEntry { .. }
                | v2::Request::PrepareOwnedEntry { .. }
                | v2::Request::Acquire { .. }
                | v2::Request::QueryAcquire { .. }
                | v2::Request::Release { .. }
                | v2::Request::QueryLease { .. }
                | v2::Request::DesktopAction { .. }
                | v2::Request::InspectDesktop { .. }
                | v2::Request::QueryDesktop { .. }
                | v2::Request::CancelDesktop { .. }
                | v2::Request::ProbeDesktopReadiness { .. }
        ) {
            return Err(ClientError::Preflight);
        }
        let cancel = matches!(request, v2::Request::CancelDesktop { .. });
        if !self.pump.available(cancel) {
            return Err(ClientError::Busy);
        }
        let id = RequestId(self.next_id);
        self.next_id = self.next_id.checked_add(1).ok_or(ClientError::Exhausted)?;
        self.pump
            .submit(id, request)
            .map_err(|_| ClientError::Protocol)?;
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_broker_header_is_required_before_any_ready_event() {
        let header:v2::broker::IntegrationBrokerHeader=serde_json::from_value(serde_json::json!({"profile":"native_integration_broker_v1","binding":"11".repeat(16),"manifest_digest":"22".repeat(32),"workspace_marker":"33".repeat(16),"os":"linux","target_id":"fixture","ownership_id":"44".repeat(16),"provider_epoch":"55".repeat(32),"session_id":"supplied-session","session_epoch":"66".repeat(32),"broker_endpoint":{"kind":"unix_path","path":"/fixture/broker.sock"},"broker_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"helper_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"display":{"kind":"x11","display":":92","authority_file":"/fixture/auth","bus":"/fixture/bus.sock"},"allowed_modes":["owned_executable"]})).unwrap();
        for changed in [false, true] {
            let session = v2::SessionId::from_bytes([1; 16]);
            let mut client = BrokerSession::with_session(header.clone(), session.clone()).unwrap();
            while let Some(bytes) = client.pending_output() {
                let n = bytes.len();
                client.advance_output(n).unwrap();
            }
            let hello = v2::encode(
                &v2::Envelope::new(
                    session.clone(),
                    0,
                    v2::Message::Hello {
                        role: v2::Role::Helper,
                    },
                )
                .unwrap(),
            )
            .unwrap();
            assert!(client.feed_input(&hello).unwrap().1.is_none());
            while let Some(bytes) = client.pending_output() {
                let n = bytes.len();
                client.advance_output(n).unwrap();
            }
            let mut observed = header.clone();
            if changed {
                observed.session_epoch = "77".repeat(32);
            }
            let reply = v2::encode(
                &v2::Envelope::new(
                    session,
                    1,
                    v2::Message::Response {
                        reply_to: 1,
                        result: v2::Reply::BrokerDescription {
                            header: Box::new(observed),
                        },
                    },
                )
                .unwrap(),
            )
            .unwrap();
            let result = client.feed_input(&reply);
            if changed {
                assert!(matches!(result, Err(ClientError::Preflight)));
                assert!(!client.is_ready());
            } else {
                assert!(matches!(result, Ok((_, Some(Event::Ready)))));
                assert!(client.is_ready());
            }
        }
    }
}
