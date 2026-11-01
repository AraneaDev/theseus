"""Literal display controls independent of Theseus and terminal byte history."""
import sys
import unittest

sys.dont_write_bytecode = True
from terminal_screen import Screen, TerminalOutput


class ScreenProjectionTests(unittest.TestCase):
    def test_implicit_wrapped_frame_keeps_body_and_exact_borders(self):
        output = TerminalOutput(4, 12)
        output.feed(('┌' + '─' * 10 + '┐│Preview\x1b[3C│└' + '─' * 10 + '┘').encode())
        self.assertEqual(output.screen.text().decode().splitlines(),
                         ['┌──────────┐', '│Preview   │', '└──────────┘', ' ' * 12])
        self.assertTrue(output.has(b'Preview'))
        output.feed(b'\x1b[2;1H\x1b[2K')
        self.assertFalse(output.has(b'Preview'), 'erased body survived in display history')

    def test_final_column_waits_for_printable_even_across_sgr_and_reads(self):
        screen = Screen(2, 4)
        screen.feed(b'abcd')
        self.assertEqual(screen.text().splitlines(), [b'abcd', b'    '])
        screen.feed(b'\x1b[31')
        screen.feed(b'mX')
        self.assertEqual(screen.text().splitlines(), [b'abcd', b'X   '])

    def test_cursor_motion_cancels_pending_wrap_including_same_position(self):
        for move, expected in [(b'\x1b[1;4H', b'abcX'), (b'\x1b[0C', b'abcX'),
                               (b'\x1b[2D', b'aXcd'), (b'\x1b[1G', b'Xbcd')]:
            with self.subTest(move=move):
                screen = Screen(2, 4)
                screen.feed(b'abcd' + move + b'X')
                self.assertEqual(screen.text().splitlines(), [expected, b'    '])

    def test_absolute_and_relative_cursor_positions_clamp_without_scroll(self):
        screen = Screen(2, 4)
        screen.feed(b'\x1b[99;99HX\x1b[99AZ\x1b[99Dq')
        self.assertEqual(screen.text().splitlines(), [b'q  Z', b'   X'])
        self.assertTrue(all(0 <= row < 2 and 0 <= column < 4 for row, column in screen.cells))

    def test_crlf_does_not_double_wrap_and_cr_overwrites_current_row(self):
        screen = Screen(3, 4)
        screen.feed(b'abcd\r\nX')
        self.assertEqual(screen.text().splitlines(), [b'abcd', b'X   ', b'    '])
        screen.feed(b'\x1b[1;1Habcd\rZ')
        self.assertEqual(screen.text().splitlines(), [b'Zbcd', b'X   ', b'    '])

    def test_linefeed_preserves_column_and_only_bottom_linefeed_scrolls(self):
        screen = Screen(3, 4)
        screen.feed(b'abc\nZq')
        self.assertEqual(screen.text().splitlines(), [b'abc ', b'   Z', b'q   '])
        screen.feed(b'\r\nR')
        self.assertEqual(screen.text().splitlines(), [b'   Z', b'q   ', b'R   '])

    def test_bottom_right_cell_does_not_scroll_until_next_printable(self):
        screen = Screen(2, 4)
        screen.feed(b'abcdefgh')
        self.assertEqual(screen.text().splitlines(), [b'abcd', b'efgh'])
        screen.feed(b'i')
        self.assertEqual(screen.text().splitlines(), [b'efgh', b'i   '])
        self.assertNotIn(b'abcd', screen.text())
        self.assertLessEqual(len(screen.cells), 8)

    def test_erase_cancels_pending_wrap_without_moving_cursor(self):
        for erase, expected in [(b'\x1b[K', b'abcX'), (b'\x1b[2J', b'   X')]:
            with self.subTest(erase=erase):
                screen = Screen(2, 4)
                screen.feed(b'abcd' + erase + b'X')
                self.assertEqual(screen.text().splitlines(), [expected, b'    '])

    def test_split_wide_character_uses_two_columns_and_wraps_as_one_glyph(self):
        for raw, expected in [('ab界c'.encode(), ['ab界', 'c   ']),
                              ('abc界'.encode(), ['abc ', '界  '])]:
            with self.subTest(raw=raw):
                screen = Screen(2, 4)
                for byte in raw:
                    screen.feed(bytes([byte]))
                self.assertEqual(screen.text().decode().splitlines(), expected)
                self.assertTrue(all(column < 4 for _, column in screen.cells))

    def test_osc_title_is_never_displayed_or_executed_for_bel_or_st(self):
        for terminator in (b'\x07', b'\x1b\\'):
            with self.subTest(terminator=terminator):
                output = TerminalOutput(2, 12)
                raw = b'keep\x1b]0;Preview\x1b[2Jtitle' + terminator + b'X'
                for byte in raw:
                    output.feed(bytes([byte]))
                self.assertEqual(output.screen.text().splitlines(), [b'keepX       ', b' ' * 12])
                self.assertIn(b'Preview', output.data)
                self.assertFalse(output.has(b'Preview'))

    def test_osc_preserves_delayed_wrap_and_does_not_accumulate_payload(self):
        screen = Screen(2, 4)
        screen.feed(b'abcd\x1b]0;')
        for _ in range(8):
            screen.feed(b'Preview' * 8192)
        self.assertLess(len(screen.pending), 32, 'unterminated OSC buffered its payload')
        screen.feed(b'\x1b')
        screen.feed(b'\\X')
        self.assertEqual(screen.text().splitlines(), [b'abcd', b'X   '])

    def test_native_resize_reset_requires_a_fresh_frame(self):
        output = TerminalOutput(2, 12)
        output.feed(b'Preview')
        output.screen = Screen(3, 8)  # NativeSession's existing explicit resize boundary.
        self.assertFalse(output.has(b'Preview'))
        output.feed(b'\x1b[1;1HPreview')
        self.assertTrue(output.has(b'Preview'))

    def test_incomplete_csi_and_raw_evidence_cannot_exceed_existing_bound(self):
        payload = b'\x1b[' + b';' * (4 * 1024 * 1024)
        for incomplete in (payload, b'\x1b[' + '雪'.encode() * (2 * 1024 * 1024)):
            with self.subTest(encoded_bytes=len(incomplete)), self.assertRaises(AssertionError):
                Screen(2, 4).feed(incomplete)
        output = TerminalOutput(2, 4)
        with self.assertRaises(AssertionError):
            output.feed(payload)
        self.assertLessEqual(len(output.data), 4 * 1024 * 1024)


if __name__ == '__main__':
    unittest.main()
