//! Decode Windows VT character records without losing paste boundaries.
#[cfg(unix)]
use crate::event::sys::unix::parse::parse_event;
#[cfg(windows)]
use crate::event::sys::windows::vt::parse_event;
use crate::event::{InternalEvent, KeyModifiers};
use std::io;
use std::time::Duration;

// Above Theseus' 1 MiB draft cap, including both paste delimiters.
const MAX_SEQUENCE_BYTES: usize = 2 * 1024 * 1024;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum VtKey {
    Text,
    Ignore,
    Native,
}
#[derive(Default)]
pub(super) struct VtInput {
    buffer: Vec<u8>,
    surrogate: Option<u16>,
    failed: bool,
}
impl VtInput {
    // Win32 virtual-key values, also used by sys/windows/parse.rs. ConHost
    // leaves modifier/Alt-code scaffolding among its raw VT character records.
    pub(super) fn key_kind(
        &self,
        down: bool,
        vkey: u16,
        unit: u16,
        modifiers: KeyModifiers,
    ) -> VtKey {
        if vkey == 0 {
            return if down { VtKey::Text } else { VtKey::Ignore };
        }
        if self.pending() {
            const VK_SHIFT: u16 = 0x10;
            const VK_CONTROL: u16 = 0x11;
            const VK_MENU: u16 = 0x12;
            const VK_NUMPAD0: u16 = 0x60;
            const VK_NUMPAD9: u16 = 0x69;
            // Alt release is the Unicode-bearing record for an Alt code,
            // including each surrogate half. It is not a duplicate key-up.
            if !down && vkey == VK_MENU && unit != 0 {
                return VtKey::Text;
            }
            let modifier = matches!(vkey, VK_SHIFT | VK_CONTROL | VK_MENU);
            let alt_numpad = (VK_NUMPAD0..=VK_NUMPAD9).contains(&vkey)
                && modifiers.contains(KeyModifiers::ALT)
                && !modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL);
            if unit == 0 && (modifier || alt_numpad) {
                return VtKey::Ignore;
            }
        }
        VtKey::Native
    }
    pub(super) fn finish_poll(
        &mut self,
        timeout: Option<Duration>,
        elapsed: bool,
    ) -> io::Result<Option<InternalEvent>> {
        // Zero-timeout drains never resolve an ambiguous ESC. Positive polls
        // use only their caller's deadline; blocking read keeps upstream's
        // immediate queue-empty behavior because it has no deadline.
        if matches!(timeout, Some(duration) if duration.is_zero() || !elapsed) {
            return Ok(None);
        }
        self.finish_escape()
    }
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
        if self.buffer == b"\x1b" {
            return Ok(None);
        }
        self.parse(more)
    }
    // Explicit caller deadline or a following native event resolves Escape.
    // Partial CSI and paste remain buffered across arbitrary poll deadlines.
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
                // Input may stop after any record, including ESC or a surrogate.
                if let Some(event) = parser
                    .push(unit, i + 1 != split && i + 1 < units.len())
                    .unwrap()
                {
                    events.push(event);
                }
                if i + 1 == split && i + 1 < units.len() {
                    assert!(events.is_empty(), "partial paste dispatched input");
                    assert!(parser
                        .finish_poll(Some(Duration::ZERO), true)
                        .unwrap()
                        .is_none());
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
    fn empty_queue_does_not_finish_fragmented_escape() {
        let mut parser = VtInput::default();
        assert!(parser.push(27, false).unwrap().is_none());
        assert!(parser.pending());
        assert_eq!(
            feed(&mut parser, "[200~r\nq ñ😀\x1b[201~", false).unwrap(),
            vec![InternalEvent::Event(Event::Paste("r\nq ñ😀".into()))]
        );
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
        assert_eq!(feed(&mut parser, "\x1b", false).unwrap(), vec![]);
        assert_eq!(
            parser.finish_escape().unwrap(),
            Some(InternalEvent::Event(Event::Key(KeyCode::Esc.into())))
        );
        assert!(feed(&mut parser, "\x1b", true).unwrap().is_empty());
        assert_eq!(
            parser.finish_escape().unwrap(),
            Some(InternalEvent::Event(Event::Key(KeyCode::Esc.into())))
        );
    }
    #[test]
    fn escape_poll_boundaries_keep_zero_drains_and_partial_sequences_safe() {
        let positive = Some(Duration::from_millis(20));
        let mut parser = VtInput::default();
        assert!(parser.push(27, false).unwrap().is_none());
        assert!(parser
            .finish_poll(Some(Duration::ZERO), true)
            .unwrap()
            .is_none());
        assert!(parser.finish_poll(positive, false).unwrap().is_none());
        assert_eq!(
            parser.finish_poll(positive, true).unwrap(),
            Some(InternalEvent::Event(Event::Key(KeyCode::Esc.into())))
        );
        assert!(parser.push(27, false).unwrap().is_none());
        assert_eq!(
            parser.finish_poll(None, false).unwrap(),
            Some(InternalEvent::Event(Event::Key(KeyCode::Esc.into())))
        );
        for text in ["\x1b[", "\x1b[200~r\nq"] {
            assert!(feed(&mut parser, text, false).unwrap().is_empty());
            assert!(parser.finish_poll(positive, true).unwrap().is_none());
            assert!(parser.pending());
            parser = VtInput::default();
        }
    }
    #[test]
    fn actual_conhost_modifier_and_alt_release_records_preserve_unicode_paste() {
        // Literal full key fields from run 41440785765: down, repeat, vkey,
        // scan, UTF-16 unit, control. Native duplicate releases stay native.
        let records = [
            (true, 1, 0, 0, 27, 0),
            (true, 1, 0, 0, 91, 0),
            (true, 1, 0, 0, 50, 0),
            (true, 1, 0, 0, 48, 0),
            (true, 1, 0, 0, 48, 0),
            (true, 1, 0, 0, 126, 0),
            (true, 1, 0, 0, 114, 0),
            (false, 1, 82, 19, 114, 0),
            (true, 1, 17, 29, 0, 8),
            (true, 1, 0, 0, 10, 0),
            (false, 1, 13, 28, 10, 8),
            (false, 1, 17, 29, 0, 0),
            (true, 1, 0, 0, 113, 0),
            (false, 1, 81, 16, 113, 0),
            (true, 1, 0, 0, 32, 0),
            (false, 1, 32, 57, 32, 0),
            (true, 1, 0, 0, 241, 0),
            (false, 1, 0, 0, 241, 0),
            (true, 1, 18, 56, 0, 2),
            (true, 1, 102, 77, 0, 2),
            (false, 1, 102, 77, 0, 2),
            (true, 1, 99, 81, 0, 2),
            (false, 1, 99, 81, 0, 2),
            (false, 1, 18, 56, 55357, 0),
            (true, 1, 18, 56, 0, 2),
            (true, 1, 102, 77, 0, 2),
            (false, 1, 102, 77, 0, 2),
            (true, 1, 99, 81, 0, 2),
            (false, 1, 99, 81, 0, 2),
            (false, 1, 18, 56, 56832, 0),
            (true, 1, 0, 0, 27, 0),
            (true, 1, 0, 0, 91, 0),
            (true, 1, 0, 0, 50, 0),
            (true, 1, 0, 0, 48, 0),
            (true, 1, 0, 0, 49, 0),
            (true, 1, 0, 0, 126, 0),
            (true, 1, 16, 42, 0, 16),
            (true, 1, 0, 0, 33, 0),
        ];
        let mut parser = VtInput::default();
        let mut events = Vec::new();
        for (down, repeat, vkey, _scan, unit, control) in records {
            let mut modifiers = KeyModifiers::empty();
            if control & 3 != 0 {
                modifiers |= KeyModifiers::ALT;
            }
            if control & 12 != 0 {
                modifiers |= KeyModifiers::CONTROL;
            }
            if control & 16 != 0 {
                modifiers |= KeyModifiers::SHIFT;
            }
            match parser.key_kind(down, vkey, unit, modifiers) {
                VtKey::Text => {
                    for _ in 0..repeat {
                        if let Some(event) = parser.push(unit, false).unwrap() {
                            events.push(event);
                        }
                    }
                }
                VtKey::Ignore => assert!(unit == 0 || !down),
                VtKey::Native => {
                    // Shift for the final ! is outside paste; the native
                    // decoder ignores it. All other native records are releases.
                    assert!(!down || (vkey == 16 && unit == 0));
                }
            }
        }
        assert_eq!(
            events,
            vec![
                InternalEvent::Event(Event::Paste("r\nq ñ😀".into())),
                InternalEvent::Event(Event::Key(KeyCode::Char('!').into())),
            ]
        );
        assert!(!parser.pending());
    }
    #[test]
    fn ordinary_native_keys_and_ambiguous_nul_are_not_ignored_as_scaffolding() {
        let mut parser = VtInput::default();
        feed(&mut parser, "\x1b[200~", false).unwrap();
        for (vkey, unit, modifiers) in [
            (38, 0, KeyModifiers::empty()),
            (32, 0, KeyModifiers::CONTROL),
            (102, 0, KeyModifiers::CONTROL | KeyModifiers::ALT),
            (102, 0, KeyModifiers::SHIFT | KeyModifiers::ALT),
            (82, 114, KeyModifiers::empty()),
        ] {
            assert_eq!(parser.key_kind(true, vkey, unit, modifiers), VtKey::Native);
        }
        assert_eq!(
            VtInput::default().key_kind(false, 18, 241, KeyModifiers::empty()),
            VtKey::Native
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
