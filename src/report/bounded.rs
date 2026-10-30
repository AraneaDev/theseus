//! Streaming JSON boundaries shared by private persistence and staged CLI output.
use serde::Serialize;
use std::io::{self, Write};

pub(super) struct LimitedWriter<W> {
    inner: W,
    remaining: u64,
}
impl<W: Write> LimitedWriter<W> {
    pub fn new(inner: W, limit: u64) -> Self {
        Self {
            inner,
            remaining: limit,
        }
    }
}
impl<W: Write> Write for LimitedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.remaining {
            return Err(io::Error::other("JSON exceeds size limit"));
        }
        let written = self.inner.write(bytes)?;
        self.remaining -= written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

struct JsonBuffer {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for JsonBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let needed = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .filter(|n| *n <= self.limit)
            .ok_or_else(|| io::Error::other("JSON exceeds size limit"))?;
        if needed > self.bytes.capacity() {
            let capacity = needed
                .max(self.bytes.capacity().saturating_mul(2))
                .min(self.limit);
            self.bytes
                .try_reserve_exact(capacity - self.bytes.len())
                .map_err(io::Error::other)?;
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
/// Stage a complete bounded JSON value so errors cannot emit a partial document.
pub(crate) fn json_bytes(value: &impl Serialize, limit: usize) -> anyhow::Result<Vec<u8>> {
    let mut buffer = JsonBuffer {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer_pretty(&mut buffer, value)?;
    Ok(buffer.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn staged_json_counts_escaping_and_returns_only_complete_documents() {
        let text = "\u{0001}".repeat(128);
        assert!(json_bytes(&text, 256).is_err());
        let bytes = json_bytes(&text, 1024).unwrap();
        assert_eq!(serde_json::from_slice::<String>(&bytes).unwrap(), text);
        assert!(bytes.len() <= 1024);
    }
}
