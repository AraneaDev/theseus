use crate::{ClientError, Event, ExpectedHelper, RequestId};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};
use theseus_native_protocol::v2;

pub(crate) struct Frame {
    pub(crate) bytes: Vec<u8>,
    pub(crate) offset: usize,
}
impl Frame {
    pub(crate) fn advance(
        &mut self,
        write: impl FnOnce(&[u8]) -> std::io::Result<usize>,
    ) -> std::io::Result<bool> {
        let end = (self.offset + 4096).min(self.bytes.len());
        let n = write(&self.bytes[self.offset..end])?;
        if n == 0 || n > end - self.offset {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "invalid native pipe write progress",
            ));
        }
        self.offset += n;
        Ok(self.offset == self.bytes.len())
    }
}
pub(crate) struct Pump {
    session: v2::SessionId,
    broker_header: Option<v2::broker::IntegrationBrokerHeader>,
    pub(crate) protocol_failure: Option<v2::Error>,
    guard: v2::SessionGuard,
    decoder: v2::Decoder,
    next: u64,
    pub(crate) frames: VecDeque<Frame>,
    pending: [Option<(RequestId, u64)>; 2],
    describe: Option<u64>,
    pub(crate) ready: bool,
    pub(crate) artifact_commitment: Option<v2::Digest>,
    pub(crate) integration_description: Option<v2::IntegrationDescription>,
    started: Instant,
    pub(crate) residual: Vec<u8>,
}
impl Pump {
    pub(crate) fn new(session: v2::SessionId) -> Result<Self, v2::Error> {
        let mut pump = Self {
            protocol_failure: None,
            broker_header: None,
            guard: v2::SessionGuard::new(session.clone(), v2::Role::Controller),
            session,
            decoder: v2::Decoder::new(),
            next: 0,
            frames: VecDeque::with_capacity(2),
            pending: [None; 2],
            describe: None,
            ready: false,
            artifact_commitment: None,
            integration_description: None,
            started: Instant::now(),
            residual: Vec::with_capacity(4096),
        };
        pump.queue(v2::Message::Hello {
            role: v2::Role::Controller,
        })?;
        Ok(pump)
    }
    pub(crate) fn new_broker(
        session: v2::SessionId,
        header: v2::broker::IntegrationBrokerHeader,
    ) -> Result<Self, v2::Error> {
        header.validate()?;
        let mut pump = Self::new(session)?;
        pump.broker_header = Some(header);
        Ok(pump)
    }
    pub(crate) fn decode_broker(
        &mut self,
        bytes: &[u8],
    ) -> Result<(usize, Option<Event>), ClientError> {
        let progress = self.decoder.feed(bytes).map_err(|e| {
            self.protocol_failure = Some(e);
            ClientError::Protocol
        })?;
        let event = match progress.frame {
            Some(frame) => self.inbound_peer(frame, None)?,
            None => None,
        };
        Ok((progress.consumed, event))
    }
    fn queue(&mut self, message: v2::Message) -> Result<u64, v2::Error> {
        if self.frames.len() == 2 {
            return Err(v2::Error::TooManyRequests);
        }
        let seq = self.next;
        let frame = v2::Envelope::new(self.session.clone(), seq, message)?;
        let bytes = v2::encode(&frame)?;
        self.guard.accept_outbound(&frame)?;
        self.next = seq.checked_add(1).ok_or(v2::Error::UnexpectedSequence)?;
        self.frames.push_back(Frame { bytes, offset: 0 });
        Ok(seq)
    }
    pub(crate) fn available(&self, cancel: bool) -> bool {
        self.frames.len() < 2 && self.pending[usize::from(cancel)].is_none()
    }
    pub(crate) fn submit(&mut self, id: RequestId, request: v2::Request) -> Result<(), v2::Error> {
        let slot = usize::from(matches!(
            request,
            v2::Request::Cancel { .. } | v2::Request::CancelDesktop { .. }
        ));
        if self.pending[slot].is_some() {
            return Err(v2::Error::TooManyRequests);
        }
        let seq = self.queue(v2::Message::Request { request })?;
        self.pending[slot] = Some((id, seq));
        Ok(())
    }
    pub(crate) fn timeout(&self) -> bool {
        !self.ready && self.started.elapsed() >= Duration::from_secs(5)
    }
    pub(crate) fn decode(
        &mut self,
        bytes: &[u8],
        expected: &ExpectedHelper,
    ) -> Result<(usize, Option<Event>), ClientError> {
        let progress = self.decoder.feed(bytes).map_err(|e| {
            self.protocol_failure = Some(e);
            ClientError::Protocol
        })?;
        let event = match progress.frame {
            Some(frame) => self.inbound(frame, expected)?,
            None => None,
        };
        Ok((progress.consumed, event))
    }
    pub(crate) fn eof(&mut self) -> Result<(), v2::Error> {
        self.decoder.finish()
    }
    fn inbound(
        &mut self,
        frame: v2::Envelope,
        expected: &ExpectedHelper,
    ) -> Result<Option<Event>, ClientError> {
        self.inbound_peer(frame, Some(expected))
    }
    fn inbound_peer(
        &mut self,
        frame: v2::Envelope,
        expected: Option<&ExpectedHelper>,
    ) -> Result<Option<Event>, ClientError> {
        self.guard.accept_inbound(&frame).map_err(|e| {
            self.protocol_failure = Some(e);
            ClientError::Protocol
        })?;
        Ok(match frame.message() {
            v2::Message::Hello { .. } => {
                let seq = self
                    .queue(v2::Message::Request {
                        request: v2::Request::Describe,
                    })
                    .map_err(|_| ClientError::Protocol)?;
                self.describe = Some(seq);
                None
            }
            v2::Message::Response { reply_to, result } if self.describe == Some(*reply_to) => {
                self.describe = None;
                match result {
                    v2::Reply::BrokerDescription { header } => {
                        if expected.is_some()
                            || self.broker_header.as_ref() != Some(header.as_ref())
                        {
                            return Err(ClientError::Preflight);
                        }
                        self.ready = true;
                        Some(Event::Ready)
                    }
                    v2::Reply::Description {
                        binding,
                        manifest_digest,
                        workspace_marker,
                        os,
                        programs,
                        ..
                    } => {
                        let expected = expected.ok_or(ClientError::Preflight)?;
                        let b = &expected.binding;
                        if expected.profile != v2::HelperProfile::Fixture
                            || *binding != b.binding
                            || *manifest_digest != b.manifest_digest
                            || *workspace_marker != b.workspace_marker
                            || *os != b.os
                            || programs != &expected.programs
                        {
                            return Err(ClientError::Preflight);
                        }
                        self.ready = true;
                        Some(Event::Ready)
                    }
                    v2::Reply::IntegrationDescription { description: d } => {
                        let expected = expected.ok_or(ClientError::Preflight)?;
                        let b = &expected.binding;
                        let native = &d.native;
                        if expected.profile
                            != (v2::HelperProfile::Integration {
                                provider_epoch: native.provider_epoch,
                                core_manifest_digest: d.core_manifest_digest,
                                integration_manifest_digest: d.integration_manifest_digest,
                            })
                            || native.binding != b.binding
                            || native.manifest_digest != b.manifest_digest
                            || native.workspace_marker != b.workspace_marker
                            || native.os != b.os
                            || native.programs != expected.programs
                            || v2::integration_manifest_digest(
                                d.core_manifest_digest,
                                d.integration_manifest_digest,
                            ) != native.manifest_digest
                            || d.capabilities
                                != v2::IntegrationCapabilities::for_manifest(&d.manifest)
                                    .map_err(|_| ClientError::Preflight)?
                        {
                            return Err(ClientError::Preflight);
                        }
                        self.artifact_commitment = Some(native.artifact_commitment);
                        self.integration_description = Some(*d.clone());
                        self.ready = true;
                        Some(Event::Ready)
                    }
                    v2::Reply::NativeDescription { description: d } => {
                        let expected = expected.ok_or(ClientError::Preflight)?;
                        let b = &expected.binding;
                        if expected.profile
                            != (v2::HelperProfile::Native {
                                provider_epoch: d.provider_epoch,
                            })
                            || d.binding != b.binding
                            || d.manifest_digest != b.manifest_digest
                            || d.workspace_marker != b.workspace_marker
                            || d.os != b.os
                            || d.programs != expected.programs
                            || d.capabilities != v2::NativeCapabilities
                        {
                            return Err(ClientError::Preflight);
                        }
                        self.artifact_commitment = Some(d.artifact_commitment);
                        self.ready = true;
                        Some(Event::Ready)
                    }
                    v2::Reply::Refused {
                        code: v2::RefusalCode::OwnershipPending,
                        ..
                    } => {
                        let seq = self
                            .queue(v2::Message::Request {
                                request: v2::Request::Describe,
                            })
                            .map_err(|_| ClientError::Protocol)?;
                        self.describe = Some(seq);
                        None
                    }
                    _ => return Err(ClientError::Preflight),
                }
            }
            v2::Message::Response { reply_to, result } => {
                let slot = self
                    .pending
                    .iter()
                    .position(|p| p.as_ref().is_some_and(|(_, seq)| seq == reply_to))
                    .ok_or(ClientError::Protocol)?;
                let (request, _) = self.pending[slot].take().ok_or(ClientError::Protocol)?;
                Some(Event::Reply {
                    request,
                    reply: result.clone(),
                })
            }
            v2::Message::Output {
                lease,
                operation,
                channel,
                offset,
                bytes,
            } => Some(Event::Output {
                lease: *lease,
                operation: *operation,
                channel: *channel,
                offset: *offset,
                bytes: bytes.clone(),
            }),
            v2::Message::Log {
                lease,
                operation,
                code,
            } => Some(Event::Log {
                lease: *lease,
                operation: *operation,
                code: *code,
            }),
            v2::Message::Closed { code } => Some(Event::PeerClosed(*code)),
            _ => return Err(ClientError::Protocol),
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use theseus_native_helper_store::StoreBinding;
    pub(crate) fn expected() -> ExpectedHelper {
        ExpectedHelper {
            profile: v2::HelperProfile::Fixture,
            binding: StoreBinding {
                binding: v2::BindingId::from_bytes([1; 16]),
                manifest_digest: v2::Digest::from_bytes([2; 32]),
                workspace_marker: v2::WorkspaceMarker::from_bytes([3; 16]),
                os: v2::Os::Linux,
            },
            programs: v2::ProgramList::new(vec![v2::ProgramId::new("probe").unwrap()]).unwrap(),
        }
    }
    pub(crate) fn description() -> v2::Reply {
        v2::Reply::Description {
            binding: v2::BindingId::from_bytes([1; 16]),
            manifest_digest: v2::Digest::from_bytes([2; 32]),
            workspace_marker: v2::WorkspaceMarker::from_bytes([3; 16]),
            os: v2::Os::Linux,
            helper_principal: v2::Principal::Posix {
                effective_uid: 500,
                effective_gid: 600,
            },
            programs: v2::ProgramList::new(vec![v2::ProgramId::new("probe").unwrap()]).unwrap(),
        }
    }
    pub(super) fn inbound(
        pump: &mut Pump,
        seq: u64,
        message: v2::Message,
    ) -> Result<Option<Event>, ClientError> {
        let bytes =
            v2::encode(&v2::Envelope::new(pump.session.clone(), seq, message).unwrap()).unwrap();
        let (n, event) = pump.decode(&bytes, &expected())?;
        assert_eq!(n, bytes.len());
        Ok(event)
    }
    pub(crate) fn ready() -> Pump {
        let mut pump = Pump::new(v2::SessionId::from_bytes([10; 16])).unwrap();
        pump.frames.clear();
        assert!(
            inbound(
                &mut pump,
                0,
                v2::Message::Hello {
                    role: v2::Role::Helper
                }
            )
            .unwrap()
            .is_none()
        );
        pump.frames.clear();
        assert!(matches!(
            inbound(
                &mut pump,
                1,
                v2::Message::Response {
                    reply_to: 1,
                    result: description()
                }
            )
            .unwrap(),
            Some(Event::Ready)
        ));
        pump
    }
    #[test]
    fn native_profile_and_epoch_are_required_before_ready() {
        for (native_expected, native_reply, stale) in [
            (true, true, false),
            (true, true, true),
            (true, false, false),
            (false, true, false),
        ] {
            let mut pump = Pump::new(v2::SessionId::from_bytes([10; 16])).unwrap();
            pump.frames.clear();
            inbound(
                &mut pump,
                0,
                v2::Message::Hello {
                    role: v2::Role::Helper,
                },
            )
            .unwrap();
            pump.frames.clear();
            let mut expected = expected();
            let programs = v2::ProgramList::new(
                [
                    "check_base",
                    "check_data",
                    "check_target",
                    "configure",
                    "exercise",
                    "install_base",
                    "install_target",
                    "uninstall",
                ]
                .map(|id| v2::ProgramId::new(id).unwrap())
                .to_vec(),
            )
            .unwrap();
            expected.programs = programs.clone();
            if native_expected {
                expected.profile = v2::HelperProfile::Native {
                    provider_epoch: v2::Digest::from_bytes([4; 32]),
                };
            }
            let result = if native_reply {
                v2::Reply::NativeDescription {
                    description: Box::new(v2::NativeDescription {
                        binding: expected.binding.binding,
                        manifest_digest: expected.binding.manifest_digest,
                        workspace_marker: expected.binding.workspace_marker,
                        os: expected.binding.os,
                        helper_principal: v2::Principal::Posix {
                            effective_uid: 1000,
                            effective_gid: 1000,
                        },
                        programs,
                        capabilities: v2::NativeCapabilities,
                        provider_epoch: v2::Digest::from_bytes([if stale { 5 } else { 4 }; 32]),
                        artifact_commitment: v2::Digest::from_bytes([6; 32]),
                        build: v2::Provenance::Unavailable {
                            reason: "fixture".into(),
                        },
                        os_build: v2::Provenance::Unavailable {
                            reason: "fixture".into(),
                        },
                    }),
                }
            } else {
                let mut reply = description();
                if let v2::Reply::Description { programs: p, .. } = &mut reply {
                    *p = programs;
                }
                reply
            };
            let frame = v2::Envelope::new(
                pump.session.clone(),
                1,
                v2::Message::Response {
                    reply_to: 1,
                    result,
                },
            )
            .unwrap();
            let bytes = v2::encode(&frame).unwrap();
            let result = pump.decode(&bytes, &expected);
            if native_expected && native_reply && !stale {
                assert!(matches!(result, Ok((_, Some(Event::Ready)))));
                assert!(pump.ready);
            } else {
                assert!(matches!(result, Err(ClientError::Preflight)));
                assert!(!pump.ready);
            }
        }
    }
    #[test]
    fn every_description_field_is_bound_before_ready() {
        for field in 0..5 {
            let mut pump = Pump::new(v2::SessionId::from_bytes([10; 16])).unwrap();
            pump.frames.clear();
            inbound(
                &mut pump,
                0,
                v2::Message::Hello {
                    role: v2::Role::Helper,
                },
            )
            .unwrap();
            pump.frames.clear();
            let mut reply = description();
            let v2::Reply::Description {
                binding,
                manifest_digest,
                workspace_marker,
                os,
                programs,
                ..
            } = &mut reply
            else {
                unreachable!()
            };
            match field {
                0 => *binding = v2::BindingId::from_bytes([9; 16]),
                1 => *manifest_digest = v2::Digest::from_bytes([9; 32]),
                2 => *workspace_marker = v2::WorkspaceMarker::from_bytes([9; 16]),
                3 => *os = v2::Os::Macos,
                _ => {
                    *programs =
                        v2::ProgramList::new(vec![v2::ProgramId::new("other").unwrap()]).unwrap()
                }
            }
            assert!(matches!(
                inbound(
                    &mut pump,
                    1,
                    v2::Message::Response {
                        reply_to: 1,
                        result: reply
                    }
                ),
                Err(ClientError::Preflight)
            ));
            assert!(!pump.ready);
            assert!(pump.frames.is_empty());
        }
    }
    #[test]
    fn ownership_pending_keeps_one_internal_describe_and_bounded_clock() {
        let mut pump = Pump::new(v2::SessionId::from_bytes([10; 16])).unwrap();
        pump.frames.clear();
        inbound(
            &mut pump,
            0,
            v2::Message::Hello {
                role: v2::Role::Helper,
            },
        )
        .unwrap();
        pump.frames.clear();
        inbound(
            &mut pump,
            1,
            v2::Message::Response {
                reply_to: 1,
                result: v2::Reply::Refused {
                    code: v2::RefusalCode::OwnershipPending,
                    inspect_failure: None,
                },
            },
        )
        .unwrap();
        assert_eq!(pump.frames.len(), 1);
        assert_eq!(pump.pending, [None; 2]);
        assert_eq!(pump.describe, Some(2));
        pump.started = Instant::now() - Duration::from_secs(6);
        assert!(pump.timeout());
    }
    #[test]
    fn second_ordinary_is_refused_while_cancel_remains_reserved() {
        let mut pump = ready();
        let lease = v2::LeaseId::from_bytes([1; 16]);
        let op = v2::OperationId::from_bytes([2; 16]);
        pump.submit(RequestId(40), v2::Request::QueryLease { lease })
            .unwrap();
        assert!(!pump.available(false));
        assert!(pump.available(true));
        assert_eq!(
            pump.submit(RequestId(41), v2::Request::QueryLease { lease }),
            Err(v2::Error::TooManyRequests)
        );
        pump.submit(
            RequestId(42),
            v2::Request::Cancel {
                lease,
                operation: op,
            },
        )
        .unwrap();
        assert_eq!(pump.frames.len(), 2);
        assert!(!pump.available(true));
        assert!(matches!(
            inbound(
                &mut pump,
                2,
                v2::Message::Response {
                    reply_to: 2,
                    result: v2::Reply::Lease {
                        lease,
                        state: v2::LeaseView::Held {
                            intent: v2::IntentId::from_bytes([3; 16])
                        }
                    }
                }
            )
            .unwrap(),
            Some(Event::Reply {
                request: RequestId(40),
                ..
            })
        ));
    }
    #[test]
    fn malformed_duplicate_wrong_sequence_and_version_fail_without_ready() {
        let mut pump = ready();
        assert_eq!(
            inbound(
                &mut pump,
                1,
                v2::Message::Hello {
                    role: v2::Role::Helper
                }
            )
            .unwrap_err(),
            ClientError::Protocol
        );
        for bytes in [b"\0\0\0\x02{}".as_slice(), b"\0\0\0\x01{".as_slice()] {
            let mut pump = Pump::new(v2::SessionId::from_bytes([1; 16])).unwrap();
            assert!(matches!(
                pump.decode(bytes, &expected()),
                Err(ClientError::Protocol)
            ));
        }
        let mut pump = Pump::new(v2::SessionId::from_bytes([1; 16])).unwrap();
        let raw=b"{\"version\":3,\"session\":\"01010101010101010101010101010101\",\"sequence\":0,\"type\":\"hello\",\"role\":\"helper\"}";
        let mut bytes = (raw.len() as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(raw);
        assert!(matches!(
            pump.decode(&bytes, &expected()),
            Err(ClientError::Protocol)
        ));
    }
    #[test]
    fn frame_fragmentation_preserves_correlation_without_splicing() {
        let mut pump = ready();
        let lease = v2::LeaseId::from_bytes([1; 16]);
        pump.submit(RequestId(123), v2::Request::QueryLease { lease })
            .unwrap();
        let frame = &pump.frames[0];
        let address = frame.bytes.as_ptr();
        let prefix = frame.bytes[..32].to_vec();
        assert_eq!(pump.frames[0].bytes.as_ptr(), address);
        assert_eq!(&pump.frames[0].bytes[..32], prefix);
        let reply = v2::Envelope::new(
            pump.session.clone(),
            2,
            v2::Message::Response {
                reply_to: 2,
                result: v2::Reply::Lease {
                    lease,
                    state: v2::LeaseView::Held {
                        intent: v2::IntentId::from_bytes([3; 16]),
                    },
                },
            },
        )
        .unwrap();
        let encoded = v2::encode(&reply).unwrap();
        let mut observed = None;
        for byte in encoded {
            let (n, event) = pump.decode(&[byte], &expected()).unwrap();
            assert_eq!(n, 1);
            if event.is_some() {
                observed = event;
            }
        }
        assert!(matches!(
            observed,
            Some(Event::Reply {
                request: RequestId(123),
                ..
            })
        ));
    }
}

#[cfg(test)]
mod write_tests {
    use super::*;
    #[test]
    fn pending_prefix_address_and_bytes_stay_fixed_across_cancel_admission() {
        let mut pump = tests::ready();
        let lease = v2::LeaseId::from_bytes([1; 16]);
        let operation = v2::OperationId::from_bytes([2; 16]);
        pump.submit(
            RequestId(20),
            v2::Request::QueryOperation { lease, operation },
        )
        .unwrap();
        let address = pump.frames[0].bytes.as_ptr() as usize;
        let original = pump.frames[0].bytes.clone();
        for iteration in 0..4 {
            assert_eq!(
                pump.frames[0]
                    .advance(|bytes| {
                        assert_eq!(bytes.as_ptr() as usize, address);
                        assert_eq!(bytes, original);
                        Err(std::io::Error::from(std::io::ErrorKind::WouldBlock))
                    })
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::WouldBlock
            );
            if iteration == 0 {
                pump.submit(RequestId(21), v2::Request::Cancel { lease, operation })
                    .unwrap();
            }
        }
        let mut received = Vec::new();
        while !pump.frames[0]
            .advance(|bytes| {
                let n = bytes.len().min(7);
                received.extend_from_slice(&bytes[..n]);
                Ok(n)
            })
            .unwrap()
        {}
        assert_eq!(received, original);
        pump.frames.pop_front();
        let frame = v2::Decoder::new()
            .feed(&pump.frames[0].bytes)
            .unwrap()
            .frame
            .unwrap();
        assert!(matches!(
            frame.message(),
            v2::Message::Request {
                request: v2::Request::Cancel { .. }
            }
        ));
    }
}

#[cfg(test)]
mod file_wire_tests {
    use super::*;
    #[test]
    fn literal_old_helper_unsupported_is_preserved_for_valid_lease_file_requests() {
        // Independently authored legacy-compatible bytes; no historical binary claim.
        for request in [
            v2::Request::CreateFile {
                lease: v2::LeaseId::from_bytes([1; 16]),
                operation: v2::OperationId::from_bytes([2; 16]),
                path: v2::LeafName::new("note").unwrap(),
                bytes: v2::FileBytes::new(&[1]).unwrap(),
            },
            v2::Request::ReadFile {
                lease: v2::LeaseId::from_bytes([1; 16]),
                path: v2::LeafName::new("note").unwrap(),
                offset: 0,
                length: 1,
            },
            v2::Request::InspectFile {
                lease: v2::LeaseId::from_bytes([1; 16]),
                path: v2::LeafName::new("note").unwrap(),
            },
        ] {
            let mut pump = tests::ready();
            pump.submit(
                RequestId(41),
                v2::Request::QueryLease {
                    lease: v2::LeaseId::from_bytes([1; 16]),
                },
            )
            .unwrap();
            pump.frames.clear();
            tests::inbound(
                &mut pump,
                2,
                v2::Message::Response {
                    reply_to: 2,
                    result: v2::Reply::Lease {
                        lease: v2::LeaseId::from_bytes([1; 16]),
                        state: v2::LeaseView::Held {
                            intent: v2::IntentId::from_bytes([8; 16]),
                        },
                    },
                },
            )
            .unwrap();
            pump.submit(RequestId(42), request).unwrap();
            let body = br#"{"version":2,"session":"0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a","sequence":3,"message":{"kind":"response","reply_to":3,"result":{"kind":"refused","code":"unsupported"}}}"#;
            let mut literal = (body.len() as u32).to_be_bytes().to_vec();
            literal.extend_from_slice(body);
            let (_, event) = pump.decode(&literal, &tests::expected()).unwrap();
            assert!(matches!(
                event,
                Some(Event::Reply {
                    request: RequestId(42),
                    reply: v2::Reply::Refused {
                        code: v2::RefusalCode::Unsupported,
                        ..
                    }
                })
            ));
        }
    }
}
