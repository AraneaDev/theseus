"""Default animation PTYs: reconstruct real cells and verify terminal restoration.

Usage: tui_animation.py BINARY [--docker]. Docker mode adds real cancellation;
ordinary mode stays offline and also runs from the regular Rust TUI tests.
"""
import json
import os
import pathlib
import shutil
import signal
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
from terminal_session import NativeSession, native_fixture
from docker_absence import require_container_absent

binary = str(pathlib.Path(sys.argv[1]).resolve())
repo = pathlib.Path(__file__).resolve().parents[2]
log_root = pathlib.Path(os.environ.get('THESEUS_ANIMATION_EVIDENCE', str(pathlib.Path(tempfile.gettempdir())/'theseus-animation-pty')))
log_root.mkdir(parents=True, exist_ok=True)


selected_case = None
if '--case' in sys.argv:
    assert sys.argv.count('--case') == 1, 'provide --case once'
    case_index = sys.argv.index('--case') + 1
    assert case_index < len(sys.argv) and sys.argv[case_index], '--case requires a nonempty session name'
    selected_case = sys.argv[case_index]
executed_cases = []


class Session:
    def __init__(self, name, rows=24, columns=80, term='xterm-256color', args=(), active=False, error=False, draw_error=False, cached_error=False):
        self.name = name
        self.tmp = tempfile.TemporaryDirectory(prefix='theseus-animation-')
        self.root = pathlib.Path(self.tmp.name)
        shutil.copytree(repo / ('tests/fixtures/lifecycle' if active else 'examples/standalone'), self.root, dirs_exist_ok=True)
        if active:
            recipe = self.root / 'recipe.toml'
            recipe.write_text(recipe.read_text().replace('THESEUS_FIXTURE_FAULT = ""', 'THESEUS_FIXTURE_FAULT = "cancellation"'))
            args = ('run', '--recipe', 'recipe.toml', '--profile', 'user', '--scenario', 'clean')
        if error:
            (self.root / '.theseus-runs').write_bytes(b'exclusive collision')
        if args and args[0] == 'report':
            saved = self.root / 'saved'
            saved.mkdir()
            shutil.copyfile(args[1], saved / 'report.json')
            args = ('report', str(saved), *args[2:])
        environment = {**os.environ, 'TERM': term}
        if cached_error:
            # Fixed native peer waits for an explicit complete release record.
            self.gate = self.root / 'prepare-gate'
            executable = self.root / 'bin' / ('docker.exe' if os.name == 'nt' else 'docker')
            executable.parent.mkdir()
            shutil.copy2(native_fixture()[0], executable)
            environment['THESEUS_DOCKER_TEST_MODE'] = 'cached-error'
            environment['PATH'] = str(executable.parent) + os.pathsep + os.environ['PATH']
        if draw_error:
            # Fail exactly the first scene write, then permit Guard's restoration
            # writes. The production binary and actual PTY remain unchanged.
            source = self.root / 'fail_scene_write.c'
            source.write_text(r'''#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <string.h>
#include <unistd.h>
ssize_t write(int fd, const void *buf, size_t count) {
    static int failed;
    ssize_t (*real_write)(int, const void *, size_t) = dlsym(RTLD_NEXT, "write");
    if (fd == STDOUT_FILENO && !failed && memmem(buf, count, "T H E S E U S", 13)) {
        failed = 1;
        errno = EIO;
        return -1;
    }
    return real_write(fd, buf, count);
}
''')
            library = self.root / 'fail_scene_write.so'
            subprocess.run(['cc', '-shared', '-fPIC', str(source), '-o', str(library), '-ldl'], check=True, capture_output=True)
            environment['LD_PRELOAD'] = str(library)
        self.rows, self.columns = rows, columns
        self.native = NativeSession(binary, args, self.root, environment, rows, columns)
        self.proc = self.native
        self.before = self.native.modes['before']
        self.screen = self.native.output.screen
        self.data = self.native.output.data
        self.captions = set()
        self.heroes = set()
        self.hero_order = []

    def resize(self, rows, columns):
        self.native.resize(rows, columns)
        self.screen = self.native.output.screen
        self.rows, self.columns = rows, columns

    def drain(self):
        self.native.pump()
        text = self.screen.text()
        for caption in [b'Entering the labyrinth', b'The thread is anchored', b'Following the thread home', b'Session ended']:
            if caption in text:
                self.captions.add(caption)
        if b'T H E S E U S' in text:
            offset_x, offset_y = (self.columns-40)//2, (self.rows-18)//2
            for (y,x), cell in self.screen.cells.items():
                if cell == '@':
                    position = (x-offset_x, y-offset_y)
                    self.heroes.add(position)
                    if not self.hero_order or self.hero_order[-1] != position:
                        self.hero_order.append(position)

    def until(self, predicate, timeout=3):
        deadline = time.monotonic() + timeout
        while not predicate() and time.monotonic() < deadline:
            self.drain()
        assert predicate(), (self.name, self.proc.poll(), self.screen.text(), self.data[-2000:])

    def visible(self, text):
        return text.encode() in self.screen.text()

    def key(self, value):
        self.native.key(value)

    def intro(self):
        self.until(lambda: self.visible('Entering the labyrinth'))
        assert not list(self.root.glob('.theseus-runs/*/report.json')), 'intro launched engine'

    def preview(self):
        self.until(lambda: self.visible('Preview'))
        assert not list(self.root.glob('.theseus-runs/*/report.json')), 'preview launched engine'

    def outro(self):
        self.key(b'q')
        self.until(lambda: self.visible('Following the thread home'))

    def finish(self, code=0, timeout=2):
        self.until(lambda: self.proc.poll() is not None, timeout)
        self.drain()
        assert self.proc.returncode == code, (self.name, self.proc.returncode, self.data[-3000:])
        self.native.assert_restored()
        for marker in [b'\x1b[?1049h', b'\x1b[?1049l', b'\x1b[?25h']:
            assert marker in self.data, (self.name, 'terminal not restored', marker)

    def close(self):
        if self.proc.poll() is None:
            self.proc.kill()
            self.proc.wait()
        (log_root / (self.name + '.log')).write_bytes(self.data)
        for report in self.root.glob('.theseus-runs/*/report.json'):
            dest = log_root / self.name / report.parent.name
            shutil.copytree(report.parent, dest, dirs_exist_ok=True)
        for journal in self.root.glob('.theseus-runs/*/journal.json'):
            subprocess.run([binary, 'cleanup', str(journal.parent)], capture_output=True, timeout=15)
        self.native.close()
        self.tmp.cleanup()


