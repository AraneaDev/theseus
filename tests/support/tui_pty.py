"""Actual native terminal validation, including exclusive-root failure restoration."""
import os, pathlib, shutil, sys, tempfile, time
from terminal_session import NativeSession

def run(binary, error):
    repo = pathlib.Path(__file__).resolve().parents[2]
    with tempfile.TemporaryDirectory(prefix='theseus-pty-') as tmp:
        root = pathlib.Path(tmp)
        shutil.copytree(repo/'examples/standalone', root, dirs_exist_ok=True)
        if error:
            (root/'.theseus-runs').write_bytes(b'exclusive collision')
        terminal = NativeSession(binary, ['--no-animation'], root, os.environ)
        try:
            deadline = time.monotonic()+8
            while not terminal.output.has(b'Preview') and time.monotonic()<deadline:
                terminal.pump()
            assert terminal.output.has(b'Preview'), terminal.output.screen.text()
            assert not (root/'.theseus-runs').exists() or error, 'preview executed automatically'
            assert terminal.modes['raw'], 'raw mode not entered'
            terminal.key(b'r' if error else b'q')
            assert terminal.wait() == (2 if error else 0), terminal.output.data[-3000:]
            terminal.assert_restored()
            if error:
                assert b'create run evidence directory' in terminal.output.data, terminal.output.data[-3000:]
                assert (root/'.theseus-runs').read_bytes() == b'exclusive collision'
            print('native terminal engine-error restoration' if error else 'native terminal preview/user-exit restoration')
        finally:
            terminal.close()

if __name__ == '__main__':
    run(sys.argv[1], False)
    run(sys.argv[1], True)
