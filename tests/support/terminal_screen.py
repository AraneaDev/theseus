"""Shared cursor-addressed terminal screen and literal parser checks."""
import codecs
import unicodedata

EVIDENCE_LIMIT = 4 * 1024 * 1024

class Screen:
    """Reconstruct the cursor-addressed cells emitted by the actual Ratatui TUI.

    A diff can emit only the changed letters inside a word; matching its byte
    stream (even with ANSI stripped) does not test the text a terminal displays.
    """
    def __init__(self, rows, columns):
        self.rows, self.columns = rows, columns
        self.cells = {}
        self.row = self.column = 0
        self.decoder = codecs.getincrementaldecoder('utf-8')('replace')
        self.pending = ''
        self.wrap_pending = False
        self.osc = self.osc_escape = False

    def clamp_cursor(self):
        self.row = max(0, min(self.rows - 1, self.row))
        self.column = max(0, min(self.columns - 1, self.column))

    def linefeed(self):
        self.row += 1
        if self.row >= self.rows:
            self.cells = {(row - 1, column): cell for (row, column), cell in self.cells.items()
                          if 0 < row < self.rows and 0 <= column < self.columns}
            self.row = self.rows - 1

    def feed(self, chunk):
        assert (len(self.pending.encode('utf-8')) + len(self.decoder.getstate()[0]) + len(chunk)
                <= EVIDENCE_LIMIT), 'terminal parser sequence exceeded 4 MiB'
        text = self.pending + self.decoder.decode(chunk)
        index = 0
        while index < len(text):
            char = text[index]
            if self.osc:
                # OSC title/color payload is metadata, never display text or CSI.
                # Discard as it arrives; only a split ST's ESC needs state.
                if char == '\x07' or self.osc_escape and char == '\\':
                    self.osc = self.osc_escape = False
                else:
                    self.osc_escape = char == '\x1b'
                index += 1
                continue
            if char == '\x1b':
                if index + 1 == len(text):
                    break
                if text[index + 1] == ']':
                    self.osc = True
                    index += 2
                    continue
                if text[index + 1] != '[':
                    index += 2
                    continue
                end = index + 2
                while end < len(text) and not ('@' <= text[end] <= '~'):
                    end += 1
                if end == len(text):
                    break
                arguments, command = text[index + 2:end], text[end]
                values = [int(v) if v else 0 for v in arguments.split(';')] if not arguments.startswith('?') else []
                first = values[0] if values else 0
                if command in ['H', 'f']:
                    self.wrap_pending = False
                    self.row = (first or 1) - 1
                    self.column = ((values[1] if len(values) > 1 else 1) or 1) - 1
                elif command == 'J' and first in [2, 3]:
                    self.wrap_pending = False
                    self.cells.clear()
                elif command == 'K':
                    self.wrap_pending = False
                    for column in range(self.columns):
                        if first == 2 or first == 0 and column >= self.column or first == 1 and column <= self.column:
                            self.cells.pop((self.row, column), None)
                elif command in ['A', 'B', 'C', 'D']:
                    self.wrap_pending = False
                    amount = first or 1
                    self.row += amount if command == 'B' else -amount if command == 'A' else 0
                    self.column += amount if command == 'C' else -amount if command == 'D' else 0
                elif command == 'G':
                    self.wrap_pending = False
                    self.column = (first or 1) - 1
                self.clamp_cursor()
                index = end + 1
                continue
            if char == '\r':
                self.wrap_pending = False
                self.column = 0
            elif char == '\n':
                self.wrap_pending = False
                self.linefeed()
            elif char >= ' ' and not unicodedata.combining(char):
                width = 2 if unicodedata.east_asian_width(char) in ['W', 'F'] else 1
                if width <= self.columns:
                    # VT delays a right-margin wrap until the next printable.
                    # Cursor movement/CR/LF/erase cancel it; SGR/OSC do not.
                    if self.wrap_pending or self.column + width > self.columns:
                        self.column = 0
                        self.linefeed()
                    self.wrap_pending = False
                    self.cells[self.row, self.column] = char
                    if width == 2:
                        self.cells[self.row, self.column + 1] = ''
                    self.column += width
                    if self.column >= self.columns:
                        self.column = self.columns - 1
                        self.wrap_pending = True
            index += 1
        self.pending = text[index:]

    def text(self):
        return '\n'.join(''.join(self.cells.get((row, column), ' ') for column in range(self.columns))
                         for row in range(self.rows)).encode()


class TerminalOutput:
    """Retain bounded control bytes separately from currently displayed cells."""

    def __init__(self, rows, columns):
        self.data = bytearray()
        self.screen = Screen(rows, columns)

    def feed(self, chunk):
        assert len(self.data) + len(chunk) <= EVIDENCE_LIMIT, 'PTY evidence exceeded 4 MiB'
        self.data.extend(chunk)
        self.screen.feed(chunk)

    def has(self, *texts):
        current = self.screen.text()
        return all(text in current for text in texts)


def check_screen_reconstruction():
    # Literal controls independently establish positions, overwrites, erasure,
    # and a sequence split across reads. Old characters must not remain visible.
    screen = Screen(3, 30)
    screen.feed(b'\x1b[2J\x1b[1;1HPolicy: compatible')
    screen.feed(b'\x1b[1;9Hsafe_rejection')
    assert screen.text().splitlines()[0].strip() == b'Policy: safe_rejection'
    screen.feed(b'\x1b[2;1Hold-data\x1b[2;4Her-data')
    assert screen.text().splitlines()[1].strip() == b'older-data'
    screen.feed(b'\x1b[2;6H\x1b[K')
    assert screen.text().splitlines()[1].strip() == b'older'
    screen.feed(b'\x1b[1;1H\x1b[2K')
    assert b'Policy' not in screen.text() and b'older' in screen.text()
    screen.feed(b'\x1b[2J\x1b[3;')
    screen.feed(b'4Hdata')
    assert screen.text().splitlines() == [b' ' * 30, b' ' * 30, b'   data' + b' ' * 23]


check_screen_reconstruction()
