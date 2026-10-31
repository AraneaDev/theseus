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
sys.dont_write_bytecode = True
from terminal_screen import TerminalOutput


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
    def __init__(self, binary, args, cwd, environment, rows=36, columns=120):
        fixture, self.metadata = native_fixture()
        application = pathlib.Path(self.metadata['application']['path'])
        assert pathlib.Path(binary).is_file(), 'Cargo application executable is required'
        assert digest(application) == self.metadata['application']['sha256'], 'pinned Theseus changed'
        self.args = [str(application), *args]
        self.fixture = fixture
        self.output = TerminalOutput(rows, columns)
        self.returncode = None
        self.modes = None
        self.eof = False
        self.closed = False
        self.bridge = subprocess.Popen([str(fixture), 'terminal'], stdin=subprocess.PIPE,
                                       stdout=subprocess.PIPE)
        startup = dict(program=str(application), args=list(args), cwd=str(pathlib.Path(cwd).resolve()),
                       environment=dict({'TERM': 'xterm-256color'}, **environment), rows=rows, columns=columns)
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
        self._send(dict(op=operation, **values))
        result = self._receive()
        self.output.feed(bytes(result['bytes']))
        self.returncode = result['status']
        self.modes = result['modes']
        self.eof = result['eof']
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
            if self.bridge.poll() is None:
                self.bridge.stdin.close()  # EOF synchronously asks native ownership cleanup.
            try:
                code = self.bridge.wait(timeout=15)
            except subprocess.TimeoutExpired as error:
                # A diagnostic deadline cannot orphan the owning helper. Healthy
                # native cleanup must finish; an unresponsive kernel is not a
                # bounded-success guarantee for this private coupled transport.
                self.bridge.wait()
                raise AssertionError('terminal cleanup exceeded diagnostic deadline') from error
            assert code == 0, 'terminal helper cleanup failed'
        finally:
            self.bridge.stdout.close()
        assert digest(self.fixture) == self.metadata['fixture']['sha256']
        assert digest(self.metadata['application']['path']) == self.metadata['application']['sha256']
