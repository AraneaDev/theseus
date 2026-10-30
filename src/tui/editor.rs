use crate::preview::MAX_DRAFT_BYTES;
use anyhow::{Result, ensure};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Frame, layout::Rect, style::Style, text::Span};
use std::cell::{Cell, RefCell};

#[cfg(test)]
thread_local! {
    static SOURCE_VISITS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
fn source_visit(bytes: usize) {
    SOURCE_VISITS.with(|visits| visits.set(visits.get() + bytes));
}

#[cfg(not(test))]
fn source_visit(_bytes: usize) {}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum EditorAction {
    None,
    Validate,
    SaveCopy,
    Close,
}

pub(super) struct Editor {
    text: String,
    /// Byte offset, always at a UTF-8 scalar boundary.
    cursor: usize,
    preferred_column: Option<usize>,
    top_line: Cell<usize>,
    left_cell: Cell<usize>,
    viewport: RefCell<Option<Viewport>>,
    diagnostic: String,
}

/// A screen-sized projection. A stable draw reuses these rows instead of
/// revisiting the raw source.
struct Viewport {
    width: usize,
    frame_height: u16,
    line_index: usize,
    column: usize,
    elided_display: bool,
    rows: Vec<String>,
    cursor: Option<(usize, usize)>,
}

impl Editor {
    pub(super) fn new(text: String) -> Result<Self> {
        ensure!(text.len() <= MAX_DRAFT_BYTES, "draft exceeds 1 MiB");
        let cursor = text.len();
        Ok(Self {
            text,
            cursor,
            preferred_column: None,
            top_line: Cell::new(0),
            left_cell: Cell::new(0),
            viewport: RefCell::new(None),
            diagnostic: String::new(),
        })
    }
    pub(super) fn text(&self) -> &str {
        &self.text
    }
    #[cfg(test)]
    pub(super) fn cursor(&self) -> usize {
        self.cursor
    }
    pub(super) fn diagnostic(&self) -> &str {
        &self.diagnostic
    }
    pub(super) fn set_diagnostic(&mut self, message: &str) {
        self.viewport.get_mut().take();
        self.diagnostic.clear();
        for c in message.chars() {
            if self.diagnostic.len() + c.len_utf8() > 256 {
                break;
            }
            self.diagnostic.push(c);
        }
    }

    fn line_start(&self) -> usize {
        self.text[..self.cursor].rfind('\n').map_or(0, |at| at + 1)
    }

    fn line_end(&self) -> usize {
        self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |at| self.cursor + at)
    }

    fn column(&self) -> usize {
        self.text[self.line_start()..self.cursor].chars().count()
    }

    fn set_column_on_line(&mut self, start: usize, end: usize, column: usize) {
        self.cursor = self.text[start..end]
            .char_indices()
            .nth(column)
            .map_or(end, |(at, _)| start + at);
    }

    fn vertical(&mut self, down: bool) {
        let column = self.preferred_column.unwrap_or_else(|| self.column());
        self.preferred_column = Some(column);
        let start = self.line_start();
        let end = self.line_end();
        if down {
            if end < self.text.len() {
                let next = end + 1;
                let next_end = self.text[next..]
                    .find('\n')
                    .map_or(self.text.len(), |at| next + at);
                self.set_column_on_line(next, next_end, column);
            }
        } else if start > 0 {
            let previous_end = start - 1;
            let previous_start = self.text[..previous_end].rfind('\n').map_or(0, |at| at + 1);
            self.set_column_on_line(previous_start, previous_end, column);
        }
    }

    fn insert(&mut self, inserted: &str) {
        if inserted.len() > MAX_DRAFT_BYTES - self.text.len() {
            self.diagnostic = "Draft exceeds 1 MiB; insertion rejected".into();
            return;
        }
        self.text.insert_str(self.cursor, inserted);
        self.cursor += inserted.len();
        self.preferred_column = None;
        self.diagnostic.clear();
    }

    pub(super) fn paste(&mut self, text: &str) {
        self.viewport.get_mut().take();
        self.insert(text);
    }

    pub(super) fn key(&mut self, key: KeyEvent) -> EditorAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl {
            match key.code {
                KeyCode::Char('t') => return EditorAction::Validate,
                KeyCode::Char('s') => return EditorAction::SaveCopy,
                KeyCode::Home => self.cursor = 0,
                KeyCode::End => self.cursor = self.text.len(),
                _ => return EditorAction::None,
            }
            self.preferred_column = None;
            self.viewport.get_mut().take();
            return EditorAction::None;
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            return EditorAction::None;
        }
        match key.code {
            KeyCode::Esc => return EditorAction::Close,
            KeyCode::Left => {
                if self.cursor > 0 {
                    self.cursor = self.text[..self.cursor]
                        .char_indices()
                        .next_back()
                        .map_or(0, |(at, _)| at);
                }
            }
            KeyCode::Right => {
                if let Some(c) = self.text[self.cursor..].chars().next() {
                    self.cursor += c.len_utf8();
                }
            }
            KeyCode::Home => self.cursor = self.line_start(),
            KeyCode::End => self.cursor = self.line_end(),
            KeyCode::Up => {
                self.vertical(false);
                self.viewport.get_mut().take();
                return EditorAction::None;
            }
            KeyCode::Down => {
                self.vertical(true);
                self.viewport.get_mut().take();
                return EditorAction::None;
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    let start = self.text[..self.cursor]
                        .char_indices()
                        .next_back()
                        .map_or(0, |(at, _)| at);
                    self.text.drain(start..self.cursor);
                    self.cursor = start;
                    self.diagnostic.clear();
                }
            }
            KeyCode::Delete => {
                if let Some(c) = self.text[self.cursor..].chars().next() {
                    self.text.drain(self.cursor..self.cursor + c.len_utf8());
                    self.diagnostic.clear();
                }
            }
            KeyCode::Enter => self.insert("\n"),
            KeyCode::Tab => self.insert("\t"),
            KeyCode::Char(c) => {
                let mut bytes = [0; 4];
                self.insert(c.encode_utf8(&mut bytes));
            }
            _ => return EditorAction::None,
        }
        self.preferred_column = None;
        self.viewport.get_mut().take();
        EditorAction::None
    }

    /// Draws only visible logical lines. Offsets are interior presentation state
    /// so the caller can render from `&App` without cloning the source buffer.
    pub(super) fn draw(&self, frame: &mut Frame, area: Rect, source: &str, dirty: bool) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let width = area.width as usize;
        // Reserve one status row for a diagnostic or projection elision. Its
        // fixed position keeps viewport sizing independent of source contents.
        let body_y = area.y.saturating_add(3);
        let body_height = area.height.saturating_sub(4) as usize;
        let rebuild = self
            .viewport
            .borrow()
            .as_ref()
            .is_none_or(|view| view.width != width || view.frame_height != area.height);
        if rebuild {
            self.viewport
                .replace(Some(self.project(width, body_height, area.height)));
        }
        let view = self.viewport.borrow();
        let view = view.as_ref().expect("viewport was projected");
        let status = if dirty { "UNSAVED" } else { "unchanged" };
        let heading = format!(
            "EDIT {status} | Ln {} Col {}",
            view.line_index + 1,
            view.column + 1
        );
        draw_row(frame, area.x, area.y, width, &heading);
        if area.height > 1 {
            let source_row = format!("Source: {}", bounded_safe(source, width.saturating_mul(4)));
            draw_row(frame, area.x, area.y + 1, width, &source_row);
        }
        if area.height > 3 {
            if view.elided_display && self.diagnostic.is_empty() {
                draw_row(frame, area.x, area.y + 2, width, "Display elided");
            } else if view.elided_display {
                let message = format!("Display elided | {}", self.diagnostic);
                draw_row(frame, area.x, area.y + 2, width, &message);
            } else {
                draw_row(frame, area.x, area.y + 2, width, &self.diagnostic);
            }
        }
        if area.height > 2 {
            draw_row(
                frame,
                area.x,
                area.y + area.height - 1,
                width,
                "CtrlT test | CtrlS save | Esc close",
            );
        }
        for (row, projected) in view.rows.iter().enumerate() {
            frame.buffer_mut().set_stringn(
                area.x,
                body_y + row as u16,
                projected,
                width,
                Style::default(),
            );
        }
        if let Some((x, y)) = view.cursor {
            frame.set_cursor_position((area.x + x as u16, body_y + y as u16));
        }
    }

    fn project(&self, width: usize, body_height: usize, frame_height: u16) -> Viewport {
        source_visit(self.cursor);
        let line_index = self.text[..self.cursor]
            .bytes()
            .filter(|b| *b == b'\n')
            .count();
        let column = self.column();
        let mut view = Viewport {
            width,
            frame_height,
            line_index,
            column,
            elided_display: false,
            rows: Vec::new(),
            cursor: None,
        };
        if body_height == 0 {
            return view;
        }
        let top = self.top_line.get();
        let top = if line_index < top {
            line_index
        } else if line_index >= top.saturating_add(body_height) {
            line_index + 1 - body_height
        } else {
            top
        };
        self.top_line.set(top);
        let current = &self.text[self.line_start()..self.line_end()];
        let cursor_cell = cursor_cell(current, self.cursor - self.line_start());
        let left = self.left_cell.get();
        let left = if cursor_cell < left {
            cursor_cell
        } else if cursor_cell >= left.saturating_add(width) {
            cursor_cell + 1 - width
        } else {
            left
        };
        self.left_cell.set(left);
        for line in self.text.split('\n').skip(top).take(body_height) {
            let (row, elided) = visible_line(line, left, width);
            view.elided_display |= elided;
            view.rows.push(row);
        }
        if cursor_cell >= left && cursor_cell - left < width {
            view.cursor = Some((cursor_cell - left, line_index - top));
        }
        view
    }
}

