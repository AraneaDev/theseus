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
recipe_source = pathlib.Path(sys.argv[2]).resolve() if len(sys.argv) > 2 else repo / 'tests/fixtures/rollback'



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


def session(root, policy=None, saved=None, legacy=False, unverified=False):
    live = saved is None
    master, slave = pty.openpty()
    before = termios.tcgetattr(slave)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 32, 60, 0, 0))
    args = ['run'] if live else ['report', str(saved)]
    environment = {**docker_environment, 'TERM': 'xterm-256color'}
    if not live:
        environment['DOCKER_HOST'] = 'unix:///nonexistent-theseus-rollback-offline.sock'
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
            pathlib.Path("/tmp/theseus-rollback-pty-failure.log").write_bytes(transcript)
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
            os.write(master, b'ps')  # both -> user, explicit single declared rollback
            # Preview policy and runtime contract are real rendered evidence.
            data.clear()
            resize(60, 110)
            preview_tokens = [
                ('Policy: ' + policy.replace('-', '_')).encode(),
                b'known-good-reader', b'before_upgrade', b'before_attempt',
                b'base = known-good', b'target = failed candidate',
                b'Expected failure: attempt_upgrade exit=9',
            ]
            until(lambda: all(token in rendered() for token in preview_tokens))
            data.clear()
            resize(32, 60)
            os.write(master, b'r')
            until(lambda: (value := report()) and value['finished_at'], 120)
            saved = next(root.glob('.theseus-runs/*/report.json'))
        if legacy:
            until(lambda: b'clean' in data or b'reinstall' in data)
        elif unverified:
            until(lambda: b'incomplete' in rendered())
            value = json.loads(saved.read_text())
            index = next(i for i, stage in enumerate(value['scenarios'][0]['stages']) if stage['name'] == 'attempt_upgrade')
            os.write(master, b'\t' + b'j' * index)
            resize(50, 125)
            until(lambda: b'> attempt_upgrade passed' in rendered())
            assert b'Rollback verified:' not in rendered()
            assert b'Expected failure:' not in rendered()
        else:
            value = json.loads(saved.read_text())
            assert value['schema_version'] == 5 and value['outcome'] == 'passed', value
            assert value['selected_profiles'] == ['user']
            assert value['selected_scenarios'] == ['rollback']
            scenario = value['scenarios'][0]
            assert scenario['rollback']['restoration']['mode'] == policy.replace('-', '_')
            index = next(i for i, stage in enumerate(scenario['stages']) if stage['name'] == 'attempt_upgrade')
            os.write(master, b'\t' + b'j' * index)
            resize(32, 50)
            until(lambda: b'> attempt_upgrade passed' in rendered() and b'Expected failure: exit=9' in rendered()
                  and b'matched=true' in rendered() and b'upgrade failed after migration' in rendered())
            resize(50, 125)
            until(lambda: b'Rollback verified:' in rendered() and b'Expected failure: exit=9' in rendered()
                  and b'upgrade failed after migration' in rendered() and b'matched=true' in rendered())
            attempt = scenario['stages'][index]['commands'][0]
            assert attempt['outcome'] == 'expected_failure' and attempt['exit_status'] == 9
            assert attempt['expected_failure']['stdout_matched']
            assert attempt['expected_failure']['stdout_contains'] == 'upgrade failed after migration'
            target = next(i for i, stage in enumerate(scenario['stages']) if stage['name'] == 'rolled_back')
            os.write(master, b'j' * (target - index))
            resize(60, 130)
            until(lambda: b'> rolled_back passed' in rendered() and b'known-good-reader passed' in rendered()
                  and b'known-good-version-restored passed' in rendered())
            if policy == 'restore-data':
                os.write(master, b'/__theseus_rollback_restored_data\r')
                until(lambda: b'__theseus_rollback_restored_data passed' in rendered()
                      and b'stored' in rendered() and b'/workspace/rollback-backup/saved' in rendered()
                      and b'before_upgrade -> rolled_back' in rendered())
                proof = next(c for c in scenario['stages'][target]['checks'] if c['id'] == '__theseus_rollback_restored_data')
                assert proof['backup'] == {'source_path': '/home/theseus/.local/share/sample-cli',
                                           'stored_path': '/workspace/rollback-backup/saved'}
                needle = '__theseus_rollback_restored_data'
            else:
                os.write(master, b'/known-good-binary\r')
                needle = 'known-good-binary'
            resize(32, 50)
            narrow_tokens = [b'> rolled_back passed', (needle + ' passed').encode(),
                             b'Scope: /home/theseus/.local/', b'before_upgrade', b'rolled_back']
            until(lambda: all(token in rendered() for token in narrow_tokens))
            if policy == 'restore-data':
                until(lambda: b'/workspace/rollback-backup/saved' in rendered() and b'stored' in rendered())
            os.write(master, b'k')
            until(lambda: b'> rollback passed' in rendered() and b'No evidence for this stage/filter.' in rendered())
            os.write(master, b'j')
            until(lambda: all(token in rendered() for token in narrow_tokens))
        os.write(master, b'q')
        # Process exit can precede draining the last terminal-restoration bytes.
        until(lambda: proc.poll() is not None and b'\x1b[?1049l' in transcript)
        assert proc.returncode == (2 if unverified else 0), proc.returncode
        assert termios.tcgetattr(slave) == before, 'terminal mode not restored'
        assert b'\x1b[?1049l' in transcript, 'alternate screen not restored'
        (root / ('pty-legacy-' + saved.stem + '.log' if legacy else f'pty-{policy}-{"unverified" if unverified else "live" if live else "offline"}.log')).write_bytes(transcript)
        if not legacy and not unverified:
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


with tempfile.TemporaryDirectory(prefix='theseus-rollback-pty-') as tmp:
    parent = pathlib.Path(tmp)
    for policy in ['binary-only', 'restore-data']:
        root = parent / policy
        shutil.copytree(recipe_source, root, ignore=shutil.ignore_patterns('.theseus-runs'))
        shutil.copyfile(root / (policy + '.toml'), root / 'theseus.toml')
        saved = session(root, policy)
        session(root, policy, saved=saved)
        invalid = root / 'unverified' / 'report.json'
        invalid.parent.mkdir()
        value = json.loads(saved.read_text())
        command = next(stage for stage in value['scenarios'][0]['stages'] if stage['name'] == 'attempt_upgrade')['commands'][0]
        command.pop('expected_failure')
        invalid.write_text(json.dumps(value))
        result = subprocess.run([binary, 'report', str(invalid), '--json'], capture_output=True, text=True,
                                env={**docker_environment, 'DOCKER_HOST': 'unix:///nonexistent-rollback-offline.sock'})
        assert 'Rollback verified:' not in result.stdout
        assert result.returncode == 0, result.stderr
        assert json.loads(result.stdout)['outcome'] == 'incomplete', result.stdout
        session(root, policy, saved=invalid, unverified=True)

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
