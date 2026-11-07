use super::{Envelope, Error};
use crate::framing::{CappedWriter, FramingError, RawDecoder, encode_frame};

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
    pub fn feed(&mut self, input: &[u8]) -> Result<DecodeProgress, Error> {
        let progress = self.raw.feed(input).map_err(map_framing_error)?;
        let frame = match progress.frame {
            Some(raw) => {
                let crate::framing::RawFrame { body, footer: [] } = raw;
                match super::json::decode(&body) {
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
    pub fn finish(&mut self) -> Result<(), Error> {
        self.raw.finish().map_err(map_framing_error)
    }
}
fn map_framing_error(error: FramingError) -> Error {
    match error {
        FramingError::InvalidLength => Error::InvalidLength,
        FramingError::Truncated => Error::Truncated,
        FramingError::Poisoned => Error::Poisoned,
    }
}

pub fn encode(envelope: &Envelope) -> Result<Vec<u8>, Error> {
    let mut writer = CappedWriter::<MAX_BODY_BYTES>::new();
    let result = super::json::write_body(envelope, &mut writer);
    if writer.overflowed() {
        return Err(Error::InvalidLength);
    }
    result?;
    let body = writer.into_body().map_err(map_framing_error)?;
    encode_frame::<MAX_BODY_BYTES, 0>(&body, &[]).map_err(map_framing_error)
}
