//! Counting byte-window sink for lossless compact record serialization.
use std::io::{self, Write};
pub(crate) struct RangeWriter {
    offset: u64,
    limit: usize,
    total: u64,
    bytes: Vec<u8>,
}
impl RangeWriter {
    pub fn new(offset: u64, limit: usize) -> Self {
        Self {
            offset,
            limit,
            total: 0,
            bytes: Vec::new(),
        }
    }
    pub fn total_bytes(&self) -> u64 {
        self.total
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
impl Write for RangeWriter {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        let end = self
            .total
            .checked_add(u64::try_from(input.len()).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "record byte count overflow")
            })?)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "record byte count overflow")
            })?;
        let skip = self
            .offset
            .saturating_sub(self.total)
            .min(input.len() as u64) as usize;
        let take = (input.len() - skip).min(self.limit - self.bytes.len());
        self.bytes.reserve_exact(take);
        self.bytes.extend_from_slice(&input[skip..skip + take]);
        self.total = end;
        Ok(input.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[cfg(test)]
#[path = "range_writer_tests.rs"]
mod tests;
