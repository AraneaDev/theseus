use super::*;
pub(super) struct Conversation {
    io: AuthenticatedStream,
    decoder: v2::Decoder,
    guard: Option<v2::SessionGuard>,
    session: Option<v2::SessionId>,
    next: u64,
    input: Vec<u8>,
    offset: usize,
    frames: VecDeque<Frame>,
    pub(super) permits: Vec<ControlPermit>,
    pub(super) stopping: bool,
}
impl Conversation {
    pub(super) fn new(io: AuthenticatedStream) -> Self {
        let (_, _, permits, _) = crate::contract::channels();
        Self {
            io,
            decoder: v2::Decoder::new(),
            guard: None,
            session: None,
            next: 0,
            input: Vec::with_capacity(4096),
            offset: 0,
            frames: VecDeque::with_capacity(2),
            permits: Vec::from(permits),
            stopping: false,
        }
    }
    pub(super) fn queue(&mut self, message: v2::Message) -> io::Result<()> {
        let permit = self.permits.pop().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "bounded broker control frames occupied",
            )
        })?;
        let envelope = v2::Envelope::new(
            self.session
                .clone()
                .ok_or_else(|| io::Error::other("broker hello missing"))?,
            self.next,
            message,
        )
        .map_err(io::Error::other)?;
        self.guard
            .as_mut()
            .ok_or_else(|| io::Error::other("conversation guard missing"))?
            .accept_outbound(&envelope)
            .map_err(io::Error::other)?;
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| io::Error::other("broker sequence exhausted"))?;
        self.frames.push_back(
            Frame::encode(&envelope, FrameOwner::Control(permit)).map_err(io::Error::other)?,
        );
        Ok(())
    }
    pub(super) fn flush(&mut self) -> io::Result<bool> {
        let Some(frame) = self.frames.front_mut() else {
            return Ok(false);
        };
        match self.io.try_write(frame.write_slice())? {
            WriteStep::Pending => Ok(false),
            WriteStep::Bytes(n) => {
                if frame.acknowledged(n).map_err(io::Error::other)? {
                    let frame = self.frames.pop_front().expect("original frame");
                    if let FrameOwner::Control(permit) = frame.owner {
                        self.permits.push(permit);
                    } else {
                        return Err(io::Error::other("broker control permit changed"));
                    }
                }
                Ok(true)
            }
        }
    }
    pub(super) fn request(&mut self) -> io::Result<Option<(u64, v2::Request)>> {
        if self.permits.is_empty() {
            return Ok(None);
        }
        if self.input.len() == self.offset {
            self.input.clear();
            self.offset = 0;
            let mut bytes = [0; 4096];
            match self.io.try_read(&mut bytes)? {
                ReadStep::Pending => return Ok(None),
                ReadStep::Eof => {
                    self.decoder.finish().map_err(io::Error::other)?;
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "original broker conversation ended",
                    ));
                }
                ReadStep::Bytes(n) => self.input.extend_from_slice(&bytes[..n]),
            }
        }
        let progress = self
            .decoder
            .feed(&self.input[self.offset..])
            .map_err(io::Error::other)?;
        self.offset += progress.consumed;
        let Some(frame) = progress.frame else {
            return Ok(None);
        };
        if self.guard.is_none() {
            self.session = Some(frame.session().clone());
            self.guard = Some(v2::SessionGuard::new(
                frame.session().clone(),
                v2::Role::Helper,
            ));
        }
        self.guard
            .as_mut()
            .expect("original guard")
            .accept_inbound(&frame)
            .map_err(io::Error::other)?;
        match frame.message() {
            v2::Message::Hello {
                role: v2::Role::Controller,
            } => {
                self.queue(v2::Message::Hello {
                    role: v2::Role::Helper,
                })?;
                Ok(None)
            }
            v2::Message::Request { request } => Ok(Some((frame.sequence(), request.clone()))),
            _ => Err(io::Error::other("unexpected broker inbound message")),
        }
    }
    pub(super) fn request_stop(&mut self) {
        self.stopping = true;
        self.io.request_stop();
    }
    pub(super) fn poll_settle(&mut self) -> io::Result<bool> {
        self.io.poll_settle()
    }
}
impl Drop for Conversation {
    fn drop(&mut self) {
        self.request_stop();
        if !matches!(self.poll_settle(), Ok(true)) {
            std::mem::forget(std::mem::take(&mut self.frames));
            std::mem::forget(std::mem::take(&mut self.input));
        }
    }
}
