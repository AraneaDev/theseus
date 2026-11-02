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
                if let Some(event) = self.vt.finish_poll(timeout, poll_timeout.elapsed())? {
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
                    // Inert scaffolding and releases cannot split a VT opener.
                    // A real native press/resize/mouse event retains its order.
                    if !matches!(key_kind, Some(VtKey::Text | VtKey::Ignore))
                        && !matches!(&record, InputRecord::KeyEvent(key) if !key.key_down)
                    {
                        if let Some(event) = self.vt.finish_escape()? {
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
                            if let Some(event) = self.vt.push(unit, more)? {
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

                    if let Some(event) = event {
                        return Ok(Some(InternalEvent::Event(event)));
                    }
                }
            }

            if poll_timeout.elapsed() {
                return if self.pending_record.is_none()
                    && self.console.number_of_console_input_events()? == 0
                {
                    self.vt.finish_poll(timeout, true)
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