def run(name, action, **kwargs):
    if selected_case is not None and name != selected_case:
        return
    executed_cases.append(name)
    session = Session(name, **kwargs)
    try:
        action(session)
        print(f'PTY animation {name}: passed', flush=True)
    finally:
        session.close()


def progress(s):
    s.intro()
    # Engine keys during intro must be discarded, including a queued batch.
    s.key(b'rpsr')
    s.until(lambda: s.visible('Preview'), 7)
    assert s.heroes == {(1,11),(5,11),(5,9),(11,9),(11,5),(28,5)}, s.heroes
    s.preview()
    assert b'The thread is anchored' in s.captions
    assert s.hero_order == [(1,11),(5,11),(5,9),(11,9),(11,5),(28,5)], s.hero_order
    s.hero_order.clear()
    s.outro()
    s.finish(timeout=7)
    assert s.hero_order == [(28,5),(11,5),(11,9),(5,9),(5,11),(1,11)], s.hero_order
    assert b'Session ended' in s.captions
    assert not list(s.root.glob('.theseus-runs/*/report.json'))


def skips(s, skip):
    s.intro()
    s.key(b' r')  # queued r belongs to the intro and must be discarded
    s.preview()
    s.outro()
    s.key(skip)
    s.finish()


def intro_quit(s):
    s.intro()
    s.key(b'q')
    s.finish()
    assert b'Following the thread home' not in s.captions
    assert not list(s.root.glob('.theseus-runs/*/report.json'))


def intro_burst(s):
    s.intro()
    s.key(b' ' + b'r' * 256)
    s.preview()
    s.outro()
    s.key(b' ')
    s.finish()
    assert not list(s.root.glob('.theseus-runs/*/report.json')), 'intro burst launched engine'


def quit_batch(s):
    s.intro()
    s.key(b' ')
    s.preview()
    s.key(b'qr')
    s.until(lambda: s.visible('Following the thread home'))
    assert not list(s.root.glob('.theseus-runs/*/report.json')), 'quit batch launched engine'
    s.key(b' ')
    s.finish()


def bypass(s):
    if 'report' in s.proc.args:
        s.until(lambda: s.visible('Offline report'))
    else:
        s.preview()
    s.key(b'q')
    s.finish()
    assert not s.captions, s.captions


def resize(s, phase):
    s.intro()
    if phase == 'outro':
        s.key(b' ')
        s.preview()
        s.outro()
    s.resize(17, 39)
    if phase == 'intro':
        s.preview()
        s.key(b'q')
    s.finish()


def terminate(s, phase, action):
    s.intro()
    if phase == 'outro':
        s.key(b' ')
        s.preview()
        s.outro()
    if isinstance(action, bytes):
        s.key(action)
    else:
        s.proc.send_signal(action)
    s.finish()
    if phase == 'intro':
        assert b'Following the thread home' not in s.captions


def draw_failure(s):
    s.finish(code=2)
    assert b'Input/output error' in s.data, s.data
    assert b'Following the thread home' not in s.captions


