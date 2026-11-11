//! Fixed ASCII storyboard; time is supplied by the event loop.

use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use std::time::Duration;

const WIDTH: usize = 40;
const HEIGHT: usize = 18;
const FRAMES: usize = 6;
const FRAME_MS: u128 = 900;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Direction {
    Enter,
    Exit,
}

pub(super) struct Animation {
    direction: Direction,
    started: Duration,
    skipped: bool,
}
impl Animation {
    pub(super) fn new(direction: Direction, now: Duration) -> Self {
        Self {
            direction,
            started: now,
            skipped: false,
        }
    }
    pub(super) fn frame(&self, now: Duration) -> Option<usize> {
        let frame = now.saturating_sub(self.started).as_millis() / FRAME_MS;
        (!self.skipped && frame < FRAMES as u128).then_some(frame as usize)
    }
    pub(super) fn skip(&mut self) {
        self.skipped = true;
    }
}

enum Phase {
    Intro(Animation),
    Workbench,
    Outro(Animation),
    Done,
}
#[derive(Debug, PartialEq, Eq)]
pub(super) enum KeyAction {
    Browse,
    Ignore,
    Quit,
}

/// Decorative state only: cancellation and engine outcomes remain in the run loop.
pub(super) struct Presentation {
    phase: Phase,
    enabled: bool,
}
impl Presentation {
    pub(super) fn new(enabled: bool, now: Duration) -> Self {
        Self {
            enabled,
            phase: if enabled {
                Phase::Intro(Animation::new(Direction::Enter, now))
            } else {
                Phase::Workbench
            },
        }
    }
    pub(super) fn bypass(&mut self) {
        self.enabled = false;
        self.phase = Phase::Done;
    }
    pub(super) fn frame(
        &mut self,
        now: Duration,
        columns: u16,
        rows: u16,
    ) -> Option<(Direction, usize)> {
        if columns < WIDTH as u16 || rows < HEIGHT as u16 {
            self.enabled = false;
            self.phase = if matches!(self.phase, Phase::Outro(_) | Phase::Done) {
                Phase::Done
            } else {
                Phase::Workbench
            };
        }
        match &self.phase {
            Phase::Intro(animation) | Phase::Outro(animation) => {
                if let Some(frame) = animation.frame(now) {
                    return Some((animation.direction, frame));
                }
                self.phase = if matches!(self.phase, Phase::Intro(_)) {
                    Phase::Workbench
                } else {
                    Phase::Done
                };
            }
            _ => {}
        }
        None
    }
    pub(super) fn key(&mut self, key: KeyCode) -> KeyAction {
        match &mut self.phase {
            Phase::Intro(animation) => {
                match key {
                    KeyCode::Char('q') => {
                        self.bypass();
                        return KeyAction::Quit;
                    }
                    KeyCode::Char(' ') => animation.skip(),
                    _ => {}
                }
                KeyAction::Ignore
            }
            Phase::Outro(animation) => {
                if matches!(key, KeyCode::Char(' ') | KeyCode::Char('q')) {
                    animation.skip();
                }
                KeyAction::Ignore
            }
            Phase::Workbench => KeyAction::Browse,
            Phase::Done => KeyAction::Ignore,
        }
    }
    /// Returns true once an ordinary idle quit can leave the terminal.
    pub(super) fn quit(&mut self, now: Duration) -> bool {
        if !self.enabled || matches!(self.phase, Phase::Done) {
            return true;
        }
        if matches!(self.phase, Phase::Workbench) {
            self.phase = Phase::Outro(Animation::new(Direction::Exit, now));
        }
        false
    }
}

