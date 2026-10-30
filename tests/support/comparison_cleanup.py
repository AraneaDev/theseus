"""Fallback cleanup for only the suite references recorded by acceptance."""
import subprocess, sys, uuid

def fallback_cleanup(binary, roots, evidence, original_error=None, prefix_args=()):
    failures = []
    for root in roots:
        for saved in root.glob('.theseus-comparisons/*/comparison.json'):
            name = evidence / ('fallback-cleanup-' + uuid.uuid4().hex)
            try:
                result = subprocess.run([binary, *prefix_args, 'cleanup', str(saved)], capture_output=True)
                stdout, stderr, status = result.stdout, result.stderr, result.returncode
            except OSError as error:
                stdout, stderr, status = b'', str(error).encode(), 'spawn-error'
            name.with_suffix('.stdout').write_bytes(stdout)
            name.with_suffix('.stderr').write_bytes(stderr)
            name.with_suffix('.status').write_text(str(status))
            if status != 0:
                failures.append(f'{saved} (status {status}; evidence {name})')
    if failures:
        message = 'fallback cleanup failed: ' + '; '.join(failures)
        if original_error is not None:
            # Keep the original traceback/assertion as the primary failure.
            print(message, file=sys.stderr, flush=True)
        else:
            raise RuntimeError(message)
