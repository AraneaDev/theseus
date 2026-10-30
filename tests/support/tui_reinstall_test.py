"""The reinstall acceptance must inspect current cells, not terminal history."""
import sys
import unittest

sys.dont_write_bytecode = True
from tui_reinstall import TerminalOutput, comparison_visible


class ReinstallVisibleTests(unittest.TestCase):
    def test_cursor_diffs_form_comparison_without_complete_raw_words(self):
        output = TerminalOutput(32, 60)
        for chunk in [b'\x1b[2J\x1b[2;1HComparison: before_install',
                      b'\x1b[2;20Hreinstall -> installed', b'\x1b[2;', b'33Hreinstalled']:
            output.feed(chunk)
        self.assertNotIn(b'before_reinstall', output.data)
        self.assertTrue(comparison_visible(output))

    def test_erased_or_overwritten_comparison_is_not_visible(self):
        for erase in [b'\x1b[2J', b'\x1b[2;1H\x1b[2K', b'\x1b[2;1Hother-stage\x1b[K']:
            with self.subTest(erase=erase):
                output = TerminalOutput(32, 60)
                output.feed(b'\x1b[2;1HComparison: before_reinstall -> reinstalled')
                self.assertTrue(comparison_visible(output))
                output.feed(erase)
                self.assertFalse(comparison_visible(output))

    def test_split_unicode_and_controls_preserve_cell_positions(self):
        output = TerminalOutput(32, 60)
        for chunk in [b'\x1b[2;1H\xe7', b'\x95\x8c Comparison: before_install\x1b[2;',
                      b'23Hreinstall -> reinstalled\x1b[3;1H\xe2\x86', b'\x92']:
            output.feed(chunk)
        self.assertNotIn(b'before_reinstall', output.data)
        self.assertTrue(comparison_visible(output))

    def test_stage_names_without_a_comparison_reference_do_not_pass(self):
        output = TerminalOutput(50, 82)
        output.feed(b'\x1b[2;1Hbefore_reinstall\x1b[3;1Hreinstalled'
                    b'\x1b[50;1HTab/Enter focus')
        self.assertFalse(comparison_visible(output, resized=True))

    def test_resize_requires_comparison_and_footer_from_new_frame(self):
        narrow = TerminalOutput(32, 60)
        narrow.feed(b'\x1b[2;1HComparison: before_reinstall -> reinstalled'
                    b'\x1b[30;1HFinished | saved/report.json | Tab/Enter focus')
        narrow.screen.rows, narrow.screen.columns = 50, 82
        self.assertFalse(comparison_visible(narrow, resized=True))
        fresh = TerminalOutput(50, 82)
        fresh.feed(b'\x1b[50;1HTab/Enter focus')
        self.assertFalse(comparison_visible(fresh, resized=True))
        fresh.feed(b'\x1b[2;1HComparison: before_reinstall -> reinstalled')
        self.assertFalse(comparison_visible(fresh, resized=True))
        fresh.feed(b'\x1b[1;82H\xe2\x94\x90')
        self.assertTrue(comparison_visible(fresh, resized=True))
        fresh.feed(b'\x1b[50;1H\x1b[2K')
        self.assertFalse(comparison_visible(fresh, resized=True))


if __name__ == '__main__':
    unittest.main()
