"""Execute the standalone example headlessly and in actual live/offline/cancel PTYs."""
import json
import os
if os.name != "nt":
    import fcntl
    import pty
    import termios
import pathlib
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
from docker_absence import require_container_absent
from terminal_screen import TerminalOutput

repo = pathlib.Path(__file__).resolve().parents[2]
docker_environment = dict(os.environ)


def proof_visible(output, resized=False):
    return (output.has(b'Interruption ContainerSigkill: readiness=true stopped=true')
            and (not resized or resized_footer_visible(output)))


def comparison_visible(output, checkpoint, resized=False):
    return (output.has(('Comparison: before_interrupt -> ' + checkpoint).encode())
            and (not resized or resized_footer_visible(output)))


def resized_footer_visible(output):
    return (output.screen.rows == 50 and output.screen.columns >= 100
            and output.screen.cells.get((0, output.screen.columns - 1)) == '┐'
            and b'Tab/Enter' in b'\n'.join(output.screen.text().splitlines()[-3:]))


def check_cleanup(saved):
    value = json.loads(saved.read_text())
    journal = json.loads((saved.parent / 'journal.json').read_text())
    assert journal['schema_version'] == 1 and journal['resources']
    assert all(resource['removed'] for resource in journal['resources'])
    for resource in journal['resources']:
        inspected = subprocess.run(['docker', 'inspect', resource['container_id']],
                                   capture_output=True, text=True, timeout=10,
                                   env=docker_environment)
        require_container_absent(resource['container_id'], inspected)
    return value


