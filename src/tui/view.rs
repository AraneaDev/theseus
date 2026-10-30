//! Owned presentation cache. Catalogs borrow App only while a viewport is built.
use super::{
    App, Tab,
    evidence::{Catalog, FilterIndex, Summary},
    paging::{DETAIL_BYTES, SUMMARY_ROWS},
};
use crate::{
    model::LogReference,
    report::{self, LogFingerprint},
};
use std::{collections::VecDeque, os::unix::fs::MetadataExt, path::PathBuf};
const LOG_BYTES: usize = 4096;
#[derive(PartialEq, Eq)]
struct Context {
    revision: u64,
    root: PathBuf,
    scenario: usize,
    stage: usize,
    tab: Tab,
    cell: Option<usize>,
    filter: String,
    error: Option<(usize, usize)>,
}
impl Context {
    fn matches(&self, app: &App) -> bool {
        self.revision == app.view_revision
            && self.root == app.root
            && self.scenario == app.scenario_index
            && self.stage == app.stage_index
            && self.tab == app.tab
            && self.cell == app.comparison.as_ref().map(|v| v.row)
            && self.filter == app.filter
            && self.error
                == app
                    .terminal_error
                    .as_ref()
                    .map(|s| (s.as_ptr() as usize, s.len()))
    }
    fn new(app: &App) -> Self {
        Self {
            revision: app.view_revision,
            root: app.root.clone(),
            scenario: app.scenario_index,
            stage: app.stage_index,
            tab: app.tab,
            cell: app.comparison.as_ref().map(|v| v.row),
            filter: app.filter.clone(),
            error: app
                .terminal_error
                .as_ref()
                .map(|s| (s.as_ptr() as usize, s.len())),
        }
    }
}
/// Text offsets address sanitized UTF-8; raw offsets address stored bytes.
#[derive(Clone, Copy)]
enum Position {
    Text(usize),
    Raw(u64, Option<u64>),
}
impl Default for Position {
    fn default() -> Self {
        Self::Text(0)
    }
}
#[derive(Default)]
pub(super) struct View {
    context: Option<Context>,
    index: Option<FilterIndex>,
    width: u16,
    selected: Option<usize>,
    pub start: usize,
    pub rows: Vec<Summary>,
    pub body: String,
    pub label: String,
    position: Position,
    next: Option<Position>,
    previous: VecDeque<Position>,
    log: Option<(LogReference, LogFingerprint)>,
    dirty: bool,
    log_metadata: bool,
    pub scroll: u16,
    last_scroll: u16,
}
impl View {
    pub fn raw_log(&self) -> bool {
        !self.log_metadata
    }
    pub fn toggle_log_metadata(&mut self, app: &App) -> bool {
        self.update(app, None);
        let catalog = Catalog::for_app(app);
        if self
            .index
            .as_ref()
            .unwrap()
            .get(&catalog, app.evidence_index)
            .and_then(|s| s.record.log_reference())
            .is_none()
        {
            return false;
        }
        self.log_metadata = !self.log_metadata;
        self.reset_page(app.scroll);
        self.update(app, None);
        true
    }
    pub fn restart_detail(&mut self) {
        self.selected = None;
    }
    pub fn force_refresh(&mut self) {
        self.dirty = true;
    }
    pub fn len(&mut self, app: &App) -> usize {
        self.ensure_index(app);
        self.index.as_ref().unwrap().len()
    }
    fn reset_page(&mut self, scroll: u16) {
        self.scroll = 0;
        self.last_scroll = scroll;
        self.position = Position::default();
        self.next = None;
        self.previous.clear();
        self.log = None;
        self.dirty = true;
    }
    fn ensure_index(&mut self, app: &App) {
        if self
            .context
            .as_ref()
            .is_none_or(|context| !context.matches(app))
        {
            let context = Context::new(app);
            self.index = Some(FilterIndex::new(&Catalog::for_app(app), &app.filter));
            self.context = Some(context);
            self.log_metadata = false;
            self.selected = None;
            self.reset_page(app.scroll);
            self.rows.clear();
        }
    }
    pub fn update(&mut self, app: &App, width: Option<u16>) {
        self.scroll = if app.scroll >= self.last_scroll {
            self.scroll.saturating_add(app.scroll - self.last_scroll)
        } else {
            self.scroll.saturating_sub(self.last_scroll - app.scroll)
        };
        self.last_scroll = app.scroll;
        self.ensure_index(app);
        if let Some(width) = width
            && self.width != width
        {
            // The first draw establishes width; input may already have
            // selected a chunk. Only a subsequent resize invalidates it.
            if self.width != 0 {
                self.reset_page(app.scroll);
                self.rows.clear();
            }
            self.width = width;
        }
        let index = self.index.as_ref().unwrap();
        let position = app.evidence_index.min(index.len().saturating_sub(1));
        if self.selected != Some(position) {
            self.selected = Some(position);
            self.log_metadata = false;
            self.reset_page(app.scroll);
            self.rows.clear();
        }
        if let Some((reference, fingerprint)) = &self.log {
            let current = std::fs::symlink_metadata(app.root.join(&reference.path)).ok();
            if current.as_ref().is_none_or(|m| {
                !m.is_file()
                    || fingerprint.device != m.dev()
                    || fingerprint.inode != m.ino()
                    || fingerprint.size != m.size()
                    || fingerprint.mtime_seconds != m.mtime()
                    || fingerprint.mtime_nanos != m.mtime_nsec()
                    || fingerprint.ctime_seconds != m.ctime()
                    || fingerprint.ctime_nanos != m.ctime_nsec()
            }) {
                self.reset_page(app.scroll);
            }
        }
        if !self.dirty {
            return;
        }
        let catalog = Catalog::for_app(app);
        let index = self.index.as_ref().unwrap();
        if self.rows.is_empty() {
            self.start = position
                .saturating_sub(SUMMARY_ROWS / 2)
                .min(index.len().saturating_sub(SUMMARY_ROWS));
            self.rows = index.summaries(&catalog, self.start, SUMMARY_ROWS, 512);
        }
        let Some(selected) = index.get(&catalog, position) else {
            self.body = "No evidence for this stage/filter.".into();
            self.label = "0 records".into();
            self.dirty = false;
            return;
        };
        let result = if let Some(reference) = selected.record.log_reference()
            && !self.log_metadata
        {
            log_body(
                &app.root,
                reference,
                match self.position {
                    Position::Raw(offset, _) => offset,
                    _ => 0,
                },
                match self.position {
                    Position::Raw(_, stop) => stop,
                    _ => None,
                },
            )
            .map(|page| {
                self.position = Position::Raw(
                    page.start,
                    match self.position {
                        Position::Raw(_, stop) => stop,
                        _ => None,
                    },
                );
                self.next = page.next.map(|next| Position::Raw(next, None));
                self.log = Some((reference.clone(), page.fingerprint));
                self.label = format!("{} | m metadata", page.label);
                self.body = page.text;
            })
        } else {
            selected
                .record
                .page(
                    match self.position {
                        Position::Text(offset) => offset,
                        _ => 0,
                    },
                    DETAIL_BYTES,
                )
                .map(|page| {
                    self.position = Position::Text(page.offset);
                    self.next = page.next_offset.map(Position::Text);
                    self.label = format!(
                        "text bytes {}..{} | {}",
                        page.offset,
                        page.offset + page.text.len(),
                        if page.next_offset.is_some() {
                            "more stored text; ] next"
                        } else {
                            "last text chunk"
                        }
                    );
                    if self.log_metadata {
                        self.label = format!("log metadata | {} | m raw", self.label);
                    }
                    self.body = page.text;
                })
        };
        if let Err(error) = result {
            self.body = read_error(
                &error,
                if selected.record.log_reference().is_some() {
                    "log"
                } else {
                    "evidence"
                },
            );
            self.label = "unavailable evidence".into();
            self.next = None;
            self.log = None;
        }
        self.dirty = false;
    }
    pub fn next(&mut self, app: &App) {
        self.update(app, None);
        self.scroll = 0;
        if let Some(next) = self.next {
            if self.previous.len() == 64 {
                self.previous.pop_front();
            }
            self.previous.push_back(self.position);
            self.position = next;
            self.dirty = true;
            self.update(app, None);
        }
    }
    pub fn previous(&mut self, app: &App) {
        self.update(app, None);
        self.scroll = 0;
        if matches!(self.position, Position::Text(0) | Position::Raw(0, _)) {
            return;
        }
        if let Some(position) = self.previous.pop_back() {
            self.position = position;
            self.dirty = true;
            self.update(app, None);
            return;
        }
        let catalog = Catalog::for_app(app);
        let Some(selected) = self
            .index
            .as_ref()
            .unwrap()
            .get(&catalog, app.evidence_index)
        else {
            return;
        };
        if let Some(reference) = selected.record.log_reference()
            && !self.log_metadata
        {
            let end = match self.position {
                Position::Raw(offset, _) => offset,
                _ => 0,
            };
            match log_body(
                &app.root,
                reference,
                end.saturating_sub(LOG_BYTES as u64),
                Some(end),
            ) {
                Ok(page) => {
                    self.position = Position::Raw(page.start, Some(end));
                    self.next = Some(Position::Raw(end, None));
                    self.body = page.text;
                    self.label = format!("{} | m metadata", page.label);
                    self.log = Some((reference.clone(), page.fingerprint));
                }
                Err(error) => {
                    self.body = read_error(&error, "log");
                    self.label = "unavailable evidence".into();
                    self.next = None;
                    self.log = None;
                    self.previous.clear();
                }
            }
        } else {
            let current = match self.position {
                Position::Text(offset) => offset,
                _ => 0,
            };
            let mut offset = 0;
            while let Ok(page) = selected.record.page(offset, DETAIL_BYTES) {
                match page.next_offset {
                    Some(next) if next < current => offset = next,
                    _ => break,
                }
            }
            self.position = Position::Text(offset);
            self.dirty = true;
            self.update(app, None);
        }
    }
    pub fn log_edge(&mut self, app: &App, last: bool) {
        self.update(app, None);
        let catalog = Catalog::for_app(app);
        let Some(reference) = self
            .index
            .as_ref()
            .unwrap()
            .get(&catalog, app.evidence_index)
            .and_then(|s| s.record.log_reference())
        else {
            return;
        };
        if self.log_metadata {
            self.log_metadata = false;
            self.reset_page(app.scroll);
        }
        self.previous.clear();
        self.scroll = 0;
        if !last {
            self.position = Position::Raw(0, None);
            self.dirty = true;
            self.update(app, None);
            return;
        }
        match report::read_log_page(&app.root, reference, 0, 1)
            .map(|page| (page.readable_total, page.fingerprint))
            .and_then(|(total, fingerprint)| {
                let page = log_body(
                    &app.root,
                    reference,
                    total.saturating_sub(LOG_BYTES as u64),
                    Some(total),
                )?;
                anyhow::ensure!(
                    page.fingerprint == fingerprint,
                    "log changed while selecting final chunk"
                );
                Ok(page)
            }) {
            Ok(page) => {
                self.position = Position::Raw(page.start, Some(page.end));
                self.next = None;
                self.body = page.text;
                self.label = format!("{} | m metadata", page.label);
                self.log = Some((reference.clone(), page.fingerprint));
            }
            Err(error) => {
                self.body = read_error(&error, "log");
                self.label = "unavailable evidence".into();
                self.next = None;
                self.log = None;
                self.previous.clear();
            }
        }
    }
}
struct LogBody {
    text: String,
    label: String,
    start: u64,
    end: u64,
    next: Option<u64>,
    fingerprint: LogFingerprint,
}
/// Decode whole UTF-8/error units, including at most four raw guard bytes. Raw
/// consumed counts are independent of sanitization (tabs expand, controls replace).
fn log_body(
    root: &std::path::Path,
    reference: &LogReference,
    offset: u64,
    stop: Option<u64>,
) -> anyhow::Result<LogBody> {
    // A direct jump can land inside a scalar: inspect three preceding bytes and
    // align forward. Sequential pages already end at a complete decoding unit.
    let prefix = if stop.is_some() {
        offset.min(3) as usize
    } else {
        0
    };
    let page = report::read_log_page(root, reference, offset - prefix as u64, LOG_BYTES + 4)?;
    let end = stop.unwrap_or(page.readable_total).min(page.readable_total);
    let available = (end - page.offset).min(page.bytes.len() as u64) as usize;
    let bytes = &page.bytes[..available];
    let mut begin = 0;
    while begin < prefix {
        begin += unit(&bytes[begin..]).1;
    }
    let start = page.offset + begin as u64;
    let mut consumed = begin;
    let mut text = String::with_capacity(DETAIL_BYTES);
    while consumed < bytes.len() && consumed - begin < LOG_BYTES {
        let (c, size) = unit(&bytes[consumed..]);
        // Incomplete tails before stored EOF need the next read's guard bytes.
        // The read includes four guards, so a decoding unit starting before the
        // 4-KiB payload always fits unless this is the genuine published EOF.
        let display = if c == '\t' {
            4
        } else {
            super::paging::sanitized(c).len_utf8()
        };
        if text.len() + display > DETAIL_BYTES {
            break;
        }
        if c == '\t' {
            text.push_str("    ");
        } else {
            text.push(super::paging::sanitized(c));
        }
        consumed += size;
    }
    let raw_end = page.offset + consumed as u64;
    let next = (raw_end < page.readable_total).then_some(raw_end);
    let mut label = format!(
        "raw bytes {start}..{raw_end} / {} | {}",
        page.readable_total,
        if next.is_some() {
            "more stored bytes; ] next"
        } else {
            "last stored chunk"
        }
    );
    if reference.truncated {
        label.push_str(" | capture truncated");
    }
    if page.current_bytes < page.recorded_bytes {
        label.push_str(" | incomplete current storage");
    }
    if page.current_bytes > page.recorded_bytes {
        label.push_str(" | unpublished suffix hidden");
    }
    Ok(LogBody {
        text,
        label,
        start,
        end: raw_end,
        next,
        fingerprint: page.fingerprint,
    })
}
fn unit(bytes: &[u8]) -> (char, usize) {
    let size = match bytes[0] {
        0..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 1,
    };
    let candidate = &bytes[..size.min(bytes.len())];
    match std::str::from_utf8(candidate) {
        Ok(text) => (text.chars().next().unwrap(), size),
        Err(error) => ('�', error.error_len().unwrap_or(candidate.len())),
    }
}

fn read_error(error: &anyhow::Error, kind: &str) -> String {
    super::paging::page(
        |out| write!(out, "Cannot read {kind}: {error:#}"),
        0,
        DETAIL_BYTES,
    )
    .expect("valid error projection")
    .text
}
