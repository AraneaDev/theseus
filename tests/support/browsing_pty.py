"""Read-only public CLI PTYs over synthetic UI evidence; never a lifecycle claim."""
import fcntl, hashlib, json, os, pathlib, pty, select, signal, struct, subprocess, sys, termios, time
from terminal_screen import Screen

binary = str(pathlib.Path(sys.argv[1]).resolve())
case, root, evidence = sys.argv[2], pathlib.Path(sys.argv[3]), pathlib.Path(sys.argv[4])

def hashes():
    return {str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in root.rglob('*') if p.is_file()}

before = hashes()
(evidence/'source-before.json').write_text(json.dumps(before, indent=2))
bindir = evidence/'sentinel-bin'
bindir.mkdir()
sentinel = evidence/'docker-called'
docker = bindir/'docker'
docker.write_text('#!/bin/sh\nprintf called >> "$THESEUS_DOCKER_SENTINEL"\nexit 111\n')
docker.chmod(0o755)
environment = {**os.environ, 'TERM': 'xterm-256color',
               'PATH': str(bindir)+os.pathsep+os.environ['PATH'],
               'THESEUS_DOCKER_SENTINEL': str(sentinel)}
command = 'report' if case == 'report' else 'comparison'
# The public JSON path independently validates the fixture and recomputation.
result = subprocess.run([binary, command, str(root), '--json'], env=environment, capture_output=True)
(evidence/'public-json.stdout').write_bytes(result.stdout)
(evidence/'public-json.stderr').write_bytes(result.stderr)
assert result.returncode == 0, (result.returncode, result.stderr)
document = json.loads(result.stdout)
if case == 'comparison':
    assert document['outcome'] == 'failed', document['outcome']
    assert len(document['cells'][0]['comparison']['differences']) == 101
else:
    assert len(document['scenarios'][0]['stages'][0]['snapshots'][0]['entries']) == 70_001

master, slave = pty.openpty()
attributes = termios.tcgetattr(slave)
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 60, 180, 0, 0))
screen, raw = Screen(60, 180), bytearray()
proc = subprocess.Popen([binary, command, str(root), '--no-animation'], stdin=slave,
                        stdout=slave, stderr=slave, cwd=evidence, env=environment)
steps = []

def pump():
    if select.select([master], [], [], .05)[0]:
        chunk = os.read(master, 65536)
        raw.extend(chunk)
        screen.feed(chunk)

def text():
    return screen.text().decode()

def until(predicate, description):
    deadline = time.monotonic()+30
    while time.monotonic() < deadline:
        if predicate():
            return
        if proc.poll() is not None:
            break
        pump()
    assert predicate(), (description, proc.poll(), text())

def has(value):
    return value in text()

def snapshot(name):
    (evidence/(name+'.screen')).write_bytes(screen.text())
    steps.append(name)

def key(value):
    os.write(master, value)

def expect(value):
    until(lambda: has(value), value)

def filtered(query, commit=b'\r'):
    key(b'/'+query.encode())
    expect('Enter/Esc apply')
    key(commit)
    # Ensure the footer committed before issuing navigation.
    until(lambda: not has('Enter/Esc apply'), 'filter commit')

