//! Shared presentation vocabulary: status glyphs, focus chrome and key hints.
//! Every caller passes text that is already bounded; this module sanitizes it.
use super::safe_text;
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use std::{ffi::OsStr, sync::OnceLock};

/// <https://no-color.org>: any non-empty `NO_COLOR` disables colour. Read once
/// per process (by `init_color` at startup, or the first styled frame).
static NO_COLOR: OnceLock<bool> = OnceLock::new();
fn no_color_requested(value: Option<&OsStr>) -> bool {
    value.is_some_and(|v| !v.is_empty())
}
pub(super) fn init_color() {
    no_color();
}
pub(super) fn no_color() -> bool {
    *NO_COLOR.get_or_init(|| no_color_requested(std::env::var_os("NO_COLOR").as_deref()))
}
/// Colours are dropped; modifiers (bold, reverse, underline) stay.
fn paint(style: Style, colour: bool) -> Style {
    if colour {
        style
    } else {
        Style {
            fg: None,
            bg: None,
            ..style
        }
    }
}
/// The one way theme callers turn a colour into a style.
pub(super) fn tone(color: Color) -> Style {
    paint(Style::default().fg(color), !no_color())
}
/// Safety net for colours set outside the theme (startup scene, editor
/// syntax): under `NO_COLOR` a drawn frame keeps its glyphs and modifiers only.
pub(super) fn strip_colour(buffer: &mut Buffer) {
    for cell in &mut buffer.content {
        cell.set_fg(Color::Reset).set_bg(Color::Reset);
    }
}

/// One glyph and colour per status word, so truncated rows still show status.
pub(super) fn status(word: &str) -> (&'static str, Color) {
    match word {
        "passed" | "succeeded" | "equivalent" | "Equivalent" | "expected_failure"
        | "expected_rejection" => ("✓", Color::Green),
        "failed" | "timed_out" | "error" | "Different" | "different" => ("✗", Color::Red),
        "review" => ("!", Color::Yellow),
        "incomplete" => ("…", Color::Blue),
        "running" => ("▸", Color::Cyan),
        "pending" | "Unknown" | "unknown" => ("…", Color::DarkGray),
        "cancelled" | "interrupted" => ("⊘", Color::Magenta),
        "skipped" => ("–", Color::DarkGray),
        _ => ("·", Color::DarkGray),
    }
}

/// Status word preceded by its glyph, both coloured.
pub(super) fn status_word(word: &str, bold: bool) -> Span<'static> {
    let (glyph, color) = status(word);
    let style = tone(color);
    Span::styled(
        format!("{glyph} {}", safe_text(word)),
        if bold {
            style.add_modifier(Modifier::BOLD)
        } else {
            style
        },
    )
}

pub(super) fn dim() -> Style {
    tone(Color::DarkGray)
}
pub(super) fn accent() -> Style {
    tone(Color::Cyan).add_modifier(Modifier::BOLD)
}

/// Focused selectors get a marker, bold title and thick border, not only colour.
pub(super) fn panel(title: &str, focused: bool) -> Block<'static> {
    let title = safe_text(title);
    if focused {
        Block::default()
            .title(Line::from(vec![
                Span::styled("▶ ", accent()),
                Span::styled(title, accent()),
            ]))
            .borders(Borders::ALL)
            .border_type(BorderType::Thick)
            .border_style(tone(Color::Cyan))
    } else {
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(dim())
    }
}

/// A focused pane whose plain corners stay stable for terminal harnesses.
pub(super) fn plain_panel(title: &str, focused: bool) -> Block<'static> {
    let title = safe_text(title);
    if focused {
        Block::default()
            .title(Span::styled(title, accent()))
            .borders(Borders::ALL)
            .border_style(tone(Color::Cyan))
    } else {
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(dim())
    }
}

/// A one-line-per-row selector with a status glyph column left of the selection
/// marker. Row text is flattened so the glyph column stays aligned.
pub(super) fn status_list(
    frame: &mut Frame,
    area: Rect,
    block: Block<'static>,
    rows: Vec<(String, &str)>,
    selected: usize,
) {
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let columns = Layout::horizontal([Constraint::Length(2), Constraint::Min(1)]).split(inner);
    let (glyphs, rows): (Vec<_>, Vec<_>) = rows
        .into_iter()
        .map(|(text, word)| {
            let (glyph, color) = status(word);
            (
                (glyph, color),
                ListItem::new(safe_text(&text.replace(['\n', '\r'], " "))),
            )
        })
        .unzip();
    let mut state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(
        List::new(rows)
            .highlight_symbol("> ")
            .highlight_style(Style::default().add_modifier(Modifier::BOLD)),
        columns[1],
        &mut state,
    );
    let lines = glyphs
        .into_iter()
        .skip(state.offset())
        .take(columns[0].height as usize)
        .map(|(glyph, color)| Line::styled(glyph, tone(color)))
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), columns[0]);
}