fn scene(frame: usize, direction: Direction) -> Vec<Line<'static>> {
    let step = frame.min(FRAMES - 1);
    let index = if direction == Direction::Enter {
        step
    } else {
        FRAMES - 1 - step
    };
    let mut grid = [[' '; WIDTH]; HEIGHT];
    fn put(grid: &mut [[char; WIDTH]; HEIGHT], x: usize, y: usize, text: &str) {
        for (offset, ch) in text.chars().enumerate() {
            grid[y][x + offset] = ch;
        }
    }
    fn centered(grid: &mut [[char; WIDTH]; HEIGHT], y: usize, text: &str) {
        put(grid, (WIDTH - text.len()) / 2, y, text);
    }
    put(&mut grid, 12, 0, "T H E S E U S");
    grid[2][3..=36].fill('-');
    grid[13][3..=36].fill('-');
    for row in grid.iter_mut().take(14).skip(2) {
        row[3] = '|';
        row[36] = '|';
    }
    for (x, y) in [(3, 2), (36, 2), (3, 13), (36, 13)] {
        grid[y][x] = '+';
    }
    grid[11][3] = ' ';
    for (y, a, b) in [(4, 5, 20), (6, 5, 9), (8, 5, 9), (10, 13, 33), (12, 5, 29)] {
        for cell in &mut grid[y][a..=b] {
            *cell = '-';
        }
    }
    for (x, a, b) in [(9, 3, 7), (22, 6, 9), (30, 3, 8), (15, 7, 9)] {
        for row in &mut grid[a..=b] {
            row[x] = '|';
        }
    }
    // The six approved storyboard anchors share one continuous, axis-aligned route.
    let points = [(1, 11), (5, 11), (5, 9), (11, 9), (11, 5), (28, 5)];
    let (mut x, mut y) = points[0];
    grid[y][x] = '.';
    for &(tx, ty) in points.iter().take(index + 1).skip(1) {
        while (x, y) != (tx, ty) {
            x = match x.cmp(&tx) {
                std::cmp::Ordering::Less => x + 1,
                std::cmp::Ordering::Greater => x - 1,
                _ => x,
            };
            y = match y.cmp(&ty) {
                std::cmp::Ordering::Less => y + 1,
                std::cmp::Ordering::Greater => y - 1,
                _ => y,
            };
            grid[y][x] = '.';
        }
    }
    grid[y][x] = '@';
    centered(
        &mut grid,
        14,
        match (direction, step) {
            (Direction::Enter, 5) => "The thread is anchored",
            (Direction::Enter, _) => "Entering the labyrinth",
            (Direction::Exit, 5) => "Session ended",
            (Direction::Exit, _) => "Following the thread home",
        },
    );
    centered(&mut grid, 16, "aranea.dev");
    centered(
        &mut grid,
        17,
        if direction == Direction::Enter {
            "[ Space ] skip"
        } else {
            "[ Space / q ] skip"
        },
    );
    grid.into_iter()
        .enumerate()
        .map(|(y, row)| {
            Line::from(
                row.into_iter()
                    .map(|ch| {
                        let style = if y == 16 || y == 17 {
                            Style::default().fg(Color::DarkGray)
                        } else if (2..=13).contains(&y) && ch == '@' {
                            Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD)
                        } else if (2..=13).contains(&y) && ch == '.' {
                            Style::default().fg(Color::Yellow)
                        } else {
                            Style::default().fg(Color::Cyan)
                        };
                        Span::styled(ch.to_string(), style)
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

pub(super) fn draw(frame: &mut Frame, index: usize, direction: Direction) {
    let area = frame.area();
    let width = area.width.min(WIDTH as u16);
    let height = area.height.min(HEIGHT as u16);
    let centered = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Paragraph::new(scene(index, direction)), centered);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(frame: usize, direction: Direction) -> Vec<String> {
        scene(frame, direction)
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn animation_uses_elapsed_time_and_finishes_at_5400ms() {
        let animation = Animation::new(Direction::Enter, Duration::ZERO);
        for (ms, expected) in [
            (0, Some(0)),
            (899, Some(0)),
            (900, Some(1)),
            (5399, Some(5)),
            (5400, None),
        ] {
            assert_eq!(animation.frame(Duration::from_millis(ms)), expected);
        }
        let mut animation = Animation::new(Direction::Exit, Duration::from_secs(3));
        assert_eq!(animation.frame(Duration::ZERO), Some(0));
        animation.skip();
        assert_eq!(animation.frame(Duration::from_secs(3)), None);
    }

    #[test]
    fn storyboard_has_literal_route_static_walls_and_continuous_anchored_thread() {
        let heroes = [(1, 11), (5, 11), (5, 9), (11, 9), (11, 5), (28, 5)];
        let walls = [
            (3, 2),
            (36, 2),
            (3, 13),
            (36, 13),
            (5, 4),
            (20, 4),
            (5, 6),
            (9, 6),
            (9, 3),
            (9, 7),
            (22, 6),
            (22, 9),
            (30, 3),
            (30, 8),
            (15, 7),
            (15, 9),
            (13, 10),
            (33, 10),
            (5, 12),
            (29, 12),
        ];
        let original = rows(0, Direction::Enter);
        let mut route = vec![(1, 11), (2, 11), (3, 11), (4, 11), (5, 11), (5, 10), (5, 9)];
        route.extend((6..=11).map(|x| (x, 9)));
        route.extend((5..=8).rev().map(|y| (11, y)));
        route.extend((12..=28).map(|x| (x, 5)));
        for (frame, hero) in heroes.into_iter().enumerate() {
            let enter = rows(frame, Direction::Enter);
            let exit = rows(5 - frame, Direction::Exit);
            assert_eq!(enter.len(), 18);
            assert!(enter.iter().all(|row| row.len() == 40 && row.is_ascii()));
            for y in 2..=13 {
                assert_eq!(enter[y], exit[y]);
            }
            for y in 2..=13 {
                for x in 0..40 {
                    if matches!(original[y].as_bytes()[x], b'+' | b'-' | b'|')
                        || matches!(enter[y].as_bytes()[x], b'+' | b'-' | b'|')
                    {
                        assert_eq!(enter[y].as_bytes()[x], original[y].as_bytes()[x]);
                    }
                }
            }
            assert_eq!(enter[hero.1].as_bytes()[hero.0], b'@');
            assert_eq!(
                enter.iter().map(|r| r.matches('@').count()).sum::<usize>(),
                1
            );
            for &(x, y) in &walls {
                assert!(matches!(enter[y].as_bytes()[x], b'+' | b'-' | b'|'));
            }
            let end = route.iter().position(|p| *p == hero).unwrap();
            for (index, &(x, y)) in route.iter().enumerate() {
                assert_eq!(
                    enter[y].as_bytes()[x],
                    if index < end {
                        b'.'
                    } else if index == end {
                        b'@'
                    } else {
                        b' '
                    }
                );
            }
            assert_eq!(enter[0].trim(), "T H E S E U S");
            assert_eq!(
                enter[14].trim(),
                if frame == 5 {
                    "The thread is anchored"
                } else {
                    "Entering the labyrinth"
                }
            );
            assert_eq!(
                exit[14].trim(),
                if frame == 0 {
                    "Session ended"
                } else {
                    "Following the thread home"
                }
            );
            assert_eq!(enter[16].trim(), "aranea.dev");
            assert_eq!(enter[17].trim(), "[ Space ] skip");
            assert_eq!(exit[17].trim(), "[ Space / q ] skip");
        }
        assert_eq!(
            rows(usize::MAX, Direction::Enter),
            rows(5, Direction::Enter)
        );
        assert_eq!(rows(usize::MAX, Direction::Exit), rows(5, Direction::Exit));
    }

    #[test]
    fn storyboard_colors_hero_thread_walls_and_muted_brand() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut terminal = Terminal::new(TestBackend::new(40, 18)).unwrap();
        terminal.draw(|f| draw(f, 5, Direction::Enter)).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(28, 5)].fg, Color::White);
        assert_eq!(buffer[(1, 11)].fg, Color::Yellow);
        assert_eq!(buffer[(3, 2)].fg, Color::Cyan);
        assert_eq!(buffer[(11, 16)].fg, Color::DarkGray);
    }

    #[test]
    fn controls_discard_intro_actions_skip_and_allow_only_one_outro() {
        let mut flow = Presentation::new(true, Duration::ZERO);
        assert_eq!(flow.key(KeyCode::Char('r')), KeyAction::Ignore);
        assert_eq!(
            flow.frame(Duration::ZERO, 40, 18),
            Some((Direction::Enter, 0))
        );
        assert_eq!(flow.key(KeyCode::Char(' ')), KeyAction::Ignore);
        assert_eq!(flow.frame(Duration::ZERO, 40, 18), None);
        assert_eq!(flow.key(KeyCode::Char('r')), KeyAction::Browse);
        assert!(!flow.quit(Duration::ZERO));
        assert_eq!(
            flow.frame(Duration::ZERO, 40, 18),
            Some((Direction::Exit, 0))
        );
        assert_eq!(flow.key(KeyCode::Char('q')), KeyAction::Ignore);
        assert_eq!(flow.frame(Duration::ZERO, 40, 18), None);
        assert!(flow.quit(Duration::ZERO));
        assert!(flow.quit(Duration::ZERO));
    }

    #[test]
    fn elapsed_intro_and_outro_space_complete_without_repeating() {
        let mut flow = Presentation::new(true, Duration::ZERO);
        assert_eq!(
            flow.frame(Duration::from_millis(5399), 40, 18),
            Some((Direction::Enter, 5))
        );
        assert_eq!(flow.frame(Duration::from_millis(5400), 40, 18), None);
        assert_eq!(flow.key(KeyCode::Char('r')), KeyAction::Browse);
        assert!(!flow.quit(Duration::from_millis(5400)));
        assert_eq!(flow.key(KeyCode::Char(' ')), KeyAction::Ignore);
        assert_eq!(flow.frame(Duration::from_millis(5400), 40, 18), None);
        assert!(flow.quit(Duration::from_millis(5400)));
        assert_eq!(flow.frame(Duration::from_millis(10800), 40, 18), None);
    }

    #[test]
    fn draw_centers_canvas_and_clips_safely() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| draw(frame, 0, Direction::Enter))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(32, 3)].symbol(), "T");
        assert_eq!(buffer[(21, 14)].symbol(), "@");
        let mut tiny = Terminal::new(TestBackend::new(12, 5)).unwrap();
        tiny.draw(|frame| draw(frame, usize::MAX, Direction::Exit))
            .unwrap();
        let clipped: Vec<String> = tiny
            .backend()
            .buffer()
            .content()
            .chunks(12)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect();
        assert_eq!(
            clipped,
            [
                "            ",
                "            ",
                "   +--------",
                "   |     |  ",
                "   | ----|--"
            ]
        );
    }

    #[test]
    fn intro_quit_and_bypass_do_not_start_outro_and_small_resize_has_no_dwell() {
        let mut flow = Presentation::new(true, Duration::ZERO);
        assert_eq!(flow.key(KeyCode::Char('q')), KeyAction::Quit);
        assert!(flow.quit(Duration::ZERO));
        for (width, height) in [(39, 18), (40, 17)] {
            let mut flow = Presentation::new(true, Duration::ZERO);
            assert_eq!(flow.frame(Duration::ZERO, width, height), None);
            assert_eq!(flow.key(KeyCode::Char('r')), KeyAction::Browse);
            assert!(flow.quit(Duration::ZERO));
        }
        let mut flow = Presentation::new(false, Duration::ZERO);
        assert!(flow.quit(Duration::ZERO));
        let mut flow = Presentation::new(true, Duration::ZERO);
        flow.bypass();
        assert!(flow.quit(Duration::ZERO));
    }
}
