"""Shared cursor-addressed terminal screen and literal parser checks."""
import codecs
import unicodedata

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

    def feed(self, chunk):
        text = self.pending + self.decoder.decode(chunk)
        index = 0
        while index < len(text):
            char = text[index]
            if char == '\x1b':
                if index + 1 == len(text):
                    break
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
                    self.row = (first or 1) - 1
                    self.column = ((values[1] if len(values) > 1 else 1) or 1) - 1
                elif command == 'J' and first in [2, 3]:
                    self.cells.clear()
                elif command == 'K':
                    for column in range(self.columns):
                        if first == 2 or first == 0 and column >= self.column or first == 1 and column <= self.column:
                            self.cells.pop((self.row, column), None)
                elif command in ['A', 'B', 'C', 'D']:
                    amount = first or 1
                    self.row += amount if command == 'B' else -amount if command == 'A' else 0
                    self.column += amount if command == 'C' else -amount if command == 'D' else 0
                elif command == 'G':
                    self.column = (first or 1) - 1
                index = end + 1
                continue
            if char == '\r':
                self.column = 0
            elif char == '\n':
                self.row += 1
            elif char >= ' ' and not unicodedata.combining(char):
                self.cells[self.row, self.column] = char
                width = 2 if unicodedata.east_asian_width(char) in ['W', 'F'] else 1
                if width == 2:
                    self.cells[self.row, self.column + 1] = ''
                self.column += width
            index += 1
        self.pending = text[index:]

    def text(self):
        return '\n'.join(''.join(self.cells.get((row, column), ' ') for column in range(self.columns))
                         for row in range(self.rows)).encode()


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