def session(root, args, live=False, cancel=False, saved=None):
    evidence_path = os.environ.get('THESEUS_INTERRUPTION_EVIDENCE')
    evidence = pathlib.Path(evidence_path or tempfile.mkdtemp(prefix='theseus-interruption-evidence-'))
    evidence = evidence / ('cancel' if cancel else 'live' if live else 'offline')
    evidence.mkdir(parents=True, exist_ok=True)
    master, slave = pty.openpty()
    before = termios.tcgetattr(slave)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 32, 60, 0, 0))
    proc = subprocess.Popen([binary, '--no-animation', *args], cwd=root, stdin=slave, stdout=slave,
                            stderr=slave, env={**os.environ, 'TERM': 'xterm-256color'})
    output = TerminalOutput(32, 60)
    data = output.data
    resized = None
    observations = []

    def snapshot(name):
        observations.append({'name': name, 'bytes': len(data),
                             'size': [output.screen.rows, output.screen.columns]})
        (evidence/(name+'.raw')).write_bytes(data)
        (evidence/(name+'.screen')).write_bytes(output.screen.text())
        if resized is not None:
            (evidence/(name+'.resize.screen')).write_bytes(resized.screen.text())
        for artifact in root.glob('.theseus-runs/*/*.json'):
            if artifact.name in ['report.json', 'journal.json']:
                shutil.copyfile(artifact, evidence/(name+'.'+artifact.name))
        (evidence/'observations.json').write_text(json.dumps(observations, indent=2))

    def pump():
        if select.select([master], [], [], .03)[0]:
            chunk = os.read(master, 65536)
            output.feed(chunk)
            if resized is not None:
                resized.feed(chunk)

    def resize(columns):
        nonlocal resized
        # Only bytes emitted after this resize may establish its visible frame.
        resized = TerminalOutput(50, columns)
        output.screen.rows, output.screen.columns = 50, columns
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 50, columns, 0, 0))

    def until(predicate, seconds=15):
        deadline = time.monotonic() + seconds
        while not predicate() and time.monotonic() < deadline:
            pump()
        assert predicate(), (args, proc.poll(), str(evidence), output.screen.text(), data[-1000:])

    def report():
        paths = list(root.glob('.theseus-runs/*/report.json'))
        return json.loads(paths[0].read_text()) if paths else None

    try:
        until(lambda: b'\x1b[?1049h' in data and output.has(b'Theseus'))
        assert not (termios.tcgetattr(slave)[3] & termios.ICANON)
        if live:
            until(lambda: output.has(b'Preview'))
            assert not list(root.glob('.theseus-runs/*/report.json')), 'preview executed'
            os.write(master, b'ps')  # both -> user; all -> interrupted install
            time.sleep(.2)
            os.write(master, b'r')
            if cancel:
                until(lambda: (v := report()) and any(
                    stage['name'] == 'interrupt_install' for s in v['scenarios'] for stage in s['stages']), 45)
                snapshot('cancel-request')
                os.write(master, b'c')
            until(lambda: (value := report()) and value['finished_at'], 90)
            saved = next(root.glob('.theseus-runs/*/report.json'))
            until(lambda: output.has(b'Finished |'))
        else:
            until(lambda: output.has(b'Scenarios', b'Stages'))
        snapshot('ready')
        value = json.loads(saved.read_text())
        assert value['schema_version'] == 8
        assert value['outcome'] == ('cancelled' if cancel else 'passed'), value
        assert value['selected_profiles'] == ['user']
        assert value['selected_scenarios'] == ['interrupted_install']
        scenario = value['scenarios'][0]
        if cancel:
            assert all(not stage['commands'] for stage in scenario['stages']
                       if stage['name'] in ['recover_install', 'configure', 'exercise_after_recovery'])
        else:
            index = next(i for i, stage in enumerate(scenario['stages']) if stage['name'] == 'interrupt_install')
            os.write(master, b'\t' + b'j' * index)
            until(lambda: output.has(b'> interrupt_install'))
            snapshot('selected-narrow')
            # The interruption command proof is the first evidence record.
            os.write(master, b'\t\x1b[H')
            resize(100)
            until(lambda: proof_visible(resized, resized=True))
            snapshot('command-proof')
            command = scenario['stages'][index]['commands'][0]
            assert command['outcome'] == 'interrupted' and command.get('exit_status') is None
            proof = command['interruption']
            assert proof['readiness_verified'] and proof['stopped_verified']
            restart = next(stage['restart'] for stage in scenario['stages'] if stage.get('restart'))
            assert restart['running_verified'] and restart['container_id'] == scenario['container_id']
            os.write(master, b'\t\t')  # evidence -> scenario -> stage focus
            for checkpoint in ['interrupted', 'recovered']:
                target = next(i for i, stage in enumerate(scenario['stages']) if stage['name'] == checkpoint)
                os.write(master, b'j' * (target - index))
                index = target
                until(lambda: output.has(('> ' + checkpoint + ' passed').encode()))
                reference = {'before_stage': 'before_interrupt', 'after_stage': checkpoint}
                check = next(check for check in scenario['stages'][target]['checks']
                             if check.get('comparison') == reference)
                # Select the real check detail instead of the stage summary.
                os.write(master, b'\t/' + check['id'].encode() + b'\r')
                resize(101 + target)
                until(lambda: comparison_visible(resized, checkpoint, resized=True))
                snapshot(checkpoint+'-comparison')
                assert any(check.get('comparison') == reference
                           for check in scenario['stages'][target]['checks'])
                os.write(master, b'/\r\t\t')  # clear filter; evidence -> scenario -> stage
        os.write(master, b'q')
        until(lambda: proc.poll() is not None)
        while select.select([master], [], [], 0)[0]:
            pump()
        assert proc.returncode == (130 if cancel else 0), proc.returncode
        assert termios.tcgetattr(slave) == before, 'terminal mode not restored'
        assert b'\x1b[?1049l' in data, 'alternate screen not restored'
        check_cleanup(saved)
        snapshot('finished')
        print(f'PTY {"cancel" if cancel else "live" if live else "offline"}: schema3 selection/proof/comparison/terminal/cleanup accepted')
        print(f'PTY evidence: {evidence}')
        return saved
    except BaseException:
        snapshot('failure')
        print(f'PTY failure evidence: {evidence}', file=sys.stderr)
        raise
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait()
        for journal in root.glob('.theseus-runs/*/journal.json'):
            subprocess.run([binary, 'cleanup', str(journal.parent)], capture_output=True, env=docker_environment)
        os.close(master)
        os.close(slave)


