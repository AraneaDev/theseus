"""Actual live/offline PTYs, two explicit policies and legacy offline reports."""
import codecs
import fcntl
import json
import os
import pathlib
import pty
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unicodedata

sys.dont_write_bytecode = True
from docker_absence import require_container_absent

binary = str(pathlib.Path(sys.argv[1]).resolve())
repo = pathlib.Path(__file__).resolve().parents[2]
docker_environment = dict(os.environ)


class Screen:
    """Reconstruct the cursor-addressed cells emitted by the actual Ratatui TUI.

    A diff can emit only the changed letters inside a word; matching its byte
    stream (even with ANSI stripped) does not test the text a terminal displays.
    """
    def __init__(self, rows, columns):
        self.rows, self.columns = rows, columns
        self.cells = {}
        self.row = self.column = 0
        self.decoder = codecs.getincrementaldecoder('utf-8')('replace')
        self.pending = ''

    def feed(self, chunk):
        text = self.pending + self.decoder.decode(chunk)
        index = 0
        while index < len(text):
            char = text[index]
            if char == '\x1b':
                if index + 1 == len(text):
                    break
                if text[index + 1] != '[':
                    index += 2
                    continue
                end = index + 2
                while end < len(text) and not ('@' <= text[end] <= '~'):
                    end += 1
                if end == len(text):
                    break
                arguments, command = text[index + 2:end], text[end]
                values = [int(v) if v else 0 for v in arguments.split(';')] if not arguments.startswith('?') else []
                first = values[0] if values else 0
                if command in ['H', 'f']:
                    self.row = (first or 1) - 1
                    self.column = ((values[1] if len(values) > 1 else 1) or 1) - 1
                elif command == 'J' and first in [2, 3]:
                    self.cells.clear()
                elif command == 'K':
                    for column in range(self.columns):
                        if first == 2 or first == 0 and column >= self.column or first == 1 and column <= self.column:
                            self.cells.pop((self.row, column), None)
                elif command in ['A', 'B', 'C', 'D']:
                    amount = first or 1
                    self.row += amount if command == 'B' else -amount if command == 'A' else 0
                    self.column += amount if command == 'C' else -amount if command == 'D' else 0
                elif command == 'G':
                    self.column = (first or 1) - 1
                index = end + 1
                continue
            if char == '\r':
                self.column = 0
            elif char == '\n':
                self.row += 1
            elif char >= ' ' and not unicodedata.combining(char):
                self.cells[self.row, self.column] = char
                width = 2 if unicodedata.east_asian_width(char) in ['W', 'F'] else 1
                if width == 2:
                    self.cells[self.row, self.column + 1] = ''
                self.column += width
            index += 1
        self.pending = text[index:]

    def text(self):
        return '\n'.join(''.join(self.cells.get((row, column), ' ') for column in range(self.columns))
                         for row in range(self.rows)).encode()


def check_screen_reconstruction():
    # Literal controls independently establish positions, overwrites, erasure,
    # and a sequence split across reads. Old characters must not remain visible.
    screen = Screen(3, 30)
    screen.feed(b'\x1b[2J\x1b[1;1HPolicy: compatible')
    screen.feed(b'\x1b[1;9Hsafe_rejection')
    assert screen.text().splitlines()[0].strip() == b'Policy: safe_rejection'
    screen.feed(b'\x1b[2;1Hold-data\x1b[2;4Her-data')
    assert screen.text().splitlines()[1].strip() == b'older-data'
    screen.feed(b'\x1b[2;6H\x1b[K')
    assert screen.text().splitlines()[1].strip() == b'older'
    screen.feed(b'\x1b[1;1H\x1b[2K')
    assert b'Policy' not in screen.text() and b'older' in screen.text()
    screen.feed(b'\x1b[2J\x1b[3;')
    screen.feed(b'4Hdata')
    assert screen.text().splitlines() == [b' ' * 30, b' ' * 30, b'   data' + b' ' * 23]


check_screen_reconstruction()

def cleanup(saved):
    journal = json.loads((saved.parent / 'journal.json').read_text())
    assert journal['schema_version'] == 1 and journal['resources']
    assert all(resource['removed'] for resource in journal['resources'])
    for resource in journal['resources']:
        result = subprocess.run(['docker', 'inspect', resource['container_id']],
                                capture_output=True, text=True, timeout=10,
                                env=docker_environment)
        require_container_absent(resource['container_id'], result)


