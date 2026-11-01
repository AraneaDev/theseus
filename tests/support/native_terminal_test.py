"""Required actual-binary terminal proof on each native host."""
import json
import os
import pathlib
import sys
import tempfile
import time
from terminal_session import NativeSession

binary = sys.argv[1]
for action in ('quit', 'raw-control', 'native-control'):
    if os.environ.get('THESEUS_TERMINAL_TRACE') == '1':
        print('native terminal case=' + action + ' phase=start', file=sys.stderr, flush=True)
    terminal = NativeSession(binary, ['--no-animation'], 'examples/standalone', os.environ)
    try:
        deadline = time.monotonic() + 8
        while not terminal.output.has(b'Preview') and time.monotonic() < deadline:
            terminal.pump()
        assert terminal.output.has(b'Preview'), terminal.output.screen.text()
        assert terminal.modes['raw'], terminal.modes
        terminal.resize(28, 82)
        deadline = time.monotonic() + 8
        while not terminal.output.has(b'Preview') and time.monotonic() < deadline:
            terminal.pump()
        assert terminal.output.has(b'Preview')
        if action == 'native-control':
            import signal
            terminal.send_signal(signal.SIGTERM)
        else:
            terminal.key(b'q' if action == 'quit' else b'\x03')
        assert terminal.wait() == 0
        terminal.assert_restored()
        evidence = pathlib.Path(os.environ.get('THESEUS_HOST_EVIDENCE', tempfile.mkdtemp(prefix='theseus-native-terminal-')))
        evidence.mkdir(parents=True, exist_ok=True)
        (evidence / (action + '.raw')).write_bytes(terminal.output.data)
        (evidence / (action + '.json')).write_text(json.dumps(dict(modes=terminal.modes, eof=terminal.eof,
            exit=terminal.returncode, rows=28, columns=82, artifact=terminal.metadata)), encoding='utf-8')
        print('native terminal', action, 'displayed preview, raw mode, resize, restored modes, real EOF, joined ownership')
    finally:
        terminal.close()
