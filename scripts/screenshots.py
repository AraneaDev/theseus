#!/usr/bin/env python3
"""Generate reproducible SVG screenshots of the Theseus workbench.

Each scene runs the real binary inside a detached, fixed-size tmux session,
waits for expected text, sends keys, captures the pane with its ANSI styling
and converts it into a self-contained SVG under ``docs/images``.

Inputs are copied into a neutral directory (``<tmp>/theseus-demo``) and the
binary is started there with relative paths, so screenshots never contain a
user-specific path. The script fails if any rendered text mentions the
current user's home directory or user name.

Requirements: Python 3 (standard library only) and ``tmux`` on PATH. The
``--live`` scene also needs a reachable Docker daemon.

    python3 scripts/screenshots.py [--live] [--scenes preview,editor] [--out DIR]
"""

from __future__ import annotations

import argparse
import getpass
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import time
import unicodedata
from dataclasses import dataclass, field, replace
from typing import Callable, Iterable, Optional

ROOT = pathlib.Path(__file__).resolve().parent.parent

# --------------------------------------------------------------------------
# ANSI (SGR) parsing
# --------------------------------------------------------------------------

# Colours are stored as None (terminal default), ('i', index 0-255) or
# ('rgb', r, g, b).
Color = Optional[tuple]


@dataclass(frozen=True)
class Style:
    fg: Color = None
    bg: Color = None
    bold: bool = False
    dim: bool = False
    italic: bool = False
    underline: bool = False
    reverse: bool = False
    strike: bool = False
    hidden: bool = False


DEFAULT_STYLE = Style()


def _extended_color(params: list, i: int):
    """Parse ``5;n`` or ``2;r;g;b`` following 38/48. Returns (color, next_i)."""
    if i >= len(params):
        return None, i
    mode = params[i]
    if mode == 5 and i + 1 < len(params):
        return ('i', max(0, min(255, params[i + 1]))), i + 2
    if mode == 2 and i + 3 < len(params):
        r, g, b = (max(0, min(255, v)) for v in params[i + 1:i + 4])
        return ('rgb', r, g, b), i + 4
    return None, len(params)


def apply_sgr(style: Style, raw: str) -> Style:
    """Apply one SGR parameter string (the text between ``ESC[`` and ``m``)."""
    if raw == '':
        return DEFAULT_STYLE
    # Colon sub-parameters (``38:2::r:g:b`` / ``4:3``) are handled per group.
    groups = raw.split(';')
    flat: list = []
    for group in groups:
        if ':' in group:
            parts = [p for p in group.split(':')]
            head = int(parts[0] or 0)
            if head in (38, 48, 58) and len(parts) >= 2:
                sub = [int(p) if p else 0 for p in parts[1:]]
                if sub and sub[0] == 2 and len(sub) == 5:
                    sub = [2] + sub[2:]  # drop colour-space id
                flat.append(('ext', head, sub))
            elif head == 4:
                flat.append(('ul', int(parts[1] or 0) if len(parts) > 1 else 1))
            else:
                flat.append(head)
        else:
            flat.append(int(group) if group.isdigit() else 0)

    s = style
    i = 0
    while i < len(flat):
        p = flat[i]
        if isinstance(p, tuple):
            if p[0] == 'ext':
                color, _ = _extended_color(p[2], 0)
                if p[1] == 38:
                    s = replace(s, fg=color)
                elif p[1] == 48:
                    s = replace(s, bg=color)
            elif p[0] == 'ul':
                s = replace(s, underline=p[1] != 0)
            i += 1
            continue
        if p in (38, 48, 58):
            nums = [q if isinstance(q, int) else 0 for q in flat[i + 1:]]
            color, used = _extended_color(nums, 0)
            if p == 38:
                s = replace(s, fg=color)
            elif p == 48:
                s = replace(s, bg=color)
            i += 1 + used
            continue
        if p == 0:
            s = DEFAULT_STYLE
        elif p == 1:
            s = replace(s, bold=True)
        elif p == 2:
            s = replace(s, dim=True)
        elif p == 3:
            s = replace(s, italic=True)
        elif p in (4, 21):
            s = replace(s, underline=True)
        elif p == 7:
            s = replace(s, reverse=True)
        elif p == 8:
            s = replace(s, hidden=True)
        elif p == 9:
            s = replace(s, strike=True)
        elif p == 22:
            s = replace(s, bold=False, dim=False)
        elif p == 23:
            s = replace(s, italic=False)
        elif p == 24:
            s = replace(s, underline=False)
        elif p == 27:
            s = replace(s, reverse=False)
        elif p == 28:
            s = replace(s, hidden=False)
        elif p == 29:
            s = replace(s, strike=False)
        elif 30 <= p <= 37:
            s = replace(s, fg=('i', p - 30))
        elif p == 39:
            s = replace(s, fg=None)
        elif 40 <= p <= 47:
            s = replace(s, bg=('i', p - 40))
        elif p == 49:
            s = replace(s, bg=None)
        elif 90 <= p <= 97:
            s = replace(s, fg=('i', p - 90 + 8))
        elif 100 <= p <= 107:
            s = replace(s, bg=('i', p - 100 + 8))
        i += 1
    return s


