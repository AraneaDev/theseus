#!/usr/bin/env python3
"""Supervise fixed native stdio probes; never synthesize child observations.

Default subprocess pipes are used unchanged. Windows CreatePipe is a separately
named launcher. Each child has a 15-second kill/reap watchdog. No hosted work.
"""
import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import tempfile
import threading
import time
import unittest

CASES = (
    'idle-then-eof', 'immediate-eof', 'zero-byte-write', 'partial-input-eof',
    'partial-output', 'full-stdout', 'slow-drain', 'stderr-stall', 'stderr-alias',
    'reduced-access', 'named-byte', 'message-pipe', 'overlapped-pipe', 'file',
    'terminal', 'socket', 'parent-idle-duplicate', 'mode-sharing', 'restore-failure',
    'startup-partial-rollback', 'cancel-before-call', 'unresolved-shutdown',
)
CANDIDATES = ('direct_nowait', 'owned_io_experiment')
CONTROL_CASES = {'reduced-access', 'named-byte', 'message-pipe', 'overlapped-pipe',
                 'file', 'terminal', 'socket', 'startup-partial-rollback'}


def events(path):
    raw = path.read_bytes() if path.exists() else b''
    if len(raw) > 65536:
        raise AssertionError('native evidence exceeded 65536 bytes')
    # A single incomplete trailing line is permitted while the writer is live.
    return [json.loads(line) for line in raw.split(b'\n')[:-1]]


def measurement_complete(items, candidate, case):
    if not items or items[0].get('phase') != 'started':
        return False
    fields = {'format_version', 'candidate', 'case', 'phase', 'endpoint', 'api',
              'ok', 'raw_status', 'win32_error', 'observed'}
    if any(set(item) != fields or item['format_version'] != 1
           or item['candidate'] != candidate or item['case'] != case for item in items):
        return False
    if items[-1]['phase'] == 'result':
        return isinstance(items[-1]['observed'].get('pass'), bool)
    return (candidate == 'owned_io_experiment' and case == 'unresolved-shutdown'
            and items[-1]['phase'] == 'forced_exit')


def native_incompatibility(items, candidate, case, returncode, stdout, stderr):
    if (candidate != 'direct_nowait' or returncode != 2 or stdout or stderr
            or not measurement_complete(items, candidate, case)):
        return False
    final = items[-1]
    if final['phase'] != 'result' or final['ok'] is not False or final['observed'].get('pass') is not False:
        return False
    if 'watchdog' in str(final['observed'].get('error', '')).lower():
        return False
    forbidden = {'io', 'ready_idle', 'eof', 'output_pending', 'output_complete', 'stall',
                 'stop', 'joined', 'before_native_call', 'forced_exit', 'rollback_injection', 'rollback'}
    if any(event['phase'] in forbidden for event in items):
        return False
    markers = [(i, e) for i, e in enumerate(items) if e['phase'] == 'native_incompatibility']
    if len(markers) != 1:
        return False
    marker_index, marker = markers[0]
    index = marker['observed'].get('failure_event')
    if type(index) is not int or not 0 <= index < marker_index:
        return False
    failed = items[index]
    native_apis = {'DuplicateHandle', 'fcntl(F_DUPFD_CLOEXEC)', 'NtQueryInformationFile',
                   'GetFileType', 'fstat', 'fstat/F_GETFL', 'native pipe profile',
                   'GetNamedPipeHandleStateW', 'SetNamedPipeHandleState', 'fcntl(F_SETFL)',
                   'GetNamedPipeHandleStateW duplicate', 'fcntl(F_GETFL duplicate)'}
    if (failed['phase'] not in {'duplicate', 'query', 'qualified', 'mode', 'set_nowait', 'mode_sharing'}
            or failed['api'] not in native_apis or failed['ok'] is not False
            or not marker['observed'].get('rollback_complete') or not marker['observed'].get('before_io')):
        return False
    restored = [i for i, e in enumerate(items) if e['phase'] == 'restored'
                and e['observed'].get('reverse_order') is True and e['observed'].get('acquired_handles_closed') is True]
    rollback = [i for i, e in enumerate(items) if e['phase'] == 'qualification_rollback'
                and e['observed'].get('complete') is True]
    return (len(restored) == len(rollback) == 1
            and index < restored[0] < rollback[0] < marker_index < len(items)-1)