/// Help overlay sections, in display order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Group {
    Navigate,
    Evidence,
    Logs,
    Run,
    Editing,
}
impl Group {
    const ALL: [Self; 5] = [
        Self::Navigate,
        Self::Evidence,
        Self::Logs,
        Self::Run,
        Self::Editing,
    ];
    fn title(self) -> &'static str {
        match self {
            Self::Navigate => "Navigate",
            Self::Evidence => "Evidence",
            Self::Logs => "Logs",
            Self::Run => "Run control",
            Self::Editing => "Editing",
        }
    }
}

/// A key hint. Required hints are never dropped; optional ones go first by
/// descending `drop` rank when the footer would otherwise wrap. `HELP_ONLY`
/// hints appear in the `?` overlay but never in the footer, so both are built
/// from one list per mode and cannot drift.
#[derive(Clone, Copy, Debug)]
pub(super) struct Hint {
    pub group: Group,
    pub key: &'static str,
    pub label: &'static str,
    pub drop: u8,
}
pub(super) const fn hint(group: Group, key: &'static str, label: &'static str, drop: u8) -> Hint {
    Hint {
        group,
        key,
        label,
        drop,
    }
}
pub(super) const REQUIRED: u8 = 0;
pub(super) const HELP_ONLY: u8 = u8::MAX;
/// The footer entry that opens the overlay.
pub(super) const HELP: Hint = hint(Group::Navigate, "?", "help", 2);

fn hint_width(hints: &[Hint]) -> usize {
    hints
        .iter()
        .map(|h| Span::raw(h.key).width() + 1 + Span::raw(h.label).width() + 2)
        .sum::<usize>()
}

/// Status prefix (bounded by the caller) followed by mode-specific hints that
/// fit the width when possible.
pub(super) fn footer(status: &str, hints: &[Hint], width: u16) -> Paragraph<'static> {
    Paragraph::new(footer_line(status, hints, width)).wrap(Wrap { trim: false })
}
fn footer_line(status: &str, hints: &[Hint], width: u16) -> Line<'static> {
    let status = safe_text(status);
    let prefix = if status.is_empty() {
        String::new()
    } else {
        format!("{status} | ")
    };
    let mut kept = hints
        .iter()
        .filter(|h| h.drop != HELP_ONLY)
        .copied()
        .collect::<Vec<_>>();
    let budget = (width as usize).saturating_sub(Span::raw(prefix.as_str()).width());
    while hint_width(&kept) > budget + 2 {
        let Some((index, _)) = kept
            .iter()
            .enumerate()
            .filter(|(_, h)| h.drop != REQUIRED)
            .max_by_key(|(i, h)| (h.drop, *i))
        else {
            break;
        };
        kept.remove(index);
    }
    let mut spans = vec![Span::styled(
        prefix,
        Style::default().add_modifier(Modifier::BOLD),
    )];
    for (i, h) in kept.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(h.key, accent()));
        spans.push(Span::raw(" "));
        spans.push(Span::raw(h.label));
    }
    Line::from(spans)
}

/// Rows a footer needs at this width, at least one and at most three.
pub(super) fn footer_height(paragraph: &Paragraph<'_>, width: u16) -> u16 {
    paragraph.line_count(width).clamp(1, 3) as u16
}

