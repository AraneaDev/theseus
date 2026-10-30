"""Current detail, selection and geometry regressions for interrupted rollback PTYs."""
import sys
import unittest
sys.dont_write_bytecode = True
from terminal_screen import Screen
from tui_interrupted_rollback import detail_visible, select_detail


def literal_screen():
    screen = Screen(12, 100)
    screen.feed(('\x1b[1;1H' + '┌Theseus' + '─' * 91 + '┐' +
                 '\x1b[3;1H│> candidate-changed-state passed Comparison: before_interrupt -> interrupted' +
                 '\x1b[5;21H┌Detail — record 1 / 8' + '─' * 59 + '┐' +
                 '\x1b[6;21H│Interrupted rollback verified:' + ' ' * 49 + '│' +
                 '\x1b[9;21H└' + '─' * 78 + '┘' +
                 '\x1b[11;1HTab/Enter focus').encode())
    return screen


class CurrentDetailTests(unittest.TestCase):
    def test_list_row_and_summary_cannot_substitute_for_selected_check(self):
        screen = literal_screen()
        self.assertFalse(detail_visible(screen, b'candidate-changed-state passed',
                                        b'Comparison: before_interrupt -> interrupted'))
        screen.feed(b'\x1b[6;22Hcandidate-changed-state passed\x1b[K'
                    b'\x1b[7;22HComparison: before_interrupt -> interrupted')
        self.assertTrue(detail_visible(screen, b'candidate-changed-state passed',
                                       b'Comparison: before_interrupt -> interrupted'))
        screen.feed(b'\x1b[7;22H\x1b[K')
        self.assertFalse(detail_visible(screen, b'candidate-changed-state passed',
                                        b'Comparison: before_interrupt -> interrupted'))

    def test_cursor_updated_proof_and_wrapped_reference_use_current_cells(self):
        screen = literal_screen()
        screen.feed(b'\x1b[6;22Hknown-good-reader passed\x1b[K'
                    b'\x1b[7;22HReader/runtime expectation: exit=0 matched=false\x1b[7;65Htrue\x1b[K')
        self.assertTrue(detail_visible(screen, b'known-good-reader passed', b'matched=true'))
        screen.feed(b'\x1b[7;65Hfalse\x1b[K')
        self.assertFalse(detail_visible(screen, b'known-good-reader passed', b'matched=true'))
        screen.feed(b'\x1b[7;22HScope: /workspace/rollback-backup/\x1b[K\x1b[8;22Hsaved')
        self.assertTrue(detail_visible(screen, b'/workspace/rollback-backup/saved'))

    def test_resize_requires_new_complete_geometry_and_footer(self):
        screen = literal_screen()
        self.assertTrue(detail_visible(screen, b'Interrupted rollback verified:'))
        screen.rows, screen.columns = 15, 110
        self.assertFalse(detail_visible(screen, b'Interrupted rollback verified:'))
        screen.feed(('\x1b[1;110H┐\x1b[14;1HTab/Enter focus').encode())
        self.assertFalse(detail_visible(screen, b'Interrupted rollback verified:'))
        screen.feed(('\x1b[9;21H└' + '─'*88 + '┘').encode())
        self.assertTrue(detail_visible(screen, b'Interrupted rollback verified:'))

    def test_live_finished_path_can_fill_footer_and_hide_keyboard_help(self):
        screen = literal_screen()
        screen.feed(b'\x1b[11;1HFinished | /tmp/long-owned-run-directory/report.json\x1b[K'
                    b'\x1b[12;1Hcontinued/long-path')
        self.assertTrue(detail_visible(screen, b'Interrupted rollback verified:'))

    def test_reader_and_version_are_individual_proofs(self):
        screen = literal_screen()
        screen.feed(b'\x1b[6;22Hknown-good-reader passed\x1b[K'
                    b'\x1b[7;22HReader/runtime expectation: matched=true reader records=2 sum=40')
        self.assertTrue(detail_visible(screen, b'known-good-reader passed', b'reader records=2 sum=40'))
        self.assertFalse(detail_visible(screen, b'known-good-version-restored passed', b'sample-cli 1.0.0'))
        screen.feed(b'\x1b[6;22Hknown-good-version-restored passed\x1b[K'
                    b'\x1b[7;22HReader/runtime expectation: matched=true sample-cli 1.0.0\x1b[K')
        self.assertTrue(detail_visible(screen, b'known-good-version-restored passed', b'sample-cli 1.0.0'))
        self.assertFalse(detail_visible(screen, b'known-good-reader passed', b'reader records=2 sum=40'))

    def test_old_expected_marker_is_not_selected_actual_stdout(self):
        screen = literal_screen()
        screen.feed(b'\x1b[6;22HReader/runtime expectation: reader records=2 sum=40\x1b[K')
        checked = []
        def until(predicate):
            checked.append(predicate())
        select_detail(lambda keys: None, until, screen, 'owned-reader stdout', b'reader records=2 sum=40')
        self.assertEqual(checked, [False], 'old expected marker passed as actual stdout')
        screen.feed(('\x1b[3;1H│> owned-reader stdout\x1b[K'
                     '\x1b[5;21H┌Detail — record 1 / 1' + '─' * 59 + '┐' +
                     '\x1b[6;22Hreader records=2 sum=40\x1b[K'
                     '\x1b[10;21Hraw bytes 0..78 / 78 | last stored chunk').encode())
        select_detail(lambda keys: None, until, screen, 'owned-reader stdout', b'reader records=2 sum=40')
        self.assertEqual(checked, [False, True])

    def test_semantic_filter_selects_check_without_stealing_stage_focus(self):
        screen = literal_screen()
        sent = []
        def send(keys):
            sent.append(keys)
            screen.feed(('\x1b[5;21H┌Detail — record 1 / 1' + '─' * 59 + '┐').encode())
            screen.feed(b'\x1b[6;22Hcandidate-changed-state passed\x1b[K')
        def until(predicate):
            self.assertTrue(predicate())
        select_detail(send, until, screen, 'candidate-changed-state', b'candidate-changed-state passed')
        self.assertEqual(sent, [b'/candidate-changed-state\r'])


if __name__ == '__main__':
    unittest.main()
