"""Actual live/offline PTYs, interrupted rollback policies and historical offline reports."""
import fcntl
import json
import hashlib
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

binary = None
repo = pathlib.Path(__file__).resolve().parents[2]
docker_environment = dict(os.environ)
# Optional public recipe directory lets release acceptance reuse this exact
# live/offline PTY workflow instead of maintaining a second terminal harness.
recipe_source = repo / 'tests/fixtures/interrupted-rollback'



from terminal_screen import Screen


def detail_visible(screen, *tokens):
    """Require proof in the current Detail rectangle and a complete current frame."""
    lines = screen.text().decode().splitlines()
    if not lines or not lines[0].endswith('┐'):
        return False
    if not any(any(marker in line for marker in ['Tab/Enter focus', 'Finished |', 'Offline report |'])
               for line in lines[-3:]):
        return False
    for row, line in enumerate(lines):
        column = line.find('┌Detail — record ')
        if column < 0:
            continue
        body = []
        for following in lines[row + 1:]:
            if following[column:column + 1] == '└' and following.endswith('┘'):
                current = ''.join(''.join(body).split()).encode()
                return all(b''.join(token.split()) in current for token in tokens)
            body.append(following[column + 1:screen.columns - 1])
    return False


def select_detail(send, until, screen, needle, *tokens):
    # Filter applies across focus states, resets record selection, and leaves
    # Stage focus intact for the later k/j return through a filtered-out stage.
    send(b'/' + needle.encode() + b'\r')
    until(lambda: b'> ' + needle.encode() in screen.text()
          and b'Detail \xe2\x80\x94 record 1 / 1' in screen.text()
          and (not needle.endswith(' stdout') or b'raw bytes ' in screen.text())
          and detail_visible(screen, *tokens))


def cleanup(saved):
    journal = json.loads((saved.parent / 'journal.json').read_text())
    assert journal['schema_version'] == 1 and journal['resources']
    assert all(resource['removed'] for resource in journal['resources'])
    for resource in journal['resources']:
        result = subprocess.run(['docker', 'inspect', resource['container_id']],
                                capture_output=True, text=True, timeout=10,
                                env=docker_environment)
        require_container_absent(resource['container_id'], result)


