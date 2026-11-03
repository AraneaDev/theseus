//! Private bounded length-prefix mechanics shared by protocol codecs.
use std::io::{self, Write};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FramingError {
    InvalidLength,
    Truncated,
    Poisoned,
}

#[derive(Debug)]
pub(crate) struct RawFrame<const FOOTER: usize> {
    pub(crate) body: Vec<u8>,
    pub(crate) footer: [u8; FOOTER],
}

#[derive(Debug)]
pub(crate) struct RawProgress<const FOOTER: usize> {
    pub(crate) consumed: usize,
    pub(crate) frame: Option<RawFrame<FOOTER>>,
}

/// Retains at most one validated body and a fixed footer; never queues input.
pub(crate) struct RawDecoder<const MAX_BODY: usize, const FOOTER: usize> {
    header: [u8; 4],
    header_used: usize,
    expected: Option<usize>,
    body: Vec<u8>,
    footer: [u8; FOOTER],
    footer_used: usize,
    poisoned: bool,
}

impl<const MAX_BODY: usize, const FOOTER: usize> RawDecoder<MAX_BODY, FOOTER> {
    pub(crate) fn new() -> Self {
        Self {
            header: [0; 4],
            header_used: 0,
            expected: None,
            body: Vec::new(),
            footer: [0; FOOTER],
            footer_used: 0,
            poisoned: false,
        }
    }

    pub(crate) fn feed(&mut self, input: &[u8]) -> Result<RawProgress<FOOTER>, FramingError> {
        if self.poisoned {
            return Err(FramingError::Poisoned);
        }
        let mut consumed = 0;
        if self.expected.is_none() {
            let count = (4 - self.header_used).min(input.len());
            self.header[self.header_used..self.header_used + count]
                .copy_from_slice(&input[..count]);
            self.header_used += count;
            consumed += count;
            if self.header_used < 4 {
                return Ok(RawProgress {
                    consumed,
                    frame: None,
                });
            }
            let length = u32::from_be_bytes(self.header) as usize;
            if length == 0 || length > MAX_BODY {
                self.poison();
                return Err(FramingError::InvalidLength);
            }
            self.expected = Some(length);
            self.body = Vec::with_capacity(length);
        }
        let expected = self.expected.expect("validated body length");
        let count = (expected - self.body.len()).min(input.len() - consumed);
        self.body
            .extend_from_slice(&input[consumed..consumed + count]);
        consumed += count;
        if self.body.len() < expected {
            return Ok(RawProgress {
                consumed,
                frame: None,
            });
        }
        let count = (FOOTER - self.footer_used).min(input.len() - consumed);
        self.footer[self.footer_used..self.footer_used + count]
            .copy_from_slice(&input[consumed..consumed + count]);
        self.footer_used += count;
        consumed += count;
        if self.footer_used < FOOTER {
            return Ok(RawProgress {
                consumed,
                frame: None,
            });
        }
        let frame = RawFrame {
            body: std::mem::take(&mut self.body),
            footer: self.footer,
        };
        self.header = [0; 4];
        self.header_used = 0;
        self.expected = None;
        self.footer = [0; FOOTER];
        self.footer_used = 0;
        Ok(RawProgress {
            consumed,
            frame: Some(frame),
        })
    }

    /// Typed codec failures poison this same owner after raw extraction.
    pub(crate) fn poison(&mut self) {
        self.poisoned = true;
        self.header = [0; 4];
        self.header_used = 0;
        self.expected = None;
        self.body = Vec::new();
        self.footer = [0; FOOTER];
        self.footer_used = 0;
    }

    pub(crate) fn finish(&mut self) -> Result<(), FramingError> {
        if self.poisoned {
            return Err(FramingError::Poisoned);
        }
        if self.header_used != 0 || self.expected.is_some() {
            self.poison();
            return Err(FramingError::Truncated);
        }
        Ok(())
    }
}

