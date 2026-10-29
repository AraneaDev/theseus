"""Actual live/offline PTYs, two explicit policies and legacy offline reports."""
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

sys.dont_write_bytecode = True
from docker_absence import require_container_absent

binary = str(pathlib.Path(sys.argv[1]).resolve())
repo = pathlib.Path(__file__).resolve().parents[2]
docker_environment = dict(os.environ)
# Optional public recipe directory lets release acceptance reuse this exact
# live/offline PTY workflow instead of maintaining a second terminal harness.
recipe_source = pathlib.Path(sys.argv[2]).resolve() if len(sys.argv) > 2 else repo / 'tests/fixtures/downgrade'



from terminal_screen import Screen


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
            assert value['schema_version'] == 5 and value['outcome'] == 'passed', value
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
            # Filter and navigate at the actual 50-column size. A header alone
            # cannot prove the selected stage or its scoped comparison is usable.
            os.write(master, b'/data-stable\r')
            narrow_tokens = [
                ('> ' + checkpoint).encode(), b'data-stable passed',
                b'Scope: /home/theseus/.local/share/sample-cli',
                b'before_downgrade', checkpoint.encode(),
            ]
            until(lambda: all(token in rendered() for token in narrow_tokens))
            os.write(master, b'k')
            until(lambda: b'> downgrade passed' in rendered()
                  and b'No evidence for this stage/filter.' in rendered())
            os.write(master, b'j')
            until(lambda: all(token in rendered() for token in narrow_tokens))
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
        shutil.copytree(recipe_source, root, ignore=shutil.ignore_patterns('.theseus-runs'))
        shutil.copyfile(root / (policy + '.toml'), root / 'theseus.toml')
        saved = session(root, policy)
        session(root, policy, saved=saved)
    for schema in ([] if len(sys.argv) > 2 else [1, 2]):
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
