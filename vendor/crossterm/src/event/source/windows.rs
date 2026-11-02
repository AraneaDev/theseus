#[cfg(feature = "theseus-test-support")]
use crate::event::windows_test_trace as trace;
use std::time::Duration;

use super::windows_vt::{VtInput, VtKey};

use crossterm_winapi::{Console, Handle, InputRecord};

use crate::event::{
    sys::windows::{parse::MouseButtonsPressed, poll::WinApiPoll},
    Event, KeyModifiers,
};

#[cfg(feature = "event-stream")]
use crate::event::sys::Waker;
use crate::event::{
    source::EventSource,
    sys::windows::parse::{handle_key_event, handle_mouse_event},
    timeout::PollTimeout,
    InternalEvent,
};

pub(crate) struct WindowsEventSource {
    console: Console,
    poll: WinApiPoll,
    surrogate_buffer: Option<u16>,
    mouse_buttons_pressed: MouseButtonsPressed,
    vt: VtInput,
    pending_record: Option<InputRecord>,
}

impl WindowsEventSource {
    pub(crate) fn new() -> std::io::Result<WindowsEventSource> {
        let console = Console::from(Handle::current_in_handle()?);
        Ok(WindowsEventSource {
            console,

            #[cfg(not(feature = "event-stream"))]
            poll: WinApiPoll::new(),
            #[cfg(feature = "event-stream")]
            poll: WinApiPoll::new()?,

            surrogate_buffer: None,
            mouse_buttons_pressed: MouseButtonsPressed::default(),
            vt: VtInput::default(),
            pending_record: None,
        })
    }
}

#[cfg(feature = "theseus-test-support")]
impl WindowsEventSource {
    fn trace_result(&self, phase: trace::Phase, result: &std::io::Result<Option<InternalEvent>>) {
        let outcome = match result {
            Err(_) => trace::Outcome::Error,
            Ok(None) => trace::Outcome::None,
            Ok(Some(InternalEvent::Event(Event::Key(_)))) => trace::Outcome::Key,
            #[cfg(feature = "bracketed-paste")]
            Ok(Some(InternalEvent::Event(Event::Paste(_)))) => trace::Outcome::Paste,
            _ => trace::Outcome::Other,
        };
        trace::record(trace::Entry {
            phase,
            parser: self.vt.trace_state(),
            replay: self.pending_record.is_some(),
            record_type: 0,
            key: None,
            classification: 0,
            outcome,
        });
    }
    fn trace_record(&self, record: &InputRecord, replay: bool, kind: Option<VtKey>) {
        let (record_type, key) = match record {
            InputRecord::KeyEvent(key) => (
                1,
                Some(trace::KeyRecord {
                    down: key.key_down,
                    repeat: key.repeat_count,
                    vkey: key.virtual_key_code,
                    scan: key.virtual_scan_code,
                    unit: key.u_char,
                    // has_state exposes each original bit without layout assumptions.
                    control: (0..32).fold(0, |bits, bit| {
                        bits | if key.control_key_state.has_state(1 << bit) {
                            1 << bit
                        } else {
                            0
                        }
                    }),
                }),
            ),
            InputRecord::MouseEvent(_) => (2, None),
            InputRecord::WindowBufferSizeEvent(_) => (4, None),
            InputRecord::MenuEvent(_) => (8, None),
            InputRecord::FocusEvent(_) => (16, None),
        };
        trace::record(trace::Entry {
            phase: trace::Phase::Record,
            parser: self.vt.trace_state(),
            replay,
            record_type,
            key,
            classification: match kind {
                None => 0,
                Some(VtKey::Text) => 1,
                Some(VtKey::Ignore) => 2,
                Some(VtKey::Native) => 3,
            },
            outcome: trace::Outcome::None,
        });
    }
}