pub(crate) struct CappedWriter<const MAX_BODY: usize> {
    bytes: Vec<u8>,
    overflow: bool,
}
impl<const MAX_BODY: usize> CappedWriter<MAX_BODY> {
    pub(crate) fn new() -> Self {
        // Reserving the fixed cap prevents Vec's geometric growth exceeding it.
        Self {
            bytes: Vec::with_capacity(MAX_BODY),
            overflow: false,
        }
    }
    pub(crate) fn overflowed(&self) -> bool {
        self.overflow
    }
    pub(crate) fn into_body(self) -> Result<Vec<u8>, FramingError> {
        if self.overflow || self.bytes.is_empty() {
            Err(FramingError::InvalidLength)
        } else {
            Ok(self.bytes)
        }
    }
}
impl<const MAX_BODY: usize> Write for CappedWriter<MAX_BODY> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_BODY - self.bytes.len() {
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

pub(crate) fn encode_frame<const MAX_BODY: usize, const FOOTER: usize>(
    body: &[u8],
    footer: &[u8; FOOTER],
) -> Result<Vec<u8>, FramingError> {
    if body.is_empty() || body.len() > MAX_BODY {
        return Err(FramingError::InvalidLength);
    }
    let length = u32::try_from(body.len()).map_err(|_| FramingError::InvalidLength)?;
    let total = body
        .len()
        .checked_add(4)
        .and_then(|n| n.checked_add(FOOTER))
        .ok_or(FramingError::InvalidLength)?;
    let mut frame = Vec::with_capacity(total);
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(body);
    frame.extend_from_slice(footer);
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn capped_writer_refuses_growth_after_exact_boundary() {
        let mut writer = CappedWriter::<{ crate::MAX_BODY_BYTES }>::new();
        writer
            .write_all(&vec![b'x'; crate::MAX_BODY_BYTES])
            .unwrap();
        assert!(writer.write_all(b"x").is_err());
        assert!(writer.overflowed());
        assert_eq!(writer.bytes.len(), crate::MAX_BODY_BYTES);
        assert_eq!(writer.into_body(), Err(FramingError::InvalidLength));
    }

    // Dropping footer consumption, buffering the next frame, or resetting early breaks this.
    #[test]
    fn footer_and_coalesced_remainder_follow_every_split() {
        let literal = [0, 0, 0, 3, b'a', b'b', b'c', 11, 12];
        for split in 0..=literal.len() {
            let mut decoder = RawDecoder::<3, 2>::new();
            let first = decoder.feed(&literal[..split]).unwrap();
            assert_eq!(first.consumed, split);
            let rest = [&literal[split..], &literal[..]].concat();
            let second = decoder.feed(&rest).unwrap();
            let frame = if let Some(frame) = first.frame {
                assert_eq!(second.consumed, literal.len());
                frame
            } else {
                assert_eq!(second.consumed, literal.len() - split);
                second.frame.unwrap()
            };
            assert_eq!(frame.body, b"abc");
            assert_eq!(frame.footer, [11, 12]);
            decoder.finish().unwrap();
        }
    }

    #[test]
    fn partial_header_body_and_footer_eof_poison_and_release_storage() {
        let literal = [0, 0, 0, 3, b'a', b'b', b'c', 11, 12];
        for prefix in 1..literal.len() {
            let mut decoder = RawDecoder::<3, 2>::new();
            assert!(decoder.feed(&literal[..prefix]).unwrap().frame.is_none());
            assert_eq!(decoder.finish(), Err(FramingError::Truncated));
            assert_eq!(decoder.feed(&literal).unwrap_err(), FramingError::Poisoned);
            assert_eq!(decoder.finish(), Err(FramingError::Poisoned));
            assert_eq!(decoder.body.capacity(), 0);
        }
    }

    #[test]
    fn invalid_header_rejected_before_body_allocation() {
        for length in [0u32, 4, u32::MAX] {
            let mut decoder = RawDecoder::<3, 32>::new();
            assert_eq!(
                decoder.feed(&length.to_be_bytes()).unwrap_err(),
                FramingError::InvalidLength
            );
            assert_eq!(decoder.body.capacity(), 0);
            assert_eq!(decoder.finish(), Err(FramingError::Poisoned));
        }
    }

    #[test]
    fn explicit_typed_failure_poison_is_terminal() {
        let mut decoder = RawDecoder::<3, 0>::new();
        assert_eq!(decoder.feed(&[]).unwrap().consumed, 0);
        decoder.finish().unwrap();
        decoder.feed(&[0, 0, 0, 1, b'x']).unwrap();
        decoder.poison();
        assert_eq!(decoder.finish(), Err(FramingError::Poisoned));
        assert_eq!(decoder.feed(&[]).unwrap_err(), FramingError::Poisoned);
    }

    #[test]
    fn repeated_frames_do_not_accumulate_body_storage() {
        let mut decoder = RawDecoder::<3, 0>::new();
        for _ in 0..1000 {
            assert_eq!(
                decoder
                    .feed(&[0, 0, 0, 3, 1, 2, 3])
                    .unwrap()
                    .frame
                    .unwrap()
                    .body,
                [1, 2, 3]
            );
            assert!(decoder.body.capacity() <= 3);
        }
    }

    #[test]
    fn capped_writer_and_frame_encoding_obey_exact_bounds() {
        let mut writer = CappedWriter::<3>::new();
        writer.write_all(b"abc").unwrap();
        assert!(writer.write_all(b"d").is_err());
        assert!(writer.overflowed());
        assert_eq!(writer.bytes.len(), 3);
        assert_eq!(writer.into_body(), Err(FramingError::InvalidLength));
        assert_eq!(
            encode_frame::<3, 2>(b"abc", &[11, 12]).unwrap(),
            [0, 0, 0, 3, b'a', b'b', b'c', 11, 12]
        );
        assert_eq!(
            encode_frame::<3, 0>(b"", &[]),
            Err(FramingError::InvalidLength)
        );
        assert_eq!(
            encode_frame::<3, 0>(b"abcd", &[]),
            Err(FramingError::InvalidLength)
        );
        assert_eq!(
            CappedWriter::<3>::new().into_body(),
            Err(FramingError::InvalidLength)
        );
    }
}
