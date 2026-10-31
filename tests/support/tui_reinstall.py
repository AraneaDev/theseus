"""Actual narrow PTY selection and report-2 browsing using the public example."""
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


def comparison_visible(output, resized=False):
    footer = b'\n'.join(output.screen.text().splitlines()[-3:])
    return (output.has(b'Comparison: before_reinstall -> reinstalled')
            and (not resized or ((output.screen.rows, output.screen.columns) == (50, 82)
                                 and output.screen.cells.get((0, 81)) == '┐'
                                 and b'Tab/Enter' in footer)))


def session(root, args, live, saved=None):
    evidence_path = os.environ.get('THESEUS_REINSTALL_EVIDENCE')
    evidence = pathlib.Path(evidence_path or tempfile.mkdtemp(prefix='theseus-reinstall-evidence-'))
    evidence = evidence / ('live' if live else 'offline')
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

    def until(predicate, seconds=8):
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
            # Start with both profiles, narrow to user, cycle the single declared scenario.
            os.write(master, b'ps')
            time.sleep(.15)
            os.write(master, b'r')
            until(lambda: (value := report()) and value['finished_at'], 45)
            saved = next(root.glob('.theseus-runs/*/report.json'))
            # The persisted result precedes the UI's worker-completion boundary.
            # Only its public Finished footer establishes readiness to browse.
            until(lambda: output.has(b'Finished |'))
        else:
            until(lambda: output.has(b'Scenarios', b'Stages'))
        snapshot('ready')
        value = json.loads(saved.read_text())
        assert value['schema_version'] == 8 and value['outcome'] == 'passed', value
        assert value['selected_profiles'] == ['user'], value['selected_profiles']
        assert value['selected_scenarios'] == ['reinstall']
        assert len(value['scenarios']) == 1
        scenario = value['scenarios'][0]
        index = next(i for i, s in enumerate(scenario['stages']) if s['name'] == 'reinstalled')
        os.write(master, b'\t' + b'j' * index)
        until(lambda: output.has(b'> reinstalled'))
        snapshot('selected-narrow')
        comparison = {'before_stage': 'before_reinstall', 'after_stage': 'reinstalled'}
        check_index = next(i for i, check in enumerate(scenario['stages'][index]['checks'])
                           if check.get('comparison') == comparison)
        # Stages record 1 is its summary; the following records are its checks.
        # Enter the evidence pane to display the mapped comparison check itself.
        os.write(master, b'\t' + b'j' * (1 + check_index))
        until(lambda: comparison_visible(output))
        snapshot('comparison-narrow')
        # Make enough space for reference text while retaining the narrow selector check.
        # A fresh reconstruction receives ONLY subsequent bytes: neither changing
        # emulator dimensions nor retaining the narrow cells proves a new frame.
        resized = TerminalOutput(50, 82)
        output.screen.rows, output.screen.columns = 50, 82
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 50, 82, 0, 0))
        until(lambda: comparison_visible(resized, resized=True))
        snapshot('comparison-resized')
        assert any(c.get('comparison') == comparison
                   for c in scenario['stages'][index]['checks'])
        os.write(master, b'q')
        until(lambda: proc.poll() is not None)
        while select.select([master], [], [], 0)[0]:
            pump()
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
        snapshot('finished')
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


def main():
    global binary
    binary = str(pathlib.Path(sys.argv[1]).resolve())
    with tempfile.TemporaryDirectory(prefix='theseus-reinstall-pty-') as tmp:
        root = pathlib.Path(tmp)
        shutil.copytree(repo/'examples/reinstall', root, dirs_exist_ok=True,
                        ignore=shutil.ignore_patterns('.theseus-runs'))
        saved = session(root, ['run'], True)
        # Offline viewing must remain usable without a reachable Docker daemon.
        os.environ['DOCKER_HOST'] = 'unix:///nonexistent-theseus-offline.sock'
        session(root, ['report', str(saved.parent)], False, saved)


if __name__ == "__main__":
    main()