fn draw_row(frame: &mut Frame, x: u16, y: u16, width: usize, text: &str) {
    let projected = bounded_safe(text, width.saturating_mul(4));
    frame
        .buffer_mut()
        .set_stringn(x, y, projected, width, Style::default());
}

fn safe(c: char) -> char {
    if c.is_control() { '�' } else { c }
}

fn bounded_safe(text: &str, max_bytes: usize) -> String {
    let mut result = String::new();
    for c in text.chars() {
        let c = safe(c);
        if result.len() + c.len_utf8() > max_bytes {
            break;
        }
        result.push(c);
    }
    result
}

/// Width of a complete source grapheme after display-only tab/control handling.
fn grapheme_width(symbol: &str) -> usize {
    match symbol {
        "\t" => 4,
        _ if symbol.chars().any(char::is_control) => 1,
        _ => Span::raw(symbol).width(),
    }
}

fn cursor_cell(line: &str, cursor: usize) -> usize {
    let span = Span::raw(line);
    let mut bytes = 0;
    let mut cells = 0;
    for grapheme in span.styled_graphemes(Style::default()) {
        if bytes >= cursor {
            break;
        }
        source_visit(grapheme.symbol.len());
        bytes += grapheme.symbol.len();
        // A scalar position inside a cluster shares its trailing display edge.
        cells += grapheme_width(grapheme.symbol);
    }
    cells
}

