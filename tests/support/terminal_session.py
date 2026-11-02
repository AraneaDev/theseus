"""Native test transport. Scenario logic and terminal rendering stay in Python.

The Rust helper owns and joins every native resource; this synchronous, paired
request/response protocol makes no claim about public inherited-stdio servers.
"""
import hashlib
import json
import os
import pathlib
import shutil
import signal
import subprocess
import time
import sys
import tempfile
from contextlib import contextmanager
sys.dont_write_bytecode = True
from terminal_screen import TerminalOutput


@contextmanager
def owned_bridge_stderr(case, root=None):
    assert case and len(case) <= 64 and all(c in 'abcdefghijklmnopqrstuvwxyz0123456789-' for c in case)
    root = pathlib.Path(root or os.environ.get('THESEUS_HOST_EVIDENCE')
                        or tempfile.mkdtemp(prefix='theseus-terminal-evidence-'))
    root.mkdir(parents=True, exist_ok=True)
    # A real file descriptor, not a pipe serviced by an outer test/CI producer.
    # The unique retained file also survives partial startup or blocked cleanup.
    with tempfile.NamedTemporaryFile(mode='w+b', prefix='terminal-' + case + '-',
                                     suffix='.stderr.log', dir=root, delete=False) as output:
        yield output


class SessionDiagnostics:
    operations = {'startup', 'poll', 'input', 'resize', 'signal', 'wait', 'close'}

    def __init__(self, case='session', profile='default', root=None):
        assert case and len(case) <= 64 and all(c in 'abcdefghijklmnopqrstuvwxyz0123456789-' for c in case)
        assert profile in ('default', 'echo-off-vt-off', 'processed-off-vt-on')
        root = root or os.environ.get('THESEUS_HOST_EVIDENCE')
        if root is not None:
            pathlib.Path(root).mkdir(parents=True, exist_ok=True)
        self.directory = pathlib.Path(tempfile.mkdtemp(prefix='terminal-' + case + '-', dir=root))
        self.progress_path = self.directory / 'progress.json'
        self.case, self.profile = case, profile
        self.last_complete = self.provenance = self.stderr_path = None
        self.raw_bytes = 0
        self._publish(self._encode('case-start', None, None))

    def _encode(self, phase, operation, last_complete):
        encoded = json.dumps(dict(case=self.case, profile=self.profile, phase=phase,
            operation=operation, last_complete=last_complete, provenance=self.provenance,
            bridge_stderr=self.stderr_path, state='no-complete-response' if last_complete is None
            else 'last-complete-response'), ensure_ascii=True).encode() + b'\n'
        assert len(encoded) <= 4096, 'terminal progress record exceeded 4 KiB'
        return encoded

    def _publish(self, encoded):
        pending = self.directory / 'progress.pending'
        with pending.open('wb') as output:
            output.write(encoded)
            output.flush()
        os.replace(pending, self.progress_path)

    def bind(self, metadata):
        self.provenance = dict(source_sha=metadata['source_sha'],
            fixture_sha256=metadata['fixture']['sha256'], application_sha256=metadata['application']['sha256'])
        self._publish(self._encode('case-start', None, self.last_complete))

    def begin(self, operation):
        assert operation in self.operations, 'unknown fixed terminal diagnostic operation'
        self._publish(self._encode('operation-begin', operation, self.last_complete))

    def response(self, operation, result, screen):
        assert operation in self.operations, 'unknown fixed terminal diagnostic operation'
        raw = bytes(result['bytes'])
        assert self.raw_bytes + len(raw) <= 4 * 1024 * 1024, 'terminal diagnostic raw exceeded 4 MiB'
        assert len(screen) <= 4 * 1024 * 1024, 'terminal diagnostic screen exceeded 4 MiB'
        actual = {key: result[key] for key in ('modes', 'status', 'eof', 'total')}
        encoded = self._encode('response-complete', operation, actual)
        with (self.directory / 'session.raw').open('ab') as output:
            output.write(raw)
        (self.directory / 'last.screen').write_bytes(screen)
        self._publish(encoded)
        self.raw_bytes += len(raw)
        self.last_complete = actual


def digest(path):
    return hashlib.sha256(pathlib.Path(path).read_bytes()).hexdigest()


def native_fixture():
    path = pathlib.Path(os.environ['THESEUS_HOST_TEST_FIXTURE']).resolve()
    metadata = json.loads(path.with_name('artifacts.json').read_text())
    assert digest(path) == metadata['fixture']['sha256'], 'trusted fixture bytes changed'
    return path, metadata


def sentinel_environment(directory, sentinel):
    directory.mkdir()
    fixture, _ = native_fixture()
    executable = directory / ('docker.exe' if os.name == 'nt' else 'docker')
    shutil.copy2(fixture, executable)
    return dict(os.environ, PATH=str(directory) + os.pathsep + os.environ.get('PATH', ''),
                THESEUS_DOCKER_SENTINEL=str(sentinel))


