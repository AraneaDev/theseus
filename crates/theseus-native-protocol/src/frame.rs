use crate::{Envelope, ProtocolError};
use std::io::{self, Write};

pub const MAX_FRAME_BYTES: usize = 65_536;
pub const MAX_BODY_BYTES: usize = MAX_FRAME_BYTES - 4;

#[derive(Debug)]
pub struct DecodeProgress {
    pub consumed: usize,
    pub frame: Option<Envelope>,
}

/// Holds one bounded frame. Unconsumed coalesced input stays with the caller.
#[derive(Default)]
pub struct Decoder {
    header: [u8; 4],
    header_used: usize,
    expected: Option<usize>,
    body: Vec<u8>,
    poisoned: bool,
}
impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, input: &[u8]) -> Result<DecodeProgress, ProtocolError> {
        if self.poisoned {
            return Err(ProtocolError::Poisoned);
        }
        let mut consumed = 0;
        if self.expected.is_none() {
            let count = (4 - self.header_used).min(input.len());
            self.header[self.header_used..self.header_used + count]
                .copy_from_slice(&input[..count]);
            self.header_used += count;
            consumed += count;
            if self.header_used < 4 {
                return Ok(DecodeProgress {
                    consumed,
                    frame: None,
                });
            }
            let length = u32::from_be_bytes(self.header) as usize;
            if length == 0 || length > MAX_BODY_BYTES {
                return self.fail(ProtocolError::InvalidLength);
            }
            self.expected = Some(length);
            self.body = Vec::with_capacity(length);
        }
        let expected = self.expected.expect("validated header has a body length");
        let count = (expected - self.body.len()).min(input.len() - consumed);
        self.body
            .extend_from_slice(&input[consumed..consumed + count]);
        consumed += count;
        if self.body.len() < expected {
            return Ok(DecodeProgress {
                consumed,
                frame: None,
            });
        }
        let body = std::mem::take(&mut self.body);
        self.expected = None;
        self.header_used = 0;
        match crate::json::decode(&body) {
            Ok(frame) => Ok(DecodeProgress {
                consumed,
                frame: Some(frame),
            }),
            Err(error) => self.fail(error),
        }
    }

    pub fn finish(&mut self) -> Result<(), ProtocolError> {
        if self.poisoned {
            return Err(ProtocolError::Poisoned);
        }
        if self.header_used != 0 || self.expected.is_some() {
            return self.fail(ProtocolError::Truncated);
        }
        Ok(())
    }
    fn fail<T>(&mut self, error: ProtocolError) -> Result<T, ProtocolError> {
        self.poisoned = true;
        self.header = [0; 4];
        self.header_used = 0;
        self.expected = None;
        self.body = Vec::new();
        Err(error)
    }
}

struct CappedWriter {
    bytes: Vec<u8>,
    overflow: bool,
}
impl CappedWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            overflow: false,
        }
    }
}
impl Write for CappedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_BODY_BYTES - self.bytes.len() {
            self.overflow = true;
            return Err(io::Error::other("protocol body limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn encode(envelope: &Envelope) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = CappedWriter::new();
    let result = crate::json::write_body(envelope, &mut writer);
    if writer.overflow {
        return Err(ProtocolError::InvalidLength);
    }
    result?;
    if writer.bytes.is_empty() {
        return Err(ProtocolError::InvalidLength);
    }
    let length = u32::try_from(writer.bytes.len()).map_err(|_| ProtocolError::InvalidLength)?;
    let mut frame = Vec::with_capacity(writer.bytes.len() + 4);
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&writer.bytes);
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capped_writer_refuses_growth_after_exact_boundary() {
        let mut writer = CappedWriter::new();
        writer.write_all(&vec![b'x'; MAX_BODY_BYTES]).unwrap();
        assert!(writer.write_all(b"x").is_err());
        assert!(writer.overflow);
        assert_eq!(writer.bytes.len(), MAX_BODY_BYTES);
    }
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
            assert!(decoder.body.capacity() <= MAX_BODY_BYTES);
        }
    }
}