fn visible_line(line: &str, left: usize, width: usize) -> (String, bool) {
    // Fixed row allowance keeps ordinary multi-person ZWJ emoji intact even
    // in a two-cell viewport. It cannot grow with source or cluster length.
    let max_bytes = width.saturating_mul(8).saturating_add(64);
    let mut projected = String::with_capacity(max_bytes);
    let mut separated = false;
    let mut cell = 0;
    let mut written = 0;
    let mut elided_display = false;
    let right = left.saturating_add(width);
    let span = Span::raw(line);
    for grapheme in span.styled_graphemes(Style::default()) {
        let symbol = grapheme.symbol;
        source_visit(symbol.len());
        let cells = grapheme_width(symbol);
        if cells == 0 {
            // Standalone zero-cell clusters cannot occupy a terminal cell.
            if cell >= left && cell <= right {
                elided_display = true;
                separated = written > 0;
            }
            continue;
        }
        if cell >= right {
            break;
        }
        let end = cell.saturating_add(cells);
        if end > left {
            if separated {
                // Retain a zero-cell boundary when eliding standalone marks;
                // otherwise adjacent emoji could become a new joined cluster.
                projected.push('\u{200b}');
                separated = false;
            }
            let start = cell.max(left) - left;
            while written < start {
                projected.push(' ');
                written += 1;
            }
            let until = end.min(right) - left;
            if cell >= left && end <= right && symbol != "\t" {
                let display = if symbol.chars().any(char::is_control) {
                    "�"
                } else {
                    symbol
                };
                // Reserve a blank plus a possible three-byte zero-cell boundary
                // per remaining cell, so later printable tails always fit.
                if projected.len() + display.len() + 4 * (width - until) <= max_bytes {
                    projected.push_str(display);
                    written = until;
                } else {
                    elided_display = true;
                }
            }
            // Tabs, clipped clusters and elided clusters retain their full
            // visible cell footprint without emitting a partial grapheme.
            while written < until {
                projected.push(' ');
                written += 1;
            }
        }
        cell = end;
    }
    (projected, elided_display)
}