def run_headless(binary, root, environment):
    evidence_path = os.environ.get('THESEUS_INTERRUPTION_EVIDENCE')
    evidence = pathlib.Path(evidence_path or tempfile.mkdtemp(prefix='theseus-interruption-evidence-')) / 'headless'
    evidence.mkdir(parents=True, exist_ok=True)

    def retain(stdout, stderr, returncode, complete):
        metadata = {'capture_complete': complete, 'returncode': returncode, 'streams': {}}
        for stream, captured in [('stdout', stdout), ('stderr', stderr)]:
            available = captured or b''
            window = available[:4 * 1024 * 1024]
            (evidence/stream).write_bytes(window)
            # Available partial capture is not the process's total source output.
            metadata['streams'][stream] = {
                'available_captured_bytes': len(available), 'stored_window_bytes': len(window),
                'window_truncated': len(window) < len(available)}
        (evidence/'status').write_text('unknown' if returncode is None else str(returncode))
        (evidence/'capture.json').write_text(json.dumps(metadata, indent=2))
        for artifact in root.glob('.theseus-runs/*/*.json'):
            if artifact.name in ['report.json', 'journal.json']:
                shutil.copyfile(artifact, evidence/artifact.name)

    try:
        result = subprocess.run([binary, 'run', '--headless'], cwd=root, capture_output=True,
                                env=environment, timeout=150)
    except subprocess.TimeoutExpired as error:
        retain(error.stdout, error.stderr, None, False)
        raise
    retain(result.stdout, result.stderr, result.returncode, True)
    return result


def main():
    global binary
    binary = str(pathlib.Path(sys.argv[1]).resolve())
    with tempfile.TemporaryDirectory(prefix='theseus-interruption-public-') as tmp:
        parent = pathlib.Path(tmp)
        root = parent / 'headless'
        shutil.copytree(repo / 'examples/interruption', root, ignore=shutil.ignore_patterns('.theseus-runs'))
        # Forwarded values and the private injected helper/nonce must not enter metadata.
        recipe = root / 'theseus.toml'
        source = recipe.read_text().replace('timeout_seconds = 15', 'timeout_seconds = 15\npass_env = ["THESEUS_ACCEPTANCE_SECRET"]')
        recipe.write_text(source)
        secret = 'private-acceptance-value-456391'
        result = run_headless(binary, root, {**os.environ, 'THESEUS_ACCEPTANCE_SECRET': secret})
        assert result.returncode == 0, (result.stdout, result.stderr)
        saved = next(root.glob('.theseus-runs/*/report.json'))
        value = check_cleanup(saved)
        assert len(value['scenarios']) == 4 and value['outcome'] == 'passed'
        assert value['schema_version'] == 8
        metadata = saved.read_text()
        assert secret not in metadata and '/.theseus-' not in metadata
        assert 'THESEUS_ACCEPTANCE_SECRET' in metadata
        for scenario in value['scenarios']:
            assert all(check['outcome'] == 'passed' for stage in scenario['stages'] for check in stage['checks'])
            interrupted = next(c for stage in scenario['stages'] for c in stage['commands'] if c['outcome'] == 'interrupted')
            assert 'THESEUS_INTERRUPT_BARRIER' not in interrupted['environment']
            proof = interrupted['interruption']
            assert proof['readiness_verified'] and proof['stopped_verified']
            restart = next(stage['restart'] for stage in scenario['stages'] if stage.get('restart'))
            assert restart['running_verified'] and restart['container_id'] == scenario['container_id']
        print('Public headless: four combinations,', sum(len(stage['checks']) for scenario in value['scenarios'] for stage in scenario['stages']), 'passed checks; stop/restart proof, forwarded privacy and strict cleanup accepted')
        live = parent / 'live'
        shutil.copytree(repo / 'examples/interruption', live, ignore=shutil.ignore_patterns('.theseus-runs'))
        saved = session(live, ['run'], live=True)
        os.environ['DOCKER_HOST'] = 'unix:///nonexistent-theseus-offline.sock'
        session(live, ['report', str(saved.parent)], saved=saved)
        os.environ.clear()
        os.environ.update(docker_environment)
        cancelled = parent / 'cancel'
        shutil.copytree(repo / 'examples/interruption', cancelled, ignore=shutil.ignore_patterns('.theseus-runs'))
        installer = cancelled / 'target/install.sh'
        installer.write_text(installer.read_text().replace('    /bin/sh "$THESEUS_INTERRUPT_BARRIER"', '    sleep 30\n    /bin/sh "$THESEUS_INTERRUPT_BARRIER"'))
        session(cancelled, ['run'], live=True, cancel=True)

        if len(sys.argv) > 2:
            destination = pathlib.Path(sys.argv[2]).resolve()
            shutil.copytree(parent, destination, dirs_exist_ok=True)
            print(f'Acceptance reports and logs retained at {destination}')


if __name__ == "__main__":
    main()
