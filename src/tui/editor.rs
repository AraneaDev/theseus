use crate::preview::MAX_DRAFT_BYTES;
use anyhow::{Result, ensure};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Frame, layout::Rect, style::Style, text::Line};
use std::cell::Cell;

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
    diagnostic: String,
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
            diagnostic: String::new(),
        })
    }
    pub(super) fn text(&self) -> &str {
        &self.text
    }
    pub(super) fn cursor(&self) -> usize {
        self.cursor
    }
    pub(super) fn diagnostic(&self) -> &str {
        &self.diagnostic
    }
    pub(super) fn set_diagnostic(&mut self, message: &str) {
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
                return EditorAction::None;
            }
            KeyCode::Down => {
                self.vertical(true);
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
        EditorAction::None
    }

    /// Draws only visible logical lines. Offsets are interior presentation state
    /// so the caller can render from `&App` without cloning the source buffer.
    pub(super) fn draw(&self, frame: &mut Frame, area: Rect, source: &str, dirty: bool) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let width = area.width as usize;
        let show_diagnostic = !self.diagnostic.is_empty() && area.height >= 4;
        let chrome_height = if show_diagnostic { 4 } else { 3 };
        let body_y = area.y.saturating_add(if show_diagnostic { 3 } else { 2 });
        let body_height = area.height.saturating_sub(chrome_height) as usize;
        let line_index = self.text[..self.cursor]
            .bytes()
            .filter(|b| *b == b'\n')
            .count();
        let col = self.column();
        let status = if dirty { "UNSAVED" } else { "unchanged" };
        let heading = format!("EDIT {status} | Ln {} Col {}", line_index + 1, col + 1);
        draw_row(frame, area.x, area.y, width, &heading);
        if area.height > 1 {
            let source_row = format!("Source: {}", bounded_safe(source, width.saturating_mul(4)));
            draw_row(frame, area.x, area.y + 1, width, &source_row);
        }
        if show_diagnostic {
            draw_row(frame, area.x, area.y + 2, width, &self.diagnostic);
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
        if body_height == 0 {
            return;
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
        let cursor_cell = cell_width(&current[..self.cursor - self.line_start()]);
        let left = self.left_cell.get();
        let left = if cursor_cell < left {
            cursor_cell
        } else if cursor_cell >= left.saturating_add(width) {
            cursor_cell + 1 - width
        } else {
            left
        };
        self.left_cell.set(left);
        for (row, line) in self
            .text
            .split('\n')
            .skip(top)
            .take(body_height)
            .enumerate()
        {
            let projected = visible_line(line, left, width);
            frame.buffer_mut().set_stringn(
                area.x,
                body_y + row as u16,
                projected,
                width,
                Style::default(),
            );
        }
        if cursor_cell >= left && cursor_cell - left < width {
            frame.set_cursor_position((
                area.x + (cursor_cell - left) as u16,
                body_y + (line_index - top) as u16,
            ));
        }
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

fn scalar_width(s: &str) -> usize {
    Line::raw(s).width()
}

fn cell_width(line: &str) -> usize {
    let mut cells = 0;
    for (at, c) in line.char_indices() {
        cells += if c == '\t' {
            4
        } else if c.is_control() {
            1
        } else {
            scalar_width(&line[at..at + c.len_utf8()])
        };
    }
    cells
}

fn visible_line(line: &str, left: usize, width: usize) -> String {
    let mut projected = String::with_capacity(width.saturating_mul(4));
    let mut cell = 0;
    let mut written = 0;
    let max_bytes = width.saturating_mul(8).max(8);
    let right = left.saturating_add(width);
    for (at, c) in line.char_indices() {
        let cells = if c == '\t' {
            4
        } else if c.is_control() {
            1
        } else {
            scalar_width(&line[at..at + c.len_utf8()])
        };
        if cell >= right && cells > 0 {
            break;
        }
        if c == '\t' {
            for offset in 0..4 {
                let at_cell = cell + offset;
                if at_cell >= left && at_cell < right {
                    while written < at_cell - left {
                        projected.push(' ');
                        written += 1;
                    }
                    projected.push(' ');
                    written += 1;
                }
            }
        } else if cell >= left && cell.saturating_add(cells) <= right {
            while written < cell - left {
                projected.push(' ');
                written += 1;
            }
            let c = safe(c);
            if projected.len() + c.len_utf8() > max_bytes {
                break;
            }
            projected.push(c);
            written += cells;
        } else if cells > 0 && cell < right && cell + cells > left {
            let until = cell.saturating_add(cells).min(right) - left;
            while written < until {
                projected.push(' ');
                written += 1;
            }
        }
        cell += cells;
    }
    projected
}

#[cfg(test)]
mod tests {
    use super::{Editor, EditorAction};
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
        backend.assert_cursor_position((8, 2));
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
        assert_eq!(backend.buffer()[(0, 2)].symbol(), " ");
        assert_eq!(backend.buffer()[(1, 2)].symbol(), "a");
        backend.assert_cursor_position((1, 2));
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
}
