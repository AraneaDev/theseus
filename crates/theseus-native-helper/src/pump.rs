//! Pure bounded transport state. Native I/O remains in the runtime owner.
use crate::{
    RuntimeError,
    contract::{FinalBoundary, RunKey},
};
pub(crate) struct BoundaryTracker {
    pub(crate) key: RunKey,
    acknowledged: u32,
    boundary: Option<FinalBoundary>,
    pub(crate) retired: bool,
}
impl BoundaryTracker {
    pub(crate) fn new(key: RunKey) -> Self {
        Self {
            key,
            acknowledged: 0,
            boundary: None,
            retired: false,
        }
    }
    pub(crate) fn freeze(&mut self, b: FinalBoundary) -> Result<(), RuntimeError> {
        if b.key != self.key
            || b.final_ordinal > 16384
            || b.final_ordinal < self.acknowledged
            || self.boundary.is_some_and(|old| old != b)
        {
            return Err(RuntimeError::InvalidState);
        }
        self.boundary = Some(b);
        Ok(())
    }
    pub(crate) fn acknowledge(&mut self, key: RunKey, ordinal: u32) -> Result<(), RuntimeError> {
        if key != self.key
            || ordinal != self.acknowledged + 1
            || ordinal > 16384
            || self.boundary.is_some_and(|b| ordinal > b.final_ordinal)
        {
            return Err(RuntimeError::InvalidState);
        }
        self.acknowledged = ordinal;
        Ok(())
    }
    pub(crate) fn wire_ready(&self) -> bool {
        self.boundary
            .is_some_and(|b| b.final_ordinal == self.acknowledged)
    }
    pub(crate) fn drain_ack(&self) -> Option<(RunKey, u32)> {
        self.boundary
            .filter(|_| self.wire_ready() && !self.retired)
            .map(|b| (self.key, b.final_ordinal))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{FinalBoundary, RunKey};
    use theseus_native_protocol::v2;
    fn key() -> RunKey {
        RunKey {
            lease: v2::LeaseId::from_bytes([1; 16]),
            operation: v2::OperationId::from_bytes([2; 16]),
            generation: 1,
        }
    }
    fn boundary(n: u32) -> FinalBoundary {
        FinalBoundary {
            key: key(),
            final_ordinal: n,
            stdout_bytes: 9,
            stderr_bytes: 0,
            truncated: false,
        }
    }
    #[test]
    fn producer_boundary_does_not_ack_unseen_data() {
        let mut b = BoundaryTracker::new(key());
        b.freeze(boundary(2)).unwrap();
        assert!(!b.wire_ready());
        b.acknowledge(key(), 1).unwrap();
        assert!(!b.wire_ready());
        b.acknowledge(key(), 2).unwrap();
        assert!(b.wire_ready());
        assert!(b.acknowledge(key(), 2).is_err());
    }
    #[test]
    fn empty_producer_still_requires_explicit_boundary() {
        let mut b = BoundaryTracker::new(key());
        assert!(!b.wire_ready());
        b.freeze(boundary(0)).unwrap();
        assert!(b.wire_ready());
        assert!(b.freeze(boundary(1)).is_err());
    }
    #[test]
    fn stale_generation_and_out_of_order_ack_are_rejected() {
        let mut b = BoundaryTracker::new(key());
        assert!(b.acknowledge(key(), 2).is_err());
        let mut stale = key();
        stale.generation += 1;
        assert!(b.acknowledge(stale, 1).is_err());
    }
}