def worker_error(s):
    s.intro()
    s.key(b' ')
    s.preview()
    s.key(b'r')
    s.finish(code=2)
    assert b'create run evidence directory' in s.data
    assert b'Following the thread home' not in s.captions


def cached_worker_error(s):
    s.preview()
    s.key(b'r')
    s.until(lambda: (s.root / 'prepare-ready').is_file() and (s.root / 'prepare-ready').read_bytes() == b'ready' and s.visible('Running Started'))
    saved = next(s.root.glob('.theseus-runs/*/report.json'))
    saved.rename(saved.with_name('cached-report.json'))
    saved.mkdir()  # Make the final atomic report rename fail after cached Started.
    with s.gate.open('wb') as gate:
        gate.write(b'x')
    s.until(lambda: s.visible('Engine error'))
    s.key(b'q')
    s.finish(code=2)
    restored = bytes(s.data).rsplit(b'\x1b[?1049l', 1)[1]
    for expected in [b'Engine error:', b'Saved report:', str(saved).encode(),
                     b'Recovery: theseus cleanup', str(saved.parent).encode()]:
        assert expected in restored, (s.name, 'missing restored-terminal diagnostic', expected, restored)
    assert b'Following the thread home' not in s.captions


def active_cancel(s, action):
    s.intro()
    s.key(b' ')
    s.preview()
    s.key(b'r')
    def report():
        paths = list(s.root.glob('.theseus-runs/*/report.json'))
        return json.loads(paths[0].read_text()) if paths else None
    def child_running():
        value = report()
        if not value or not value['scenarios'] or not value['scenarios'][0]['container_id']:
            return False
        return subprocess.run(['docker', 'exec', value['scenarios'][0]['container_id'], '/bin/sh', '-c', 'test -s /workspace/child.pid'], capture_output=True, timeout=3).returncode == 0
    s.until(child_running, 25)
    start = time.monotonic()
    if isinstance(action, bytes):
        s.key(action)
    else:
        s.proc.send_signal(action)
    # Cancellation must reach the existing teardown status before exit, with no
    # decorative outro even though animation is enabled by default.
    s.until(lambda: s.visible('Cancellation requested; waiting for owned-resource teardown'), 2)
    assert b'Following the thread home' not in s.captions
    s.finish(code=130, timeout=7)
    assert time.monotonic()-start < 7
    assert b'Following the thread home' not in s.captions
    value = report()
    assert value['cancelled'] and value['outcome'] == 'cancelled', value
    journal_path = next(s.root.glob('.theseus-runs/*/journal.json'))
    journal = json.loads(journal_path.read_text())
    assert journal['resources'] and all(r['removed'] for r in journal['resources']), journal
    for resource in journal['resources']:
        require_container_absent(resource['container_id'], subprocess.run(
            ['docker','inspect',resource['container_id']], capture_output=True, text=True, timeout=10))


if '--docker' in sys.argv:
    for name, action in [('q',b'q'),('ctrl-c',b'\x03'),('sigint',signal.SIGINT),('sigterm',signal.SIGTERM)]:
        run('active-'+name, lambda s, a=action: active_cancel(s,a), active=True)
else:
    run('default-progression', progress)
    for name, skip in [('space',b' '),('q',b'q')]:
        run('skip-'+name, lambda s, k=skip: skips(s,k))
    run('intro-q', intro_quit)
    run('quit-batch', quit_batch)
    run('intro-burst', intro_burst)
    run('no-animation-default', bypass, args=('--no-animation',))
    run('no-animation-run', bypass, args=('run','--no-animation'))
    run('no-animation-report', bypass, args=('report',str(repo/'tests/fixtures/reports/schema1-report.json'),'--no-animation'))
    for rows, columns in [(18,39),(17,40)]:
        run(f'small-{columns}x{rows}', bypass, rows=rows, columns=columns)
    run('dumb', bypass, term='dumb')
    for phase in ['intro','outro']:
        run('resize-'+phase, lambda s, p=phase: resize(s,p))
        for name, action in [('ctrl-c',b'\x03'),('sigint',signal.SIGINT),('sigterm',signal.SIGTERM)]:
            run(phase+'-'+name, lambda s, p=phase, a=action: terminate(s,p,a))
    run('worker-error', worker_error, error=True)
    run('cached-worker-error-disabled', cached_worker_error, cached_error=True, args=('--no-animation',))
    run('cached-worker-error-dumb', cached_worker_error, cached_error=True, term='dumb')
    run('cached-worker-error-small', cached_worker_error, cached_error=True, rows=17, columns=39)
    # Linux-only supplemental write(2) interposition. Actual native engine-error
    # restoration and common guard/join tests run on every host; this is not a
    # claimed Windows/macOS first-scene syscall fault run.
    if sys.platform == 'linux':
        run('draw-error', draw_failure, draw_error=True)

assert executed_cases, f'unknown or unavailable --case: {selected_case!r}'