impl EventSource for WindowsEventSource {
    fn try_read(&mut self, timeout: Option<Duration>) -> std::io::Result<Option<InternalEvent>> {
        self.vt.check()?;
        let poll_timeout = PollTimeout::new(timeout);

        loop {
            // A split ConPTY write may temporarily exhaust this queue after
            // ESC. Only the caller's positive deadline may resolve it; a
            // zero-timeout drain must leave it buffered for the next poll.
            if self.pending_record.is_none() && self.console.number_of_console_input_events()? == 0
            {
                let parsed = self.vt.finish_poll(timeout, poll_timeout.elapsed());
                #[cfg(feature = "theseus-test-support")]
                self.trace_result(trace::Phase::FinishPoll, &parsed);
                if let Some(event) = parsed? {
                    return Ok(Some(event));
                }
            }
            let ready = if self.pending_record.is_some() {
                Some(true)
            } else {
                self.poll.poll(poll_timeout.leftover())?
            };
            if let Some(event_ready) = ready {
                let number = self.console.number_of_console_input_events()?;
                if event_ready && (number != 0 || self.pending_record.is_some()) {
                    #[cfg(feature = "theseus-test-support")]
                    let replay = self.pending_record.is_some();
                    let record = match self.pending_record.take() {
                        Some(record) => record,
                        None => self.console.read_single_input_event()?,
                    };
                    let key_kind = match &record {
                        InputRecord::KeyEvent(key) => Some(self.vt.key_kind(
                            key.key_down,
                            key.virtual_key_code,
                            key.u_char,
                            KeyModifiers::from(&key.control_key_state),
                        )),
                        _ => None,
                    };
                    #[cfg(feature = "theseus-test-support")]
                    self.trace_record(&record, replay, key_kind);
                    // Inert scaffolding and releases cannot split a VT opener.
                    // A real native press/resize/mouse event retains its order.
                    if !matches!(key_kind, Some(VtKey::Text | VtKey::Ignore))
                        && !matches!(&record, InputRecord::KeyEvent(key) if !key.key_down)
                    {
                        let parsed = self.vt.finish_escape();
                        #[cfg(feature = "theseus-test-support")]
                        self.trace_result(trace::Phase::FinishEscape, &parsed);
                        if let Some(event) = parsed? {
                            self.pending_record = Some(record);
                            return Ok(Some(event));
                        }
                    }
                    let event = match record {
                        InputRecord::KeyEvent(mut record) if key_kind == Some(VtKey::Text) => {
                            let unit = record.u_char;
                            if record.repeat_count > 1 {
                                record.repeat_count -= 1;
                                self.pending_record = Some(InputRecord::KeyEvent(record));
                            }
                            let more = self.pending_record.is_some()
                                || self.console.number_of_console_input_events()? != 0;
                            let parsed = self.vt.push(unit, more);
                            #[cfg(feature = "theseus-test-support")]
                            self.trace_result(trace::Phase::Push, &parsed);
                            if let Some(event) = parsed? {
                                return Ok(Some(event));
                            }
                            None
                        }
                        InputRecord::KeyEvent(_) if key_kind == Some(VtKey::Ignore) => None,
                        InputRecord::KeyEvent(record) => {
                            // Preserve the legacy native decoder, but never let
                            // a non-VT key escape an unfinished paste/sequence.
                            if record.key_down && self.vt.pending() {
                                return Err(self.vt.reject());
                            }
                            handle_key_event(record, &mut self.surrogate_buffer)
                        }
                        InputRecord::MouseEvent(record) => {
                            let mouse_event =
                                handle_mouse_event(record, &self.mouse_buttons_pressed);
                            self.mouse_buttons_pressed = MouseButtonsPressed {
                                left: record.button_state.left_button(),
                                right: record.button_state.right_button(),
                                middle: record.button_state.middle_button(),
                            };

                            mouse_event
                        }
                        InputRecord::WindowBufferSizeEvent(record) => {
                            // windows starts counting at 0, unix at 1, add one to replicate unix behaviour.
                            Some(Event::Resize(
                                record.size.x as u16 + 1,
                                record.size.y as u16 + 1,
                            ))
                        }
                        InputRecord::FocusEvent(record) => {
                            let event = if record.set_focus {
                                Event::FocusGained
                            } else {
                                Event::FocusLost
                            };
                            Some(event)
                        }
                        _ => None,
                    };

                    #[cfg(feature = "theseus-test-support")]
                    self.trace_result(
                        trace::Phase::Native,
                        &Ok(event.as_ref().cloned().map(InternalEvent::Event)),
                    );
                    if let Some(event) = event {
                        return Ok(Some(InternalEvent::Event(event)));
                    }
                }
            }

            if poll_timeout.elapsed() {
                return if self.pending_record.is_none()
                    && self.console.number_of_console_input_events()? == 0
                {
                    let parsed = self.vt.finish_poll(timeout, true);
                    #[cfg(feature = "theseus-test-support")]
                    self.trace_result(trace::Phase::FinishPoll, &parsed);
                    parsed
                } else {
                    Ok(None)
                };
            }
        }
    }

    #[cfg(feature = "event-stream")]
    fn waker(&self) -> Waker {
        self.poll.waker()
    }
}
