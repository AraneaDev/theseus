"""Rejected transport records must still join native ownership after acquisition."""
from contextlib import contextmanager
import json
import os
import pathlib
import subprocess
import unittest
from terminal_session import native_fixture


@contextmanager
def diagnostic_stderr(case):
    if os.environ.get('THESEUS_TERMINAL_TRACE') == '1':
        # Fixed case names, a CI-owned evidence directory, and no tee worker.
        # The native child writes directly to this file, preserving partial
        # diagnostics even when communicate/wait cannot complete.
        root = pathlib.Path(os.environ['THESEUS_HOST_EVIDENCE'])
        with (root / ('terminal-protocol-' + case + '.stderr.log')).open('w+b') as output:
            yield output
    else:
        yield subprocess.PIPE


def captured_stderr(target, data):
    if target == subprocess.PIPE:
        return data
    target.flush()
    target.seek(0)
    result = target.read(1024 * 1024 + 1)
    assert len(result) <= 1024 * 1024, 'bounded protocol diagnostic stderr'
    return result


class Protocol(unittest.TestCase):
    def startup(self):
        fixture, metadata = native_fixture()
        return fixture, dict(program=metadata['application']['path'], args=['--no-animation'],
            cwd=str(pathlib.Path('examples/standalone').resolve()), environment=dict(os.environ),
            rows=36, columns=120)

    def test_startup_rejects_unknown_fields_dimensions_and_non_theseus(self):
        for mutation in ('unknown', 'rows', 'program', 'profile'):
            fixture, startup = self.startup()
            if mutation == 'unknown': startup['shell'] = 'forbidden'
            elif mutation == 'rows': startup['rows'] = 0
            elif mutation == 'profile': startup['console_profile'] = 'arbitrary-mode'
            else: startup['program'] = str(fixture)
            with diagnostic_stderr('startup-' + mutation) as diagnostic:
                result = subprocess.run([str(fixture), 'terminal'], input=json.dumps(startup).encode()+b'\n',
                                        stdout=subprocess.PIPE, stderr=diagnostic, timeout=15)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(result.stdout, b'')

    def test_bad_records_after_native_acquisition_fail_and_join(self):
        for index, message in enumerate((b'{"op":"shell","text":"forbidden"}\n', b'{"op":"poll", "path":"forbidden"}\n',
                        b'{"op":', b'x' * 524289 + b'\n')):
            fixture, startup = self.startup()
            with diagnostic_stderr('acquired-' + str(index)) as diagnostic, subprocess.Popen(
                    [str(fixture), 'terminal'], stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE, stderr=diagnostic) as child:
                child.stdin.write(json.dumps(startup).encode()+b'\n')
                child.stdin.flush()
                self.assertEqual(json.loads(child.stdout.readline()), {'ready': True})
                stdout, stderr = child.communicate(message, timeout=15)
                stderr = captured_stderr(diagnostic, stderr)
                self.assertEqual(child.returncode, 2, (stdout, stderr))
                self.assertIn(b'terminal:', stderr)

    def test_protocol_eof_after_readiness_completes_owned_cleanup(self):
        fixture, startup = self.startup()
        with diagnostic_stderr('eof') as diagnostic, subprocess.Popen(
                [str(fixture), 'terminal'], stdin=subprocess.PIPE,
                stdout=subprocess.PIPE, stderr=diagnostic) as child:
            stdout, stderr = child.communicate(json.dumps(startup).encode()+b'\n', timeout=15)
            stderr = captured_stderr(diagnostic, stderr)
            self.assertEqual(child.returncode, 0, stderr)
            self.assertEqual(json.loads(stdout), {'ready': True})


if __name__ == '__main__':
    unittest.main()
