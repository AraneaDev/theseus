"""Rejected transport records must still join native ownership after acquisition."""
import json
import os
import pathlib
import subprocess
import unittest
from terminal_session import native_fixture


class Protocol(unittest.TestCase):
    def startup(self):
        fixture, metadata = native_fixture()
        return fixture, dict(program=metadata['application']['path'], args=['--no-animation'],
            cwd=str(pathlib.Path('examples/standalone').resolve()), environment=dict(os.environ),
            rows=36, columns=120)

    def test_startup_rejects_unknown_fields_dimensions_and_non_theseus(self):
        for mutation in ('unknown', 'rows', 'program'):
            fixture, startup = self.startup()
            if mutation == 'unknown': startup['shell'] = 'forbidden'
            elif mutation == 'rows': startup['rows'] = 0
            else: startup['program'] = str(fixture)
            result = subprocess.run([str(fixture), 'terminal'], input=json.dumps(startup).encode()+b'\n',
                                    capture_output=True, timeout=15)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(result.stdout, b'')

    def test_bad_records_after_native_acquisition_fail_and_join(self):
        for message in (b'{"op":"shell","text":"forbidden"}\n', b'{"op":"poll", "path":"forbidden"}\n',
                        b'{"op":', b'x' * 524289 + b'\n'):
            fixture, startup = self.startup()
            with subprocess.Popen([str(fixture), 'terminal'], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE) as child:
                child.stdin.write(json.dumps(startup).encode()+b'\n')
                child.stdin.flush()
                self.assertEqual(json.loads(child.stdout.readline()), {'ready': True})
                stdout, stderr = child.communicate(message, timeout=15)
                self.assertEqual(child.returncode, 2, (stdout, stderr))
                self.assertIn(b'terminal:', stderr)

    def test_protocol_eof_after_readiness_completes_owned_cleanup(self):
        fixture, startup = self.startup()
        with subprocess.Popen([str(fixture), 'terminal'], stdin=subprocess.PIPE,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE) as child:
            stdout, stderr = child.communicate(json.dumps(startup).encode()+b'\n', timeout=15)
            self.assertEqual(child.returncode, 0, stderr)
            self.assertEqual(json.loads(stdout), {'ready': True})


if __name__ == '__main__':
    unittest.main()
