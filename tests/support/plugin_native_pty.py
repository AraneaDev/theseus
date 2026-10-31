"""Real native live/offline 80-column PTYs. Text screen assertions, retained raw bytes."""
import json
import os
if os.name != "nt":
    import fcntl
    import pty
    import termios
from pathlib import Path
import select
import signal
import struct
import subprocess
import sys
import time

from plugin_native import (attempt, checks, commands, digest, prepare, prerequisites,
                           stages, variant, verify_common, wrapper_waiting)
from terminal_screen import Screen


class Session:
    def __init__(self, binary, evidence, name, args, offline=False):
        self.evidence, self.name = evidence, name
        self.master, self.slave = pty.openpty()
        self.before = termios.tcgetattr(self.slave)
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack('HHHH', 50, 80, 0, 0))
        self.screen, self.raw, self.steps = Screen(50, 80), bytearray(), []
        env = {**os.environ, 'TERM': 'xterm-256color'}
        self.sentinel = evidence / (name + '-docker-called')
        if offline:
            bindir = evidence / (name + '-sentinel-bin')
            bindir.mkdir()
            docker = bindir / 'docker'
            docker.write_text('#!/bin/sh\nprintf called > "$THESEUS_DOCKER_SENTINEL"\nexit 111\n')
            docker.chmod(0o755)
            env.update(PATH=str(bindir) + os.pathsep + os.environ['PATH'],
                       THESEUS_DOCKER_SENTINEL=str(self.sentinel))
        argv = [binary, '--no-animation', *map(str, args)]
        (evidence / (name + '.command.json')).write_text(json.dumps(argv))
        self.proc = subprocess.Popen(argv, stdin=self.slave, stdout=self.slave,
                                     stderr=self.slave, cwd=evidence, env=env)

    def pump(self):
        if select.select([self.master], [], [], .05)[0]:
            data = os.read(self.master, 65536)
            self.raw.extend(data)
            self.screen.feed(data)

    def text(self):
        return self.screen.text().decode()

    def until(self, predicate, description, seconds=30):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline and not predicate():
            assert self.proc.poll() is None, (description, self.proc.returncode, self.text())
            self.pump()
        assert predicate(), (description, self.text())

    def expect(self, text):
        self.until(lambda: text in self.text(), text)

    def key(self, value):
        os.write(self.master, value)

    def snap(self, label):
        self.steps.append(label)
        (self.evidence / (self.name + '-' + label + '.screen')).write_bytes(self.screen.text())

    def filtered(self, text):
        self.key(b'/' + text.encode())
        self.expect('Enter/Esc apply')
        self.key(b'\r')
        self.until(lambda: 'Enter/Esc apply' not in self.text(), 'filter applied')

    def seek(self, text, pages=12):
        for _ in range(pages):
            for _ in range(3):
                self.pump()
            if text in self.text():
                return
            self.key(b'\x1b[6~')
        self.expect(text)

    def finish(self, code):
        self.until(lambda: self.proc.poll() is not None, 'terminal exit', 25)
        while select.select([self.master], [], [], .02)[0]:
            self.pump()
        assert self.proc.returncode == code, (self.proc.returncode, self.text())
        assert termios.tcgetattr(self.slave) == self.before, 'termios not restored'
        for enter, leave in [(b'\x1b[?1049h', b'\x1b[?1049l'), (b'\x1b[?2004h', b'\x1b[?2004l')]:
            assert enter in self.raw and leave in self.raw and self.raw.rfind(leave) > self.raw.rfind(enter)
        assert not self.sentinel.exists(), 'offline viewing or r invoked Docker'

    def close(self):
        if self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
            try:
                self.proc.wait(timeout=12)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()
        (self.evidence / (self.name + '.raw')).write_bytes(self.raw)
        (self.evidence / (self.name + '-last.screen')).write_bytes(self.screen.text())
        (self.evidence / (self.name + '.terminal.json')).write_text(json.dumps({
            'columns': 80, 'rows': 50, 'exit': self.proc.returncode, 'steps': self.steps,
            'before': self.before, 'after': termios.tcgetattr(self.slave)}, default=list))
        os.close(self.master)
        os.close(self.slave)


def latest(root, recipe):
    for path in (root / '.theseus-runs').glob('*/report.json'):
        try:
            report = json.loads(path.read_text())
        except (ValueError, OSError):
            continue
        if report['recipe_path'] == str(recipe) and report.get('finished_at'):
            return path
    return None


