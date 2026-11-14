"""Unit tests for the ANSI-to-SVG converter in scripts/screenshots.py.

Portable: needs neither tmux nor the theseus binary.
Run: python3 tests/support/screenshots_test.py
"""

import importlib.util
import pathlib
import sys
import unittest
import xml.etree.ElementTree as ET

ROOT = pathlib.Path(__file__).resolve().parents[2]
_spec = importlib.util.spec_from_file_location('screenshots', ROOT / 'scripts' / 'screenshots.py')
shots = importlib.util.module_from_spec(_spec)
sys.modules['screenshots'] = shots
_spec.loader.exec_module(shots)

SVG_NS = '{http://www.w3.org/2000/svg}'


def first_cell_style(data):
    return shots.parse_ansi(data)[0][0].style


class SgrTest(unittest.TestCase):
    def test_256_and_truecolor(self):
        s = first_cell_style('\x1b[38;5;196;48;2;1;2;3mX')
        self.assertEqual(s.fg, ('i', 196))
        self.assertEqual(s.bg, ('rgb', 1, 2, 3))
        self.assertEqual(shots.color_hex(s.fg, '#000'), '#ff0000')
        self.assertEqual(shots.color_hex(('i', 232), '#000'), '#080808')

    def test_colon_truecolor(self):
        s = first_cell_style('\x1b[38:2::10:20:30mX')
        self.assertEqual(s.fg, ('rgb', 10, 20, 30))

    def test_16_bright_and_defaults(self):
        s = first_cell_style('\x1b[31;102mX')
        self.assertEqual((s.fg, s.bg), (('i', 1), ('i', 10)))
        s = first_cell_style('\x1b[31;102m\x1b[39;49mX')
        self.assertEqual((s.fg, s.bg), (None, None))

    def test_attributes_and_reset(self):
        s = first_cell_style('\x1b[1;2;3;4;7mX')
        self.assertTrue(s.bold and s.dim and s.italic and s.underline and s.reverse)
        s = first_cell_style('\x1b[1;2m\x1b[22mX')
        self.assertFalse(s.bold or s.dim)
        s = first_cell_style('\x1b[1;7;38;5;3m\x1b[0mX')
        self.assertEqual(s, shots.DEFAULT_STYLE)
        s = first_cell_style('\x1b[1;7m\x1b[mX')
        self.assertEqual(s, shots.DEFAULT_STYLE)

    def test_reverse_swaps_colours(self):
        s = first_cell_style('\x1b[7;31mX')
        fg, bg = shots.effective_colors(s)
        self.assertEqual(bg, shots.ANSI16[1])
        self.assertEqual(fg, shots.THEME['bg'])

    def test_state_carries_across_rows_and_ignores_other_escapes(self):
        rows = shots.parse_ansi('\x1b[32mA\n\x1b]8;;http://x\x07B\x1b[?25l')
        self.assertEqual(rows[1][0].text, 'B')
        self.assertEqual(rows[1][0].style.fg, ('i', 2))
        self.assertEqual(len(rows[1]), 1)


class RenderTest(unittest.TestCase):
    def render(self, data, cols=20, nrows=2, title='t'):
        svg = shots.render_svg(shots.parse_ansi(data), cols, nrows, title)
        return svg, ET.fromstring(svg)

    def test_xml_escaping(self):
        svg, root = self.render('<a & "b">', title='x < y & z')
        self.assertIn('&lt;a &amp; &quot;b&quot;&gt;', svg)
        texts = [''.join(t.itertext()) for t in root.iter(SVG_NS + 'text')]
        self.assertIn('<a & "b">', texts)
        self.assertIn('x < y & z', texts)

    def test_wide_char_advance(self):
        rows = shots.parse_ansi('a界b\U0001F600c')
        self.assertEqual([c.width for c in rows[0]], [1, 2, 1, 2, 1])
        runs = shots.row_runs(rows[0])
        starts = [(r[0], r[1]) for r in runs]
        self.assertEqual(starts, [(0, 'a'), (1, '界'), (3, 'b'), (4, '\U0001F600'), (6, 'c')])
        self.assertEqual(shots.char_width('─'), 1)  # box drawing
        self.assertEqual(shots.char_width('́'), 0)  # combining

    def test_run_merging(self):
        rows = shots.parse_ansi('\x1b[31mab\x1b[31mcd e\x1b[32mf   \x1b[31mg')
        runs = shots.row_runs(rows[0])
        self.assertEqual([(r[0], r[1], r[2]) for r in runs], [(0, 'abcd e', 6), (6, 'f', 1), (10, 'g', 1)])
        _, root = self.render('\x1b[31mab\x1b[31mcd e')
        tspans = list(root.iter(SVG_NS + 'tspan'))
        self.assertEqual(len(tspans), 1)

    def test_background_rects_merge(self):
        rows = shots.parse_ansi('\x1b[44m   \x1b[1m  \x1b[0m x')
        self.assertEqual(shots.row_backgrounds(rows[0]), [(0, 5, shots.ANSI16[4])])

    def test_geometry(self):
        _, root = self.render('x', cols=10, nrows=3)
        width = float(root.get('width'))
        self.assertAlmostEqual(width, 10 * shots.CELL_W + 2 * shots.PAD)


class PrivacyGuardTest(unittest.TestCase):
    def test_home_and_username(self):
        markers = ['/home/alice', 'alice']
        self.assertEqual(shots.find_private('see /home/alice/x', markers), ['/home/alice', 'alice'])
        self.assertEqual(shots.find_private('owner alice ran', markers), ['alice'])
        self.assertEqual(shots.find_private('malice and alicent', markers), [])
        self.assertEqual(shots.find_private('/home/theseus/.local', markers), [])

    def test_default_markers_include_home(self):
        import os
        home = os.path.expanduser('~')
        if home not in ('/', '~'):
            self.assertTrue(shots.find_private('path %s/file' % home))


if __name__ == '__main__':
    unittest.main()