def session(root, policy=None, saved=None, legacy=False):
    live = saved is None
    master, slave = pty.openpty()
    before = termios.tcgetattr(slave)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 32, 60, 0, 0))
    args = ['run'] if live else ['report', str(saved)]
    environment = {**docker_environment, 'TERM': 'xterm-256color'}
    if not live:
        environment['DOCKER_HOST'] = 'unix:///nonexistent-theseus-downgrade-offline.sock'
    proc = subprocess.Popen([binary, *args], cwd=root, stdin=slave, stdout=slave,
                            stderr=slave, env=environment)
    data = bytearray()
    screen = Screen(32, 60)
    transcript = bytearray()

    def until(predicate, seconds=20):
        deadline = time.monotonic() + seconds
        while not predicate() and time.monotonic() < deadline:
            if select.select([master], [], [], .03)[0]:
                chunk = os.read(master, 65536)
                data.extend(chunk)
                screen.feed(chunk)
                transcript.extend(chunk)
        if not predicate():
            pathlib.Path("/tmp/theseus-downgrade-pty-failure.log").write_bytes(transcript)
        assert predicate(), (args, proc.poll(), data[:4000], data[-1000:])

    def rendered():
        return screen.text()

    def report():
        paths = list(root.glob('.theseus-runs/*/report.json'))
        return json.loads(paths[0].read_text()) if paths else None

    def resize(rows, cols):
        screen.rows, screen.columns = rows, cols
        screen.cells.clear()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, 0, 0))

    try:
        until(lambda: b'\x1b[?1049h' in data and b'Theseus' in data)
        assert not (termios.tcgetattr(slave)[3] & termios.ICANON)
        if live:
            until(lambda: b'Preview' in data)
            assert not list(root.glob('.theseus-runs/*/report.json')), 'preview executed'
            os.write(master, b'ps')  # both -> user, explicit single declared downgrade
            # Preview policy and runtime contract are real rendered evidence.
            data.clear()
            resize(60, 110)
            preview_tokens = [
                ('Policy: ' + ('compatible' if policy == 'compatible' else 'safe_rejection')).encode(),
                b'older-data' if policy == 'compatible' else b'data-stable',
                b'before_downgrade', b'target = newer', b'base = older',
            ]
            until(lambda: all(token in rendered() for token in preview_tokens))
            data.clear()
            resize(32, 60)
            os.write(master, b'r')
            until(lambda: (value := report()) and value['finished_at'], 120)
            saved = next(root.glob('.theseus-runs/*/report.json'))
        if legacy:
            until(lambda: b'clean' in data or b'reinstall' in data)
        else:
            value = json.loads(saved.read_text())
            assert value['schema_version'] == 4 and value['outcome'] == 'passed', value
            assert value['selected_profiles'] == ['user']
            assert value['selected_scenarios'] == ['downgrade']
            scenario = value['scenarios'][0]
            assert scenario['downgrade']['expect'] == ('compatible' if policy == 'compatible' else 'safe_rejection')
            index = next(i for i, stage in enumerate(scenario['stages']) if stage['name'] == 'downgrade')
            # First navigate while genuinely narrow, then inspect the full proof.
            os.write(master, b'\t' + b'j' * index)
            resize(50, 110)
            expected = b'Compatible downgrade passed' if policy == 'compatible' else b'Declared rejection checks passed'
            until(lambda: expected in rendered())
            if policy == 'rejection':
                until(lambda: b'actual=Some(9)' in rendered() and b'expected=9' in rendered() and b'matched=true' in rendered())
                attempt = scenario['stages'][index]['commands'][0]
                assert attempt['outcome'] == 'expected_rejection' and attempt['exit_status'] == 9
                assert attempt['rejection']['stdout_matched']
            checkpoint = 'downgraded' if policy == 'compatible' else 'downgrade_rejected'
            target = next(i for i, stage in enumerate(scenario['stages']) if stage['name'] == checkpoint)
            data.clear()
            os.write(master, b'j' * (target - index))
            resize(51, 111)
            until(lambda: b'before_downgrade' in rendered() and checkpoint.encode() in rendered() and b'data-stable' in rendered())
            assert any(check.get('comparison') == {'before_stage': 'before_downgrade', 'after_stage': checkpoint}
                       for check in scenario['stages'][target]['checks'])
            assert scenario['downgrade']['data_checks'] == (['older-data'] if policy == 'compatible' else ['data-stable'])
            data.clear()
            resize(32, 50)
            until(lambda: b'Theseus' in rendered())
        os.write(master, b'q')
        # Process exit can precede draining the last terminal-restoration bytes.
        until(lambda: proc.poll() is not None and b'\x1b[?1049l' in transcript)
        assert proc.returncode == 0, proc.returncode
        assert termios.tcgetattr(slave) == before, 'terminal mode not restored'
        assert b'\x1b[?1049l' in transcript, 'alternate screen not restored'
        (root / ('pty-legacy-' + saved.stem + '.log' if legacy else f'pty-{policy}-{"live" if live else "offline"}.log')).write_bytes(transcript)
        if not legacy:
            cleanup(saved)
        print(f'PTY {policy or saved.stem} {"live" if live else "offline"}: selection/policy/status/data/comparison/narrow/restoration accepted')
        return saved
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait()
        for journal in root.glob('.theseus-runs/*/journal.json'):
            subprocess.run([binary, 'cleanup', str(journal.parent)], capture_output=True, env=docker_environment)
        os.close(master)
        os.close(slave)


with tempfile.TemporaryDirectory(prefix='theseus-downgrade-pty-') as tmp:
    parent = pathlib.Path(tmp)
    for policy in ['compatible', 'rejection']:
        root = parent / policy
        shutil.copytree(repo / 'tests/fixtures/downgrade', root)
        shutil.copyfile(root / (policy + '.toml'), root / 'theseus.toml')
        saved = session(root, policy)
        session(root, policy, saved=saved)
    for schema in [1, 2]:
        root = parent / f'schema{schema}'
        root.mkdir()
        saved = root / 'report.json'
        shutil.copyfile(repo / f'tests/fixtures/reports/schema{schema}-report.json', saved)
        session(root, saved=saved, legacy=True)
    destination = os.environ.get('THESEUS_ACCEPTANCE_EVIDENCE')
    if destination:
        destination = pathlib.Path(destination) / 'pty'
        shutil.copytree(parent, destination, dirs_exist_ok=True)
        print(f'PTY acceptance reports and transcripts retained at {destination}')