def browse(session, report, version, sha):
    # Scenario -> stage -> evidence. Select the real base checkpoint.
    session.expect('Records')
    session.key(b'\t\x1b[H' + b'j' * next(i for i, s in enumerate(stages(report)) if s['name'] == 'base_installed') + b'\t')
    session.filtered('Plugin theseus-probe')
    session.expect('scope: user')
    session.expect(version)
    session.expect('manual hook rehearsal')
    session.snap('scope-version-boundary')
    session.filtered('Artifact target')
    session.seek('client/claude')
    session.seek(sha)
    session.snap('native-file-digest')
    session.filtered('base-inventory')
    session.key(b'\x1b[F')  # contract record also mentions this proof; select actual check
    session.expect('JSON proof:')
    session.seek('matched_count: 1')
    session.snap('identity-count-json-proof')
    session.filtered('base-components')
    session.key(b'\x1b[F')
    session.expect('source/cache manifests')
    session.seek('manual-hook verified')
    session.snap('source-cache-manual-label')
    session.filtered('')
    session.key(b'\x1b[C' * 4)  # Stages -> Logs
    # Typed stdout list, explicit actual stored byte ranges and metadata toggle.
    session.filtered('stdout')
    session.key(b'\x1b[H')
    session.expect('raw bytes')
    session.snap('bounded-log-body')
    session.key(b'm')
    session.expect('log metadata')
    session.snap('bounded-log-metadata')
    session.key(b'm')
    component = next(c for c in commands(report) if c['phase'] == 'check:base-components')
    session.filtered(component['id'] + ' stdout')
    session.expect('Manual cache hook output: theseus-hook-1.0.0')
    session.seek('Skills (2)')
    session.snap('manual-hook-and-native-details-log')


def run(binary):
    client, version = prerequisites()
    evidence = attempt()
    root = prepare(evidence, client, version, 'user')
    sha, size = digest(client), client.stat().st_size
    recipe = root / 'theseus.toml'
    s = Session(binary, evidence, 'live', ['run', '--recipe', recipe, '--profile', 'user', '--scenario', 'plugin-lifecycle'])
    try:
        s.expect('Preview')
        assert not (root / '.theseus-runs').exists(), 'preview started work'
        assert termios.tcgetattr(s.slave)[3] & termios.ICANON == 0
        s.seek('scope: user')
        s.seek(version)
        s.snap('preview-explicit-start')
        assert not (root / '.theseus-runs').exists(), 'preview browsing started work'
        s.key(b'r')
        s.until(lambda: latest(root, recipe) is not None, 'native live completion', 180)
        path = latest(root, recipe)
        report = verify_common(binary, evidence, 'live', path, 'passed', 'user', version, sha, size)
        browse(s, report, version, sha)
        s.key(b'q')
        s.finish(0)
    finally:
        s.close()
    before = {str(p.relative_to(path.parent)): digest(p) for p in path.parent.rglob('*') if p.is_file()}
    s = Session(binary, evidence, 'offline', ['report', path.parent], offline=True)
    try:
        browse(s, report, version, sha)
        s.key(b'rq')
        s.finish(0)
    finally:
        s.close()
    assert before == {str(p.relative_to(path.parent)): digest(p) for p in path.parent.rglob('*') if p.is_file()}
    cancel_recipe = variant(root, 'cancel')
    s = Session(binary, evidence, 'cancel', ['run', '--recipe', cancel_recipe, '--profile', 'user', '--scenario', 'plugin-lifecycle'])
    try:
        s.expect('Preview')
        s.key(b'r')
        def updating():
            for candidate in (root / '.theseus-runs').glob('*/report.json'):
                current = json.loads(candidate.read_text())
                if current['recipe_path'] == str(cancel_recipe) and current['scenarios']:
                    if any(stage['name'] == 'upgrade' and not stage['commands'] for stage in stages(current)):
                        return wrapper_waiting(current, evidence)
            return False
        s.until(updating, 'native cancellable update', 150)
        s.key(b'c')
        s.until(lambda: latest(root, cancel_recipe) is not None, 'native cancellation cleanup', 30)
        cancel_path = latest(root, cancel_recipe)
        cancelled = verify_common(binary, evidence, 'pty-cancel', cancel_path, 'cancelled', 'user', version, sha, size)
        assert commands(cancelled)[-1]['phase'] == 'upgrade' and commands(cancelled)[-1]['cancelled']
        assert all(c['outcome'] == 'passed' for c in checks(cancelled) if c['checkpoint'] == 'base_installed')
        assert not any(c['phase'] in ('disable_plugin', 'enable_plugin', 'uninstall') for c in commands(cancelled))
        s.snap('active-cancellation')
        s.key(b'q')
        s.finish(130)
    finally:
        s.close()
    assert digest(client) == sha
    (evidence / 'PASS').write_text('Real native 80-column live/offline/cancel PTYs; terminal restoration and offline immutability passed\n')
    print('PASS native narrow live/offline/cancel PTYs; no claim of model/event dispatch', flush=True)


if __name__ == '__main__':
    run(str(Path(sys.argv[1]).resolve()))