def char_width(ch: str) -> int:
    """Terminal cell width of one code point (0, 1 or 2)."""
    cp = ord(ch)
    if cp == 0x200D or 0xFE00 <= cp <= 0xFE0F:
        return 0
    if unicodedata.combining(ch) or unicodedata.category(ch) in ('Mn', 'Me', 'Cf'):
        return 0
    if unicodedata.east_asian_width(ch) in ('W', 'F'):
        return 2
    if 0x1F300 <= cp <= 0x1FAFF:
        return 2
    return 1


@dataclass
class Cell:
    text: str
    style: Style
    width: int = 1  # 2 for wide characters; continuation cells are not stored


# Matches CSI (any final byte), OSC (BEL or ST terminated) and lone ESC pairs.
_ESCAPE = re.compile(r'\x1b(?:\[([0-9;:?<=>]*)([ -/]*[@-~])|\][^\x07\x1b]*(?:\x07|\x1b\\)|[@-Z\\-_])')


def parse_ansi(data: str) -> list:
    """Parse captured text into rows of Cells. SGR state carries across rows."""
    rows: list = []
    style = DEFAULT_STYLE
    for line in data.rstrip('\n').split('\n'):
        row: list = []
        pos = 0
        for m in _ESCAPE.finditer(line):
            _emit_text(row, line[pos:m.start()], style)
            pos = m.end()
            if m.group(2) == 'm' and not (m.group(1) or '').startswith(('?', '<', '=', '>')):
                style = apply_sgr(style, m.group(1) or '')
        _emit_text(row, line[pos:], style)
        rows.append(row)
    return rows


def _emit_text(row: list, text: str, style: Style) -> None:
    for ch in text:
        if ch == '\r':
            continue
        if ch == '\t':
            for _ in range(8 - (sum(c.width for c in row) % 8)):
                row.append(Cell(' ', style))
            continue
        if ord(ch) < 0x20 or ord(ch) == 0x7F:
            continue
        w = char_width(ch)
        if w == 0:
            if row:
                row[-1].text += ch  # combining mark joins the previous cell
            continue
        row.append(Cell(ch, style, w))


def plain_text(rows: list) -> str:
    return '\n'.join(''.join(c.text for c in row).rstrip() for row in rows)


# --------------------------------------------------------------------------
# SVG rendering
# --------------------------------------------------------------------------

THEME = {
    'page': '#0b0e14',
    'window': '#11151c',
    'titlebar': '#1b212c',
    'border': '#2b3240',
    'title': '#8a94a6',
    'fg': '#d5d9e0',
    'bg': '#11151c',
}

# A muted dark palette for the 16 ANSI colours.
ANSI16 = [
    '#1c212b', '#e06c75', '#98c379', '#e5c07b', '#61afef', '#c678dd', '#56b6c2', '#c8ccd4',
    '#5c6370', '#ff7b86', '#b5e890', '#ffd68a', '#82c4ff', '#de9bf0', '#7fd6e0', '#ffffff',
]