def validate(items, candidate, case, returncode):
    assert items and items[0]['phase'] == 'started', 'missing native started event'
    for event in items:
        assert set(event) == {'format_version', 'candidate', 'case', 'phase',
                              'endpoint', 'api', 'ok', 'raw_status', 'win32_error', 'observed'}
        assert event['format_version'] == 1 and event['candidate'] == candidate and event['case'] == case
    phases = {event['phase'] for event in items}
    if items[-1]['phase'] == 'result':
        assert items[-1]['observed'].get('pass') is (returncode == 0), 'exit/result mismatch'
        assert items[-1]['ok'] is (returncode == 0), 'result ok/exit mismatch'
    if case == 'unresolved-shutdown' and candidate == 'owned_io_experiment':
        assert returncode == 2 and 'forced_exit' in phases
        forced = next(e['observed'] for e in items if e['phase'] == 'forced_exit')
        assert forced['reader_owner'] and forced['grace_ms'] == 5000
        return
    if case == 'restore-failure':
        assert returncode == 2 and {'expected_restore_failure', 'restore_fault', 'eof'} <= phases
        return
    assert returncode == 0, 'native candidate failed compatibility'
    assert items[-1]['phase'] == 'result' and items[-1]['observed']['pass'] is True
    if case in CONTROL_CASES:
        if case == 'named-byte':
            assert 'restored' in phases and sum(e['phase'] == 'io' and e['observed'].get('count') == 8 and e['ok'] for e in items) == 2
        if case in {'message-pipe', 'overlapped-pipe'}:
            assert 'rejected' in phases or any(e['phase'] == 'platform_scope' and e['observed'].get('applicable') is False for e in items)
        if case in {'file', 'terminal', 'socket', 'reduced-access'}:
            assert 'rejected' in phases
        if case == 'startup-partial-rollback':
            assert {'rollback_injection', 'rollback', 'restored', 'qualification_rollback'} <= phases
            injection = next(e['observed'] for e in items if e['phase'] == 'rollback_injection')
            rollback = next(e['observed'] for e in items if e['phase'] == 'rollback')
            assert rollback.get('injection_reached') is True and rollback.get('reverse_verified') is True
            assert injection['obligation'] == rollback['obligation']
            assert injection['acquired_endpoints'] == rollback['acquired_endpoints'] == 2
            assert injection['mutated_endpoints'] == rollback['mutated_endpoints']
        return
    assert 'restored' in phases, 'normal completion lacks restoration'
    if candidate == 'owned_io_experiment':
        assert sum(e['phase'] == 'joined' for e in items) == 2
    if case == 'full-stdout':
        assert {'output_pending', 'stall'} <= phases
    elif case in {'partial-output', 'slow-drain'}:
        assert 'output_complete' in phases
    elif case == 'cancel-before-call' and candidate == 'owned_io_experiment':
        assert {'before_native_call', 'cancel_request', 'cancel_complete'} <= phases
        if os.name == 'nt':
            cancellations = [e for e in items if e['api'] == 'CancelSynchronousIo']
            assert cancellations and cancellations[0]['win32_error'] == 1168, 'pre-call cancellation must observe ERROR_NOT_FOUND'
            assert any(e['ok'] for e in cancellations[1:]), 'repeated cancellation did not reach the real pending call'
            assert any(e['phase'] == 'io' and e['win32_error'] == 995 for e in items), 'missing ERROR_OPERATION_ABORTED completion'
    elif case not in {'cancel-before-call', 'unresolved-shutdown'}:
        assert 'eof' in phases
        if case != 'immediate-eof':
            assert 'ready_idle' in phases
        if case == 'stderr-alias':
            alias = next(e['observed'] for e in items if e['phase'] == 'stderr_policy')
            assert alias.get('same_object', alias.get('numeric_equal')) is True
            assert alias['stderr_selected'] is False
        eof = next(e for e in items if e['phase'] == 'eof')
        assert eof['observed']['read_bytes'] == (3 if case == 'partial-input-eof' else 0)


