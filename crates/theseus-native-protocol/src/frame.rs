use crate::framing::{CappedWriter, FramingError, RawDecoder, encode_frame};
use crate::{Envelope, ProtocolError};

pub const MAX_FRAME_BYTES: usize = 65_536;
pub const MAX_BODY_BYTES: usize = MAX_FRAME_BYTES - 4;

#[derive(Debug)]
pub struct DecodeProgress {
    pub consumed: usize,
    pub frame: Option<Envelope>,
}

/// Holds one bounded frame. Unconsumed coalesced input stays with the caller.
pub struct Decoder {
    raw: RawDecoder<MAX_BODY_BYTES, 0>,
}
impl Default for Decoder {
    fn default() -> Self {
        Self {
            raw: RawDecoder::new(),
        }
    }
}
impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn feed(&mut self, input: &[u8]) -> Result<DecodeProgress, ProtocolError> {
        let progress = self.raw.feed(input).map_err(map_framing_error)?;
        let frame = match progress.frame {
            Some(raw) => {
                let crate::framing::RawFrame { body, footer: [] } = raw;
                match crate::json::decode(&body) {
                    Ok(frame) => Some(frame),
                    Err(error) => {
                        self.raw.poison();
                        return Err(error);
                    }
                }
            }
            None => None,
        };
        Ok(DecodeProgress {
            consumed: progress.consumed,
            frame,
        })
    }
    pub fn finish(&mut self) -> Result<(), ProtocolError> {
        self.raw.finish().map_err(map_framing_error)
    }
}
fn map_framing_error(error: FramingError) -> ProtocolError {
    match error {
        FramingError::InvalidLength => ProtocolError::InvalidLength,
        FramingError::Truncated => ProtocolError::Truncated,
        FramingError::Poisoned => ProtocolError::Poisoned,
    }
}

pub fn encode(envelope: &Envelope) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = CappedWriter::<MAX_BODY_BYTES>::new();
    let result = crate::json::write_body(envelope, &mut writer);
    if writer.overflowed() {
        return Err(ProtocolError::InvalidLength);
    }
    result?;
    let body = writer.into_body().map_err(map_framing_error)?;
    encode_frame::<MAX_BODY_BYTES, 0>(&body, &[]).map_err(map_framing_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_frames_do_not_accumulate_body_storage() {
        let envelope = crate::Envelope {
            session: crate::SessionId::from_bytes([0; 16]),
            sequence: 0,
            message: crate::Message::Hello {
                role: crate::Role::Helper,
            },
        };
        let bytes = encode(&envelope).unwrap();
        let mut decoder = Decoder::new();
        for _ in 0..1000 {
            assert_eq!(decoder.feed(&bytes).unwrap().frame, Some(envelope.clone()));
        }
    }
}
