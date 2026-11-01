use std::{collections::VecDeque, io, time::Duration};

use mio::{unix::SourceFd, Events, Interest, Poll, Token};
use signal_hook_mio::v1_0::Signals;

#[cfg(feature = "event-stream")]
use crate::event::sys::Waker;
use crate::event::{
    source::EventSource, sys::unix::parse::parse_event, timeout::PollTimeout, Event, InternalEvent,
};
use crate::terminal::sys::file_descriptor::{tty_fd, FileDesc};

// Tokens to identify file descriptor
const TTY_TOKEN: Token = Token(0);
const SIGNAL_TOKEN: Token = Token(1);
#[cfg(feature = "event-stream")]
const WAKE_TOKEN: Token = Token(2);

// I (@zrzka) wasn't able to read more than 1_022 bytes when testing
// reading on macOS/Linux -> we don't need bigger buffer and 1k of bytes
// is enough.
const TTY_BUFFER_SIZE: usize = 1_024;

pub(crate) struct UnixInternalEventSource {
    poll: Poll,
    events: Events,
    // Edge-triggered readiness survives yielding one parsed or signal event.
    pending: VecDeque<Token>,
    parser: Parser,
    tty_buffer: [u8; TTY_BUFFER_SIZE],
    tty_fd: FileDesc<'static>,
    signals: Signals,
    #[cfg(feature = "event-stream")]
    waker: Waker,
}

impl UnixInternalEventSource {
    pub fn new() -> io::Result<Self> {
        UnixInternalEventSource::from_file_descriptor(tty_fd()?)
    }

    pub(crate) fn from_file_descriptor(input_fd: FileDesc<'static>) -> io::Result<Self> {
        let poll = Poll::new()?;
        let registry = poll.registry();

        let tty_raw_fd = input_fd.raw_fd();
        let mut tty_ev = SourceFd(&tty_raw_fd);
        registry.register(&mut tty_ev, TTY_TOKEN, Interest::READABLE)?;

        let mut signals = Signals::new([signal_hook::consts::SIGWINCH])?;
        registry.register(&mut signals, SIGNAL_TOKEN, Interest::READABLE)?;

        #[cfg(feature = "event-stream")]
        let waker = Waker::new(registry, WAKE_TOKEN)?;

        Ok(UnixInternalEventSource {
            poll,
            events: Events::with_capacity(3),
            pending: VecDeque::with_capacity(3),
            parser: Parser::default(),
            tty_buffer: [0u8; TTY_BUFFER_SIZE],
            tty_fd: input_fd,
            signals,
            #[cfg(feature = "event-stream")]
            waker,
        })
    }
}