class NativeSession:
    def __init__(self, binary, args, cwd, environment, rows=36, columns=120, console_profile="default", diagnostics=None):
        self.diagnostics = diagnostics or SessionDiagnostics(profile=console_profile)
        fixture, self.metadata = native_fixture()
        application = pathlib.Path(self.metadata['application']['path'])
        assert pathlib.Path(binary).is_file(), 'Cargo application executable is required'
        assert digest(application) == self.metadata['application']['sha256'], 'pinned Theseus changed'
        self.diagnostics.bind(self.metadata)
        self.args = [str(application), *args]
        self.fixture = fixture
        self.output = TerminalOutput(rows, columns)
        self.returncode = None
        self.modes = None
        self.eof = False
        self.closed = False
        self.last_operation = "startup"
        with owned_bridge_stderr('bridge', self.diagnostics.directory) as diagnostic:
            self.diagnostics.stderr_path = pathlib.Path(diagnostic.name).name
            self.diagnostics.begin('startup')
            self.bridge = subprocess.Popen([str(fixture), 'terminal'], stdin=subprocess.PIPE,
                                           stdout=subprocess.PIPE, stderr=diagnostic)
        startup = dict(program=str(application), args=list(args), cwd=str(pathlib.Path(cwd).resolve()),
                       environment=dict({'TERM': 'xterm-256color'}, **environment), rows=rows, columns=columns,
                       console_profile=console_profile)
        try:
            self._send(startup)
            assert self._receive() == {'ready': True}, 'native terminal startup failed'
            self.pump()
        except BaseException:
            self.close()
            raise

    def _send(self, value):
        data = json.dumps(value, ensure_ascii=False).encode('utf-8') + b'\n'
        assert len(data) <= 262144, 'terminal protocol record bound'
        self.bridge.stdin.write(data)
        self.bridge.stdin.flush()

    def _receive(self):
        data = self.bridge.stdout.readline(20 * 1024 * 1024)
        assert data.endswith(b'\n'), 'terminal helper exited or oversized response'
        return json.loads(data)

    def request(self, operation, **values):
        self.last_operation = operation
        self.diagnostics.begin(operation)
        self._send(dict(op=operation, **values))
        result = self._receive()
        self.output.feed(bytes(result['bytes']))
        self.returncode = result['status']
        self.modes = result['modes']
        self.eof = result['eof']
        self.diagnostics.response(operation, result, self.output.screen.text())
        return result

    def pump(self):
        self.request('poll')
        time.sleep(.01)

    def key(self, data):
        self.request('input', bytes=list(data))

    def resize(self, rows, columns):
        # Preserve the existing reconstruction reset at explicit resize boundaries.
        from terminal_screen import Screen
        self.output.screen = Screen(rows, columns)
        self.request('resize', rows=rows, columns=columns)

    def poll(self):
        if self.returncode is None:
            self.pump()
        return self.returncode

    def send_signal(self, event):
        self.request('signal', event='interrupt' if event == signal.SIGINT else 'terminate')

    def wait(self, timeout=15):
        self.diagnostics.begin('wait')
        deadline = time.monotonic() + timeout
        while self.poll() is None and time.monotonic() < deadline:
            pass
        if self.returncode is None:
            raise subprocess.TimeoutExpired('owned terminal client', timeout)
        return self.returncode

    def kill(self):
        self.request('finish')

    def assert_restored(self):
        assert self.returncode is not None and self.eof, 'client or native EOF incomplete'
        assert self.modes['before'] == self.modes['current'], self.modes
        data = self.output.data
        assert b'\x1b[?1049h' in data and b'\x1b[?1049l' in data, 'alternate screen not restored'

    def close(self):
        if self.closed:
            return
        self.closed = True
        try:
            try:
                self.diagnostics.begin('close')
            finally:
                # Even a failed evidence write must settle every existing owner.
                if self.bridge.poll() is None:
                    self.bridge.stdin.close()  # EOF synchronously asks native ownership cleanup.
                try:
                    code = self.bridge.wait(timeout=15)
                except subprocess.TimeoutExpired as error:
                    # A diagnostic deadline cannot orphan the owning helper.
                    self.bridge.wait()
                    raise AssertionError('terminal cleanup exceeded diagnostic deadline') from error
                assert code == 0, 'terminal helper cleanup failed'
        finally:
            self.bridge.stdout.close()
        assert (self.diagnostics.directory / self.diagnostics.stderr_path).stat().st_size <= 4 * 1024 * 1024, 'terminal diagnostic stderr exceeded 4 MiB'
        assert digest(self.fixture) == self.metadata['fixture']['sha256']
        assert digest(self.metadata['application']['path']) == self.metadata['application']['sha256']
