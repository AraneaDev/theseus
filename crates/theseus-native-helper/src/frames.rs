//! Immutable storage for an admitted frame and its sole resource permit.
use crate::contract::{ControlPermit, DataChunk};
use theseus_native_protocol::v2;
pub(crate) enum FrameOwner {
    Control(ControlPermit),
    Data(DataChunk),
}
pub(crate) struct Frame {
    bytes: Box<[u8]>,
    offset: usize,
    // Pending native writes must be retried with precisely this slice.
    slice_len: usize,
    pub(crate) owner: FrameOwner,
}
impl Frame {
    pub(crate) fn encode(envelope: &v2::Envelope, owner: FrameOwner) -> Result<Self, v2::Error> {
        Ok(Self {
            bytes: v2::encode(envelope)?.into_boxed_slice(),
            offset: 0,
            slice_len: 0,
            owner,
        })
    }
    #[cfg(feature = "test-support")]
    pub(crate) fn encode_with_fixture(
        envelope: &v2::Envelope,
        owner: FrameOwner,
        control: Option<&crate::test_support::FixtureControl>,
    ) -> Result<Self, v2::Error> {
        let mut frame = Self::encode(envelope, owner)?;
        if control.is_some_and(crate::test_support::FixtureControl::is_output_pressure)
            && matches!(envelope.message(), v2::Message::Output { .. })
        {
            let mut bytes = frame.bytes.into_vec();
            bytes.resize(v2::MAX_FRAME_BYTES, b' ');
            bytes[..4].copy_from_slice(&(v2::MAX_BODY_BYTES as u32).to_be_bytes());
            frame.bytes = bytes.into_boxed_slice();
        }
        Ok(frame)
    }
    pub(crate) fn write_slice(&mut self) -> &[u8] {
        if self.slice_len == 0 {
            self.slice_len = (self.bytes.len() - self.offset).min(4096)
        }
        &self.bytes[self.offset..self.offset + self.slice_len]
    }
    pub(crate) fn acknowledged(&mut self, n: usize) -> Result<bool, crate::RuntimeError> {
        if n == 0 || n > self.slice_len {
            return Err(crate::RuntimeError::InvalidState);
        }
        self.offset += n;
        self.slice_len = 0;
        Ok(self.offset == self.bytes.len())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_slice_has_stable_address_length_and_content() {
        let (_, _, [permit, _], _) = crate::contract::channels();
        let session = v2::SessionId::from_bytes([1; 16]);
        let envelope = v2::Envelope::new(
            session,
            0,
            v2::Message::Hello {
                role: v2::Role::Helper,
            },
        )
        .unwrap();
        let mut f = Frame::encode(&envelope, FrameOwner::Control(permit)).unwrap();
        let first = f.write_slice();
        let pointer = first.as_ptr();
        let bytes = first.to_vec();
        for _ in 0..4 {
            let next = f.write_slice();
            assert_eq!(pointer, next.as_ptr());
            assert_eq!(bytes, next)
        }
        assert!(!f.acknowledged(1).unwrap());
        assert_eq!(f.write_slice(), &bytes[1..]);
    }
}

#[cfg(all(test, feature = "test-support"))]
mod pressure_tests {
    use super::*;
    use crate::{
        contract::RunKey,
        test_support::{FixtureControl, FixtureMode},
    };
    #[test]
    fn pressure_only_output_has_maximum_valid_frame_and_stable_pending_storage() {
        let (_, _, [control, _], [p0, p1, p2, _]) = crate::contract::channels();
        let envelope = v2::Envelope::new(
            v2::SessionId::from_bytes([1; 16]),
            5,
            v2::Message::Output {
                lease: v2::LeaseId::from_bytes([2; 16]),
                operation: v2::OperationId::from_bytes([3; 16]),
                channel: v2::Channel::Stdout,
                offset: 0,
                bytes: v2::FileBytes::new(&[42; 512]).unwrap(),
            },
        )
        .unwrap();
        let chunk = |permit| {
            FrameOwner::Data(crate::contract::DataChunk {
                key: RunKey {
                    lease: v2::LeaseId::from_bytes([2; 16]),
                    operation: v2::OperationId::from_bytes([3; 16]),
                    generation: 1,
                },
                ordinal: 1,
                channel: v2::Channel::Stdout,
                offset: 0,
                len: 512,
                permit,
            })
        };
        let ordinary = Frame::encode(&envelope, chunk(p0)).unwrap();
        let observe = Frame::encode_with_fixture(
            &envelope,
            chunk(p1),
            Some(&FixtureControl::new(FixtureMode::Observe)),
        )
        .unwrap();
        assert_eq!(ordinary.bytes, observe.bytes);
        assert!(ordinary.bytes.len() < 4096);
        let mut pressure = Frame::encode_with_fixture(
            &envelope,
            chunk(p2),
            Some(&FixtureControl::new(FixtureMode::OutputPressure)),
        )
        .unwrap();
        assert_eq!(pressure.bytes.len(), 65536);
        assert_eq!(&pressure.bytes[..4], &65532u32.to_be_bytes());
        let mut decoder = v2::Decoder::new();
        let decoded = decoder.feed(&pressure.bytes).unwrap();
        assert_eq!(decoded.consumed, 65536);
        assert_eq!(decoded.frame, Some(envelope));
        decoder.finish().unwrap();
        let first = pressure.write_slice();
        let pointer = first.as_ptr();
        let frozen = first.to_vec();
        for _ in 0..3 {
            let retry = pressure.write_slice();
            assert_eq!(retry.as_ptr(), pointer);
            assert_eq!(retry, frozen);
        }
        assert!(!pressure.acknowledged(1).unwrap());
        assert_eq!(pressure.write_slice().as_ptr(), pointer.wrapping_add(1));
        let hello = v2::Envelope::new(
            v2::SessionId::from_bytes([1; 16]),
            0,
            v2::Message::Hello {
                role: v2::Role::Helper,
            },
        )
        .unwrap();
        let compact = Frame::encode_with_fixture(
            &hello,
            FrameOwner::Control(control),
            Some(&FixtureControl::new(FixtureMode::OutputPressure)),
        )
        .unwrap();
        assert_eq!(&*compact.bytes, v2::encode(&hello).unwrap());
    }
}
