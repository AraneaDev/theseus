"""Real diagnostic pipe-producer lifetime and bounded progress-record controls."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

sys.dont_write_bytecode = True
from terminal_session import NativeSession, SessionDiagnostics, native_fixture, owned_bridge_stderr


def line_from_pipe(stream, seconds=5):
    os.set_blocking(stream.fileno(), False)
    result = bytearray()
    until = time.monotonic() + seconds
    while time.monotonic() < until:
        try:
            chunk = os.read(stream.fileno(), 4096)
        except BlockingIOError:
            chunk = None
        if chunk:
            result.extend(chunk)
            if result.endswith(b'\n'):
                return bytes(result)
        elif chunk == b'':
            break
        time.sleep(.005)
    raise AssertionError(('controlled child did not publish complete record', bytes(result)))


def producer():
    print('partial native phase', file=sys.stderr, flush=True)
    print('producer-ready', flush=True)
    sys.stdin.buffer.read(1)


def driver(root):
    child = None
    observed = {}
    try:
        with owned_bridge_stderr('descriptor', root) as diagnostic:
            path = diagnostic.name if diagnostic is not None else None
            child = subprocess.Popen([sys.executable, '-B', __file__, '--producer'],
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=diagnostic)
        ready = line_from_pipe(child.stdout)
        os.close(2)  # Release this process's producer; the nested child remains live.
        sys.stderr = open(os.devnull, 'w')
        print(json.dumps({'ready': ready.decode().strip(), 'child_live': child.poll() is None}), flush=True)
        sys.stdin.buffer.read(1)
        observed = {'child_live_after_observation': child.poll() is None, 'stderr_path': path}
    finally:
        if child is not None:
            child.stdin.close()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
            child.stdout.close()
            observed['child_reaped'] = child.returncode is not None
    print(json.dumps(observed), flush=True)


class ProducerLifetimeTests(unittest.TestCase):
    def test_nested_live_bridge_cannot_keep_original_diagnostic_pipe_open(self):
        with tempfile.TemporaryDirectory() as root:
            read_fd, write_fd = os.pipe()
            outer = None
            observation = {}
            try:
                outer = subprocess.Popen([sys.executable, '-B', __file__, '--driver', root],
                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=write_fd)
                os.close(write_fd)
                write_fd = None
                ready = json.loads(line_from_pipe(outer.stdout))
                os.set_blocking(read_fd, False)
                until = time.monotonic() + 1
                eof = False
                captured = bytearray()
                while time.monotonic() < until:
                    try:
                        chunk = os.read(read_fd, 4096)
                    except BlockingIOError:
                        chunk = None
                    if chunk == b'':
                        eof = True
                        break
                    if chunk:
                        captured.extend(chunk)
                    time.sleep(.005)
                observation = {'original_pipe_eof': eof, 'outer_live_at_observation': outer.poll() is None,
                               'ready': ready, 'captured_original_pipe': captured.decode()}
            finally:
                if write_fd is not None:
                    os.close(write_fd)
                if outer is not None:
                    outer.stdin.write(b'!')
                    outer.stdin.close()
                    outer.stdin = None
                    # The driver owns and rescues its producer before returning.
                    os.set_blocking(outer.stdout.fileno(), True)
                    remaining, _ = outer.communicate(timeout=10)
                    observation['driver_exit'] = outer.returncode
                    observation.update(json.loads(remaining))
                os.close(read_fd)
            print(json.dumps(observation), flush=True)
            # Assertions happen only after both controlled processes are reaped.
            self.assertTrue(observation['outer_live_at_observation'])
            self.assertTrue(observation['ready']['child_live'])
            self.assertTrue(observation['child_live_after_observation'])
            self.assertTrue(observation['child_reaped'])
            self.assertEqual(observation['driver_exit'], 0)
            self.assertTrue(observation['original_pipe_eof'], observation)
            self.assertEqual(observation['captured_original_pipe'], '')
            self.assertIn('partial native phase', Path(observation['stderr_path']).read_text())


class ProgressRecordTests(unittest.TestCase):
    def test_start_record_exists_before_any_session_or_response(self):
        with tempfile.TemporaryDirectory() as root:
            diagnostic = SessionDiagnostics('quit', 'default', root)
            self.assertTrue(diagnostic.progress_path.is_file(), 'case start was not published')
            record = json.loads(diagnostic.progress_path.read_text())
            self.assertEqual(record['case'], 'quit')
            self.assertEqual(record['profile'], 'default')
            self.assertEqual(record['phase'], 'case-start')
            self.assertEqual(record['state'], 'no-complete-response')
            self.assertIsNone(record['last_complete'])
            self.assertIsNone(record['provenance'])
            self.assertNotIn('modes', record)

    def test_preoperation_record_keeps_actual_last_response_and_separate_raw_screen(self):
        with tempfile.TemporaryDirectory() as root:
            diagnostic = SessionDiagnostics('quit', 'default', root)
            diagnostic.bind({'source_sha': 'a' * 40, 'fixture': {'sha256': 'b' * 64},
                             'application': {'sha256': 'c' * 64}})
            diagnostic.begin('startup')
            result = {'bytes': list(b'raw'), 'modes': {'kind': 'console', 'before': [503, 7],
                       'current': [496, 7], 'raw': True}, 'status': None, 'eof': False, 'total': 3}
            diagnostic.response('poll', result, b'displayed cells')
            diagnostic.begin('resize')
            self.assertTrue(diagnostic.progress_path.is_file(), 'operation record missing')
            record = json.loads(diagnostic.progress_path.read_text())
            self.assertEqual(record['phase'], 'operation-begin')
            self.assertEqual(record['operation'], 'resize')
            self.assertEqual(record['last_complete'], {key: result[key] for key in ('modes', 'status', 'eof', 'total')})
            self.assertEqual(record['provenance']['source_sha'], 'a' * 40)
            self.assertEqual((diagnostic.directory / 'session.raw').read_bytes(), b'raw')
            self.assertEqual((diagnostic.directory / 'last.screen').read_bytes(), b'displayed cells')
            diagnostic.response('poll', {**result, 'bytes': [], 'status': 0, 'eof': True}, b'final cells')
            diagnostic.begin('close')
            record = json.loads(diagnostic.progress_path.read_text())
            self.assertEqual(record['last_complete']['status'], 0)
            self.assertIs(record['last_complete']['eof'], True)

    def test_unknown_oversized_records_fail_without_replacing_prior_evidence(self):
        with tempfile.TemporaryDirectory() as root:
            diagnostic = SessionDiagnostics('quit', 'default', root)
            with self.assertRaises(AssertionError):
                diagnostic.begin('shell')
            self.assertTrue(diagnostic.progress_path.is_file())
            before = diagnostic.progress_path.read_bytes()
            with self.assertRaises(AssertionError):
                diagnostic.response('poll', {'bytes': [], 'modes': {'unbounded': 'x' * 8192},
                                    'status': None, 'eof': False, 'total': 0}, b'')
            self.assertEqual(diagnostic.progress_path.read_bytes(), before)
            with self.assertRaises(AssertionError):
                diagnostic.response('poll', {'bytes': [0] * (4 * 1024 * 1024 + 1),
                                    'modes': {}, 'status': None, 'eof': False, 'total': 0}, b'')
            self.assertEqual(diagnostic.progress_path.read_bytes(), before)

    def test_case_directories_and_bridge_files_are_unique_and_retained(self):
        with tempfile.TemporaryDirectory() as root:
            first = SessionDiagnostics('quit', 'default', root)
            second = SessionDiagnostics('quit', 'default', root)
            self.assertNotEqual(first.directory, second.directory)
            with owned_bridge_stderr('bridge', first.directory) as one, owned_bridge_stderr('bridge', first.directory) as two:
                self.assertNotEqual(one.name, two.name)
                one.write(b'partial phase')
                path = Path(one.name)
            self.assertEqual(path.read_bytes(), b'partial phase')


class NativeFinishTests(unittest.TestCase):
    def test_existing_kill_finishes_live_native_session_and_settles_owned_bridge(self):
        _, metadata = native_fixture()
        diagnostic = SessionDiagnostics('forced-finish')
        session = NativeSession(metadata['application']['path'], ['--no-animation'],
            str(Path(__file__).resolve().parents[2] / 'examples/standalone'), os.environ,
            diagnostics=diagnostic)
        observation = {}
        try:
            until = time.monotonic() + 8
            while not session.output.has(b'Preview') and time.monotonic() < until:
                session.pump()
            self.assertTrue(session.output.has(b'Preview'), session.output.screen.text())
            live = session.request('poll')
            self.assertIsNone(live['status'])
            self.assertFalse(live['eof'])
            self.assertIsNone(session.bridge.poll())
            observation['live_before_finish'] = True
            try:
                session.kill()  # Existing forced cleanup API, not a synthetic record.
                observation['finish_error'] = None
            except BaseException as error:
                observation['finish_error'] = repr(error)
            observation['status_after_finish'] = session.returncode
            observation['eof_after_finish'] = session.eof
            observation['finish_record'] = json.loads(diagnostic.progress_path.read_text())
            if observation['finish_error'] is None:
                observation['wait_status'] = session.wait()
        finally:
            # RED's rejected request must still rescue the live native owner.
            session.close()
            observation['bridge_exit'] = session.bridge.returncode
            observation['bridge_stdout_closed'] = session.bridge.stdout.closed
            (diagnostic.directory / 'forced-finish-observation.json').write_text(json.dumps(observation))
        print(json.dumps(observation), flush=True)
        self.assertIsNone(observation['finish_error'], observation)
        self.assertTrue(observation['live_before_finish'])
        self.assertIsInstance(observation['status_after_finish'], int)
        self.assertTrue(observation['eof_after_finish'])
        self.assertEqual(observation['wait_status'], observation['status_after_finish'])
        self.assertEqual(observation['finish_record']['phase'], 'response-complete')
        self.assertEqual(observation['finish_record']['operation'], 'finish')
        self.assertTrue(observation['finish_record']['last_complete']['eof'])
        self.assertEqual(observation['bridge_exit'], 0)
        self.assertTrue(observation['bridge_stdout_closed'])


if __name__ == '__main__':
    if sys.argv[1:] == ['--producer']:
        producer()
    elif len(sys.argv) == 3 and sys.argv[1] == '--driver':
        driver(sys.argv[2])
    else:
        unittest.main()
