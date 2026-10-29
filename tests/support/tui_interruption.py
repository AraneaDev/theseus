"""Execute the standalone example headlessly and in actual live/offline/cancel PTYs."""
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
    master, slave = pty.openpty()
    before = termios.tcgetattr(slave)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 32, 60, 0, 0))
    proc = subprocess.Popen([binary, *args], cwd=root, stdin=slave, stdout=slave,
                            stderr=slave, env={**os.environ, 'TERM': 'xterm-256color'})
    data = bytearray()

    def until(predicate, seconds=15):
        deadline = time.monotonic() + seconds
        while not predicate() and time.monotonic() < deadline:
            if select.select([master], [], [], .03)[0]:
                data.extend(os.read(master, 65536))
        assert predicate(), (args, proc.poll(), data[-4000:])

    def report():
        paths = list(root.glob('.theseus-runs/*/report.json'))
        return json.loads(paths[0].read_text()) if paths else None

    try:
        until(lambda: b'\x1b[?1049h' in data and b'Theseus' in data)
        assert not (termios.tcgetattr(slave)[3] & termios.ICANON)
        if live:
            until(lambda: b'Preview' in data)
            assert not list(root.glob('.theseus-runs/*/report.json')), 'preview executed'
            os.write(master, b'ps')  # both -> user; all -> interrupted install
            time.sleep(.2)
            os.write(master, b'r')
            if cancel:
                until(lambda: (v := report()) and any(
                    stage['name'] == 'interrupt_install' for s in v['scenarios'] for stage in s['stages']), 45)
                os.write(master, b'c')
            until(lambda: (value := report()) and value['finished_at'], 90)
            saved = next(root.glob('.theseus-runs/*/report.json'))
        value = json.loads(saved.read_text())
        assert value['schema_version'] == 3
        assert value['outcome'] == ('cancelled' if cancel else 'passed'), value
        assert value['selected_profiles'] == ['user']
        assert value['selected_scenarios'] == ['interrupted_install']
        scenario = value['scenarios'][0]
        if cancel:
            assert all(not stage['commands'] for stage in scenario['stages']
                       if stage['name'] in ['recover_install', 'configure', 'exercise_after_recovery'])
        else:
            index = next(i for i, stage in enumerate(scenario['stages']) if stage['name'] == 'interrupt_install')
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 50, 100, 0, 0))
            os.write(master, b'\t' + b'j' * index)
            until(lambda: b'readiness=true' in data and b'stopped=true' in data)
            command = scenario['stages'][index]['commands'][0]
            assert command['outcome'] == 'interrupted' and command.get('exit_status') is None
            proof = command['interruption']
            assert proof['readiness_verified'] and proof['stopped_verified']
            restart = next(stage['restart'] for stage in scenario['stages'] if stage.get('restart'))
            assert restart['running_verified'] and restart['container_id'] == scenario['container_id']
            for checkpoint in ['interrupted', 'recovered']:
                target = next(i for i, stage in enumerate(scenario['stages']) if stage['name'] == checkpoint)
                data.clear()
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 50, 101 + target, 0, 0))
                os.write(master, b'j' * (target - index))
                index = target
                until(lambda: b'before_interrupt' in data and checkpoint.encode() in data)
                assert any(check.get('comparison') == {'before_stage': 'before_interrupt', 'after_stage': checkpoint}
                           for check in scenario['stages'][target]['checks'])
        os.write(master, b'q')
        until(lambda: proc.poll() is not None)
        assert proc.returncode == (130 if cancel else 0), proc.returncode
        assert termios.tcgetattr(slave) == before, 'terminal mode not restored'
        assert b'\x1b[?1049l' in data, 'alternate screen not restored'
        check_cleanup(saved)
        print(f'PTY {"cancel" if cancel else "live" if live else "offline"}: schema3 selection/proof/comparison/terminal/cleanup accepted')
        return saved
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait()
        for journal in root.glob('.theseus-runs/*/journal.json'):
            subprocess.run([binary, 'cleanup', str(journal.parent)], capture_output=True, env=docker_environment)
        os.close(master)
        os.close(slave)


with tempfile.TemporaryDirectory(prefix='theseus-interruption-public-') as tmp:
    parent = pathlib.Path(tmp)
    root = parent / 'headless'
    shutil.copytree(repo / 'examples/interruption', root, ignore=shutil.ignore_patterns('.theseus-runs'))
    # Forwarded values and the private injected helper/nonce must not enter metadata.
    recipe = root / 'theseus.toml'
    source = recipe.read_text().replace('timeout_seconds = 15', 'timeout_seconds = 15\npass_env = ["THESEUS_ACCEPTANCE_SECRET"]')
    recipe.write_text(source)
    secret = 'private-acceptance-value-456391'
    result = subprocess.run([binary, 'run', '--headless'], cwd=root, capture_output=True,
                            env={**os.environ, 'THESEUS_ACCEPTANCE_SECRET': secret}, timeout=150)
    assert result.returncode == 0, (result.stdout, result.stderr)
    saved = next(root.glob('.theseus-runs/*/report.json'))
    value = check_cleanup(saved)
    assert len(value['scenarios']) == 4 and value['outcome'] == 'passed'
    assert value['schema_version'] == 3
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
