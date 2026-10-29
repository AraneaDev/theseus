"""Actual narrow PTY selection and report-2 browsing using the public example."""
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


def session(root, args, live, saved=None):
    master, slave = pty.openpty()
    before = termios.tcgetattr(slave)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 32, 60, 0, 0))
    proc = subprocess.Popen([binary, '--no-animation', *args], cwd=root, stdin=slave, stdout=slave,
                            stderr=slave, env={**os.environ, 'TERM': 'xterm-256color'})
    data = bytearray()

    def until(predicate, seconds=8):
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
            # Start with both profiles, narrow to user, cycle the single declared scenario.
            os.write(master, b'ps')
            time.sleep(.15)
            os.write(master, b'r')
            until(lambda: (value := report()) and value['finished_at'], 45)
            saved = next(root.glob('.theseus-runs/*/report.json'))
        value = json.loads(saved.read_text())
        assert value['schema_version'] == 6 and value['outcome'] == 'passed', value
        assert value['selected_profiles'] == ['user'], value['selected_profiles']
        assert value['selected_scenarios'] == ['reinstall']
        assert len(value['scenarios']) == 1
        scenario = value['scenarios'][0]
        index = next(i for i, s in enumerate(scenario['stages']) if s['name'] == 'reinstalled')
        os.write(master, b'\t' + b'j' * index)
        time.sleep(.2)
        # Make enough space for reference text while retaining the narrow selector check.
        data.clear()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 50, 82, 0, 0))
        until(lambda: b'before_reinstall' in data and b'reinstalled' in data)
        assert any(c.get('comparison') == {'before_stage': 'before_reinstall', 'after_stage': 'reinstalled'}
                   for c in scenario['stages'][index]['checks'])
        os.write(master, b'q')
        until(lambda: proc.poll() is not None)
        assert proc.returncode == 0
        assert termios.tcgetattr(slave) == before, 'terminal mode not restored'
        assert b'\x1b[?1049l' in data, 'alternate screen not restored'
        journal = json.loads((saved.parent/'journal.json').read_text())
        assert journal['schema_version'] == 1
        assert all(r['removed'] for r in journal['resources'])
        for resource in journal['resources']:
            inspected = subprocess.run(['docker', 'inspect', resource['container_id']],
                                       capture_output=True, text=True, timeout=10, env=docker_environment)
            require_container_absent(resource['container_id'], inspected)
        print(f'PTY {"live" if live else "offline"} reinstall: user-only selection, comparison visible, terminal restored, owned cleanup verified')
        return saved
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait()
        for journal in root.glob('.theseus-runs/*/journal.json'):
            subprocess.run([binary, 'cleanup', str(journal.parent)], capture_output=True, env=docker_environment)
        os.close(master)
        os.close(slave)


with tempfile.TemporaryDirectory(prefix='theseus-reinstall-pty-') as tmp:
    root = pathlib.Path(tmp)
    shutil.copytree(repo/'examples/reinstall', root, dirs_exist_ok=True,
                    ignore=shutil.ignore_patterns('.theseus-runs'))
    saved = session(root, ['run'], True)
    # Offline viewing must remain usable without a reachable Docker daemon.
    os.environ['DOCKER_HOST'] = 'unix:///nonexistent-theseus-offline.sock'
    session(root, ['report', str(saved.parent)], False, saved)
