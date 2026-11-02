//! Bounded newline ingress and four generation-checked request slots.
use super::{budget::OutboundFrame, types::RpcId};
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FrameError {
    TooLarge,
    FrameReady,
}
#[derive(Default)]
pub(crate) struct FrameDecoder {
    bytes: Vec<u8>,
    ready: bool,
}
impl FrameDecoder {
    pub fn push(&mut self, input: &[u8]) -> Result<usize, FrameError> {
        if self.ready {
            return Err(FrameError::FrameReady);
        }
        let n = input
            .iter()
            .position(|b| *b == b'\n')
            .map_or(input.len(), |i| i + 1);
        let ready = n > 0 && input[n - 1] == b'\n';
        // Without a newline the final byte must remain available for termination.
        if n > 65_536 - self.bytes.len() || (!ready && self.bytes.len() + n == 65_536) {
            return Err(FrameError::TooLarge);
        }
        self.bytes.reserve_exact(n);
        self.bytes.extend_from_slice(&input[..n]);
        self.ready = ready;
        Ok(n)
    }
    pub fn take_frame(&mut self) -> Option<Vec<u8>> {
        if !self.ready {
            return None;
        }
        self.ready = false;
        Some(std::mem::take(&mut self.bytes))
    }
    pub fn eof(&mut self) {
        self.bytes.clear();
        self.ready = false;
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SlotId {
    pub index: u8,
    pub generation: u64,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SlotError {
    DuplicateId,
    Full,
    GenerationExhausted,
    StaleSlot,
    InvalidProgress,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CancelWrite {
    Absent,
    Removed(SlotId),
    ClosePartial(SlotId),
}
#[derive(Default)]
pub(crate) struct Slots {
    entries: [Option<Entry>; 4],
    generation: u64,
}
struct Entry {
    id: Option<RpcId>,
    generation: u64,
    frame: Option<OutboundFrame>,
}
impl Slots {
    pub fn reserve_anonymous(&mut self) -> Result<SlotId, SlotError> {
        self.reserve_protocol(None)
    }
    pub fn reserve(&mut self, id: RpcId) -> Result<SlotId, SlotError> {
        self.reserve_protocol(Some(id))
    }
    fn reserve_protocol(&mut self, id: Option<RpcId>) -> Result<SlotId, SlotError> {
        if id.is_some() && self.entries.iter().flatten().any(|e| e.id == id) {
            return Err(SlotError::DuplicateId);
        }
        let index = self
            .entries
            .iter()
            .position(Option::is_none)
            .ok_or(SlotError::Full)?;
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(SlotError::GenerationExhausted)?;
        self.generation = generation;
        self.entries[index] = Some(Entry {
            id,
            generation,
            frame: None,
        });
        Ok(SlotId {
            index: index as u8,
            generation,
        })
    }
    pub fn stage(&mut self, slot: SlotId, frame: OutboundFrame) -> Result<(), SlotError> {
        let e = self.entry_mut(slot)?;
        if e.frame.is_some()
            || e.id != frame.id
            || frame.offset != 0
            || frame.bytes.is_empty()
            || frame.bytes.len() > 65_536
            || frame.bytes.last() != Some(&b'\n')
        {
            return Err(SlotError::InvalidProgress);
        }
        e.frame = Some(frame);
        Ok(())
    }
    pub fn advance(&mut self, slot: SlotId, n: usize) -> Result<bool, SlotError> {
        let e = self.entry_mut(slot)?;
        let f = e.frame.as_mut().ok_or(SlotError::InvalidProgress)?;
        if n == 0 || n > f.bytes.len() - f.offset {
            return Err(SlotError::InvalidProgress);
        }
        f.offset += n;
        let done = f.offset == f.bytes.len();
        if done {
            self.entries[usize::from(slot.index)] = None;
        }
        Ok(done)
    }
    pub fn cancel(&mut self, id: &RpcId) -> CancelWrite {
        let Some(index) = self
            .entries
            .iter()
            .position(|e| e.as_ref().is_some_and(|e| e.id.as_ref() == Some(id)))
        else {
            return CancelWrite::Absent;
        };
        let e = self.entries[index].as_ref().expect("occupied entry");
        let slot = SlotId {
            index: index as u8,
            generation: e.generation,
        };
        if e.frame.as_ref().is_some_and(|f| f.offset > 0) {
            return CancelWrite::ClosePartial(slot);
        }
        self.entries[index] = None;
        CancelWrite::Removed(slot)
    }
    pub fn frame(&self, slot: SlotId) -> Result<Option<&OutboundFrame>, SlotError> {
        let e = self
            .entries
            .get(usize::from(slot.index))
            .and_then(Option::as_ref)
            .filter(|e| e.generation == slot.generation)
            .ok_or(SlotError::StaleSlot)?;
        Ok(e.frame.as_ref())
    }
    fn entry_mut(&mut self, slot: SlotId) -> Result<&mut Entry, SlotError> {
        self.entries
            .get_mut(usize::from(slot.index))
            .and_then(Option::as_mut)
            .filter(|e| e.generation == slot.generation)
            .ok_or(SlotError::StaleSlot)
    }
}
#[cfg(test)]
#[path = "framing_tests.rs"]
mod tests;
