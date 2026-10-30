use super::*;
use crate::tui::paging::{MATCH_CHECKPOINTS, SUMMARY_ROWS, sanitized};
use std::fmt::{self, Write};
/// Sparse match checkpoints. Counts every match, including those beyond the cache.
/// Rebuild when report/root/selection/filter changes; never rescan from render().
pub struct FilterIndex {
    needle: Vec<char>,
    contextual: bool,
    failure: Vec<usize>,
    checkpoints: Vec<usize>,
    count: usize,
    stride: usize,
}
impl FilterIndex {
    pub fn new(catalog: &Catalog<'_>, needle: &str) -> Self {
        let needle: Vec<_> = needle.to_lowercase().chars().collect();
        let mut failure = vec![0; needle.len()];
        for i in 1..needle.len() {
            let mut j = failure[i - 1];
            while j > 0 && needle[i] != needle[j] {
                j = failure[j - 1];
            }
            if needle[i] == needle[j] {
                j += 1;
            }
            failure[i] = j;
        }
        let mut index = Self {
            contextual: needle.iter().any(|c| matches!(c, 'σ' | 'ς')),
            needle,
            failure,
            checkpoints: Vec::new(),
            count: 0,
            stride: catalog.count().div_ceil(MATCH_CHECKPOINTS).max(1),
        };
        for selected in catalog.records() {
            if index.matches(selected.record) {
                if index.count.is_multiple_of(index.stride) {
                    index.checkpoints.push(selected.key);
                }
                index.count += 1;
            }
        }
        index
    }
    pub fn records<'a, 's>(
        &'s self,
        catalog: &'s Catalog<'a>,
    ) -> impl Iterator<Item = SelectedRecord<'a>> + 's
    where
        'a: 's,
    {
        catalog.records().filter(|r| self.matches(r.record))
    }
    pub fn len(&self) -> usize {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    pub fn checkpoint_count(&self) -> usize {
        self.checkpoints.len()
    }
    pub fn get<'a>(&self, catalog: &Catalog<'a>, position: usize) -> Option<SelectedRecord<'a>> {
        if position >= self.count {
            return None;
        }
        let checkpoint = position / self.stride;
        let rank = checkpoint * self.stride;
        let key = self.checkpoints[checkpoint];
        catalog
            .records()
            .skip(key)
            .filter(|r| self.matches(r.record))
            .nth(position - rank)
    }
    pub fn summaries(
        &self,
        catalog: &Catalog<'_>,
        start: usize,
        limit: usize,
        bytes: usize,
    ) -> Vec<Summary> {
        let Some(first) = self.get(catalog, start) else {
            return vec![];
        };
        catalog
            .records()
            .skip(first.key)
            .filter(|r| self.matches(r.record))
            .take(limit.min(SUMMARY_ROWS))
            .map(|r| Summary {
                key: r.key,
                text: r.record.summary(bytes),
            })
            .collect()
    }
    fn matches(&self, record: Record<'_>) -> bool {
        if self.needle.is_empty() || matches!(record, Record::TerminalError(_)) {
            return true;
        }
        let mut sink = Matcher {
            index: self,
            normal: State::default(),
            pending: None,
            previous_cased: false,
            cache: ClassCache::default(),
        };
        let _ = record.write(&mut sink);
        sink.pending.unwrap_or(sink.normal).found
    }
}
#[derive(Clone, Copy, Default)]
struct State {
    matched: usize,
    found: bool,
}
impl State {
    fn push(&mut self, c: char, index: &FilterIndex) {
        if self.found {
            return;
        }
        while self.matched > 0 && c != index.needle[self.matched] {
            self.matched = index.failure[self.matched - 1];
        }
        if c == index.needle[self.matched] {
            self.matched += 1;
        }
        self.found = self.matched == index.needle.len();
    }
}
#[derive(Clone, Copy, Default)]
struct Class {
    ignorable: bool,
    cased: bool,
}
struct ClassCache {
    entries: [(char, Class); 64],
    len: usize,
    next: usize,
}
impl Default for ClassCache {
    fn default() -> Self {
        Self {
            entries: [('\0', Class::default()); 64],
            len: 0,
            next: 0,
        }
    }
}
impl ClassCache {
    fn get(&mut self, c: char) -> Class {
        if c.is_ascii_alphabetic() {
            return Class {
                ignorable: false,
                cased: true,
            };
        }
        if c.is_ascii_digit() || c.is_ascii_whitespace() || "/_-".contains(c) {
            return Class::default();
        }
        if let Some((_, class)) = self.entries[..self.len].iter().find(|(key, _)| *key == c) {
            return *class;
        }
        // Ask the same standard-library Unicode tables used by str::to_lowercase.
        // c + Σ has final sigma iff c is cased and is not case-ignorable.
        // A + Σ + c + A has ordinary sigma iff c is cased or case-ignorable.
        // Both probes are <=8 input bytes, even for a four-byte scalar.
        let mut bytes = [0u8; 8];
        let n = c.encode_utf8(&mut bytes[..4]).len();
        bytes[n..n + 2].copy_from_slice("Σ".as_bytes());
        let cased = std::str::from_utf8(&bytes[..n + 2])
            .unwrap()
            .to_lowercase()
            .ends_with('ς');
        bytes[..3].copy_from_slice("AΣ".as_bytes());
        let n = c.encode_utf8(&mut bytes[3..7]).len();
        bytes[3 + n] = b'A';
        let bridge = std::str::from_utf8(&bytes[..4 + n])
            .unwrap()
            .to_lowercase()
            .chars()
            .nth(1)
            == Some('σ');
        let class = Class {
            cased,
            ignorable: bridge && !cased,
        };
        self.entries[self.next] = (c, class);
        self.next = (self.next + 1) % self.entries.len();
        self.len = (self.len + 1).min(self.entries.len());
        class
    }
}
struct Matcher<'a> {
    index: &'a FilterIndex,
    normal: State,
    pending: Option<State>,
    previous_cased: bool,
    cache: ClassCache,
}
impl Write for Matcher<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for c in text.chars().map(sanitized) {
            if !self.index.contextual {
                for lower in c.to_lowercase() {
                    self.normal.push(lower, self.index);
                }
                if self.normal.found {
                    return Err(fmt::Error);
                }
                continue;
            }
            let class = self.cache.get(c);
            if !class.ignorable
                && let Some(final_sigma) = self.pending.take()
            {
                if !class.cased {
                    self.normal = final_sigma;
                }
                if self.normal.found {
                    return Err(fmt::Error);
                }
            }
            if c == 'Σ' && self.previous_cased {
                let mut final_sigma = self.normal;
                self.normal.push('σ', self.index);
                final_sigma.push('ς', self.index);
                self.pending = Some(final_sigma);
            } else {
                for lower in c.to_lowercase() {
                    self.normal.push(lower, self.index);
                    if let Some(pending) = &mut self.pending {
                        pending.push(lower, self.index);
                    }
                }
            }
            if !class.ignorable {
                self.previous_cased = class.cased;
            }
            if self.normal.found && self.pending.is_none_or(|s| s.found) {
                return Err(fmt::Error);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::RunReport, tui::App};
    fn streamed(index: &FilterIndex, parts: &[&str]) -> bool {
        let mut sink = Matcher {
            index,
            normal: State::default(),
            pending: None,
            previous_cased: false,
            cache: ClassCache::default(),
        };
        for part in parts {
            if sink.write_str(part).is_err() {
                break;
            }
        }
        sink.pending.unwrap_or(sink.normal).found
    }
    #[test]
    fn contextual_lowercase_matches_std_across_fragments_and_at_eof() {
        let app = App::offline(RunReport::new("synthetic"), "/tmp/synthetic".into());
        let catalog = Catalog::for_app(&app);
        let long = "\u{301}".repeat(100_000);
        let cases: Vec<Vec<&str>> = vec![
            vec!["Ο", "Σ"],
            vec!["Ο", "Σ", "Α"],
            vec!["AΣ", "\u{345}", "B"],
            vec!["\u{345}", "Σ"],
            vec!["ʰ", "Σ"],
            vec!["ǅ", "Σ"],
            vec!["A", "Σ", &long],
            vec!["AΣ", &long, "B"],
            vec!["İΣ", "\u{200d}", "B"],
            vec!["AΣ", "\x1b", "B"],
        ];
        for needle in ["ος", "οσ", "aς", "aσ", "σ", "ς", "i\u{307}σ", "σ\u{345}b"] {
            let index = FilterIndex::new(&catalog, needle);
            for parts in &cases {
                let text = parts.concat();
                let oracle = crate::tui::safe_text(&text)
                    .to_lowercase()
                    .contains(&needle.to_lowercase());
                assert_eq!(
                    streamed(&index, parts),
                    oracle,
                    "needle={needle:?}, prefix={:?}",
                    &text[..text.floor_char_boundary(text.len().min(20))]
                );
            }
        }
    }
    #[test]
    fn unicode_sigma_classification_and_expansions_match_full_lowercase_oracle() {
        let app = App::offline(RunReport::new("synthetic"), "/tmp/synthetic".into());
        let catalog = Catalog::for_app(&app);
        let chars = [
            'A', 'Σ', 'σ', 'ς', 'λ', 'İ', 'ǅ', 'ʰ', '\u{345}', '\u{301}', '\u{200d}', '\'', ':',
            '.', '^', '`', ' ', '🐈',
        ];
        for needle in ["σ", "ς", "aσ", "aς", "i\u{307}σ"] {
            let index = FilterIndex::new(&catalog, needle);
            for a in chars {
                for b in chars {
                    for c in chars {
                        let a = a.to_string();
                        let b = b.to_string();
                        let c = c.to_string();
                        let parts = [a.as_str(), b.as_str(), c.as_str()];
                        assert_eq!(
                            streamed(&index, &parts),
                            parts
                                .concat()
                                .to_lowercase()
                                .contains(&needle.to_lowercase()),
                            "{parts:?} / {needle}"
                        );
                    }
                }
            }
        }
    }
}