class CreatePipeChild:
    """Literal CreatePipe + STARTF_USESTDHANDLES; default access is untouched."""
    def __init__(self, argv, idle_duplicate=False):
        import msvcrt
        from ctypes import wintypes as w
        self.api_events = []
        kernel = ctypes.WinDLL('kernel32', use_last_error=True)
        self.kernel = kernel
        class Security(ctypes.Structure):
            _fields_ = [('size', w.DWORD), ('descriptor', w.LPVOID), ('inherit', w.BOOL)]
        class Startup(ctypes.Structure):
            _fields_ = [('cb', w.DWORD), ('reserved', w.LPWSTR), ('desktop', w.LPWSTR), ('title', w.LPWSTR),
                        ('x', w.DWORD), ('y', w.DWORD), ('xs', w.DWORD), ('ys', w.DWORD),
                        ('xc', w.DWORD), ('yc', w.DWORD), ('fill', w.DWORD), ('flags', w.DWORD),
                        ('show', w.WORD), ('reserved2size', w.WORD), ('reserved2', ctypes.POINTER(ctypes.c_byte)),
                        ('stdin', w.HANDLE), ('stdout', w.HANDLE), ('stderr', w.HANDLE)]
        class Process(ctypes.Structure):
            _fields_ = [('process', w.HANDLE), ('thread', w.HANDLE), ('pid', w.DWORD), ('tid', w.DWORD)]
        kernel.CreatePipe.argtypes = [ctypes.POINTER(w.HANDLE), ctypes.POINTER(w.HANDLE), ctypes.POINTER(Security), w.DWORD]
        kernel.CreatePipe.restype = w.BOOL
        kernel.SetHandleInformation.argtypes = [w.HANDLE, w.DWORD, w.DWORD]
        kernel.SetHandleInformation.restype = w.BOOL
        kernel.CloseHandle.argtypes = [w.HANDLE]
        kernel.CloseHandle.restype = w.BOOL
        kernel.CreateProcessW.argtypes = [w.LPCWSTR, w.LPWSTR, w.LPVOID, w.LPVOID, w.BOOL, w.DWORD, w.LPVOID, w.LPCWSTR, ctypes.POINTER(Startup), ctypes.POINTER(Process)]
        kernel.CreateProcessW.restype = w.BOOL
        kernel.GetExitCodeProcess.argtypes = [w.HANDLE, ctypes.POINTER(w.DWORD)]
        kernel.GetExitCodeProcess.restype = w.BOOL
        kernel.WaitForSingleObject.argtypes = [w.HANDLE, w.DWORD]
        kernel.WaitForSingleObject.restype = w.DWORD
        kernel.TerminateProcess.argtypes = [w.HANDLE, w.UINT]
        kernel.TerminateProcess.restype = w.BOOL
        kernel.GetCurrentProcess.restype = w.HANDLE
        kernel.DuplicateHandle.argtypes = [w.HANDLE, w.HANDLE, w.HANDLE, ctypes.POINTER(w.HANDLE), w.DWORD, w.BOOL, w.DWORD]
        kernel.DuplicateHandle.restype = w.BOOL
        self.idle_handle = None
        owned = []
        def checked(api, result):
            error = ctypes.get_last_error() if not result else None
            self.api_events.append({'api': api, 'ok': bool(result), 'win32_error': error})
            if not result:
                raise OSError(error, api)
        try:
            pairs = []
            # Default security descriptor, default pipe size, noninheritable by
            # default; mark ONLY the three child ends inheritable afterward.
            for index in range(3):
                read, write = w.HANDLE(), w.HANDLE()
                security = Security(ctypes.sizeof(Security), None, False)
                checked('CreatePipe', kernel.CreatePipe(ctypes.byref(read), ctypes.byref(write), ctypes.byref(security), 0))
                owned.extend([read.value, write.value])
                child_end = read if index == 0 else write
                checked('SetHandleInformation(child inherit)', kernel.SetHandleInformation(child_end, 1, 1))
                pairs.append((read.value, write.value))
            if idle_duplicate:
                retained = w.HANDLE()
                checked('DuplicateHandle(parent-held child stdin, same access)', kernel.DuplicateHandle(kernel.GetCurrentProcess(), pairs[0][0], kernel.GetCurrentProcess(), ctypes.byref(retained), 0, False, 2))
                self.idle_handle = retained.value
                owned.append(retained.value)
            startup = Startup()
            startup.cb = ctypes.sizeof(startup)
            startup.flags = 0x100  # STARTF_USESTDHANDLES
            startup.stdin, startup.stdout = pairs[0][0], pairs[1][1]
            startup.stderr = pairs[2][1]
            process = Process()
            command = ctypes.create_unicode_buffer(subprocess.list2cmdline(argv))
            checked('CreateProcessW', kernel.CreateProcessW(argv[0], command, None, None, True, 0, None, None, ctypes.byref(startup), ctypes.byref(process)))
            self.process, self.thread, self.pid = process.process, process.thread, process.pid
            self.returncode = None
            for handle in [pairs[0][0], pairs[1][1], pairs[2][1]]:
                checked('CloseHandle(unneeded child end)', kernel.CloseHandle(handle))
                owned.remove(handle)
            self.stdin = os.fdopen(msvcrt.open_osfhandle(pairs[0][1], os.O_WRONLY | os.O_BINARY), 'wb', buffering=0)
            owned.remove(pairs[0][1])
            self.stdout = os.fdopen(msvcrt.open_osfhandle(pairs[1][0], os.O_RDONLY | os.O_BINARY), 'rb', buffering=0)
            owned.remove(pairs[1][0])
            self.stderr = os.fdopen(msvcrt.open_osfhandle(pairs[2][0], os.O_RDONLY | os.O_BINARY), 'rb', buffering=0)
            owned.remove(pairs[2][0])
            if self.idle_handle is not None:
                owned.remove(self.idle_handle)
        except BaseException:
            if hasattr(self, 'process'):
                kernel.TerminateProcess(self.process, 2)
                kernel.WaitForSingleObject(self.process, 5000)
                kernel.CloseHandle(self.thread)
                kernel.CloseHandle(self.process)
            raise
        finally:
            for handle in owned:
                kernel.CloseHandle(handle)

    def poll(self):
        from ctypes import wintypes as w
        if self.returncode is None:
            if self.kernel.WaitForSingleObject(self.process, 0) == 0:
                code = w.DWORD()
                if not self.kernel.GetExitCodeProcess(self.process, ctypes.byref(code)):
                    raise ctypes.WinError(ctypes.get_last_error())
                self.returncode = code.value
        return self.returncode

    def kill(self):
        if not self.kernel.TerminateProcess(self.process, 2):
            raise ctypes.WinError(ctypes.get_last_error())

    def close(self):
        if self.idle_handle is not None:
            self.kernel.CloseHandle(self.idle_handle)
        self.kernel.CloseHandle(self.thread)
        self.kernel.CloseHandle(self.process)