const HELP_TITLE: &str = " Help — ? or Esc closes ";
fn help_lines(hints: &[Hint]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for group in Group::ALL {
        let mut rows = hints.iter().filter(|h| h.group == group).peekable();
        if rows.peek().is_none() {
            continue;
        }
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(
            group.title(),
            Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        ));
        for h in rows {
            let pad = 12usize.saturating_sub(Span::raw(h.key).width()).max(1);
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(h.key, accent()),
                Span::raw(" ".repeat(pad)),
                Span::raw(h.label),
            ]));
        }
    }
    lines
}
/// Centred, bordered key reference drawn over the current mode.
pub(super) fn help(frame: &mut Frame, hints: &[Hint]) {
    let lines = help_lines(hints);
    let area = frame.area();
    let content = lines
        .iter()
        .map(Line::width)
        .chain([Span::raw(HELP_TITLE).width()])
        .max()
        .unwrap_or(0);
    let width = (u16::try_from(content).unwrap_or(u16::MAX).saturating_add(4)).min(area.width);
    let height = (u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2))
    .min(area.height);
    let [row] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let [rect] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(row);
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(Span::styled(HELP_TITLE, accent()))
                .borders(Borders::ALL)
                .border_type(BorderType::Thick)
                .border_style(tone(Color::Cyan))
                .padding(ratatui::widgets::Padding::horizontal(1)),
        ),
        rect,
    );
}

