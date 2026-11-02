//! Bounded, same-thread observation for the fixed Theseus native test fixture.
//! This module is absent unless the non-default test-support feature is enabled.
use std::cell::RefCell;

pub const LIMIT: usize = 512;
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ParserState {
    pub length: usize,
    pub prefix: [u8; 8],
    pub surrogate: Option<u16>,
    pub failed: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyRecord {
    pub down: bool,
    pub repeat: u16,
    pub vkey: u16,
    pub scan: u16,
    pub unit: u16,
    pub control: u32,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    Record,
    Push,
    FinishEscape,
    FinishPoll,
    Native,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    None,
    Key,
    Paste,
    Other,
    Error,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Entry {
    pub phase: Phase,
    pub parser: ParserState,
    pub replay: bool,
    pub record_type: u16,
    pub key: Option<KeyRecord>,
    /// 0 = not a key, 1 = VT text, 2 = ignored, 3 = native.
    pub classification: u8,
    pub outcome: Outcome,
}
#[derive(Debug, Default)]
pub struct Capture {
    pub entries: Vec<Entry>,
    pub overflow: bool,
}
thread_local! { static TRACE: RefCell<Option<Capture>> = const { RefCell::new(None) }; }
/// Begin observation on this thread only. No I/O or callbacks are installed.
pub fn arm() {
    TRACE.with(|slot| *slot.borrow_mut() = Some(Capture::default()));
}
/// Stop observation and transfer its bounded records to the fixture.
pub fn take() -> Option<Capture> {
    TRACE.with(|slot| slot.borrow_mut().take())
}
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn record(entry: Entry) {
    TRACE.with(|slot| {
        if let Some(capture) = slot.borrow_mut().as_mut() {
            if capture.entries.len() < LIMIT {
                capture.entries.push(entry);
            } else {
                capture.overflow = true;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry() -> Entry {
        Entry {
            phase: Phase::Record,
            parser: ParserState::default(),
            replay: false,
            record_type: 1,
            key: None,
            classification: 0,
            outcome: Outcome::None,
        }
    }
    #[test]
    fn capture_requires_arm_and_take_disarms() {
        take();
        record(entry());
        assert!(take().is_none());
        arm();
        record(entry());
        assert_eq!(take().unwrap().entries, vec![entry()]);
        record(entry());
        assert!(take().is_none());
    }
    #[test]
    fn capture_keeps_first_512_and_reports_overflow() {
        arm();
        for _ in 0..LIMIT {
            record(entry());
        }
        let capture = take().unwrap();
        assert_eq!(capture.entries.len(), LIMIT);
        assert!(!capture.overflow);
        arm();
        for _ in 0..LIMIT + 7 {
            record(entry());
        }
        let capture = take().unwrap();
        assert_eq!(capture.entries.len(), LIMIT);
        assert!(capture.overflow);
    }
    #[test]
    fn capture_is_thread_local_and_preserves_replay_identity() {
        arm();
        let mut replay = entry();
        replay.replay = true;
        record(replay);
        std::thread::spawn(|| {
            record(entry());
            assert!(take().is_none());
            arm();
            record(entry());
            assert!(!take().unwrap().entries[0].replay);
        })
        .join()
        .unwrap();
        assert_eq!(take().unwrap().entries, vec![replay]);
    }
}
