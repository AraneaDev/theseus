#!/usr/bin/env python3
"""Native server supervision. Every timeout fails and reaps the complete child group."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import queue
import signal
import sys
import subprocess
import threading
import time

MODERN = {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}
NAMES = ["recipe_inspect", "run_start", "run_list", "run_status", "run_cancel", "run_evidence"]
TIMEOUT = 15


# Windows keeps its Job handle after the leader exits. The process is suspended
# until assignment, so no descendant can escape through the assignment race.
class WindowsJob:
    def __init__(self, child):
        import ctypes as c
        from ctypes import wintypes as w
        self.c, self.w = c, w
        self.api = c.WinDLL("kernel32", use_last_error=True)
        class Basic(c.Structure):
            _fields_ = [("process_time", c.c_int64), ("job_time", c.c_int64), ("flags", w.DWORD), ("minimum", c.c_size_t), ("maximum", c.c_size_t), ("active", w.DWORD), ("affinity", c.c_size_t), ("priority", w.DWORD), ("scheduling", w.DWORD)]
        class Limits(c.Structure):
            _fields_ = [("basic", Basic), ("io", c.c_uint64 * 6), ("process_memory", c.c_size_t), ("job_memory", c.c_size_t), ("peak_process", c.c_size_t), ("peak_job", c.c_size_t)]
        class Thread(c.Structure):
            _fields_ = [("size", w.DWORD), ("usage", w.DWORD), ("tid", w.DWORD), ("pid", w.DWORD), ("base", w.LONG), ("delta", w.LONG), ("flags", w.DWORD)]
        signatures = {
            "CreateJobObjectW": ([c.c_void_p, w.LPCWSTR], w.HANDLE),
            "SetInformationJobObject": ([w.HANDLE, c.c_int, c.c_void_p, w.DWORD], w.BOOL),
            "AssignProcessToJobObject": ([w.HANDLE, w.HANDLE], w.BOOL),
            "TerminateJobObject": ([w.HANDLE, w.UINT], w.BOOL),
            "QueryInformationJobObject": ([w.HANDLE, c.c_int, c.c_void_p, w.DWORD, c.c_void_p], w.BOOL),
            "CreateToolhelp32Snapshot": ([w.DWORD, w.DWORD], w.HANDLE),
            "Thread32First": ([w.HANDLE, c.POINTER(Thread)], w.BOOL),
            "Thread32Next": ([w.HANDLE, c.POINTER(Thread)], w.BOOL),
            "OpenThread": ([w.DWORD, w.BOOL, w.DWORD], w.HANDLE),
            "ResumeThread": ([w.HANDLE], w.DWORD),
            "CloseHandle": ([w.HANDLE], w.BOOL),
        }
        for name, (args, result) in signatures.items():
            function = getattr(self.api, name)
            function.argtypes, function.restype = args, result
        self.handle = self.api.CreateJobObjectW(None, None)
        if not self.handle:
            raise c.WinError(c.get_last_error())
        try:
            limits = Limits()
            limits.basic.flags = 0x2000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE; no breakaway.
            self.check(self.api.SetInformationJobObject(self.handle, 9, c.byref(limits), c.sizeof(limits)))
            self.check(self.api.AssignProcessToJobObject(self.handle, int(child._handle)))
            snapshot = self.api.CreateToolhelp32Snapshot(4, 0)
            self.check(snapshot != c.c_void_p(-1).value)
            try:
                entry = Thread()
                entry.size = c.sizeof(entry)
                found = self.api.Thread32First(snapshot, c.byref(entry))
                primary = None
                while found:
                    if entry.pid == child.pid:
                        assert primary is None, "suspended process has multiple initial threads"
                        primary = entry.tid
                    entry.size = c.sizeof(entry)
                    found = self.api.Thread32Next(snapshot, c.byref(entry))
                assert primary is not None, "missing suspended primary thread"
                thread = self.api.OpenThread(2, False, primary)
                self.check(thread)
                try:
                    assert self.api.ResumeThread(thread) == 1, "primary thread was not suspended once"
                finally:
                    self.check(self.api.CloseHandle(thread))
            finally:
                self.check(self.api.CloseHandle(snapshot))
        except BaseException:
            self.api.TerminateJobObject(self.handle, 2)
            self.api.CloseHandle(self.handle)
            self.handle = None
            raise

    def check(self, value):
        if not value:
            raise self.c.WinError(self.c.get_last_error())

    def terminate_and_wait(self):
        c = self.c
        class Accounting(c.Structure):
            _fields_ = [("times", c.c_int64 * 4), ("faults", c.c_uint32), ("total", c.c_uint32), ("active", c.c_uint32), ("terminated", c.c_uint32)]
        info = Accounting()
        self.check(self.api.QueryInformationJobObject(self.handle, 1, c.byref(info), c.sizeof(info), None))
        before = info.active
        self.check(self.api.TerminateJobObject(self.handle, 2))
        deadline = time.monotonic() + 5
        while True:
            self.check(self.api.QueryInformationJobObject(self.handle, 1, c.byref(info), c.sizeof(info), None))
            if info.active == 0:
                break
            assert time.monotonic() < deadline, "job still has active processes after termination"
            time.sleep(.01)
        self.check(self.api.CloseHandle(self.handle))
        self.handle = None
        return {"containment": "windows_job_before_resume", "active_before_kill": before, "active_after_kill": 0, "tree_empty": True}


def spawn(command, **kwargs):
    if sys.platform == "linux":
        # Adopt orphaned descendants so the supervisor can reap them, not PID 1.
        import ctypes
        libc = ctypes.CDLL(None, use_errno=True)
        libc.prctl.argtypes = [ctypes.c_int] + [ctypes.c_ulong] * 4
        libc.prctl.restype = ctypes.c_int
        if libc.prctl(36, 1, 0, 0, 0) != 0:
            raise OSError(ctypes.get_errno(), "PR_SET_CHILD_SUBREAPER")
    options = {"creationflags": subprocess.CREATE_NEW_PROCESS_GROUP | 4} if os.name == "nt" else {"start_new_session": True}
    options.update({"stdin": subprocess.PIPE, "stdout": subprocess.PIPE, "stderr": subprocess.PIPE})
    options.update(kwargs)
    child = subprocess.Popen([str(x) for x in command], **options)
    child._tree_lock = threading.Lock()
    child._tree_receipt = None
    try:
        child._job = WindowsJob(child) if os.name == "nt" else None
    except BaseException:
        child.kill()
        child.wait(timeout=5)
        raise
    return child


def reap(child):
    # Never infer tree death from the leader's status. Preserve cleanup authority
    # until all processes are gone, even after wait() already reaped the leader.
    with child._tree_lock:
        if child._tree_receipt is not None:
            return child._tree_receipt
        leader_exited = child.poll() is not None
        if os.name == "nt":
            receipt = child._job.terminate_and_wait()
            child.wait(timeout=5)
        else:
            kill_permission_error = None
            try:
                os.killpg(child.pid, signal.SIGKILL)
                killed_group = True
            except ProcessLookupError:
                killed_group = False
            except PermissionError as error:
                # Darwin can reject a zombie-only group. This does not prove
                # completion: retain the error and still require disappearance.
                killed_group = False
                kill_permission_error = error.errno
            child.wait(timeout=5)
            reaped = []
            probe_permission_errors = 0
            deadline = time.monotonic() + 5
            while True:
                try:
                    pid, status = os.waitpid(-child.pid, os.WNOHANG)
                    if pid:
                        reaped.append({"pid": pid, "status": status})
                        continue
                except ChildProcessError:
                    pass
                try:
                    os.killpg(child.pid, 0)
                except ProcessLookupError:
                    break
                except PermissionError:
                    # Unknown is not empty; only ESRCH above can settle the tree.
                    probe_permission_errors += 1
                assert time.monotonic() < deadline, ("process group did not disappear after SIGKILL",
                                                     {"kill_permission_error": kill_permission_error,
                                                      "probe_permission_errors": probe_permission_errors})
                time.sleep(.01)
            receipt = {"containment": "posix_process_group", "kill_requested": killed_group,
                       "kill_permission_error": kill_permission_error,
                       "probe_permission_errors": probe_permission_errors,
                       "descendants_reaped": reaped, "tree_empty": True}
        receipt["leader_exited_before_cleanup"] = leader_exited
        child._tree_receipt = receipt
        return receipt


def wire(value):
    return json.dumps(value, separators=(",", ":"), ensure_ascii=False).encode() + b"\n"


def message(method, params=None, ident=1):
    return {"jsonrpc": "2.0", "id": ident, "method": method, "params": {**(params or {}), "_meta": MODERN}}


def identity(binary):
    root = Path(__file__).resolve().parents[2]
    files = ["src/cli.rs", "src/main.rs", "src/mcp_server/coordinator.rs", "src/mcp_server/fixture.rs", "src/mcp_server/query.rs", "src/mcp_server/worker.rs", "tests/mcp_server_stdio.rs", "tests/support/mcp_server_stdio.py"]
    return {"binary": str(Path(binary).resolve()), "binary_sha256": hashlib.sha256(Path(binary).read_bytes()).hexdigest(), "source_sha": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip(), "sources": {p: hashlib.sha256((root / p).read_bytes()).hexdigest() for p in files}, "rustc": subprocess.check_output(["rustc", "-Vv"], text=True), "os": platform.platform(), "machine": platform.machine()}


class Server:
    def __init__(self, binary, project, output, evidence=None, era="modern"):
        self.child = spawn([binary, "serve", "--project-root", project, "--output-root", output])
        self.era, self.serial, self.lines = era, 0, queue.Queue()
        self.stdout, self.stderr = bytearray(), bytearray()
        self.evidence = Path(evidence) if evidence else None
        self.readers = []
        for stream, sink, framed in [(self.child.stdout, self.stdout, True), (self.child.stderr, self.stderr, False)]:
            def consume(stream=stream, sink=sink, framed=framed):
                while True:
                    data = stream.readline() if framed else stream.read(4096)
                    if not data:
                        if framed:
                            self.lines.put(None)
                        return
                    sink.extend(data)
                    if framed:
                        self.lines.put(data)
            thread = threading.Thread(target=consume, daemon=True)
            thread.start()
            self.readers.append(thread)
        if era == "legacy":
            self.request("initialize", {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "native-test", "version": "1"}})
            self.notify("notifications/initialized")

    def send(self, value):
        self.child.stdin.write(wire(value))
        self.child.stdin.flush()

    def notify(self, method, params=None):
        self.send({"jsonrpc": "2.0", "method": method, "params": params or {}})

    def receive(self, timeout=TIMEOUT):
        try:
            line = self.lines.get(timeout=timeout)
            assert line is not None, "server closed before response"
            assert line.endswith(b"\n") and b"\x1b" not in line, "incomplete/decorated stdout"
            value = json.loads(line)
            assert value.get("jsonrpc") == "2.0", "non-protocol stdout"
            assert line == wire(value), "stdout must be compact newline JSON"
            return value
        except BaseException:
            reap(self.child)
            for reader in self.readers:
                reader.join(timeout=5)
                assert not reader.is_alive(), "reader remained after empty tree"
            self._save()
            raise

    def request(self, method, params=None):
        self.serial += 1
        params = dict(params or {})
        if self.era == "modern":
            params["_meta"] = MODERN
        self.send({"jsonrpc": "2.0", "id": self.serial, "method": method, "params": params})
        value = self.receive()
        assert value.get("id") == self.serial, value
        return value

    def tool(self, name, arguments):
        value = self.request("tools/call", {"name": name, "arguments": arguments})
        assert "result" in value, value
        result = value["result"]
        application = result.get("structuredContent")
        if application is None:
            application = json.loads(result["content"][0]["text"])
        assert result["isError"] == (not application["ok"]), value
        return application

    def _save(self):
        if self.evidence:
            self.evidence.parent.mkdir(parents=True, exist_ok=True)
            self.evidence.with_suffix(".stdout.jsonl").write_bytes(self.stdout)
            self.evidence.with_suffix(".stderr").write_bytes(self.stderr)

    def close(self, expected=0):
        try:
            if self.child.stdin and not self.child.stdin.closed:
                self.child.stdin.close()
            code = self.child.wait(timeout=TIMEOUT)
            reap(self.child)
            for reader in self.readers:
                reader.join(timeout=2)
                assert not reader.is_alive(), "stream reader did not settle"
            assert code == expected, (code, bytes(self.stderr))
            assert not self.stderr, bytes(self.stderr)
            for line in bytes(self.stdout).splitlines(keepends=True):
                assert wire(json.loads(line)) == line
            return code
        finally:
            reap(self.child)
            for reader in self.readers:
                reader.join(timeout=5)
                assert not reader.is_alive(), "reader remained after empty tree"
            self._save()


def project_at(path):
    project = path / "project"
    project.mkdir()
    (project / "target").mkdir()
    source = Path(__file__).resolve().parents[1] / "fixtures/recipes/v2_clean.toml"
    (project / "theseus.toml").write_bytes(source.read_bytes())
    return project


def ordinary(binary, root, era):
    project = project_at(root)
    server = Server(binary, project, root / "output", root / era, era=era)
    try:
        ping = server.request("ping")["result"]
        assert ping == {} if era == "legacy" else ping["resultType"] == "complete"
        if era == "modern":
            assert "result" in server.request("server/discover")
        listing = server.request("tools/list")["result"]["tools"]
        assert [x["name"] for x in listing] == NAMES
        assert all(x["inputSchema"]["additionalProperties"] is False for x in listing)
        inspect = server.tool(NAMES[0], {"recipe_path": "theseus.toml", "selector": {"kind": "overview"}})
        assert inspect["ok"] and inspect["data"]["max_run_seconds"] == 3600, inspect
        instance = inspect["server_instance"]
        assert len(instance) == 32
        runs = server.tool(NAMES[2], {"server_instance": instance})
        assert runs["ok"], runs
        handle = instance + ":1"
        for name, arguments in [(NAMES[3], {"handle": handle}), (NAMES[4], {"handle": handle}), (NAMES[5], {"handle": handle, "selector": {"kind": "catalog", "collection": "run"}})]:
            value = server.tool(name, {"server_instance": instance, **arguments})
            assert not value["ok"], value
        stale = "0" * 32 if instance != "0" * 32 else "1" * 32
        start = server.tool(NAMES[1], {"server_instance": stale, "request_key": "no-engine", "recipe_path": "theseus.toml", "recipe_digest": inspect["data"]["recipe_digest"], "profiles": ["user"], "scenarios": ["clean"]})
        assert not start["ok"] and start["error"]["code"] == "stale_instance", start
        invalid = server.tool(NAMES[0], {"recipe_path": "theseus.toml", "selector": {"kind": "overview"}, "extra": True})
        assert not invalid["ok"] and invalid["error"]["code"] == "invalid_arguments", invalid
    finally:
        server.close()
    assert not list((root / "output").iterdir()), "read-only transcript dispatched engine"
    return {"case": era, "exit": 0, "launcher": "python_subprocess_pipes", "watchdog": "completed"}


def raw_case(binary, root, case):
    project = project_at(root)
    stderr_read = stderr_write = None
    extra = {}
    if case == "stderr-alias":
        extra["stderr"] = subprocess.STDOUT
    if case == "stderr-blocked":
        stderr_read, stderr_write = os.pipe()
        os.set_blocking(stderr_write, False)
        while True:
            try:
                os.write(stderr_write, b"X" * 4096)
            except BlockingIOError:
                break
        os.set_blocking(stderr_write, True)
        extra["stderr"] = stderr_write
    command = [binary, "serve", "--project-root", project if case != "startup-error" else project / "missing", "--output-root", root / "output"]
    if case == "malformed-startup":
        command += ["--max-run-seconds", "0"]
    child = spawn(command, **extra)
    timed_out = threading.Event()
    def timeout():
        timed_out.set()
        reap(child)
    watchdog = threading.Timer(TIMEOUT, timeout)
    watchdog.start()
    started = time.monotonic()
    output = b""
    expected = 0
    try:
        if case in ["eof", "incomplete-eof"]:
            if case == "incomplete-eof":
                child.stdin.write(b'{"jsonrpc":')
                child.stdin.flush()
            child.stdin.close()
        elif case == "broken-stdout":
            child.stdout.close()
            child.stdin.write(wire(message("tools/list")))
            child.stdin.flush()
            expected = 2
        elif case == "four-slots":
            values = [message("tools/list", ident=i) for i in range(1, 5)]
            values.append({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 2}})
            child.stdin.write(b"".join(wire(x) for x in values))
            child.stdin.flush()
            responses = [child.stdout.readline() for _ in range(3)]
            assert [json.loads(x)["id"] for x in responses] == [1, 3, 4]
            output = b"".join(responses)
            child.stdin.close()
        elif case in ["full-stdout", "slow-stdout"]:
            child.stdin.write(b"".join(wire(message("tools/list", ident=i)) for i in (range(1, 5) if case == "full-stdout" else [1])))
            child.stdin.flush()
            if case == "full-stdout":
                expected = 2
            else:
                while not output.endswith(b"\n"):
                    byte = child.stdout.read(1)
                    assert byte, "closed during slow response"
                    output += byte
                    if len(output) % 128 == 0:
                        time.sleep(.003)
                assert len(json.loads(output)["result"]["tools"]) == 6
                child.stdin.close()
        elif case in ["stderr-alias", "stderr-blocked", "os-stop"]:
            child.stdin.write(wire(message("ping")))
            child.stdin.flush()
            output = child.stdout.readline()
            assert json.loads(output)["id"] == 1
            if case == "os-stop":
                child.send_signal(signal.CTRL_BREAK_EVENT if os.name == "nt" else signal.SIGTERM)
            else:
                child.stdin.close()
        elif case in ["startup-error", "malformed-startup"]:
            # Early rejection may close stdin before any attempted write.
            expected = 2
        code = child.wait(timeout=TIMEOUT)
        reap(child)
        if not child.stdout.closed:
            output += child.stdout.read()
        error = child.stderr.read() if child.stderr else b""
        assert not timed_out.is_set(), "outer watchdog always fails"
        assert code == expected, (case, code, error)
        if case == "malformed-startup":
            assert error and not output, "CLI rejection must precede protocol dispatch"
        else:
            assert not error, error
        if case == "startup-error":
            assert not output, "root failure must not dispatch"
        elapsed = time.monotonic() - started
        if case == "full-stdout":
            assert 4.8 <= elapsed < TIMEOUT, elapsed
            assert output and not output.endswith(b"\n"), "must actually fill native pipe"
        elif case != "broken-stdout":
            for line in output.splitlines(keepends=True):
                assert wire(json.loads(line)) == line
        return {"case": case, "exit": code, "elapsed": elapsed, "watchdog": "completed", "stdout_bytes": len(output)}
    finally:
        watchdog.cancel()
        reap(child)
        watchdog.join(timeout=5)
        assert not watchdog.is_alive(), "watchdog failed to settle"
        (root / "stdout.bin").write_bytes(output)
        for fd in [stderr_read, stderr_write]:
            if fd is not None:
                os.close(fd)


def fixture_case(binary, root, case):
    project = project_at(root)
    receipt = root / "receipt.json"
    child = spawn([binary, case, project, root / "output", receipt])
    timed_out = threading.Event()
    def timeout():
        timed_out.set()
        reap(child)
    watchdog = threading.Timer(TIMEOUT, timeout)
    watchdog.start()
    started = time.monotonic()
    transcript = bytearray()
    try:
        if case == "partial-cancel":
            child.stdin.write(wire(message("tools/list")))
            child.stdin.flush()
            transcript.extend(child.stdout.read(1))
            assert transcript == b"{"
            child.stdin.write(wire({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 1}}))
            child.stdin.flush()
            assert child.wait(timeout=TIMEOUT) == 2
            reap(child)
            data = json.loads(receipt.read_text())
            assert not data["forced"] and data["workers_joined"] and data["transport_settled"]
            assert not child.stdout.read() and not child.stderr.read()
            assert not timed_out.is_set()
            return {"case": case, "exit": 2, "watchdog": "completed", "actual_native_bytes": 1, **data}
        # Startup ping proves fixture setup completed before triggering shutdown.
        child.stdin.write(wire(message("ping")))
        child.stdin.flush()
        line = child.stdout.readline()
        transcript.extend(line)
        assert json.loads(line)["id"] == 1, "fixture did not enter actual runtime"
        if case in ["query-stuck", "lost-start-reply"]:
            start = {"server_instance": "0123456789abcdef0123456789abcdef", "request_key": "same-intent", "recipe_path": "theseus.toml", "recipe_digest": "0" * 64, "profiles": ["user"], "scenarios": ["clean"]}
            child.stdin.write(wire(message("tools/call", {"name": "run_start", "arguments": start}, 2)))
            child.stdin.flush()
            accepted = child.stdout.readline()
            transcript.extend(accepted)
            original = json.loads(accepted)["result"]["structuredContent"]["data"]
            assert original["state"] == "validating"
            while not receipt.with_suffix(".entered").exists():
                assert child.poll() is None and not timed_out.is_set(), "query did not enter"
                time.sleep(.01)
            if case == "lost-start-reply":
                # Client deliberately discards its first result and retries exact intent.
                child.stdin.write(wire(message("tools/call", {"name": "run_start", "arguments": start}, 3)))
                child.stdin.flush()
                replay = child.stdout.readline()
                transcript.extend(replay)
                value = json.loads(replay)["result"]["structuredContent"]["data"]
                assert value["handle"] == original["handle"] and value["replayed"]
                child.stdin.write(wire(message("tools/call", {"name": "run_list", "arguments": {"server_instance": start["server_instance"]}}, 4)))
                child.stdin.flush()
                listing = child.stdout.readline()
                transcript.extend(listing)
                assert len(json.loads(listing)["result"]["structuredContent"]["data"]["runs"]) == 1
        shutdown_started = time.monotonic()
        child.stdin.close()
        code = child.wait(timeout=TIMEOUT)
        reap(child)
        transcript.extend(child.stdout.read())
        assert not child.stderr.read(), "fixture leaked stderr"
        assert not timed_out.is_set(), "outer watchdog always fails"
        data = json.loads(receipt.read_text())
        forced = case in ["query-stuck", "engine-stuck", "finalizer-stuck"]
        assert code == (2 if forced else 0), (case, code, data)
        assert data["forced"] == forced
        assert data["workers_joined"] == (not forced), data
        assert data["transport_settled"], data
        if forced:
            assert 4.8 <= time.monotonic() - shutdown_started < TIMEOUT
            assert not receipt.with_suffix(".dropped").exists(), "forced owner unwound before process exit"
        else:
            assert receipt.with_suffix(".dropped").exists(), "orderly owner did not return"
        return {"case": case, "exit": code, "watchdog": "completed", "elapsed": time.monotonic() - started, "cleanup": "incomplete" if forced else "joined", **data}
    finally:
        watchdog.cancel()
        reap(child)
        watchdog.join(timeout=5)
        assert not watchdog.is_alive(), "watchdog failed to settle"
        (root / "stdout.jsonl").write_bytes(transcript)


def supervisor_regression(root):
    """The leader exits while its descendant still owns both ordinary pipes."""
    root.mkdir(parents=True, exist_ok=True)
    pidfile = root / "descendant.pid"
    script = "import subprocess,sys,pathlib; p=subprocess.Popen([sys.executable,'-c',\"import sys,time; print('descendant-out',flush=True); print('descendant-err',file=sys.stderr,flush=True); time.sleep(60)\"]); pathlib.Path(sys.argv[1]).write_text(str(p.pid))"
    child = spawn([sys.executable, "-c", script, pidfile])
    started = time.monotonic()
    try:
        assert child.wait(timeout=5) == 0
        descendant = int(pidfile.read_text())
        receipt = reap(child)
        stdout, stderr = child.communicate(timeout=2)
        assert receipt["leader_exited_before_cleanup"] and receipt["tree_empty"]
        if sys.platform == "linux":
            assert any(item["pid"] == descendant and os.WIFSIGNALED(item["status"]) and os.WTERMSIG(item["status"]) == signal.SIGKILL for item in receipt["descendants_reaped"]), receipt
        if os.name == "nt":
            assert receipt["active_before_kill"] >= 1 and receipt["active_after_kill"] == 0
        assert time.monotonic() - started < 10
        receipt.update({"case": "exited-leader-inherited-pipes", "descendant": descendant, "stdout_eof": True, "stderr_eof": True, "elapsed": time.monotonic() - started})
        (root / "receipt.json").write_text(json.dumps(receipt, indent=2))
        (root / "stdout.bin").write_bytes(stdout)
        (root / "stderr.bin").write_bytes(stderr)
        return receipt
    finally:
        reap(child)


def self_test():
    import errno
    import unittest
    from types import SimpleNamespace
    from unittest.mock import Mock, call, patch

    class PermissionCleanup(unittest.TestCase):
        def child(self):
            return SimpleNamespace(pid=12345, _tree_lock=threading.Lock(), _tree_receipt=None,
                                   poll=Mock(return_value=0), wait=Mock(return_value=0))

        def fake_os(self, kill):
            return SimpleNamespace(name="posix", killpg=kill, WNOHANG=1,
                                   waitpid=Mock(side_effect=ChildProcessError))

        def test_signal_and_probe_permission_errors_wait_for_actual_group_disappearance(self):
            child = self.child()
            kill = Mock(side_effect=[PermissionError(errno.EPERM, "signal denied"),
                                     PermissionError(errno.EPERM, "unknown group"),
                                     PermissionError(errno.EPERM, "unknown group"),
                                     ProcessLookupError(errno.ESRCH, "group gone")])
            clock = SimpleNamespace(monotonic=Mock(side_effect=[0, .1, .2]), sleep=Mock())
            with patch(__name__ + ".os", self.fake_os(kill)), patch(__name__ + ".time", clock):
                receipt = reap(child)
                self.assertIs(reap(child), receipt)
            self.assertEqual(kill.call_args_list, [call(child.pid, signal.SIGKILL),
                                                   call(child.pid, 0), call(child.pid, 0), call(child.pid, 0)])
            child.wait.assert_called_once_with(timeout=5)
            self.assertFalse(receipt["kill_requested"])
            self.assertEqual(receipt["kill_permission_error"], errno.EPERM)
            self.assertEqual(receipt["probe_permission_errors"], 2)
            self.assertTrue(receipt["tree_empty"])

        def test_persistent_permission_error_never_establishes_tree_empty(self):
            for denied_signal in (False, True):
                with self.subTest(denied_signal=denied_signal):
                    child = self.child()
                    def kill(group, sig):
                        if sig == signal.SIGKILL and not denied_signal:
                            return
                        raise PermissionError(errno.EPERM, "unknown group")
                    clock = SimpleNamespace(monotonic=Mock(side_effect=[0, .1, 5.1]), sleep=Mock())
                    with patch(__name__ + ".os", self.fake_os(Mock(side_effect=kill))), patch(__name__ + ".time", clock):
                        with self.assertRaisesRegex(AssertionError, "process group did not disappear"):
                            reap(child)
                    self.assertIsNone(child._tree_receipt)
                    child.wait.assert_called_once_with(timeout=5)

    # These controls exercise POSIX cleanup even on a Windows CI host.
    with patch(__name__ + ".signal", SimpleNamespace(SIGKILL=9)):
        result = unittest.TextTestRunner().run(unittest.defaultTestLoader.loadTestsFromTestCase(PermissionCleanup))
    return 0 if result.wasSuccessful() else 1


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary")
    parser.add_argument("--supervisor-regression", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--fixture")
    parser.add_argument("--evidence")
    args = parser.parse_args()
    if args.self_test:
        raise SystemExit(self_test())
    if not args.evidence:
        parser.error("--evidence is required")
    root = Path(args.evidence)
    root.mkdir(parents=True, exist_ok=True)
    if args.supervisor_regression:
        print(json.dumps(supervisor_regression(root)))
        return
    assert args.binary, "--binary required for native server cases"
    (root / "identity.json").write_text(json.dumps(identity(args.binary), indent=2))
    results = []
    try:
        results.append(supervisor_regression(root / "supervisor-regression"))
        for era in ["legacy", "modern"]:
            directory = root / era
            directory.mkdir()
            results.append(ordinary(args.binary, directory, era))
        for case in ["startup-error", "malformed-startup", "eof", "incomplete-eof", "broken-stdout", "four-slots", "full-stdout", "slow-stdout", "stderr-alias", "stderr-blocked", "os-stop"]:
            directory = root / case
            directory.mkdir()
            results.append(raw_case(args.binary, directory, case))
        if args.fixture:
            (root / "fixture-identity.json").write_text(json.dumps(identity(args.fixture), indent=2))
            for case in ["query-stuck", "engine-stuck", "finalizer-stuck", "successful-joins", "lost-start-reply", "partial-cancel"]:
                directory = root / case
                directory.mkdir()
                results.append(fixture_case(args.fixture, directory, case))
    finally:
        (root / "summary.json").write_text(json.dumps(results, indent=2))
    print(json.dumps({"passed": len(results), "evidence": str(root)}))


if __name__ == "__main__":
    main()