def session(root, policy=None, saved=None, legacy=False, unverified=False, negative=False):
    live = saved is None
    source_hashes = {path: hashlib.sha256(path.read_bytes()).hexdigest()
                     for path in saved.parent.rglob('*') if path.is_file()} if saved else {}
    master, slave = pty.openpty()
    before = termios.tcgetattr(slave)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 32, 60, 0, 0))
    args = ['run'] if live else ['report', str(saved.parent)]
    environment = {**docker_environment, 'TERM': 'xterm-256color'}
    sentinel = root / 'offline-docker-called'
    if not live:
        environment['DOCKER_HOST'] = 'unix:///nonexistent-theseus-interrupted-rollback-offline.sock'
        bindir = root / 'offline-sentinel-bin'
        bindir.mkdir(exist_ok=True)
        docker = bindir / 'docker'
        docker.write_text('#!/bin/sh\nprintf called > "$THESEUS_DOCKER_SENTINEL"\nexit 97\n')
        docker.chmod(0o755)
        environment.update(PATH=str(bindir) + os.pathsep + os.environ['PATH'],
                           THESEUS_DOCKER_SENTINEL=str(sentinel))
    proc = subprocess.Popen([binary, '--no-animation', *args], cwd=root, stdin=slave, stdout=slave,
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
            pathlib.Path("/tmp/theseus-interrupted-rollback-pty-failure.log").write_bytes(transcript)
        assert predicate(), (args, proc.poll(), screen.text(), data[:4000], data[-1000:])

    def rendered():
        return screen.text()

    def proof(needle, *tokens):
        select_detail(lambda keys: os.write(master, keys), until, screen, needle, *tokens)
        (root / ('proof-' + needle.replace(' ', '-') + '.txt')).write_bytes(rendered())

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
                b'known-good-reader', b'before_upgrade', b'before_interrupt',
                b'base = known-good', b'target = interrupted candidate',
                b'THESEUS_INTERRUPT_BARRIER', b'same-ID restart', b'before_rollback',
            ]
            until(lambda: all(token in rendered() for token in preview_tokens))
            data.clear()
            resize(32, 60)
            os.write(master, b'r')
            until(lambda: (value := report()) and value['finished_at'], 120)
            saved = next(root.glob('.theseus-runs/*/report.json'))
        if legacy:
            until(lambda: b'clean' in rendered() or b'reinstall' in rendered())
        elif negative:
            value = json.loads(saved.read_text())
            assert value['outcome'] in ['failed', 'incomplete']
            until(lambda: value['outcome'].encode() in rendered())
            scenario = value['scenarios'][0]
            index = next(i for i, stage in enumerate(scenario['stages']) if stage['name'] == 'rolled_back')
            failed_proof = next(c for c in scenario['stages'][index]['checks'] if c['id'] == '__theseus_rollback_restored_data')
            assert failed_proof['outcome'] in ['failed', 'skipped']
            os.write(master, b'\t' + b'j' * index)
            resize(60, 130)
            until(lambda: b'> rolled_back ' in rendered())
            select_detail(lambda keys: os.write(master, keys), until, screen,
                          '__theseus_rollback_restored_data',
                          ('__theseus_rollback_restored_data ' + failed_proof['outcome']).encode())
            assert b'Interrupted rollback verified:' not in rendered()
            resize(32, 50)
            os.write(master, b'k')
            until(lambda: b'> rollback ' in rendered()
                  and detail_visible(screen, b'No evidence for this stage/filter.'))
            assert b'Interrupted rollback verified:' not in rendered()
            os.write(master, b'j')
            until(lambda: b'> rolled_back ' in rendered()
                  and detail_visible(screen, ('__theseus_rollback_restored_data ' + failed_proof['outcome']).encode()))
            assert b'Interrupted rollback verified:' not in rendered()
        elif unverified:
            until(lambda: b'incomplete' in rendered())
            value = json.loads(saved.read_text())
            index = next(i for i, stage in enumerate(value['scenarios'][0]['stages']) if stage['name'] == 'interrupt_upgrade')
            os.write(master, b'\t' + b'j' * index)
            resize(50, 125)
            until(lambda: b'> interrupt_upgrade passed' in rendered())
            assert b'Interrupted rollback verified:' not in rendered()
        else:
            value = json.loads(saved.read_text())
            assert value['schema_version'] == 8 and value['outcome'] == 'passed', value
            assert value['selected_profiles'] == ['user']
            assert value['selected_scenarios'] == ['interrupted_rollback']
            scenario = value['scenarios'][0]
            assert scenario['interrupted_rollback']['restoration']['mode'] == policy.replace('-', '_')
            index = next(i for i, stage in enumerate(scenario['stages']) if stage['name'] == 'interrupt_upgrade')
            os.write(master, b'\t' + b'j' * index)
            resize(32, 50)
            until(lambda: b'> interrupt_upgrade passed' in rendered()
                  and detail_visible(screen, b'Interruption ContainerSigkill:',
                                     b'readiness=true', b'stopped=true'))
            resize(50, 125)
            until(lambda: b'Interrupted rollback verified:' in rendered()
                  and detail_visible(screen, b'Interruption ContainerSigkill:',
                                     b'readiness=true', b'stopped=true'))
            attempt = scenario['stages'][index]['commands'][0]
            assert attempt['outcome'] == 'interrupted' and attempt['exit_status'] is None
            assert attempt['interruption']['readiness_verified'] and attempt['interruption']['stopped_verified']
            os.write(master, b'j')
            until(lambda: b'> restart_after_interrupt' in rendered()
                  and b'restart_after_interrupt: passed' in rendered()
                  and detail_visible(screen, b'Restart:', b'running=true',
                                     scenario['container_id'].encode()))
            os.write(master, b'j')
            until(lambda: b'> interrupted passed' in rendered())
            proof('candidate-changed-state', b'candidate-changed-state passed',
                  b'Scope: /home/theseus/.local/share/sample-cli',
                  b'Comparison: before_interrupt -> interrupted')
            os.write(master, b'/\r')
            index += 2
            target = next(i for i, stage in enumerate(scenario['stages']) if stage['name'] == 'rolled_back')
            os.write(master, b'j' * (target - index))
            resize(60, 130)
            until(lambda: b'> rolled_back passed' in rendered())
            for check_id, marker in [('known-good-reader', b'reader records=2 sum=40'),
                                     ('known-good-version-restored', b'sample-cli 1.0.0')]:
                check = next(c for c in scenario['stages'][target]['checks'] if c['id'] == check_id)
                expectation = check['command_expectation']
                assert check['outcome'] == 'passed' and expectation['expected_exit'] == 0
                assert expectation['stdout_matched'] and expectation['stdout_match'].encode() == marker
                tokens = [check_id.encode() + b' passed', b'Reader/runtime expectation:',
                          b'exit=0 matched=true', marker]
                if check_id == 'known-good-version-restored':
                    assert expectation['executable'] == {
                        'name': 'sample-cli', 'resolved_path': '/home/theseus/.local/bin/sample-cli',
                        'args': ['--version']}
                    tokens.extend([b'/home/theseus/.local/bin/sample-cli', b'--version'])
                proof(check_id, *tokens)
                command = next(c for c in scenario['stages'][target]['commands']
                               if c['id'] in check['command_ids'] and c['phase'] == 'check:' + check_id)
                assert command['outcome'] == 'succeeded' and command['exit_status'] == 0
                assert command['stdout']['bytes'] > 0 and not command['stdout']['truncated']
                # Read actual stdout via the public Logs pane, bound to the
                # selected check's command, not its expected marker string.
                os.write(master, b'\t' + b'\x1b[C' * 4)
                actual = [marker] + ([b'alpha=11', b'beta=29'] if check_id == 'known-good-reader' else [])
                proof(command['id'] + ' stdout', *actual)
                os.write(master, b'\x1b[C\t\t')  # Logs -> Stages, Evidence -> Scenario -> Stage
            if policy == 'restore-data':
                needle = '__theseus_rollback_restored_data'
                backup_proof = next(c for c in scenario['stages'][target]['checks'] if c['id'] == needle)
                assert backup_proof['backup'] == {'source_path': '/home/theseus/.local/share/sample-cli',
                                                   'stored_path': '/workspace/rollback-backup/saved'}
                scope = b'/home/theseus/.local/share/sample-cli'
            else:
                needle = 'known-good-binary'
                scope = b'/home/theseus/.local/bin/sample-cli'
            proof_tokens = [(needle + ' passed').encode(), b'Scope: ' + scope,
                            b'Comparison: before_upgrade -> rolled_back']
            if policy == 'restore-data':
                proof_tokens.extend([b'source /home/theseus/.local/share/sample-cli',
                                     b'stored /workspace/rollback-backup/saved'])
            proof(needle, *proof_tokens)
            resize(32, 50)
            until(lambda: b'> rolled_back passed' in rendered() and detail_visible(screen, *proof_tokens))
            os.write(master, b'k')
            until(lambda: b'> rollback passed' in rendered() and detail_visible(screen, b'No evidence for this stage/filter.'))
            os.write(master, b'j')
            until(lambda: b'> rolled_back passed' in rendered() and detail_visible(screen, *proof_tokens))
        os.write(master, b'q')
        # Process exit can precede draining the last terminal-restoration bytes.
        until(lambda: proc.poll() is not None and b'\x1b[?1049l' in transcript)
        expected_exit = {'failed': 1, 'incomplete': 2}[value['outcome']] if negative else 2 if unverified else 0
        assert proc.returncode == expected_exit, proc.returncode
        assert not sentinel.exists(), 'offline viewing invoked Docker'
        for path, digest in source_hashes.items():
            assert hashlib.sha256(path.read_bytes()).hexdigest() == digest, ('offline viewing changed source', path)
        assert termios.tcgetattr(slave) == before, 'terminal mode not restored'
        assert b'\x1b[?1049l' in transcript, 'alternate screen not restored'
        (root / ('pty-legacy-' + saved.stem + '.log' if legacy else f'pty-{policy}-{"negative" if negative else "unverified" if unverified else "live" if live else "offline"}.log')).write_bytes(transcript)
        if not legacy and not unverified:
            cleanup(saved)
        if negative:
            print(f'PTY {saved}: negative restoration proof/status/navigation/terminal restoration accepted')
        else:
            print(f'PTY {policy or saved.stem} {"live" if live else "offline"}: selection/policy/status/data/comparison/narrow/restoration accepted')
        return saved
    finally:
        if sys.exc_info()[0] is not None:
            destination = pathlib.Path(os.environ.get('THESEUS_ACCEPTANCE_EVIDENCE', '/tmp/theseus-interrupted-rollback-evidence'))
            destination = destination / f'pty-failure-{policy or "legacy"}-{time.time_ns()}'
            shutil.copytree(root, destination, dirs_exist_ok=True)
            if saved is not None:
                shutil.copytree(saved.parent, destination / 'source-run', dirs_exist_ok=True)
                (destination / 'source.json').write_text(json.dumps({
                    'report': str(saved), 'sha256': hashlib.sha256(saved.read_bytes()).hexdigest(),
                    'argv': [binary, '--no-animation', *args]}, indent=2))
            (destination / 'failure-transcript.log').write_bytes(transcript)
            (destination / 'failure-screen.txt').write_bytes(screen.text())
            print(f'PTY first-failure artifacts: {destination}', flush=True)
        if proc.poll() is None:
            proc.kill()
            proc.wait()
        for journal in root.glob('.theseus-runs/*/journal.json'):
            subprocess.run([binary, 'cleanup', str(journal.parent)], capture_output=True, env=docker_environment)
        os.close(master)
        os.close(slave)