HOME, END = b'\x1b[H', b'\x1b[F'
PGUP, PGDN = b'\x1b[5~', b'\x1b[6~'
CTRLHOME, CTRLEND = b'\x1b[1;5H', b'\x1b[1;5F'
try:
    expect('Records')
    assert not (termios.tcgetattr(slave)[3] & termios.ICANON)
    if case == 'report':
        key(b'\t\t\x1b[C\x1b[C')  # scenario -> stage -> evidence; Files
        key(END)
        expect('record 70002 / 70002')
        expect('file-69999')
        snapshot('metadata-last-past-u16')
        key(b']]]')
        expect('DETAIL_TAIL')
        assert '�' not in text(), 'valid Unicode detail gained a replacement'
        snapshot('unicode-detail-tail')
        key(b'[')
        until(lambda: not has('DETAIL_TAIL'), 'previous detail chunk')
        key(b']')
        expect('DETAIL_TAIL')
        filtered('file-')  # 70k matches exceed the 65536 cached-match bound
        key(END)
        expect('record 70000 / 70000')
        expect('file-69999')
        snapshot('filter-cache-overflow-last')
        filtered('file-69999', b'\x1b')
        expect('record 1 / 1')
        snapshot('sparse-last-filter')
        filtered('')
        key(b'\x1b[C\x1b[C')  # Files -> Commands -> Logs
        expect('STDOUT_ONLY')
        assert not has('STDERR_ONLY'), 'typed stdout selected ambiguous stderr path'
        snapshot('typed-stdout-head')
        key(PGDN)
        expect('🙂')
        assert '�' not in text(), 'split valid scalar was corrupted'
        expect('raw bytes 0..4099')
        snapshot('utf8-boundary-first')
        key(b']')
        expect('SECOND_CHUNK')
        expect('raw bytes 4099..')
        assert '�' not in text()
        snapshot('utf8-boundary-next')
        key(CTRLEND)
        expect('LOG_TAIL')
        expect('last stored chunk')
        expect('capture truncated')
        assert not has('complete stored stream')
        snapshot('log-beyond-64k-capture-truncated')
        key(b'[')
        until(lambda: not has('LOG_TAIL'), 'previous raw chunk')
        key(b']')
        expect('LOG_TAIL')
        key(b'm')
        expect('log metadata')
        expect('PATH_TAIL')
        expect('truncated=true')
        assert not has('LOG_TAIL')
        snapshot('long-contained-reference-metadata')
        key(b'm')
        expect('STDOUT_ONLY')
        expect('raw bytes 0..4099')
        key(CTRLEND)
        expect('LOG_TAIL')
        key(CTRLHOME)
        expect('STDOUT_ONLY')
        key(b'j')
        expect('STDERR_ONLY')
        expect('raw bytes 0..11 / 11 | last stored chunk')
        assert not has('capture truncated') and not has('incomplete current storage')
        snapshot('typed-stderr-complete')
    else:
        key(b'\t')  # comparison begins with cell focus
        key(END)
        expect('file-00100')
        snapshot('comparison-difference-101')
        key(b']]')
        key(PGDN+PGDN)
        expect('DETAIL_TAIL')
        assert '�' not in text()
        snapshot('comparison-full-unicode-change')
        filtered('SOURCE_TAIL', b'\x1b')
        expect('SOURCE_TAIL')
        expect('record 1 / 2')
        snapshot('comparison-source-over-512')
        filtered('Complete evidence')
        expect('theseus comparison')
        expect('--json')
        snapshot('comparison-full-json-command')
    # Offline r is inert; q exits with the retained report outcome.
    key(b'rq')
    until(lambda: proc.poll() is not None, 'offline exit')
    while select.select([master], [], [], .05)[0]:
        pump()
    assert proc.returncode == (0 if case == 'report' else 1), (proc.returncode, text())
    assert termios.tcgetattr(slave) == attributes, 'termios not restored'
    assert b'\x1b[?1049h' in raw and b'\x1b[?1049l' in raw, 'alternate screen not restored'
    assert not sentinel.exists(), 'offline JSON/view/r invoked Docker'
    assert hashes() == before, 'offline JSON/view/r changed source bytes'
    (evidence/'source-after.json').write_text(json.dumps(hashes(), indent=2))
finally:
    if proc.poll() is None:
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=8)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
    (evidence/'terminal.raw').write_bytes(raw)
    (evidence/'last.screen').write_bytes(screen.text())
    (evidence/'terminal.json').write_text(json.dumps({'exit': proc.returncode, 'before': attributes,
        'after': termios.tcgetattr(slave), 'steps': steps}, default=lambda value: list(value)))
    os.close(master)
    os.close(slave)

print('PASS synthetic UI', case, len(steps), 'screens; source bytes unchanged; Docker sentinel absent; termios restored')
