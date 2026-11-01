"""Required actual-binary terminal proof on each native host."""
import json
import os
import pathlib
import sys
import tempfile
import time
from terminal_session import NativeSession


def resized_frame(terminal):
    cells = terminal.output.screen.cells
    # A buffered 120-column Preview is not evidence of the new 82-column frame.
    return (terminal.output.has(b'Preview') and cells.get((0, 81)) == '┐'
            and cells.get((3, 0)) == '└' and cells.get((3, 81)) == '┘')


binary = sys.argv[1]
evidence = pathlib.Path(os.environ.get('THESEUS_HOST_EVIDENCE') or tempfile.mkdtemp(prefix='theseus-native-terminal-'))
evidence.mkdir(parents=True, exist_ok=True)
profiles = ('default', 'echo-off-vt-off', 'processed-off-vt-on') if os.name == 'nt' else ('default',)
for profile in profiles:
    for action in ('quit', 'raw-control', 'native-control', 'resize-quit'):
        case = action if profile == 'default' else profile + '-' + action
        if os.environ.get('THESEUS_TERMINAL_TRACE') == '1':
            print('native terminal case=' + case + ' phase=start', file=sys.stderr, flush=True)
        terminal = NativeSession(binary, ['--no-animation'], 'examples/standalone', os.environ,
                                 console_profile=profile)
        outcome = 'failed'
        try:
            deadline = time.monotonic() + 8
            while not terminal.output.has(b'Preview') and time.monotonic() < deadline:
                terminal.pump()
            assert terminal.output.has(b'Preview'), terminal.output.screen.text()
            assert terminal.modes['raw'], terminal.modes
            terminal.resize(28, 82)
            if action != 'resize-quit':
                deadline = time.monotonic() + 8
                while not resized_frame(terminal) and time.monotonic() < deadline:
                    terminal.pump()
                assert resized_frame(terminal), terminal.output.screen.text()
            if action == 'native-control':
                import signal
                terminal.send_signal(signal.SIGTERM)
            else:
                terminal.key(b'\x03' if action == 'raw-control' else b'q')
            assert terminal.wait() == 0
            terminal.assert_restored()
            if os.name == 'nt':
                assert terminal.modes['profile'] == profile
                if profile == 'default':
                    assert terminal.modes['before'] == terminal.modes['original']
                elif profile == 'echo-off-vt-off':
                    assert terminal.modes['before'][0] & 4 == 0 and terminal.modes['before'][1] & 4 == 0
                else:
                    assert terminal.modes['before'][0] & 1 == 0 and terminal.modes['before'][1] & 4 != 0
            outcome = 'passed'
            print('native terminal', case, 'displayed preview, raw mode, resize, restored modes, real EOF, joined ownership')
        finally:
            # Record the last complete response and display BEFORE forced cleanup;
            # failure evidence never claims this was a successful app exit.
            try:
                (evidence / (case + '.raw')).write_bytes(terminal.output.data)
                (evidence / (case + '.screen')).write_bytes(terminal.output.screen.text())
                (evidence / (case + '.json')).write_text(json.dumps(dict(
                    phase='before-harness-cleanup', outcome=outcome, state='last-complete-response',
                    modes=terminal.modes, eof=terminal.eof, exit=terminal.returncode,
                    last_operation=terminal.last_operation, rows=terminal.output.screen.rows,
                    columns=terminal.output.screen.columns,
                    artifact=terminal.metadata)), encoding='utf-8')
            finally:
                terminal.close()