def main():
    global binary, recipe_source
    binary = str(pathlib.Path(sys.argv[1]).resolve())
    if len(sys.argv) > 2 and not sys.argv[2].startswith('--'):
        recipe_source = pathlib.Path(sys.argv[2]).resolve()
    # Focused offline sessions let repairs reuse already-created actual reports.
    if len(sys.argv) > 2 and sys.argv[2] in ['--offline', '--unverified', '--legacy']:
        mode = sys.argv[2]
        policy = None if mode == '--legacy' else sys.argv[3]
        reports = sys.argv[3:] if mode == '--legacy' else sys.argv[4:]
        assert reports, 'saved session mode requires report paths'
        with tempfile.TemporaryDirectory(prefix='theseus-interrupted-rollback-replay-') as tmp:
            parent = pathlib.Path(tmp)
            for index, report_path in enumerate(reports):
                root = parent / str(index)
                root.mkdir()
                session(root, policy, saved=pathlib.Path(report_path).resolve(), legacy=mode == '--legacy', unverified=mode == '--unverified')
            destination = os.environ.get('THESEUS_ACCEPTANCE_EVIDENCE')
            if destination:
                shutil.copytree(parent, pathlib.Path(destination) / f'pty-replay-{mode[2:]}-{time.time_ns()}')
        return


    # Run only saved negative reports when healthy public sessions already passed.
    # Uses the same screen, navigation, terminal guard and strict absence workflow.
    if len(sys.argv) > 2 and sys.argv[2] == '--negative':
        assert len(sys.argv) > 3, 'negative PTY mode requires at least one saved report'
        with tempfile.TemporaryDirectory(prefix='theseus-interrupted-rollback-negative-pty-') as tmp:
            parent = pathlib.Path(tmp)
            for index, report_path in enumerate(sys.argv[3:]):
                root = parent / str(index)
                root.mkdir()
                session(root, 'restore-data', saved=pathlib.Path(report_path).resolve(), negative=True)
            destination = os.environ.get('THESEUS_ACCEPTANCE_EVIDENCE')
            if destination:
                shutil.copytree(parent, pathlib.Path(destination) / 'negative-pty', dirs_exist_ok=True)
        return


    with tempfile.TemporaryDirectory(prefix='theseus-interrupted-rollback-pty-') as tmp:
        parent = pathlib.Path(tmp)
        for policy in ['binary-only', 'restore-data']:
            root = parent / policy
            shutil.copytree(recipe_source, root, ignore=shutil.ignore_patterns('.theseus-runs'))
            shutil.copyfile(root / (policy + '.toml'), root / 'theseus.toml')
            saved = session(root, policy)
            session(root, policy, saved=saved)
            for missing in ['stop', 'restart', 'derived']:
                invalid = root / ('unverified-' + missing) / 'report.json'
                invalid.parent.mkdir()
                value = json.loads(saved.read_text())
                stages = value['scenarios'][0]['stages']
                if missing == 'stop':
                    next(stage for stage in stages if stage['name'] == 'interrupt_upgrade')['commands'][0]['interruption']['stopped_verified'] = False
                elif missing == 'restart':
                    next(stage for stage in stages if stage['name'] == 'restart_after_interrupt').pop('restart')
                elif policy == 'restore-data':
                    stage = next(stage for stage in stages if stage['name'] == 'rolled_back')
                    stage['checks'] = [check for check in stage['checks'] if check['id'] != '__theseus_rollback_restored_data']
                else:
                    next(stage for stage in stages if stage['name'] == 'rolled_back')['checks'][1].pop('command_expectation')
                invalid.write_text(json.dumps(value))
                result = subprocess.run([binary, 'report', str(invalid), '--json'], capture_output=True, text=True,
                                        env={**docker_environment, 'DOCKER_HOST': 'unix:///nonexistent-interrupted-rollback-offline.sock'})
                assert result.returncode == 0, result.stderr
                assert json.loads(result.stdout)['outcome'] == 'incomplete', result.stdout
                session(root, policy, saved=invalid, unverified=True)

        for schema in [1, 2, 3, 4, 5]:
            root = parent / f'schema{schema}'
            root.mkdir()
            saved = root / 'report.json'
            # The schema-2 reinstall fixture uses only fields valid in every later reader.
            # Keep schemas 1/2 literal; reopen 3/4/5 as historical reinstall reports.
            source = repo / f'tests/fixtures/reports/schema{min(schema, 2)}-report.json'
            value = json.loads(source.read_text())
            value['schema_version'] = schema
            saved.write_text(json.dumps(value))
            session(root, saved=saved, legacy=True)
        destination = os.environ.get('THESEUS_ACCEPTANCE_EVIDENCE')
        if destination:
            destination = pathlib.Path(destination) / 'pty'
            shutil.copytree(parent, destination, dirs_exist_ok=True)
            print(f'PTY acceptance reports and transcripts retained at {destination}')


if __name__ == '__main__':
    main()