#[cfg(test)]
mod tests {
    use super::{Editor, EditorAction, SOURCE_VISITS};
    use crate::preview::MAX_DRAFT_BYTES;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        static TRACK_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
        static LARGEST_ALLOCATION: Cell<usize> = const { Cell::new(0) };
    }

    struct AllocationMeter;

    // Per-thread accounting keeps concurrent Rust tests out of this measurement.
    unsafe impl GlobalAlloc for AllocationMeter {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            TRACK_ALLOCATIONS
                .try_with(|active| {
                    if active.get() {
                        let _ = LARGEST_ALLOCATION.try_with(|largest| {
                            largest.set(largest.get().max(layout.size()));
                        });
                    }
                })
                .ok();
            unsafe { System.alloc(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static ALLOCATION_METER: AllocationMeter = AllocationMeter;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn screen(editor: &Editor, width: u16, height: u16) -> (String, TestBackend) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| editor.draw(frame, Rect::new(0, 0, width, height), "source.toml", true))
            .unwrap();
        let backend = terminal.backend().clone();
        let text = backend
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        (text, backend)
    }

    #[test]
    fn unicode_mutations_keep_exact_bytes_and_scalar_boundaries() {
        let mut editor = Editor::new("a界🙂\nβ".into()).unwrap();
        editor.key(ctrl(KeyCode::Home));
        editor.key(KeyCode::Right.into());
        editor.key(KeyCode::Delete.into());
        assert_eq!(editor.text(), "a🙂\nβ");
        editor.key(KeyCode::Right.into());
        editor.key(KeyCode::Backspace.into());
        assert_eq!(editor.text(), "a\nβ");
        editor.key(KeyCode::Delete.into());
        assert_eq!(editor.text(), "aβ");
        editor.key(KeyCode::Char('界').into());
        assert_eq!(editor.text(), "a界β");
        assert_eq!(editor.cursor(), "a界".len());
    }

    #[test]
    fn preferred_scalar_column_survives_short_and_empty_lines() {
        let mut editor = Editor::new("abcde\nx\n\nabcde".into()).unwrap();
        editor.key(ctrl(KeyCode::Home));
        for _ in 0..4 {
            editor.key(key(KeyCode::Right));
        }
        editor.key(key(KeyCode::Down));
        assert_eq!(editor.cursor(), "abcde\nx".len());
        editor.key(key(KeyCode::Down));
        assert_eq!(editor.cursor(), "abcde\nx\n".len());
        editor.key(key(KeyCode::Down));
        assert_eq!(editor.cursor(), "abcde\nx\n\nabcd".len());
        editor.key(key(KeyCode::Up));
        editor.key(key(KeyCode::Up));
        editor.key(key(KeyCode::Up));
        assert_eq!(editor.cursor(), 4);
    }

    #[test]
    fn home_end_document_edges_and_literal_controls() {
        let mut editor = Editor::new("a\nβ".into()).unwrap();
        editor.key(key(KeyCode::Home));
        assert_eq!(editor.cursor(), 2);
        editor.key(key(KeyCode::End));
        assert_eq!(editor.cursor(), "a\nβ".len());
        editor.key(ctrl(KeyCode::Home));
        assert_eq!(editor.cursor(), 0);
        editor.key(ctrl(KeyCode::End));
        assert_eq!(editor.cursor(), "a\nβ".len());
        for c in "qrpsd".chars() {
            assert_eq!(editor.key(key(KeyCode::Char(c))), EditorAction::None);
        }
        editor.key(key(KeyCode::Enter));
        editor.key(key(KeyCode::Tab));
        assert_eq!(editor.text(), "a\nβqrpsd\n\t");
        assert_eq!(editor.key(ctrl(KeyCode::Char('t'))), EditorAction::Validate);
        assert_eq!(editor.key(ctrl(KeyCode::Char('s'))), EditorAction::SaveCopy);
        assert_eq!(editor.key(key(KeyCode::Esc)), EditorAction::Close);
        assert_eq!(editor.key(ctrl(KeyCode::Char('c'))), EditorAction::None);
    }

    #[test]
    fn insertion_and_paste_cap_are_atomic() {
        let mut editor = Editor::new("x".repeat(MAX_DRAFT_BYTES - 2)).unwrap();
        editor.paste("é");
        assert_eq!(editor.text().len(), MAX_DRAFT_BYTES);
        let before = editor.cursor();
        let before_text = editor.text().to_owned();
        editor.paste("z");
        assert_eq!(editor.text().len(), MAX_DRAFT_BYTES);
        assert_eq!(editor.text(), before_text);
        assert_eq!(editor.cursor(), before);
        assert!(editor.diagnostic().contains("1 MiB"));
        assert!(Editor::new("x".repeat(MAX_DRAFT_BYTES + 1)).is_err());

        let mut near = Editor::new("x".repeat(MAX_DRAFT_BYTES - 1)).unwrap();
        near.key(key(KeyCode::Char('界')));
        assert_eq!(near.text().len(), MAX_DRAFT_BYTES - 1);
        assert_eq!(near.cursor(), MAX_DRAFT_BYTES - 1);
    }

    #[test]
    fn viewport_sanitizes_controls_and_places_cursor_after_tab_wide_and_combining() {
        let mut editor = Editor::new("a\t界e\u{301}\u{1b}Z".into()).unwrap();
        editor.key(ctrl(KeyCode::Home));
        for _ in 0..5 {
            editor.key(key(KeyCode::Right));
        }
        let (text, mut backend) = screen(&editor, 36, 8);
        assert!(text.contains("界"));
        assert!(text.contains("�"));
        assert!(!text.contains('\u{1b}'));
        backend.assert_cursor_position((8, 3));
        assert_eq!(editor.text(), "a\t界e\u{301}\u{1b}Z");
    }

    #[test]
    fn viewport_reaches_large_tail_and_handles_tiny_areas() {
        let editor = Editor::new(format!("HEAD{}TAIL", "x".repeat(MAX_DRAFT_BYTES - 8))).unwrap();
        let (text, _) = screen(&editor, 20, 6);
        assert!(text.contains("TAIL"));
        assert!(!text.contains("HEAD"));
        for (w, h) in [(0, 0), (1, 1), (2, 2), (4, 3)] {
            let _ = screen(&editor, w, h);
        }
        assert_eq!(editor.text().len(), MAX_DRAFT_BYTES);
    }

    #[test]
    fn horizontal_window_preserves_cell_alignment_across_clipped_wide_scalar() {
        let mut editor = Editor::new("界a".into()).unwrap();
        editor.key(ctrl(KeyCode::Home));
        editor.key(key(KeyCode::Right));
        let (_, mut backend) = screen(&editor, 2, 6);
        assert_eq!(backend.buffer()[(0, 3)].symbol(), " ");
        assert_eq!(backend.buffer()[(1, 3)].symbol(), "a");
        backend.assert_cursor_position((1, 3));
    }

    #[test]
    fn grapheme_end_carets_match_rendered_emoji_and_printable_tail() {
        for glyph in ["👩‍💻", "👨‍👩‍👧‍👦", "👍🏽", "🇳🇱", "1\u{fe0f}\u{20e3}", "❤\u{fe0f}"]
        {
            let raw = format!("{glyph}x");
            let editor = Editor::new(raw.clone()).unwrap();
            let (_, mut backend) = screen(&editor, 30, 8);
            assert_eq!(backend.buffer()[(0, 3)].symbol(), glyph);
            assert_eq!(backend.buffer()[(2, 3)].symbol(), "x");
            backend.assert_cursor_position((3, 3));
            assert_eq!(editor.text(), raw);
        }
    }

    #[test]
    fn grapheme_interior_scalar_carets_use_trailing_edge_and_raw_column() {
        for glyph in ["👩‍💻", "👨‍👩‍👧‍👦", "👍🏽", "🇳🇱", "1\u{fe0f}\u{20e3}", "❤\u{fe0f}"]
        {
            let mut editor = Editor::new(format!("{glyph}x")).unwrap();
            editor.key(ctrl(KeyCode::Home));
            let (_, mut backend) = screen(&editor, 40, 8);
            backend.assert_cursor_position((0, 3));
            for (index, (at, c)) in glyph.char_indices().enumerate() {
                editor.key(key(KeyCode::Right));
                let (text, mut backend) = screen(&editor, 40, 8);
                assert_eq!(editor.cursor(), at + c.len_utf8());
                assert!(text.contains(&format!("Ln 1 Col {}", index + 2)));
                assert_eq!(backend.buffer()[(0, 3)].symbol(), glyph);
                assert_eq!(backend.buffer()[(2, 3)].symbol(), "x");
                backend.assert_cursor_position((2, 3));
            }
        }
    }

    #[test]
    fn grapheme_horizontal_windows_clip_whole_clusters_and_keep_tail_aligned() {
        for glyph in ["👩‍💻", "👨‍👩‍👧‍👦", "👍🏽", "🇳🇱", "1\u{fe0f}\u{20e3}", "❤\u{fe0f}"]
        {
            let mut editor = Editor::new(format!("{glyph}x")).unwrap();
            let (_, mut backend) = screen(&editor, 3, 8);
            assert_eq!(backend.buffer()[(0, 3)].symbol(), " ");
            assert_eq!(backend.buffer()[(1, 3)].symbol(), "x");
            backend.assert_cursor_position((2, 3));
            editor.key(ctrl(KeyCode::Home));
            let (_, mut backend) = screen(&editor, 2, 8);
            assert_eq!(backend.buffer()[(0, 3)].symbol(), glyph);
            backend.assert_cursor_position((0, 3));
            editor.key(key(KeyCode::Right));
            let (_, mut backend) = screen(&editor, 2, 8);
            assert_eq!(backend.buffer()[(0, 3)].symbol(), " ");
            assert_eq!(backend.buffer()[(1, 3)].symbol(), "x");
            backend.assert_cursor_position((1, 3));
            editor.key(ctrl(KeyCode::Home));
            let (_, mut backend) = screen(&editor, 1, 8);
            assert_eq!(backend.buffer()[(0, 3)].symbol(), " ");
            backend.assert_cursor_position((0, 3));
        }
    }

    #[test]
    fn grapheme_zero_cell_separator_does_not_join_neighboring_emoji() {
        let editor = Editor::new("👍\u{200b}🏽x".into()).unwrap();
        let (_, mut backend) = screen(&editor, 30, 8);
        assert_eq!(backend.buffer()[(0, 3)].symbol(), "👍");
        assert_eq!(backend.buffer()[(2, 3)].symbol(), "🏽");
        assert_eq!(backend.buffer()[(4, 3)].symbol(), "x");
        backend.assert_cursor_position((5, 3));
    }

    #[test]
    fn grapheme_pathological_cluster_keeps_footprint_tail_and_bounded_allocation() {
        let raw = format!("👩{}x", "\u{200d}💻".repeat((MAX_DRAFT_BYTES - 5) / 7));
        let editor = Editor::new(raw.clone()).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
        LARGEST_ALLOCATION.with(|largest| largest.set(0));
        TRACK_ALLOCATIONS.with(|active| active.set(true));
        terminal
            .draw(|frame| editor.draw(frame, Rect::new(0, 0, 40, 8), "source.toml", true))
            .unwrap();
        TRACK_ALLOCATIONS.with(|active| active.set(false));
        assert!(LARGEST_ALLOCATION.with(Cell::get) < 64 * 1024);
        assert_eq!(terminal.backend().buffer()[(0, 3)].symbol(), " ");
        assert_eq!(terminal.backend().buffer()[(1, 3)].symbol(), " ");
        assert_eq!(terminal.backend().buffer()[(2, 3)].symbol(), "x");
        terminal.backend_mut().assert_cursor_position((3, 3));
        assert_eq!(editor.text(), raw);
        assert!(screen(&editor, 40, 8).0.contains("Display elided"));
        SOURCE_VISITS.with(|visits| visits.set(0));
        for _ in 0..3 {
            terminal
                .draw(|frame| editor.draw(frame, Rect::new(0, 0, 40, 8), "source.toml", true))
                .unwrap();
        }
        assert_eq!(SOURCE_VISITS.with(Cell::get), 0);
    }

    #[test]
    fn grapheme_scalar_deletion_keeps_raw_joiners_and_modifiers_editable() {
        let mut editor = Editor::new("👩‍💻👍🏽".into()).unwrap();
        editor.key(key(KeyCode::Backspace));
        assert_eq!(editor.text(), "👩‍💻👍");
        editor.key(ctrl(KeyCode::Home));
        editor.key(key(KeyCode::Right));
        editor.key(key(KeyCode::Delete));
        assert_eq!(editor.text(), "👩💻👍");
        let (_, mut backend) = screen(&editor, 30, 8);
        backend.assert_cursor_position((2, 3));
    }

    #[test]
    fn repeated_large_source_draws_never_allocate_a_full_copy() {
        let editor = Editor::new(format!("HEAD{}TAIL", "x".repeat(MAX_DRAFT_BYTES - 8))).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
        LARGEST_ALLOCATION.with(|largest| largest.set(0));
        TRACK_ALLOCATIONS.with(|active| active.set(true));
        for _ in 0..3 {
            terminal
                .draw(|frame| editor.draw(frame, Rect::new(0, 0, 40, 8), "source.toml", true))
                .unwrap();
        }
        TRACK_ALLOCATIONS.with(|active| active.set(false));
        let largest = LARGEST_ALLOCATION.with(Cell::get);
        assert!(largest < 64 * 1024, "a draw allocated {largest} bytes");
    }

    #[test]
    fn validation_diagnostic_is_bounded_and_sanitized_on_screen() {
        let mut editor = Editor::new("x".into()).unwrap();
        editor.set_diagnostic(&format!("bad\u{1b}{}", "z".repeat(1_000)));
        assert!(editor.diagnostic().len() <= 256);
        let (text, _) = screen(&editor, 40, 6);
        assert!(text.contains("bad�"));
        assert!(text.contains("CtrlT"));
        assert!(!text.contains('\u{1b}'));
    }

    #[test]
    fn repeated_draws_at_long_line_tail_visit_only_bounded_source() {
        let editor = Editor::new(format!("HEAD{}TAIL", "x".repeat(MAX_DRAFT_BYTES - 8))).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
        terminal
            .draw(|frame| editor.draw(frame, Rect::new(0, 0, 40, 8), "source.toml", true))
            .unwrap();
        SOURCE_VISITS.with(|visits| visits.set(0));
        for _ in 0..3 {
            terminal
                .draw(|frame| editor.draw(frame, Rect::new(0, 0, 40, 8), "source.toml", true))
                .unwrap();
        }
        let visits = SOURCE_VISITS.with(Cell::get);
        assert!(
            visits < 10_000,
            "unchanged long-line viewport visited {visits} source bytes"
        );
    }

    #[test]
    fn repeated_draws_at_many_line_tail_visit_only_bounded_source() {
        let editor = Editor::new("x\n".repeat(MAX_DRAFT_BYTES / 2)).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
        terminal
            .draw(|frame| editor.draw(frame, Rect::new(0, 0, 40, 8), "source.toml", true))
            .unwrap();
        SOURCE_VISITS.with(|visits| visits.set(0));
        for _ in 0..3 {
            terminal
                .draw(|frame| editor.draw(frame, Rect::new(0, 0, 40, 8), "source.toml", true))
                .unwrap();
        }
        let visits = SOURCE_VISITS.with(Cell::get);
        assert!(
            visits < 10_000,
            "unchanged many-line viewport visited {visits} source bytes"
        );
    }

    #[test]
    fn viewport_reprojects_after_navigation_edit_and_diagnostic() {
        let mut editor = Editor::new(format!("HEAD{}TAIL", "x".repeat(100))).unwrap();
        assert!(screen(&editor, 20, 6).0.contains("TAIL"));
        editor.key(ctrl(KeyCode::Home));
        let (text, mut backend) = screen(&editor, 20, 6);
        assert!(text.contains("HEAD"));
        assert!(!text.contains("TAIL"));
        backend.assert_cursor_position((0, 3));
        editor.key(key(KeyCode::Char('Z')));
        assert!(screen(&editor, 20, 6).0.contains("ZHEAD"));
        editor.set_diagnostic("invalid draft");
        let (text, mut backend) = screen(&editor, 20, 6);
        assert!(text.contains("invalid draft"));
        backend.assert_cursor_position((1, 3));
    }

    #[test]
    fn huge_combining_run_elides_marks_but_keeps_printable_tail_and_cursor() {
        let raw = format!("e{}Z", "\u{301}".repeat((MAX_DRAFT_BYTES - 2) / 2));
        let editor = Editor::new(raw.clone()).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
        terminal
            .draw(|frame| editor.draw(frame, Rect::new(0, 0, 40, 8), "source.toml", true))
            .unwrap();
        let first = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(first.contains("Display elided"));
        assert!(first.contains('Z'));
        terminal.backend_mut().assert_cursor_position((2, 3));
        assert_eq!(editor.text(), raw);
        SOURCE_VISITS.with(|visits| visits.set(0));
        LARGEST_ALLOCATION.with(|largest| largest.set(0));
        TRACK_ALLOCATIONS.with(|active| active.set(true));
        for _ in 0..3 {
            terminal
                .draw(|frame| editor.draw(frame, Rect::new(0, 0, 40, 8), "source.toml", true))
                .unwrap();
        }
        TRACK_ALLOCATIONS.with(|active| active.set(false));
        assert!(SOURCE_VISITS.with(Cell::get) < 10_000);
        assert!(LARGEST_ALLOCATION.with(Cell::get) < 64 * 1024);
    }

    #[test]
    fn leading_combining_run_elides_marks_before_printable_text() {
        let raw = format!("{}Z", "\u{301}".repeat((MAX_DRAFT_BYTES - 1) / 2));
        let editor = Editor::new(raw.clone()).unwrap();
        let (text, mut backend) = screen(&editor, 40, 8);
        assert!(text.contains("Display elided"));
        assert!(text.contains('Z'));
        backend.assert_cursor_position((1, 3));
        assert_eq!(editor.text(), raw);
    }

    #[test]
    fn action_keys_leave_unchanged_viewport_cached() {
        let mut editor = Editor::new("x".repeat(MAX_DRAFT_BYTES)).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
        terminal
            .draw(|frame| editor.draw(frame, Rect::new(0, 0, 40, 8), "source.toml", true))
            .unwrap();
        assert_eq!(editor.key(ctrl(KeyCode::Char('t'))), EditorAction::Validate);
        assert_eq!(editor.key(ctrl(KeyCode::Char('s'))), EditorAction::SaveCopy);
        SOURCE_VISITS.with(|visits| visits.set(0));
        terminal
            .draw(|frame| editor.draw(frame, Rect::new(0, 0, 40, 8), "source.toml", true))
            .unwrap();
        assert!(SOURCE_VISITS.with(Cell::get) < 10_000);
    }
}
