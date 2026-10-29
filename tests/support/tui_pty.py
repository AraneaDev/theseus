"""Actual Unix PTY validation, including termios and alternate-screen restoration."""
import fcntl, os, pathlib, pty, select, shutil, struct, subprocess, sys, tempfile, termios, time
binary = str(pathlib.Path(sys.argv[1]).resolve())
repo = pathlib.Path(__file__).resolve().parents[2]
def run(error):
    with tempfile.TemporaryDirectory(prefix='theseus-pty-') as tmp:
        root = pathlib.Path(tmp)
        shutil.copytree(repo/'examples/standalone', root, dirs_exist_ok=True)
        if error:
            (root/'.theseus-runs').symlink_to('/proc', target_is_directory=True)
        master, slave = pty.openpty()
        before = termios.tcgetattr(slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 36, 120, 0, 0))
        proc = subprocess.Popen([binary], cwd=root, stdin=slave, stdout=slave, stderr=slave, env={**os.environ,'TERM':'xterm-256color'})
        data = bytearray()
        def drain(until):
            while time.monotonic() < until:
                if select.select([master], [], [], .05)[0]:
                    try: data.extend(os.read(master, 65536))
                    except OSError: break
                if proc.poll() is not None: break
        try:
            ready_deadline = time.monotonic()+5
            while b'Preview' not in data and proc.poll() is None and time.monotonic() < ready_deadline:
                drain(time.monotonic()+.05)
            assert b'Preview' in data, data[-2000:]
            assert not (root/'.theseus-runs').exists() or error, 'preview executed automatically'
            assert not (termios.tcgetattr(slave)[3] & termios.ICANON), 'raw mode was not entered'
            os.write(master, b'r' if error else b'q')
            drain(time.monotonic()+8)
            assert proc.poll() is not None, 'UI failed to exit'
            # Read final pending restoration/error bytes after child exit.
            while select.select([master], [], [], .05)[0]:
                data.extend(os.read(master, 65536))
            assert proc.returncode == (2 if error else 0), (proc.returncode, data[-3000:])
            assert termios.tcgetattr(slave) == before, 'terminal attributes were not restored'
            assert b'\x1b[?1049h' in data and b'\x1b[?1049l' in data, 'alternate screen not restored'
            if error: assert b'create run evidence directory' in data, data[-3000:]
            print('PTY engine-error restoration passed' if error else 'PTY preview/user-exit restoration passed')
        finally:
            if proc.poll() is None: proc.kill(); proc.wait()
            os.close(master); os.close(slave)
run(False)
run(True)