def peer_write(stream, data):
    if os.name == 'nt':
        import msvcrt
        from ctypes import wintypes as w
        kernel = ctypes.WinDLL('kernel32', use_last_error=True)
        kernel.WriteFile.argtypes = [w.HANDLE, w.LPCVOID, w.DWORD, ctypes.POINTER(w.DWORD), w.LPVOID]
        kernel.WriteFile.restype = w.BOOL
        written = w.DWORD()
        buffer = ctypes.create_string_buffer(data)
        result = kernel.WriteFile(msvcrt.get_osfhandle(stream.fileno()), buffer, len(data), ctypes.byref(written), None)
        error = ctypes.get_last_error() if not result else None
        assert result and written.value == len(data), f'peer WriteFile failed: {error}'
        return {'api': 'WriteFile', 'raw_status': result, 'bytes': written.value, 'win32_error': error}
    count = os.write(stream.fileno(), data)
    assert count == len(data)
    return {'api': 'write', 'raw_status': count, 'bytes': count}


def run_case(fixture, directory, launcher, candidate, case):
    name = f'{launcher}-{candidate}-{case}'
    path = directory / f'{name}.native.jsonl'
    result = {'launcher': launcher, 'candidate': candidate, 'case': case,
              'watchdog': 'pending', 'test_pass': False, 'native_result': None,
              'expected_fatal_control': case in {'restore-failure', 'unresolved-shutdown'},
              'sidecar': path.name}
    argv = [str(fixture), 'stdio-preflight', f'{candidate}/{case}', str(path)]
    child = None
    drainer = None
    drained = bytearray()
    errors = b''
    output_observed = False
    duplicate = None
    started = time.monotonic()
    try:
        if launcher == 'python_popen_default':
            child = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.PIPE)
        else:
            child = CreatePipeChild(argv, idle_duplicate=case == 'parent-idle-duplicate')
            result['launcher_apis'] = child.api_events
        if case == 'parent-idle-duplicate':
            duplicate = os.dup(child.stdin.fileno())
            result['parent_idle_duplicate'] = 'peer writer; CreatePipe additionally retains same-access child reader' if launcher == 'windows_createpipe' else 'peer writer'
        if case == 'immediate-eof':
            child.stdin.close()
        input_released = case == 'immediate-eof'
        zero_at = None
        while child.poll() is None:
            if time.monotonic() - started > 15:
                result['watchdog'] = 'expired'
                raise TimeoutError('native child exceeded 15 seconds')
            items = events(path)
            phases = {item['phase'] for item in items}
            if 'ready_idle' in phases and not input_released:
                if case == 'zero-byte-write':
                    if zero_at is None:
                        result['peer_write'] = peer_write(child.stdin, b'')
                        zero_at = time.monotonic()
                    assert 'eof' not in phases, 'zero-byte write was mistaken for EOF'
                    if time.monotonic() - zero_at < 0.1:
                        time.sleep(0.005)
                        continue
                if case == 'partial-input-eof':
                    result['peer_write'] = peer_write(child.stdin, b'abc')
                if case not in {'full-stdout', 'slow-drain', 'partial-output'}:
                    child.stdin.close()
                    if duplicate is not None:
                        os.close(duplicate)
                        duplicate = None
                    input_released = True
            if case == 'slow-drain' and 'output_pending' in phases and drainer is None:
                def drain():
                    while True:
                        chunk = child.stdout.read(1024)
                        if not chunk:
                            break
                        drained.extend(chunk)
                        if len(drained) > 262144:
                            break
                        time.sleep(0.001)
                drainer = threading.Thread(target=drain)
                drainer.start()
            time.sleep(0.005)
        result['watchdog'] = 'completed'
        if child.stdin and not child.stdin.closed:
            child.stdin.close()
        if drainer:
            drainer.join(timeout=5)
            if drainer.is_alive():
                raise TimeoutError('parent pipe reader failed to join')
        else:
            drained.extend(child.stdout.read(262145))
        errors = child.stderr.read(513) if child.stderr else b''
        output_observed = True
        assert not errors, 'fixture unexpectedly wrote stderr'
        assert len(drained) <= 262144 and all(byte == ord('O') for byte in drained)
        if case not in {'partial-output', 'slow-drain', 'full-stdout'}:
            assert not drained, 'unexpected stdout bytes'
        if case == 'partial-output':
            assert drained == b'O' * 37
        if case == 'slow-drain':
            assert drained == b'O' * 262144
        if case == 'full-stdout':
            assert drained, 'full stdout control lacks positive output'
        items = events(path)
        result['native_result'] = next((e['observed'] for e in reversed(items) if e['phase'] == 'result'), None)
        validate(items, candidate, case, child.returncode)
        result['test_pass'] = True
    except Exception as error:
        result['error'] = str(error)
    finally:
        if child is not None:
            if child.poll() is None:
                child.kill()
                deadline = time.monotonic() + 5
                while child.poll() is None and time.monotonic() < deadline:
                    time.sleep(0.005)
                if child.poll() is None:
                    # Do not abandon a live process or reader in an ordinary return.
                    os._exit(3)
            if drainer and drainer.is_alive():
                drainer.join(timeout=5)
                if drainer.is_alive():
                    os._exit(3)
            result['exit_code'] = child.returncode
            for stream in (child.stdin, child.stdout, child.stderr):
                if stream and not stream.closed:
                    stream.close()
            if isinstance(child, CreatePipeChild):
                child.close()
        if duplicate is not None:
            os.close(duplicate)
        try:
            native = events(path)
            result['measurement_complete'] = measurement_complete(native, candidate, case)
            result['native_result'] = next((e['observed'] for e in reversed(native) if e['phase'] == 'result'), None)
            result['native_incompatibility'] = output_observed and native_incompatibility(native, candidate, case, result.get('exit_code'), drained, errors)
            if path.exists() and not path.read_bytes().endswith(b'\n'):
                result['measurement_complete'] = False
                result['native_incompatibility'] = False
        except Exception as evidence_error:
            result['measurement_complete'] = False
            result['evidence_error'] = str(evidence_error)
        result['elapsed_seconds'] = round(time.monotonic() - started, 3)
        (directory / f'{name}.supervisor.json').write_text(json.dumps(result, indent=2), encoding='utf-8')
    return result


