"""Real PTYs: external termination restores idle/live terminals; c keeps browsing."""
import fcntl
import json
import os
import pathlib
import pty
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

sys.dont_write_bytecode = True
from terminal_screen import Screen
from docker_absence import require_container_absent

binary = str(pathlib.Path(sys.argv[1]).resolve())
repo = pathlib.Path(__file__).resolve().parents[2]
docker = '--docker' in sys.argv


selected_case = None
if '--case' in sys.argv:
    assert sys.argv.count('--case') == 1, 'provide --case once'
    case_index = sys.argv.index('--case') + 1
    assert case_index < len(sys.argv) and sys.argv[case_index], '--case requires a nonempty session name'
    selected_case = sys.argv[case_index]
executed_cases = []


def session(mode, action):
    case = f'{mode}-{action if isinstance(action, str) else signal.Signals(action).name.lower()}'
    if selected_case is not None and case != selected_case:
        return
    executed_cases.append(case)
    with tempfile.TemporaryDirectory(prefix='theseus-signals-') as tmp:
        root = pathlib.Path(tmp)
        shutil.copytree(repo/'tests/fixtures/lifecycle', root, dirs_exist_ok=True)
        if mode == 'active':
            recipe = root/'recipe.toml'
            recipe.write_text(recipe.read_text().replace('THESEUS_FIXTURE_FAULT = ""', 'THESEUS_FIXTURE_FAULT = "cancellation"'))
        args = ['run', '--recipe', 'recipe.toml', '--profile', 'user', '--scenario', 'clean']
        if mode == 'offline':
            saved = root/'saved'
            saved.mkdir()
            (saved/'report.json').write_text(json.dumps(dict(
                schema_version=1, run_id='offline-signal', recipe_path='', recipe_digest='',
                image='', image_id=None, network='none', artifacts=[], selected_profiles=[],
                selected_scenarios=[], started_at='', finished_at=None, outcome='passed',
                cancelled=False, scenarios=[], errors=[])))
            args = ['report', str(saved)]
        master, slave = pty.openpty()
        before = termios.tcgetattr(slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 36, 120, 0, 0))
        proc = subprocess.Popen([binary, '--no-animation', *args], cwd=root, stdin=slave, stdout=slave,
                                stderr=slave, env={**os.environ, 'TERM': 'xterm-256color'})
        data = bytearray()
        screen = Screen(36, 120)
        report = None
        def drain():
            if select.select([master], [], [], .03)[0]:
                chunk = os.read(master, 65536)
                data.extend(chunk)
                screen.feed(chunk)
        def until(predicate, timeout):
            deadline = time.monotonic()+timeout
            while not predicate() and time.monotonic() < deadline:
                drain()
            assert predicate(), (mode, action, proc.poll(), data[-3000:])
        def saved_report():
            nonlocal report
            paths = list(root.glob('.theseus-runs/*/report.json'))
            if paths:
                report = paths[0]
                return json.loads(report.read_text())
            return None
        try:
            until(lambda: b'\x1b[?1049h' in data and not (termios.tcgetattr(slave)[3] & termios.ICANON), 5)
            # Wait for initial rendering before delivering any input/signal.
            until(lambda: b'Preview' in data if mode != 'offline' else b'offline-signal' in data, 5)
            if mode in ('active', 'completed'):
                os.write(master, b'r')
                if mode == 'active':
                    def child_running():
                        value = saved_report()
                        if not value or not value['scenarios']:
                            return False
                        container = value['scenarios'][0]['container_id']
                        if not container:
                            return False
                        return subprocess.run(['docker', 'exec', container, '/bin/sh', '-c', 'test -s /workspace/child.pid'], capture_output=True).returncode == 0
                    until(child_running, 20)
                else:
                    until(lambda: (value := saved_report()) and value['finished_at'] is not None, 30)
                    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 36, 121, 0, 0))
                    screen.columns = 121
                    until(lambda: b'Finished' in screen.text(), 5)
            start = time.monotonic()
            if action == 'c':
                os.write(master, b'c')
                until(lambda: (value := saved_report()) and value['finished_at'] is not None, 7)
                # Reconstruct the displayed cells: a status update may split
                # Finished across separate Ratatui diff writes.
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 36, 121, 0, 0))
                screen.columns = 121
                until(lambda: b'Finished' in screen.text(), 5)
                assert proc.poll() is None, 'c quit rather than allowing report browsing'
                assert not (termios.tcgetattr(slave)[3] & termios.ICANON)
                os.write(master, b'\t\t\x1b[C')
                drain()
                assert proc.poll() is None, 'cancelled evidence cannot be browsed'
                os.write(master, b'q')
            else:
                proc.send_signal(action)
            until(lambda: proc.poll() is not None, 7)
            elapsed = time.monotonic()-start
            while select.select([master], [], [], .05)[0]:
                chunk = os.read(master, 65536)
                data.extend(chunk)
                screen.feed(chunk)
            assert proc.returncode == (130 if mode == 'active' else 0), (mode, action, proc.returncode, data[-3000:])
            assert termios.tcgetattr(slave) == before, 'termios not restored'
            assert b'\x1b[?1049h' in data and b'\x1b[?1049l' in data, 'alternate screen not restored'
            if mode in ('active', 'completed'):
                value = saved_report()
                assert value['outcome'] == ('cancelled' if mode == 'active' else 'passed'), value
                journal = json.loads((report.parent/'journal.json').read_text())
                assert all(r['removed'] for r in journal['resources']), journal
                for resource in journal['resources']:
                    require_container_absent(resource['container_id'], subprocess.run(
                        ['docker', 'inspect', resource['container_id']], capture_output=True, text=True, timeout=10))
            cleanup = ', owned cleanup verified' if mode in ('active', 'completed') else ''
            print(f'PTY {mode} {action}: exit={proc.returncode}, restored{cleanup}, {elapsed:.2f}s')
        finally:
            pathlib.Path(f'/tmp/theseus-signals-{case}.log').write_bytes(data)
            if proc.poll() is None:
                proc.kill()
                proc.wait()
            # Recovery on assertion failure also uses the production ownership gate.
            for journal in root.glob('.theseus-runs/*/journal.json'):
                subprocess.run([binary, 'cleanup', str(journal.parent)], capture_output=True)
            os.close(master)
            os.close(slave)


for sig in (signal.SIGINT, signal.SIGTERM):
    for mode in (('active', 'completed') if docker else ('preview', 'offline')):
        session(mode, sig)
if docker:
    session('active', 'c')

assert executed_cases, f'unknown or unavailable --case: {selected_case!r}'