FONT_STACK = "ui-monospace, 'SFMono-Regular', 'DejaVu Sans Mono', Menlo, Consolas, 'Liberation Mono', monospace"
FONT_SIZE = 14
CELL_W = 8.4
CELL_H = 18
PAD = 16
TITLE_H = 30


def color_hex(color: Color, default: str) -> str:
    if color is None:
        return default
    if color[0] == 'rgb':
        return '#%02x%02x%02x' % color[1:4]
    n = color[1]
    if n < 16:
        return ANSI16[n]
    if n < 232:
        n -= 16
        levels = [0, 95, 135, 175, 215, 255]
        return '#%02x%02x%02x' % (levels[n // 36], levels[(n // 6) % 6], levels[n % 6])
    v = 8 + (n - 232) * 10
    return '#%02x%02x%02x' % (v, v, v)


def effective_colors(style: Style) -> tuple:
    """Resolved (fg, bg) hex colours; bg is None when it is the default."""
    fg = color_hex(style.fg, THEME['fg'])
    bg = None if style.bg is None else color_hex(style.bg, THEME['bg'])
    if style.reverse:
        fg, bg = (bg or THEME['bg']), fg
    return fg, bg


def xml_escape(text: str) -> str:
    return (text.replace('&', '&amp;').replace('<', '&lt;').replace('>', '&gt;')
            .replace('"', '&quot;'))


def _text_key(style: Style) -> tuple:
    fg, _ = effective_colors(style)
    return (fg, style.bold, style.dim, style.italic, style.underline, style.strike)


def row_runs(row: list) -> list:
    """Group a row into text runs: (start_col, text, cell_count, style_key).

    Adjacent cells with the same text style merge. Spaces without decoration
    join the surrounding run (their colour is invisible); runs of two or more
    blank cells split runs so each run's x is set explicitly. Wide characters
    get a run of their own so their two-cell advance is exact.
    """
    runs: list = []
    cur = None  # [start, text, cells, key, trailing_spaces]
    col = 0
    for cell in row:
        visible = cell.text.strip() != '' and not cell.style.hidden
        key = _text_key(cell.style)
        if not visible and not (cell.style.underline or cell.style.strike):
            if cur is not None:
                cur[1] += ' '
                cur[2] += 1
                cur[4] += 1
                if cur[4] >= 2:
                    _close(runs, cur)
                    cur = None
            col += cell.width
            continue
        text = ' ' if cell.style.hidden else cell.text
        if cell.width == 2:
            if cur is not None:
                _close(runs, cur)
            runs.append((col, text, 2, key))
            cur = None
        elif cur is not None and cur[3] == key:
            cur[1] += text
            cur[2] += 1
            cur[4] = 0
        else:
            if cur is not None:
                _close(runs, cur)
            cur = [col, text, 1, key, 0]
        col += cell.width
    if cur is not None:
        _close(runs, cur)
    return runs


def _close(runs: list, cur: list) -> None:
    text, cells = cur[1], cur[2]
    trail = len(text) - len(text.rstrip(' '))
    if trail:
        text, cells = text[:-trail], cells - trail
    if text:
        runs.append((cur[0], text, cells, cur[3]))


def row_backgrounds(row: list) -> list:
    """Merged background spans: (start_col, cell_count, colour)."""
    spans: list = []
    col = 0
    for cell in row:
        _, bg = effective_colors(cell.style)
        if bg is not None:
            if spans and spans[-1][2] == bg and spans[-1][0] + spans[-1][1] == col:
                s = spans[-1]
                spans[-1] = (s[0], s[1] + cell.width, bg)
            else:
                spans.append((col, cell.width, bg))
        col += cell.width
    return spans


def _fmt(v: float) -> str:
    return ('%.2f' % v).rstrip('0').rstrip('.')


def render_svg(rows: list, cols: int, nrows: int, title: str) -> str:
    rows = list(rows[:nrows]) + [[] for _ in range(max(0, nrows - len(rows)))]
    term_w = cols * CELL_W
    term_h = nrows * CELL_H
    width = term_w + 2 * PAD
    height = term_h + 2 * PAD + TITLE_H
    ox, oy = PAD, TITLE_H + PAD

    classes: dict = {}

    def cls(key: tuple) -> str:
        if key not in classes:
            classes[key] = 'c%d' % len(classes)
        return classes[key]

    body: list = []
    for r, row in enumerate(rows):
        y = oy + r * CELL_H
        for start, count, bg in row_backgrounds(row):
            body.append('<rect x="%s" y="%s" width="%s" height="%d" fill="%s"/>' % (
                _fmt(ox + start * CELL_W), _fmt(y), _fmt(count * CELL_W), CELL_H, bg))
    for r, row in enumerate(rows):
        runs = row_runs(row)
        if not runs:
            continue
        baseline = oy + r * CELL_H + CELL_H * 0.75
        spans = []
        for start, text, cells, key in runs:
            spans.append('<tspan x="%s" textLength="%s" class="%s">%s</tspan>' % (
                _fmt(ox + start * CELL_W), _fmt(cells * CELL_W), cls(key), xml_escape(text)))
        body.append('<text y="%s">%s</text>' % (_fmt(baseline), ''.join(spans)))

    css = ['text{font-family:%s;font-size:%dpx;white-space:pre}' % (FONT_STACK, FONT_SIZE)]
    for key, name in classes.items():
        fg, bold, dim, italic, underline, strike = key
        rule = 'fill:%s' % fg
        if bold:
            rule += ';font-weight:bold'
        if dim:
            rule += ';opacity:.6'
        if italic:
            rule += ';font-style:italic'
        deco = ' '.join(d for d, on in (('underline', underline), ('line-through', strike)) if on)
        if deco:
            rule += ';text-decoration:%s' % deco
        css.append('.%s{%s}' % (name, rule))

    w, h = _fmt(width), _fmt(height)
    out = [
        '<svg xmlns="http://www.w3.org/2000/svg" width="%s" height="%s" viewBox="0 0 %s %s" '
        'role="img" aria-label="%s">' % (w, h, w, h, xml_escape(title)),
        '<title>%s</title>' % xml_escape(title),
        '<style>%s</style>' % ''.join(css),
        '<rect width="%s" height="%s" rx="10" fill="%s" stroke="%s"/>' % (w, h, THEME['window'], THEME['border']),
        '<path d="M0 10a10 10 0 0 1 10-10h%sa10 10 0 0 1 10 10v%dH0z" fill="%s"/>' % (
            _fmt(width - 20), TITLE_H - 10, THEME['titlebar']),
        '<circle cx="18" cy="15" r="5" fill="#ff5f57"/><circle cx="34" cy="15" r="5" fill="#febc2e"/>'
        '<circle cx="50" cy="15" r="5" fill="#28c840"/>',
        '<text x="%s" y="20" text-anchor="middle" style="font-size:12px;fill:%s">%s</text>' % (
            _fmt(width / 2), THEME['title'], xml_escape(title)),
    ]
    out += body
    out.append('</svg>')
    return '\n'.join(out) + '\n'


# --------------------------------------------------------------------------
# Privacy guard
# --------------------------------------------------------------------------

def private_markers() -> list:
    markers = []
    home = os.path.expanduser('~')
    if home and home not in ('/', '~'):
        markers.append(home.rstrip('/\\'))
    try:
        user = getpass.getuser()
    except Exception:  # noqa: BLE001 - no user name available
        user = ''
    if user:
        markers.append(user)
    return markers


def find_private(text: str, markers: Optional[list] = None) -> list:
    """Return the private markers found in ``text``.

    Paths match as substrings; the user name matches as a whole word so that
    ordinary words containing it are not reported.
    """
    found = []
    for marker in (private_markers() if markers is None else markers):
        if not marker:
            continue
        if '/' in marker or '\\' in marker:
            if marker in text:
                found.append(marker)
        elif re.search(r'(?<![A-Za-z0-9_])%s(?![A-Za-z0-9_])' % re.escape(marker), text, re.IGNORECASE):
            found.append(marker)
    return found


# --------------------------------------------------------------------------
# tmux driving
# --------------------------------------------------------------------------

class SceneError(RuntimeError):
    pass


class Tmux:
    def __init__(self, workdir: pathlib.Path, conf: pathlib.Path):
        self.socket = 'theseus-shots-%d' % os.getpid()
        self.conf = conf
        self.workdir = workdir
        self.session = None

    def _cmd(self, *args: str, check: bool = True) -> subprocess.CompletedProcess:
        return subprocess.run(['tmux', '-L', self.socket, '-f', str(self.conf), *args],
                              capture_output=True, text=True, check=check)

    def start(self, name: str, argv: list, cols: int, rows: int, env: dict) -> None:
        self.session = name
        envs = []
        for k, v in env.items():
            envs += ['-e', '%s=%s' % (k, v)]
        self._cmd('new-session', '-d', '-s', name, '-x', str(cols), '-y', str(rows),
                  '-c', str(self.workdir), *envs, *argv)

    def capture(self, ansi: bool = False) -> str:
        args = ['capture-pane', '-p', '-N', '-t', self.session]
        if ansi:
            args.insert(2, '-e')
        return self._cmd(*args).stdout

    def keys(self, *keys: str) -> None:
        self._cmd('send-keys', '-t', self.session, *keys)

    def pane_dead(self) -> bool:
        res = self._cmd('display-message', '-p', '-t', self.session, '#{pane_dead}', check=False)
        return res.stdout.strip() == '1'

    def wait_exit(self, what: str, timeout: float) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.pane_dead():
                return
            time.sleep(0.1)
        raise SceneError('timed out after %.0fs waiting for %s; screen was:\n%s'
                         % (timeout, what, self.capture()))

    def kill(self) -> None:
        if self.session:
            self._cmd('kill-session', '-t', self.session, check=False)
            self.session = None

    def shutdown(self) -> None:
        self._cmd('kill-server', check=False)

    def wait_for(self, predicate: Callable[[str], bool], what: str, timeout: float,
                 stable: bool = True) -> str:
        """Poll the pane until ``predicate`` holds (and, if ``stable``, the screen settles)."""
        deadline = time.monotonic() + timeout
        last, stable_since = None, None
        while time.monotonic() < deadline:
            screen = self.capture()
            if predicate(screen):
                if not stable:
                    return screen
                if screen == last:
                    if time.monotonic() - stable_since >= 0.4:
                        return screen
                else:
                    last, stable_since = screen, time.monotonic()
            else:
                last = None
                if self.pane_dead():
                    raise SceneError('process exited while waiting for %s:\n%s' % (what, screen))
            time.sleep(0.1)
        raise SceneError('timed out after %.0fs waiting for %s; screen was:\n%s'
                         % (timeout, what, self.capture()))


def contains(*needles: str) -> Callable[[str], bool]:
    return lambda screen: all(n in screen for n in needles)


def contains_any(*needles: str) -> Callable[[str], bool]:
    return lambda screen: any(n in screen for n in needles)


# --------------------------------------------------------------------------
# Scenes
# --------------------------------------------------------------------------

REPORT_RUN = 'examples/interrupted-rollback/.theseus-runs/1793309242520942542-1672854'
SPINNER = '⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏'


@dataclass
class Step:
    keys: tuple = ()
    until: Optional[Callable[[str], bool]] = None
    what: str = ''
    timeout: float = 30.0
    capture: Optional[str] = None  # output name to capture after this step
    title: str = ''
    stable: bool = True  # also require an unchanged screen (off while a spinner animates)
    exits: bool = False  # after the keys, wait for the process to exit


@dataclass
class Scene:
    name: str
    title: str
    args: list
    steps: list
    cols: int = 120
    rows: int = 34
    slow: bool = False
    needs: tuple = ()  # 'report', 'docker'
    probe: Optional[str] = None  # feature that must be probed first


def live_running(screen: str) -> bool:
    """A stage is running (spinner + scenario · stage) after some checks passed."""
    header = '\n'.join(screen.splitlines()[:4])
    m = re.search(r'passed (\d+)', header)
    return (any(ch in header for ch in SPINNER) and ' · ' in header
            and 'between stages' not in header
            and m is not None and int(m.group(1)) >= 8)


def scenes() -> list:
    preview_ready = contains('Will run:')
    report_ready = contains('Stages')
    report_args = ['report', REPORT_RUN]
    return [
        Scene('preview', 'theseus run --recipe examples/standalone/theseus.toml',
              ['run', '--recipe', 'examples/standalone/theseus.toml'],
              [Step(until=preview_ready, what='recipe preview', capture='preview')]),
        Scene('editor', 'theseus run — recipe editor (e)',
              ['run', '--recipe', 'examples/standalone/theseus.toml'],
              [Step(until=preview_ready, what='recipe preview'),
               Step(keys=('e',), until=contains('Ln 1'), what='editor', capture='editor')]),
        Scene('report', 'theseus report — saved interrupted-rollback run',
              report_args,
              [Step(until=report_ready, what='report overview', capture='report')],
              needs=('report',)),
        Scene('evidence', 'theseus report — stage evidence (Files)',
              report_args,
              [Step(until=report_ready, what='report overview'),
               Step(keys=('Tab',), until=lambda s: True, what='stage focus'),
               Step(keys=('Down', 'Down', 'Down'), until=lambda s: True, what='stage selection'),
               Step(keys=('Tab',), until=lambda s: True, what='evidence focus'),
               Step(keys=('Right', 'Right'), until=contains('Files'), what='files tab',
                    capture='evidence')],
              needs=('report',)),
        Scene('comparison', 'theseus compare --suite examples/comparisons/methods.toml',
              ['compare', '--suite', 'examples/comparisons/methods.toml'],
              [Step(until=contains('Variants'), what='suite preview', capture='comparison')]),
        Scene('help', 'theseus report — key help (?)',
              report_args,
              [Step(until=report_ready, what='report overview'),
               Step(keys=('?',), until=contains_any('Help', 'help', 'Keys'), what='help overlay',
                    capture='help')],
              needs=('report',), probe='help'),
        Scene('live', 'theseus run — live run in progress',
              ['run', '--recipe', 'examples/standalone/theseus.toml'],
              [Step(until=preview_ready, what='recipe preview'),
               Step(keys=('r',), until=live_running, what='running stage with spinner',
                    timeout=180, stable=False, capture='live',
                    title='theseus run — live run in progress'),
               Step(keys=('c',), until=lambda s: not any(ch in s for ch in SPINNER),
                    what='cancelled run', timeout=120, capture='live-finished',
                    title='theseus run — cancelled run retained for browsing'),
               Step(keys=('q',), exits=True, what='exit after cancellation', timeout=60)],
              slow=True, needs=('docker',)),
    ]


# --------------------------------------------------------------------------
# Demo workspace
# --------------------------------------------------------------------------

def find_report_run() -> Optional[pathlib.Path]:
    """Locate the example saved run (it is git-ignored, so may live in the main checkout)."""
    candidates = [ROOT / REPORT_RUN]
    try:
        common = subprocess.run(['git', '-C', str(ROOT), 'rev-parse', '--path-format=absolute',
                                 '--git-common-dir'], capture_output=True, text=True, check=True).stdout.strip()
        candidates.append(pathlib.Path(common).parent / REPORT_RUN)
    except (OSError, subprocess.CalledProcessError):
        pass
    for c in candidates:
        if (c / 'report.json').is_file():
            return c
    return None


def prepare_demo(report_run: Optional[pathlib.Path]) -> pathlib.Path:
    demo = pathlib.Path(tempfile.gettempdir()) / 'theseus-demo'
    if demo.exists():
        shutil.rmtree(demo)
    for rel in ('examples/standalone', 'examples/comparisons'):
        shutil.copytree(ROOT / rel, demo / rel, ignore=shutil.ignore_patterns('.theseus-runs', '.theseus-comparisons'))
    if report_run is not None:
        recipe_dir = demo / 'examples/interrupted-rollback'
        shutil.copytree(ROOT / 'examples/interrupted-rollback', recipe_dir,
                        ignore=shutil.ignore_patterns('.theseus-runs'))
        target = demo / REPORT_RUN
        shutil.copytree(report_run, target)
        # The saved report records the absolute recipe path from the machine
        # that produced it; point it at the relative copy instead.
        report = target / 'report.json'
        data = json.loads(report.read_text(encoding='utf-8'))
        if isinstance(data.get('recipe_path'), str):
            data['recipe_path'] = 'examples/interrupted-rollback/' + pathlib.PurePath(data['recipe_path']).name
        report.write_text(json.dumps(data, indent=2) + '\n', encoding='utf-8')
    return demo


def docker_containers() -> set:
    res = subprocess.run(['docker', 'ps', '-a', '-q', '--no-trunc'], capture_output=True, text=True, check=True)
    return set(res.stdout.split())


# --------------------------------------------------------------------------
# Main
# --------------------------------------------------------------------------

def build_binary() -> pathlib.Path:
    print('building release binary (cargo build --release)...', flush=True)
    subprocess.run(['cargo', 'build', '--release', '--locked'], cwd=ROOT, check=True)
    exe = ROOT / 'target' / 'release' / ('theseus.exe' if os.name == 'nt' else 'theseus')
    if not exe.is_file():
        raise SystemExit('error: build finished but %s is missing' % exe)
    return exe


def run_scene(tmux: Tmux, scene: Scene, binary: pathlib.Path) -> dict:
    """Run one scene; return {output_name: (title, ansi_text)}."""
    env = {'TERM': 'xterm-256color', 'COLORTERM': 'truecolor', 'LANG': 'C.UTF-8',
           'LC_ALL': 'C.UTF-8'}
    argv = [str(binary), '--no-animation', *scene.args]
    captures = {}
    tmux.start('scene-' + scene.name, argv, scene.cols, scene.rows, env)
    try:
        for step in scene.steps:
            before = tmux.capture()
            if step.keys:
                tmux.keys(*step.keys)
            if step.exits:
                tmux.wait_exit(step.what, step.timeout)
            elif step.until is not None:
                predicate = step.until
                if step.keys:
                    # After keys, also wait for the screen to react.
                    base = predicate
                    deadline = time.monotonic() + 3
                    predicate = lambda s, base=base, before=before, deadline=deadline: base(s) and (
                        s != before or time.monotonic() > deadline)
                tmux.wait_for(predicate, step.what, step.timeout, stable=step.stable)
            if step.capture:
                captures[step.capture] = (step.title or scene.title, tmux.capture(ansi=True))
    finally:
        tmux.kill()
    return captures


def probe_help(tmux: Tmux, binary: pathlib.Path) -> bool:
    """Return True when ``?`` opens a help overlay in the report view."""
    scene = Scene('probe', '', ['report', REPORT_RUN], [])
    tmux.start('probe', [str(binary), '--no-animation', *scene.args], 120, 34, {'TERM': 'xterm-256color'})
    try:
        base = tmux.wait_for(contains('Stages'), 'report overview', 30)
        tmux.keys('?')
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            screen = tmux.capture()
            if screen != base and re.search(r'\b(Help|Keys|Key bindings|Shortcuts)\b', screen):
                return True
            time.sleep(0.1)
        return False
    finally:
        tmux.kill()


def main(argv: Optional[list] = None) -> int:
    all_scenes = scenes()
    names = [s.name for s in all_scenes]
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--binary', type=pathlib.Path, help='theseus binary (default: cargo build --release)')
    parser.add_argument('--out', type=pathlib.Path, default=ROOT / 'docs' / 'images', help='output directory')
    parser.add_argument('--scenes', help='comma-separated subset of: %s' % ', '.join(names))
    parser.add_argument('--live', action='store_true', help='include the slow live-run scene (needs Docker)')
    parser.add_argument('--report-run', type=pathlib.Path,
                        help='saved run directory for report scenes (default: %s)' % REPORT_RUN)
    args = parser.parse_args(argv)

    if shutil.which('tmux') is None:
        print('error: tmux is required on PATH to capture screenshots (install tmux and retry)', file=sys.stderr)
        return 2

    if args.scenes:
        wanted = [n.strip() for n in args.scenes.split(',') if n.strip()]
        unknown = sorted(set(wanted) - set(names))
        if unknown:
            parser.error('unknown scene(s): %s' % ', '.join(unknown))
        if args.live and 'live' not in wanted:
            wanted.append('live')
    else:
        wanted = [s.name for s in all_scenes if not s.slow or args.live]
    selected = [s for s in all_scenes if s.name in wanted]

    binary = args.binary.resolve() if args.binary else build_binary()
    if not binary.is_file():
        print('error: binary not found: %s' % binary, file=sys.stderr)
        return 2

    report_run = args.report_run.resolve() if args.report_run else find_report_run()
    if report_run is None and any('report' in s.needs for s in selected):
        print('note: saved example run %s not found; skipping report scenes '
              '(pass --report-run DIR)' % REPORT_RUN, file=sys.stderr)
        selected = [s for s in selected if 'report' not in s.needs]

    demo = prepare_demo(report_run)
    conf = demo / '.tmux.conf'
    conf.write_text('set -g status off\nset -g remain-on-exit on\nset -g history-limit 0\n'
                    'set -g default-terminal tmux-256color\nset -g escape-time 0\n', encoding='utf-8')
    tmux = Tmux(demo, conf)
    out_dir = args.out if args.out.is_absolute() else (pathlib.Path.cwd() / args.out)
    out_dir.mkdir(parents=True, exist_ok=True)

    written, failures, notes = [], [], []
    try:
        for scene in selected:
            if scene.probe == 'help' and not probe_help(tmux, binary):
                notes.append('help: this binary has no `?` help overlay; scene skipped')
                continue
            before = None
            if 'docker' in scene.needs:
                before = docker_containers()
            print('scene %s...' % scene.name, flush=True)
            try:
                captures = run_scene(tmux, scene, binary)
            except SceneError as err:
                failures.append('%s: %s' % (scene.name, err))
                continue
            finally:
                if before is not None:
                    _await_docker_cleanup(before, failures, scene.name)
            for name, (title, ansi) in captures.items():
                rows = parse_ansi(ansi)
                leaked = find_private(plain_text(rows) + '\n' + title)
                if leaked:
                    failures.append('%s: rendered text contains private value(s) %s' % (name, leaked))
                    continue
                svg = render_svg(rows, scene.cols, scene.rows, title)
                path = out_dir / ('tui-%s.svg' % name)
                path.write_text(svg, encoding='utf-8')
                written.append(path)
    finally:
        tmux.shutdown()

    for path in written:
        leaked = find_private(path.read_text(encoding='utf-8'))
        if leaked:
            failures.append('%s contains private value(s) %s' % (path.name, leaked))
        print('wrote %s (%.1f KB)' % (path, path.stat().st_size / 1024))
    for note in notes:
        print('note: ' + note)
    if failures:
        for f in failures:
            print('error: ' + f, file=sys.stderr)
        return 1
    return 0


def _await_docker_cleanup(before: set, failures: list, scene: str, timeout: float = 60) -> None:
    deadline = time.monotonic() + timeout
    while True:
        leftover = docker_containers() - before
        if not leftover:
            return
        if time.monotonic() > deadline:
            failures.append('%s: containers left behind: %s' % (scene, ', '.join(sorted(leftover))))
            return
        time.sleep(0.5)


if __name__ == '__main__':
    sys.exit(main())
