//! Bounded streaming projections; offsets count sanitized UTF-8 bytes.
use anyhow::{Result, ensure};
use std::fmt::{self, Write};
pub const DETAIL_BYTES: usize = 16 * 1024;
pub const SUMMARY_ROWS: usize = 64;
pub const MATCH_CHECKPOINTS: usize = 65_536;
#[derive(Debug, Clone)]
pub struct TextPage {
    pub text: String,
    pub offset: usize,
    pub next_offset: Option<usize>,
}
/// Format directly into a small window. Never retains the skipped prefix or suffix.
pub(crate) fn page(
    format: impl FnOnce(&mut dyn Write) -> fmt::Result,
    offset: usize,
    limit: usize,
) -> Result<TextPage> {
    ensure!(
        (4..=DETAIL_BYTES).contains(&limit),
        "text page limit must be 4..=16384"
    );
    let mut sink = Window {
        text: String::with_capacity(limit),
        offset,
        position: 0,
        limit,
        start: None,
        more: false,
    };
    let _ = format(&mut sink);
    ensure!(
        offset <= sink.position,
        "text page offset exceeds text length"
    );
    ensure!(
        sink.start.unwrap_or(sink.position) == offset,
        "text offset splits a Unicode scalar"
    );
    Ok(TextPage {
        next_offset: sink.more.then_some(offset + sink.text.len()),
        text: sink.text,
        offset,
    })
}
struct Window {
    text: String,
    offset: usize,
    position: usize,
    limit: usize,
    start: Option<usize>,
    more: bool,
}
impl Write for Window {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for raw in text.chars() {
            // Fixed tab projection is independent of width and chunk boundaries.
            let count = if raw == '\t' { 4 } else { 1 };
            let c = if raw == '\t' { ' ' } else { sanitized(raw) };
            for _ in 0..count {
                if self.position >= self.offset {
                    self.start.get_or_insert(self.position);
                    if self.text.len() + c.len_utf8() > self.limit {
                        self.more = true;
                        return Err(fmt::Error);
                    }
                    self.text.push(c);
                }
                self.position += c.len_utf8();
            }
        }
        Ok(())
    }
}
pub(crate) fn sanitized(c: char) -> char {
    if c.is_control() && c != '\n' && c != '\t' {
        '�'
    } else {
        c
    }
}
/// Compatibility projection keeps tabs, matching the original items() filter semantics.
pub(crate) struct SafeString(pub String);
impl Write for SafeString {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.0.extend(text.chars().map(sanitized));
        Ok(())
    }
}