class LiteralControls(unittest.TestCase):
    def test_incomplete_native_line_does_not_create_ready(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'events'
            path.write_bytes(b'{"phase":"ready_idle"}')
            self.assertEqual(events(path), [])
            path.write_bytes(b'x' * 65537)
            with self.assertRaises(AssertionError):
                events(path)

    def test_missing_direct_measurement_is_not_a_permitted_incompatibility(self):
        self.assertFalse(measurement_complete([], 'direct_nowait', 'idle-then-eof'))
        self.assertFalse(measurement_complete([{'phase': 'started'}], 'direct_nowait', 'idle-then-eof'))

    def test_direct_failures_are_not_blanket_waived(self):
        for reason in ('missing_eof', 'missing_restore', 'wrong_bytes', 'internal_watchdog', 'trailing_evidence', 'corrupt_evidence'):
            with self.subTest(reason=reason):
                self.assertFalse(qualification_case_pass({'candidate': 'direct_nowait',
                    'watchdog': 'completed', 'measurement_complete': True,
                    'test_pass': False, 'error': reason, 'native_incompatibility': False}))

    def test_native_incompatibility_requires_exact_failure_and_reversal(self):
        def event(phase, api='fixture', ok=True, observed=None):
            return {'format_version': 1, 'candidate': 'direct_nowait', 'case': 'idle-then-eof',
                    'phase': phase, 'endpoint': 'stdin', 'api': api, 'ok': ok,
                    'raw_status': None, 'win32_error': None, 'observed': observed or {}}
        items = [event('started'), event('qualified', 'fstat/F_GETFL', False),
                 event('restored', observed={'reverse_order': True, 'acquired_handles_closed': True}),
                 event('qualification_rollback', observed={'complete': True}),
                 event('native_incompatibility', observed={'failure_event': 1, 'rollback_complete': True, 'before_io': True}),
                 event('result', ok=False, observed={'pass': False, 'error': 'endpoint access is incompatible'})]
        check = lambda values, code=2, out=b'': native_incompatibility(values, 'direct_nowait', 'idle-then-eof', code, out, b'')
        successful = [event('started'), event('ready_idle'), event('eof', observed={'read_bytes': 0}),
                      event('restored'), event('result', observed={'pass': True})]
        validate(successful, 'direct_nowait', 'idle-then-eof', 0)
        for phase in ('ready_idle', 'eof', 'restored'):
            with self.assertRaises(AssertionError):
                validate([e for e in successful if e['phase'] != phase], 'direct_nowait', 'idle-then-eof', 0)
        self.assertTrue(check(items))
        self.assertFalse(check(items, code=0))
        self.assertFalse(check(items, out=b'wrong bytes'))
        for removed in ('qualified', 'restored', 'qualification_rollback', 'native_incompatibility'):
            with self.subTest(removed=removed):
                self.assertFalse(check([e for e in items if e['phase'] != removed]))
        self.assertFalse(check(items[:-1] + [event('io')] + items[-1:]))
        self.assertFalse(check(items[:-1] + [event('result', ok=False, observed={'pass': False, 'error': 'direct operation watchdog'})]))
        with tempfile.TemporaryDirectory() as temporary:
            path=Path(temporary)/'native'
            path.write_text(''.join(json.dumps(e)+'\n' for e in items)+'trailing', encoding='utf-8')
            self.assertFalse(path.read_bytes().endswith(b'\n'))
            path.write_bytes(b'{broken}\n')
            with self.assertRaises(json.JSONDecodeError):
                events(path)

    def test_missing_native_observations_never_pass(self):
        with self.assertRaises(AssertionError):
            validate([], 'owned_io_experiment', 'idle-then-eof', 0)


def qualification_case_pass(result):
    return (result["watchdog"] == "completed" and result["measurement_complete"]
            and (result["test_pass"] or (result["candidate"] == "direct_nowait"
                 and result.get("native_incompatibility") is True)))

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--fixture', type=Path)
    parser.add_argument('--evidence', type=Path)
    parser.add_argument('--self-test', action='store_true')
    args = parser.parse_args()
    if args.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(LiteralControls)
        return 0 if unittest.TextTestRunner().run(suite).wasSuccessful() else 1
    fixture = (args.fixture or Path(os.environ['THESEUS_HOST_TEST_FIXTURE'])).resolve()
    directory = (args.evidence or Path(os.environ.get('THESEUS_HOST_EVIDENCE', tempfile.mkdtemp())) / 'mcp-stdio-preflight').resolve()
    directory.mkdir(parents=True, exist_ok=True)
    if (directory / 'identity.json').exists():
        directory = Path(tempfile.mkdtemp(prefix='repeat-', dir=directory))
    metadata = {'source_sha': subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
                'fixture': str(fixture), 'sha256': hashlib.sha256(fixture.read_bytes()).hexdigest(),
                'os': platform.platform(), 'machine': platform.machine(), 'python': sys.version,
                'rustc': subprocess.check_output(['rustc', '-Vv'], text=True)}
    (directory / 'identity.json').write_text(json.dumps(metadata, indent=2), encoding='utf-8')
    results = []
    early_control = run_case(fixture, directory, 'python_popen_default', 'direct_nowait', 'startup-early-failure')
    early_control_pass = (early_control.get('native_incompatibility') is True and not early_control['test_pass']
                          and qualification_case_pass(early_control))
    launchers = ['python_popen_default'] + (['windows_createpipe'] if os.name == 'nt' else [])
    # Ordinary compatibility first, so a decisive rights failure is visible early.
    for launcher in launchers:
        for case in CASES:
            for candidate in CANDIDATES:
                result = run_case(fixture, directory, launcher, candidate, case)
                results.append(result)
                print(json.dumps(result), flush=True)
    summary = {'format_version': 1, 'production_selection': 'not_adopted', 'early_failure_control_pass': early_control_pass,
               'candidates': {candidate: {'pass': all(item['test_pass'] and qualification_case_pass(item) for item in results if item['candidate'] == candidate),
                                           'cases': sum(item['candidate'] == candidate for item in results)} for candidate in CANDIDATES}}
    (directory / 'summary.json').write_text(json.dumps(summary, indent=2), encoding='utf-8')
    # A measured direct incompatibility is retained, not promoted to support.
    # Owned qualification is mandatory; a timeout/missing case fails the job.
    return 0 if early_control_pass and summary['candidates']['owned_io_experiment']['pass'] and all(qualification_case_pass(item) for item in results) else 1


if __name__ == '__main__':
    raise SystemExit(main())