/// A lowercase needle for display highlighting; final and ordinary sigma fold
/// together, as the filter's contextual lowering does.
pub(super) fn needle(filter: &str) -> Vec<char> {
    filter.to_lowercase().chars().map(fold).collect()
}
fn fold(c: char) -> char {
    if c == 'ς' { 'σ' } else { c }
}
/// Byte ranges of case-insensitive, non-overlapping needle matches in
/// already-bounded display text, widened to whole grapheme clusters.
fn matches(text: &str, needle: &[char]) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return vec![];
    }
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
    // Each lowered scalar remembers the source scalar it came from.
    let mut starts = std::collections::VecDeque::with_capacity(needle.len());
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut matched = 0;
    for (at, c) in text.char_indices() {
        let end = at + c.len_utf8();
        for lower in c.to_lowercase().map(fold) {
            while matched > 0 && lower != needle[matched] {
                matched = failure[matched - 1];
            }
            if lower == needle[matched] {
                matched += 1;
            }
            if starts.len() == needle.len() {
                starts.pop_front();
            }
            starts.push_back(at);
            if matched == needle.len() {
                let start = starts[starts.len() - needle.len()];
                match ranges.last_mut() {
                    Some(last) if last.1 >= start => last.1 = end,
                    _ => ranges.push((start, end)),
                }
                matched = 0;
            }
        }
    }
    // Snap to grapheme boundaries so a match never splits a cluster.
    let mut boundaries = vec![0];
    let mut offset = 0;
    for grapheme in Span::raw(text).styled_graphemes(Style::default()) {
        offset += grapheme.symbol.len();
        boundaries.push(offset);
    }
    if offset != text.len() {
        boundaries.push(text.len());
    }
    let mut snapped: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        let start = *boundaries.iter().rev().find(|b| **b <= start).unwrap_or(&0);
        let end = *boundaries
            .iter()
            .find(|b| **b >= end)
            .unwrap_or(&text.len());
        match snapped.last_mut() {
            Some(last) if last.1 >= start => last.1 = end,
            _ => snapped.push((start, end)),
        }
    }
    snapped
}
pub(super) fn match_style() -> Style {
    tone(Color::Yellow).add_modifier(Modifier::REVERSED | Modifier::BOLD)
}
/// One display line with filter matches highlighted. The text is already
/// sanitized and bounded by the caller; nothing beyond it is scanned.
pub(super) fn highlighted(text: String, needle: &[char]) -> Line<'static> {
    let ranges = matches(&text, needle);
    if ranges.is_empty() {
        return Line::raw(text);
    }
    let mut spans = Vec::with_capacity(ranges.len() * 2 + 1);
    let mut at = 0;
    for (start, end) in ranges {
        if start > at {
            spans.push(Span::raw(text[at..start].to_owned()));
        }
        spans.push(Span::styled(text[start..end].to_owned(), match_style()));
        at = end;
    }
    if at < text.len() {
        spans.push(Span::raw(text[at..].to_owned()));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn statuses_have_distinct_glyphs_and_unknown_words_stay_neutral() {
        assert_eq!(status("passed").0, "✓");
        assert_eq!(status("failed").0, "✗");
        assert_eq!(status("review").0, "!");
        assert_eq!(status("skipped").0, "–");
        assert_eq!(status("incomplete").0, "…");
        assert_eq!(status("not-a-status").0, "·");
    }
    #[test]
    fn footer_drops_optional_hints_before_required_ones() {
        let hints = [
            hint(Group::Navigate, "Tab/Enter", "focus", REQUIRED),
            hint(Group::Evidence, "PgUp/Dn", "scroll", 3),
            hint(Group::Evidence, "/", "filter", 1),
            hint(Group::Navigate, "j/k", "select", HELP_ONLY),
            hint(Group::Run, "q", "quit", REQUIRED),
        ];
        let text = |w| footer_line("Offline report", &hints, w).to_string();
        let wide = text(200);
        assert!(wide.contains("PgUp/Dn") && wide.contains("filter"));
        assert!(wide.starts_with("Offline report | Tab/Enter focus"));
        assert!(
            !wide.contains("j/k"),
            "help-only hints stay out of the footer"
        );
        let narrow = text(50);
        assert!(!narrow.contains("PgUp/Dn"), "{narrow}");
        assert!(narrow.contains("Tab/Enter") && narrow.contains("quit"));
    }
    #[test]
    fn help_lists_every_hint_once_under_its_group_in_order() {
        let hints = [
            hint(Group::Run, "q", "quit", REQUIRED),
            hint(Group::Navigate, "Tab/Enter", "focus", REQUIRED),
            hint(Group::Navigate, "j/k", "select", HELP_ONLY),
            hint(Group::Logs, "m", "metadata", 3),
        ];
        let text = help_lines(&hints)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let position = |needle: &str| text.iter().position(|l| l.contains(needle)).unwrap();
        assert!(position("Navigate") < position("Tab/Enter"));
        assert!(position("j/k") < position("Logs"));
        assert!(position("Logs") < position("Run control"));
        assert!(position("Run control") < position("quit"));
        assert!(
            !text
                .iter()
                .any(|l| l.contains("Evidence") || l.contains("Editing"))
        );
    }
    #[test]
    fn no_color_is_any_non_empty_value_and_paint_keeps_only_modifiers() {
        assert!(!no_color_requested(None));
        assert!(!no_color_requested(Some(OsStr::new(""))));
        assert!(no_color_requested(Some(OsStr::new("1"))));
        assert!(no_color_requested(Some(OsStr::new("0"))));
        let style = Style::default()
            .fg(Color::Cyan)
            .bg(Color::Green)
            .add_modifier(Modifier::BOLD | Modifier::REVERSED);
        assert_eq!(paint(style, true), style);
        let plain = paint(style, false);
        assert_eq!((plain.fg, plain.bg), (None, None));
        assert!(
            plain
                .add_modifier
                .contains(Modifier::BOLD | Modifier::REVERSED)
        );
        let mut buffer = Buffer::empty(Rect::new(0, 0, 3, 1));
        buffer.set_string(0, 0, "✓ x", style);
        strip_colour(&mut buffer);
        assert_eq!(buffer[(0, 0)].symbol(), "✓");
        assert_eq!(buffer[(0, 0)].fg, Color::Reset);
        assert_eq!(buffer[(0, 0)].bg, Color::Reset);
        assert!(buffer[(0, 0)].modifier.contains(Modifier::BOLD));
    }
    #[test]
    fn highlighting_is_case_insensitive_and_keeps_unicode_whole() {
        let ranges = |text: &str, filter: &str| matches(text, &needle(filter));
        assert_eq!(
            ranges("Alpha alpha ALPHA", "alpha"),
            [(0, 5), (6, 11), (12, 17)]
        );
        assert_eq!(ranges("no hit", "zzz"), []);
        assert_eq!(ranges("anything", ""), []);
        // Multibyte scalars and an expanding lowercase (İ -> i̇) stay whole.
        assert_eq!(ranges("café CAFÉ", "café"), [(0, 5), (6, 11)]);
        let text = "xİy";
        let found = ranges(text, "i̇");
        assert_eq!(found, [(1, 3)]);
        assert!(text.is_char_boundary(found[0].0) && text.is_char_boundary(found[0].1));
        // A combining mark after the match is part of the same grapheme.
        let text = "e\u{301}x";
        assert_eq!(ranges(text, "e"), [(0, 3)]);
        // Final sigma folds with ordinary sigma.
        assert_eq!(ranges("ΟΔΟΣ", "οδος"), [(0, 8)]);
        // Highlighted spans reproduce the escaped display text exactly.
        let line = highlighted("a\u{fffd}b A".into(), &needle("a"));
        assert_eq!(line.to_string(), "a\u{fffd}b A");
        assert_eq!(
            line.spans
                .iter()
                .filter(|s| s.style == match_style())
                .count(),
            2
        );
    }
}
