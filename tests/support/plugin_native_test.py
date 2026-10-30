"""Host-only native harness failure paths; no Docker or native client execution."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import plugin_native as native


def stalled_inspection(argv, **kwargs):
    # Model an unresponsive daemon at the subprocess boundary. Without a
    # deadline it cannot provide either an image ID or successful absence.
    if kwargs.get('timeout') is not None:
        raise subprocess.TimeoutExpired(argv, kwargs['timeout'])
    return subprocess.CompletedProcess(argv, 125, '', 'synthetic unresponsive daemon')


class InspectionDeadlines(unittest.TestCase):
    def assert_timeout_diagnostic(self, error, target):
        self.assertEqual(error.timeout, 10)
        self.assertIn(target, error.cmd)
        self.assertIn('docker', str(error))
        self.assertIn('10 seconds', str(error))

    def test_prerequisite_stall_propagates_with_image_and_deadline(self):
        with tempfile.TemporaryDirectory() as directory:
            client = Path(directory) / 'never-executed-elf'
            client.write_bytes(b'\x7fELFsynthetic')
            with patch.dict(os.environ, {'THESEUS_PLUGIN_CLIENT': str(client),
                                        'THESEUS_PLUGIN_CLIENT_VERSION': 'synthetic'}), \
                    patch.object(native.subprocess, 'run', side_effect=stalled_inspection):
                with self.assertRaises(subprocess.TimeoutExpired) as caught:
                    native.prerequisites()
            self.assert_timeout_diagnostic(caught.exception, native.IMAGE)

    def test_cleanup_stall_cannot_be_accepted_as_owned_container_absence(self):
        report = {
            'schema_version': 8, 'outcome': 'passed', 'errors': [],
            'image_id': native.IMAGE, 'network': 'none',
            'artifacts': [{'name': 'target', 'files': [
                {'path': 'client/claude', 'sha256': 'synthetic-sha', 'size': 4}]}],
            'scenarios': [{'plugin': {'scope': 'user', 'client_version': 'synthetic'},
                           'teardown': {'status': 'complete'}, 'container_id': 'owned-synthetic'}],
        }
        readback = subprocess.CompletedProcess([], 0, json.dumps(report), '')
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with patch.object(native, 'command', return_value=readback), \
                    patch.object(native.subprocess, 'run', side_effect=stalled_inspection):
                with self.assertRaises(subprocess.TimeoutExpired) as caught:
                    native.verify_common('never-executed', root, 'cleanup', root / 'report.json',
                                         'passed', 'user', 'synthetic', 'synthetic-sha', 4)
            self.assert_timeout_diagnostic(caught.exception, 'owned-synthetic')
            self.assertFalse((root / 'cleanup-inspect.stderr').exists())


if __name__ == '__main__':
    unittest.main()