impl EventSource for UnixInternalEventSource {
    fn try_read(&mut self, timeout: Option<Duration>) -> io::Result<Option<InternalEvent>> {
        if let Some(event) = self.parser.next() {
            return Ok(Some(event));
        }

        let timeout = PollTimeout::new(timeout);

        loop {
            // Pending work must not prevent newly-ready signals or wakeups from
            // joining the next bounded round. Never block while work is queued.
            let wait = if self.pending.is_empty() {
                timeout.leftover()
            } else {
                Some(Duration::ZERO)
            };
            if let Err(e) = self.poll.poll(&mut self.events, wait) {
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            // At most three registered sources. Deduplicate readiness and retain
            // every token even when an earlier source yields first.
            for token in self.events.iter().map(|event| event.token()) {
                if !self.pending.contains(&token) {
                    self.pending.push_back(token);
                }
            }
            if self.pending.is_empty() {
                return Ok(None);
            }

            // One read per source per round prevents partial input from starving
            // resize/wake notifications. A retained TTY rotates to the back.
            for _ in 0..self.pending.len() {
                let token = self.pending.pop_front().unwrap();
                match token {
                    TTY_TOKEN => {
                        // tty_fd may borrow blocking stdin. Do not change its
                        // shared file flags or perform a speculative blocking
                        // read after yielding an event from the previous chunk.
                        // Crossterm's serialized event source is the sole input
                        // reader. A competing external reader can invalidate any
                        // readiness observation; this is not a hard nonblock claim.
                        #[cfg(not(feature = "libc"))]
                        let fd = &self.tty_fd;
                        // SAFETY: self.tty_fd keeps this descriptor alive; the
                        // optional libc FileDesc has no AsFd implementation.
                        #[cfg(feature = "libc")]
                        let fd = unsafe {
                            rustix::fd::BorrowedFd::borrow_raw(self.tty_fd.raw_fd())
                        };
                        let available = rustix::io::ioctl_fionread(fd)?;
                        if available == 0 {
                            continue;
                        }
                        let length = available.min(TTY_BUFFER_SIZE as u64) as usize;
                        match self.tty_fd.read(&mut self.tty_buffer[..length]) {
                            Ok(0) => continue,
                            Ok(read_count) => {
                                self.pending.push_back(TTY_TOKEN);
                                self.parser.advance(
                                    &self.tty_buffer[..read_count],
                                    read_count == TTY_BUFFER_SIZE,
                                );
                            }
                            Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                            Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                                self.pending.push_back(TTY_TOKEN);
                                continue;
                            }
                            Err(e) => return Err(e),
                        }
                        if let Some(event) = self.parser.next() {
                            return Ok(Some(event));
                        }
                    }
                    SIGNAL_TOKEN => {
                        if self.signals.pending().next() == Some(signal_hook::consts::SIGWINCH) {
                            let new_size = crate::terminal::size()?;
                            return Ok(Some(InternalEvent::Event(Event::Resize(
                                new_size.0,
                                new_size.1,
                            ))));
                        }
                    }
                    #[cfg(feature = "event-stream")]
                    WAKE_TOKEN => {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "Poll operation was woken up by `Waker::wake`",
                        ));
                    }
                    _ => unreachable!("Synchronize Evented handle registration & token handling"),
                }
            }

            // Processing above can take some time, check if timeout expired
            if timeout.elapsed() {
                return Ok(None);
            }
        }
    }

    #[cfg(feature = "event-stream")]
    fn waker(&self) -> Waker {
        self.waker.clone()
    }
}

//
// Following `Parser` structure exists for two reasons:
//
//  * mimic anes Parser interface
//  * move the advancing, parsing, ... stuff out of the `try_read` method
//
#[derive(Debug)]
struct Parser {
    buffer: Vec<u8>,
    internal_events: VecDeque<InternalEvent>,
}

impl Default for Parser {
    fn default() -> Self {
        Parser {
            // This buffer is used for -> 1 <- ANSI escape sequence. Are we
            // aware of any ANSI escape sequence that is bigger? Can we make
            // it smaller?
            //
            // Probably not worth spending more time on this as "there's a plan"
            // to use the anes crate parser.
            buffer: Vec::with_capacity(256),
            // TTY_BUFFER_SIZE is 1_024 bytes. How many ANSI escape sequences can
            // fit? What is an average sequence length? Let's guess here
            // and say that the average ANSI escape sequence length is 8 bytes. Thus
            // the buffer size should be 1024/8=128 to avoid additional allocations
            // when processing large amounts of data.
            //
            // There's no need to make it bigger, because when you look at the `try_read`
            // method implementation, all events are consumed before the next TTY_BUFFER
            // is processed -> events pushed.
            internal_events: VecDeque::with_capacity(128),
        }
    }
}

impl Parser {
    fn advance(&mut self, buffer: &[u8], more: bool) {
        for (idx, byte) in buffer.iter().enumerate() {
            let more = idx + 1 < buffer.len() || more;

            self.buffer.push(*byte);

            match parse_event(&self.buffer, more) {
                Ok(Some(ie)) => {
                    self.internal_events.push_back(ie);
                    self.buffer.clear();
                }
                Ok(None) => {
                    // Event can't be parsed, because we don't have enough bytes for
                    // the current sequence. Keep the buffer and process next bytes.
                }
                Err(_) => {
                    // Event can't be parsed (not enough parameters, parameter is not a number, ...).
                    // Clear the buffer and continue with another sequence.
                    self.buffer.clear();
                }
            }
        }
    }
}

impl Iterator for Parser {
    type Item = InternalEvent;

    fn next(&mut self) -> Option<Self::Item> {
        self.internal_events.pop_front()
    }
}
