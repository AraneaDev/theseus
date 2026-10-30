"""Public interruption PTY conditions must describe currently displayed proof."""
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
from terminal_screen import TerminalOutput
from tui_interruption import comparison_visible, proof_visible, run_headless


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


class HeadlessEvidenceTests(unittest.TestCase):
    def test_timeout_retains_partial_output_and_artifacts_after_fixture_removal(self):
        for stderr in [b'err', None]:
            with self.subTest(stderr=stderr), tempfile.TemporaryDirectory() as evidence_tmp:
                evidence = pathlib.Path(evidence_tmp)
                error = subprocess.TimeoutExpired(['theseus', 'run', '--headless'], 150,
                                                  output=b'out', stderr=stderr)
                with self.assertRaises(subprocess.TimeoutExpired) as raised:
                    with tempfile.TemporaryDirectory() as fixture_tmp:
                        root = pathlib.Path(fixture_tmp)
                        saved = root / '.theseus-runs' / 'owned-run'
                        saved.mkdir(parents=True)
                        (saved/'report.json').write_bytes(b'{"outcome":"incomplete"}')
                        (saved/'journal.json').write_bytes(b'{"resources":[]}')
                        with patch.dict('os.environ', {'THESEUS_INTERRUPTION_EVIDENCE': str(evidence)}):
                            with patch('tui_interruption.subprocess.run', side_effect=error):
                                run_headless('theseus', root, {})
                self.assertIs(raised.exception, error)
                self.assertFalse(root.exists())
                captured = evidence/'headless'
                self.assertTrue((captured/'stdout').is_file(), 'timeout evidence was not retained')
                self.assertEqual((captured/'stdout').read_bytes(), b'out')
                self.assertEqual((captured/'stderr').read_bytes(), stderr or b'')
                self.assertEqual((captured/'report.json').read_bytes(), b'{"outcome":"incomplete"}')
                self.assertEqual((captured/'journal.json').read_bytes(), b'{"resources":[]}')
                self.assertEqual((captured/'status').read_text(), 'unknown')
                self.assertTrue((captured/'capture.json').is_file(), 'capture metadata was not retained')
                metadata = json.loads((captured/'capture.json').read_text())
                self.assertFalse(metadata['capture_complete'])
                self.assertIsNone(metadata['returncode'])
                self.assertEqual(metadata['streams']['stdout'], {
                    'available_captured_bytes': 3, 'stored_window_bytes': 3, 'window_truncated': False})
                self.assertEqual(metadata['streams']['stderr']['available_captured_bytes'],
                                 3 if stderr else 0)

    def test_oversized_stream_records_available_bytes_and_truncated_window(self):
        with tempfile.TemporaryDirectory() as evidence_tmp, tempfile.TemporaryDirectory() as fixture_tmp:
            evidence = pathlib.Path(evidence_tmp)
            result = subprocess.CompletedProcess(['theseus', 'run', '--headless'], 0,
                                                 stdout=b'x' * 4194311, stderr=b'w' * 4194307)
            with patch.dict('os.environ', {'THESEUS_INTERRUPTION_EVIDENCE': str(evidence)}):
                with patch('tui_interruption.subprocess.run', return_value=result):
                    self.assertIs(run_headless('theseus', pathlib.Path(fixture_tmp), {}), result)
            captured = evidence/'headless'
            self.assertEqual((captured/'stdout').read_bytes(), b'x' * 4194304)
            self.assertEqual((captured/'stderr').read_bytes(), b'w' * 4194304)
            self.assertTrue((captured/'capture.json').is_file(), 'capture metadata was not retained')
            metadata = json.loads((captured/'capture.json').read_text())
            self.assertTrue(metadata['capture_complete'])
            self.assertEqual(metadata['returncode'], 0)
            self.assertEqual(metadata['streams']['stdout'], {
                'available_captured_bytes': 4194311, 'stored_window_bytes': 4194304,
                'window_truncated': True})
            self.assertEqual(metadata['streams']['stderr'], {
                'available_captured_bytes': 4194307, 'stored_window_bytes': 4194304,
                'window_truncated': True})

    def test_small_completed_capture_keeps_real_status_and_complete_streams(self):
        with tempfile.TemporaryDirectory() as evidence_tmp, tempfile.TemporaryDirectory() as fixture_tmp:
            evidence = pathlib.Path(evidence_tmp)
            result = subprocess.CompletedProcess(['theseus', 'run', '--headless'], 17,
                                                 stdout=b'hello', stderr=b'error')
            def completed_run(args, **kwargs):
                self.assertEqual(args, ['theseus', 'run', '--headless'])
                self.assertEqual(kwargs, {'cwd': pathlib.Path(fixture_tmp), 'capture_output': True,
                                          'env': {'fixture': 'value'}, 'timeout': 150})
                return result
            with patch.dict('os.environ', {'THESEUS_INTERRUPTION_EVIDENCE': str(evidence)}):
                with patch('tui_interruption.subprocess.run', side_effect=completed_run):
                    self.assertIs(run_headless('theseus', pathlib.Path(fixture_tmp),
                                               {'fixture': 'value'}), result)
            captured = evidence/'headless'
            self.assertEqual((captured/'stdout').read_bytes(), b'hello')
            self.assertEqual((captured/'stderr').read_bytes(), b'error')
            self.assertEqual((captured/'status').read_text(), '17')
            self.assertTrue((captured/'capture.json').is_file(), 'capture metadata was not retained')
            metadata = json.loads((captured/'capture.json').read_text())
            self.assertEqual(metadata, {'capture_complete': True, 'returncode': 17, 'streams': {
                'stdout': {'available_captured_bytes': 5, 'stored_window_bytes': 5,
                           'window_truncated': False},
                'stderr': {'available_captured_bytes': 5, 'stored_window_bytes': 5,
                           'window_truncated': False}}})


if __name__ == '__main__':
    unittest.main()
