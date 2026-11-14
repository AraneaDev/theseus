//! Shared presentation vocabulary: status glyphs, focus chrome and key hints.
//! Every caller passes text that is already bounded; this module sanitizes it.
use super::safe_text;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

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
    let style = Style::default().fg(color);
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
    Style::default().fg(Color::DarkGray)
}

/// Focused selectors get a marker, bold title and thick border, not only colour.
pub(super) fn panel(title: &str, focused: bool) -> Block<'static> {
    let title = safe_text(title);
    if focused {
        let accent = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        Block::default()
            .title(Line::from(vec![
                Span::styled("▶ ", accent),
                Span::styled(title, accent),
            ]))
            .borders(Borders::ALL)
            .border_type(BorderType::Thick)
            .border_style(Style::default().fg(Color::Cyan))
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
            .title(Span::styled(
                title,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
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
        .map(|(glyph, color)| Line::styled(glyph, Style::default().fg(color)))
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), columns[0]);
}

/// A key hint. Required hints are never dropped; optional ones go first by
/// descending `drop` rank when the footer would otherwise wrap.
#[derive(Clone, Copy)]
pub(super) struct Hint {
    pub key: &'static str,
    pub label: &'static str,
    pub drop: u8,
}
pub(super) const fn hint(key: &'static str, label: &'static str, drop: u8) -> Hint {
    Hint { key, label, drop }
}
pub(super) const REQUIRED: u8 = 0;

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
    let mut kept = hints.to_vec();
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
        spans.push(Span::styled(
            h.key,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::raw(h.label));
    }
    Line::from(spans)
}

/// Rows a footer needs at this width, at least one and at most three.
pub(super) fn footer_height(paragraph: &Paragraph<'_>, width: u16) -> u16 {
    paragraph.line_count(width).clamp(1, 3) as u16
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
            hint("Tab/Enter", "focus", REQUIRED),
            hint("PgUp/Dn", "scroll", 3),
            hint("/", "filter", 1),
            hint("q", "quit", REQUIRED),
        ];
        let text = |w| footer_line("Offline report", &hints, w).to_string();
        let wide = text(200);
        assert!(wide.contains("PgUp/Dn") && wide.contains("filter"));
        assert!(wide.starts_with("Offline report | Tab/Enter focus"));
        let narrow = text(50);
        assert!(!narrow.contains("PgUp/Dn"), "{narrow}");
        assert!(narrow.contains("Tab/Enter") && narrow.contains("quit"));
    }
}
