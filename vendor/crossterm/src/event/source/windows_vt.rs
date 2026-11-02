//! Decode Windows VT character records without losing paste boundaries.
#[cfg(unix)]
use crate::event::sys::unix::parse::parse_event;
#[cfg(windows)]
use crate::event::sys::windows::vt::parse_event;
use crate::event::InternalEvent;
use std::io;

// Above Theseus' 1 MiB draft cap, including both paste delimiters.
const MAX_SEQUENCE_BYTES: usize = 2 * 1024 * 1024;
#[derive(Default)]
pub(super) struct VtInput {
    buffer: Vec<u8>,
    surrogate: Option<u16>,
    failed: bool,
}
impl VtInput {
    pub(super) fn check(&mut self) -> io::Result<()> {
        if self.failed {
            Err(self.reject())
        } else {
            Ok(())
        }
    }
    pub(super) fn pending(&self) -> bool {
        !self.buffer.is_empty() || self.surrogate.is_some()
    }
    pub(super) fn reject(&mut self) -> io::Error {
        self.failed = true;
        self.buffer.clear();
        self.surrogate = None;
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid or oversized Windows VT input",
        )
    }
    pub(super) fn push(&mut self, unit: u16, more: bool) -> io::Result<Option<InternalEvent>> {
        if self.failed {
            return Err(self.reject());
        }
        let ch = if let Some(high) = self.surrogate.take() {
            match char::decode_utf16([high, unit]).next().unwrap() {
                Ok(ch) => ch,
                Err(_) => return Err(self.reject()),
            }
        } else if (0xd800..=0xdbff).contains(&unit) {
            self.surrogate = Some(unit);
            return Ok(None);
        } else {
            match char::from_u32(u32::from(unit)) {
                Some(ch) => ch,
                None => return Err(self.reject()),
            }
        };
        let mut utf8 = [0; 4];
        let bytes = ch.encode_utf8(&mut utf8).as_bytes();
        if self.buffer.len() + bytes.len() > MAX_SEQUENCE_BYTES {
            return Err(self.reject());
        }
        self.buffer.extend_from_slice(bytes);
        self.parse(more)
    }
    // Only an actual empty console queue resolves a lone Escape. Partial CSI
    // and paste remain buffered across arbitrary poll deadlines.
    pub(super) fn finish_escape(&mut self) -> io::Result<Option<InternalEvent>> {
        if self.failed {
            return Err(self.reject());
        }
        if self.buffer == b"\x1b" && self.surrogate.is_none() {
            self.parse(false)
        } else {
            Ok(None)
        }
    }
    fn parse(&mut self, more: bool) -> io::Result<Option<InternalEvent>> {
        let event = match parse_event(&self.buffer, more) {
            Ok(event) => event,
            Err(_) => return Err(self.reject()),
        };
        if event.is_some() {
            self.buffer.clear();
        }
        #[cfg(windows)]
        return Ok(match event {
            Some(crate::event::sys::windows::VtEvent::Event(event)) => {
                Some(InternalEvent::Event(event))
            }
            // Console APIs own these queries on Windows. Consume complete
            // reports without growing crossterm's internal skipped-event queue.
            Some(crate::event::sys::windows::VtEvent::CursorPosition(column, row)) => {
                let _ = (column, row);
                None
            }
            Some(crate::event::sys::windows::VtEvent::KeyboardEnhancementFlags(flags)) => {
                let _ = flags;
                None
            }
            Some(crate::event::sys::windows::VtEvent::PrimaryDeviceAttributes) | None => None,
        });
        #[cfg(unix)]
        Ok(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    fn feed(parser: &mut VtInput, text: &str, more: bool) -> io::Result<Vec<InternalEvent>> {
        let units: Vec<_> = text.encode_utf16().collect();
        let mut events = Vec::new();
        for (i, unit) in units.iter().copied().enumerate() {
            if let Some(event) = parser.push(unit, more || i + 1 < units.len())? {
                events.push(event);
            }
        }
        Ok(events)
    }
    #[test]
    fn paste_keeps_commands_controls_and_surrogates_literal_across_every_split() {
        let text = "\x1b[200~r\nq\tñ😀\x1b[A\x1b[201~";
        let units: Vec<_> = text.encode_utf16().collect();
        for split in 1..units.len() {
            let mut parser = VtInput::default();
            let mut events = Vec::new();
            for (i, unit) in units.iter().copied().enumerate() {
                // A complete ConPTY delimiter is queued atomically; a read may
                // yield between any records, including the surrogate halves.
                if let Some(event) = parser.push(unit, i + 1 < units.len()).unwrap() {
                    events.push(event);
                }
                if i + 1 == split && i + 1 < units.len() {
                    assert!(events.is_empty(), "partial paste dispatched input");
                }
            }
            assert_eq!(
                events,
                vec![InternalEvent::Event(Event::Paste("r\nq\tñ😀\x1b[A".into()))]
            );
            assert!(!parser.pending());
        }
    }
    #[test]
    fn controls_arrow_and_standalone_escape_survive_raw_records() {
        let mut parser = VtInput::default();
        assert_eq!(
            feed(&mut parser, "\x1b[A", false).unwrap(),
            vec![InternalEvent::Event(Event::Key(KeyCode::Up.into()))]
        );
        assert_eq!(
            feed(&mut parser, "\x03", false).unwrap(),
            vec![InternalEvent::Event(Event::Key(KeyEvent::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL
            )))]
        );
        assert_eq!(
            feed(&mut parser, "\x1b", false).unwrap(),
            vec![InternalEvent::Event(Event::Key(KeyCode::Esc.into()))]
        );
        assert!(feed(&mut parser, "\x1b", true).unwrap().is_empty());
        assert_eq!(
            parser.finish_escape().unwrap(),
            Some(InternalEvent::Event(Event::Key(KeyCode::Esc.into())))
        );
    }
    #[test]
    fn incomplete_paste_is_never_flushed_as_keys() {
        let mut parser = VtInput::default();
        assert!(feed(&mut parser, "\x1b[200~rq", false).unwrap().is_empty());
        assert!(parser.finish_escape().unwrap().is_none());
        assert_eq!(
            feed(&mut parser, "\x1b[201~q", false).unwrap(),
            vec![
                InternalEvent::Event(Event::Paste("rq".into())),
                InternalEvent::Event(Event::Key(KeyCode::Char('q').into()))
            ]
        );
    }
    #[test]
    fn malformed_coordinates_and_button_offsets_error_and_keep_command_keys_poisoned() {
        // Independent protocol literals exercise each one-based conversion,
        // including the byte-offset X10 format and both button-offset formats.
        for text in [
            "\x1b[0;1R",
            "\x1b[1;0R",
            "\x1b[0;0R",
            "\x1b[<0;0;1M",
            "\x1b[<0;1;0m",
            "\x1b[<0;0;0M",
            "\x1b[32;0;1M",
            "\x1b[32;1;0M",
            "\x1b[31;1;1M",
            "\x1b[M  !",
            "\x1b[M ! ",
            "\x1b[M \x1f!",
            "\x1b[M !\x1f",
            "\x1b[M\x1f!!",
        ] {
            let mut parser = VtInput::default();
            let error = feed(&mut parser, text, false).expect_err(text);
            assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{text:?}");
            for key in [b'r', b'q'] {
                assert_eq!(
                    parser.push(u16::from(key), false).unwrap_err().kind(),
                    io::ErrorKind::InvalidData,
                    "{text:?}"
                );
            }
            assert!(parser.finish_escape().is_err());
        }
    }
    #[test]
    fn invalid_utf16_and_oversize_input_poison_reader_without_releasing_keys() {
        for first in [0xd800, 0xdc00] {
            let mut parser = VtInput::default();
            let initial = parser.push(first, true);
            assert!(initial.is_err() || parser.push(b'q' as u16, false).is_err());
            assert!(parser.push(b'r' as u16, false).is_err());
        }
        let mut parser = VtInput::default();
        feed(&mut parser, "\x1b[200~", true).unwrap();
        for _ in parser.buffer.len()..MAX_SEQUENCE_BYTES {
            assert!(parser.push(b'x' as u16, true).unwrap().is_none());
        }
        assert!(parser.push(b'r' as u16, false).is_err());
        assert!(parser.push(b'q' as u16, false).is_err());
        assert!(parser.check().is_err());
    }
}
