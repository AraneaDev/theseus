"""Public interruption PTY conditions must describe currently displayed proof."""
import sys
import unittest

sys.dont_write_bytecode = True
from terminal_screen import TerminalOutput
from tui_interruption import comparison_visible, proof_visible


class InterruptionVisibleTests(unittest.TestCase):
    def test_cursor_updates_reconstruct_actual_command_proof(self):
        output = TerminalOutput(50, 100)
        for chunk in [b'\x1b[2;1HInterruption ContainerSigkill: readiness=false stopped=false',
                      b'\x1b[2;42Htrue stopped=false\x1b[2;', b'55Htrue\x1b[K']:
            output.feed(chunk)
        self.assertNotIn(b'readiness=true', output.data)
        self.assertTrue(proof_visible(output))

    def test_erased_or_unverified_proof_cannot_pass_from_history(self):
        for erase in [b'\x1b[2J', b'\x1b[2;1H\x1b[2K',
                      b'\x1b[2;55Hfalse\x1b[K']:
            with self.subTest(erase=erase):
                output = TerminalOutput(50, 100)
                output.feed(b'\x1b[2;1HInterruption ContainerSigkill: readiness=true stopped=true')
                self.assertTrue(proof_visible(output))
                output.feed(erase)
                self.assertFalse(proof_visible(output))

    def test_cursor_updates_reconstruct_each_exact_comparison(self):
        for checkpoint in ['interrupted', 'recovered']:
            with self.subTest(checkpoint=checkpoint):
                output = TerminalOutput(50, 110)
                output.feed(b'\x1b[2;1HComparison: before_install')
                output.feed(b'\x1b[2;20Hinterrupt -> ' + checkpoint.encode())
                self.assertNotIn(b'before_interrupt', output.data)
                self.assertTrue(comparison_visible(output, checkpoint))
                output.feed(b'\x1b[2;1H\x1b[2K')
                self.assertFalse(comparison_visible(output, checkpoint))

    def test_unrelated_names_are_not_a_comparison(self):
        output = TerminalOutput(50, 110)
        output.feed(b'before_interrupt\x1b[3;1Hinterrupted')
        self.assertFalse(comparison_visible(output, 'interrupted'))

    def test_resized_proof_and_comparison_require_new_bottom_footer(self):
        output = TerminalOutput(32, 60)
        output.feed(b'\x1b[2;1HInterruption ContainerSigkill: readiness=true stopped=true'
                    b'\x1b[3;1HComparison: before_interrupt -> interrupted'
                    b'\x1b[30;1HTab/Enter focus')
        output.screen.rows, output.screen.columns = 50, 110
        self.assertFalse(proof_visible(output, resized=True))
        self.assertFalse(comparison_visible(output, 'interrupted', resized=True))
        output.feed(b'\x1b[50;1HTab/Enter focus')
        self.assertFalse(proof_visible(output, resized=True))
        self.assertFalse(comparison_visible(output, 'interrupted', resized=True))
        output.feed(b'\x1b[1;110H\xe2\x94\x90')
        self.assertTrue(proof_visible(output, resized=True))
        self.assertTrue(comparison_visible(output, 'interrupted', resized=True))
        self.assertFalse(comparison_visible(output, 'recovered', resized=True))
        output.feed(b'\x1b[50;1H\x1b[2K')
        self.assertFalse(proof_visible(output, resized=True))
        self.assertFalse(comparison_visible(output, 'interrupted', resized=True))


if __name__ == '__main__':
    unittest.main()
