#!/usr/bin/env python3
"""Bounded, test-only Windows pipe identity observations; never an admission rule."""
import argparse
import ctypes
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

SCENARIOS = ("popen", "separate-anonymous", "same-anonymous", "duplicate-end",
             "separate-named", "same-named")
QUERIES = ("compare", "internal", "file-id", "file-name", "object-name",
           "fs-device", "fs-attribute", "pipe-local")
MAX_CELL_BYTES = 100_000
MAX_TOTAL_BYTES = MAX_CELL_BYTES * len(SCENARIOS) * len(QUERIES)


def bounded_name(base, capacity, address, length):
    if length < 0 or length % 2 or address < base:
        return None
    offset = address - base
    if offset > capacity or length > capacity - offset:
        return None
    return offset, offset + length


def read_receipt(path):
    with path.open("rb") as stream:
        data = stream.read(MAX_CELL_BYTES + 1)
    if len(data) > MAX_CELL_BYTES:
        raise ValueError("cell receipt exceeds fixed bound")
    return [json.loads(line) for line in data.splitlines()]


def terminate_and_join(child, emit=None):
    try:
        child.kill()
    except BaseException:
        pass  # A failed termination request is not evidence that the child joined.
    try:
        return child.wait(timeout=1)
    except BaseException:
        # Retain caller's child and peer owners; even receipt failure must not unwind.
        try:
            record = {"outcome": "fatal_unjoined", "complete": False,
                      "joined": False, "production_acceptance": False}
            if emit is None:
                sys.stderr.write(json.dumps(record) + "\n")
                sys.stderr.flush()
            else:
                emit(record)
        finally:
            os._exit(2)


def wait_child(child, timeout, emit=None):
    try:
        return child.wait(timeout=timeout), False
    except subprocess.TimeoutExpired:
        return terminate_and_join(child, emit), True
    except BaseException:
        terminate_and_join(child, emit)
        raise


def windows_api():
    from ctypes import wintypes as w
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    native = ctypes.WinDLL("ntdll")
    signatures = {
        "GetStdHandle": ([w.DWORD], w.HANDLE),
        "CloseHandle": ([w.HANDLE], w.BOOL),
        "GetCurrentProcess": ([], w.HANDLE),
        "DuplicateHandle": ([w.HANDLE, w.HANDLE, w.HANDLE, ctypes.POINTER(w.HANDLE),
                             w.DWORD, w.BOOL, w.DWORD], w.BOOL),
        "CreatePipe": ([ctypes.POINTER(w.HANDLE), ctypes.POINTER(w.HANDLE),
                        ctypes.c_void_p, w.DWORD], w.BOOL),
        "CreateNamedPipeW": ([w.LPCWSTR, w.DWORD, w.DWORD, w.DWORD, w.DWORD,
                              w.DWORD, w.DWORD, ctypes.c_void_p], w.HANDLE),
        "CreateFileW": ([w.LPCWSTR, w.DWORD, w.DWORD, ctypes.c_void_p,
                         w.DWORD, w.DWORD, w.HANDLE], w.HANDLE),
    }
    for name, (arguments, result) in signatures.items():
        function = getattr(kernel, name)
        function.argtypes, function.restype = arguments, result
    for name in ("NtQueryInformationFile", "NtQueryVolumeInformationFile"):
        function = getattr(native, name)
        function.argtypes = [w.HANDLE, ctypes.c_void_p, ctypes.c_void_p,
                             w.ULONG, w.ULONG]
        function.restype = ctypes.c_int32
    native.NtQueryObject.argtypes = [w.HANDLE, w.ULONG, ctypes.c_void_p,
                                    w.ULONG, ctypes.POINTER(w.ULONG)]
    native.NtQueryObject.restype = ctypes.c_int32
    return kernel, native


def child_probe(query, receipt):
    kernel, native = windows_api()

    class IOSB(ctypes.Structure):
        _fields_ = [("status", ctypes.c_size_t), ("information", ctypes.c_size_t)]

    class UnicodeString(ctypes.Structure):
        _fields_ = [("length", ctypes.c_uint16), ("maximum", ctypes.c_uint16),
                    ("buffer", ctypes.c_void_p)]

    handles = [kernel.GetStdHandle(-10 & 0xffffffff),
               kernel.GetStdHandle(-11 & 0xffffffff)]
    retained = [handles]
    written = 0
    with receipt.open("x", encoding="utf-8") as stream:
        def emit(record):
            nonlocal written
            line = json.dumps(record, sort_keys=True) + "\n"
            written += len(line.encode())
            if written > MAX_CELL_BYTES:
                os._exit(3)
            stream.write(line)
            stream.flush()

        if query == "compare":
            # This API is exported by KernelBase, unlike the ordinary handle APIs.
            # Resolve it only for this candidate, never in the parent launcher.
            from ctypes import wintypes as w
            compare = ctypes.WinDLL("kernelbase", use_last_error=True).CompareObjectHandles
            compare.argtypes = [w.HANDLE, w.HANDLE]
            compare.restype = w.BOOL
            for repeat in range(2):
                ctypes.set_last_error(0)
                result = compare(*handles)
                emit({"query": query, "repeat": repeat, "equal_object": bool(result),
                      "last_error": ctypes.get_last_error()})
            return 0
        classes = {"internal": (6, 8), "file-id": (59, 24),
                   "file-name": (9, 4096), "pipe-local": (24, 40),
                   "fs-device": (4, 8), "fs-attribute": (5, 4096)}
        for endpoint, handle in zip(("stdin", "stdout"), handles):
            previous_identity = None
            for repeat in range(2):
                capacity = 4096 if query == "object-name" else classes[query][1]
                buffer = ctypes.create_string_buffer(capacity)
                iosb = IOSB(0x55555555, 0)
                returned = ctypes.c_uint32(0)
                # Never release query output storage before known completion.
                retained.extend((buffer, iosb, returned))
                if query == "object-name":
                    status = native.NtQueryObject(handle, 1, buffer, capacity,
                                                  ctypes.byref(returned))
                else:
                    function = (native.NtQueryVolumeInformationFile if query.startswith("fs-")
                                else native.NtQueryInformationFile)
                    status = function(handle, ctypes.byref(iosb), buffer,
                                      capacity, classes[query][0])
                if status == 0x103:
                    # No output reads or Python unwinding while the kernel owns storage.
                    try:
                        emit({"query": query, "endpoint": endpoint, "repeat": repeat,
                              "status": status, "capacity": capacity, "pending": True,
                              "complete": False})
                    finally:
                        os._exit(2)
                information = returned.value if query == "object-name" else iosb.information
                record = {"query": query, "endpoint": endpoint, "repeat": repeat,
                          "status": status & 0xffffffff,
                          "iosb_status": iosb.status & 0xffffffff,
                          "information": information, "capacity": capacity,
                          "pending": status == 0x103,
                          "buffer_hex": buffer.raw.hex()}
                record["unsupported_status"] = (status & 0xffffffff) in (
                    0xc0000002, 0xc0000003, 0xc0000010, 0xc00000bb)
                record["reply_exceeds_buffer"] = information > capacity
                if query in ("internal", "file-id", "fs-device", "pipe-local"):
                    record["short_reply"] = information < capacity
                complete = status == 0 and (query == "object-name" or iosb.status == 0)
                record["complete"] = complete
                if complete and query == "object-name" and information >= ctypes.sizeof(UnicodeString):
                    header = UnicodeString.from_buffer(buffer)
                    span = bounded_name(ctypes.addressof(buffer), min(information, capacity),
                                        header.buffer or 0, header.length)
                    record.update(name_length=header.length, name_maximum=header.maximum,
                                  name_pointer_in_buffer=span is not None)
                    if span is not None:
                        record["name_utf16_hex"] = buffer.raw[span[0]:span[1]].hex()
                elif complete and query == "file-name" and 4 <= information <= capacity:
                    length = int.from_bytes(buffer.raw[:4], "little")
                    record["name_length"] = length
                    record["name_valid"] = length % 2 == 0 and length <= information - 4
                    if record["name_valid"]:
                        record["name_utf16_hex"] = buffer.raw[4:4 + length].hex()
                elif complete and query in ("internal", "file-id") and information == capacity:
                    record["identity_hex"] = buffer.raw.hex()
                    record["all_zero"] = not any(buffer.raw)
                    record["file_id_zero"] = not any(buffer.raw[8:] if query == "file-id" else buffer.raw)
                elif complete and query == "pipe-local" and information == 40:
                    fields = ("type", "configuration", "maximum_instances", "current_instances",
                              "inbound_quota", "read_available", "outbound_quota", "write_available",
                              "state", "end")
                    record["profile"] = dict(zip(fields, (int.from_bytes(buffer.raw[i:i+4], "little")
                                                         for i in range(0, 40, 4))))
                identity = record.get("identity_hex", record.get("name_utf16_hex"))
                if repeat == 1 and identity is not None and previous_identity is not None:
                    record["stable_requery"] = identity == previous_identity
                previous_identity = identity
                emit(record)
    return 0


def launch_cell(scenario, query, receipt):
    kernel, _ = windows_api()
    from ctypes import wintypes as w
    owned = []

    def checked_handle(value):
        if value in (None, 0, ctypes.c_void_p(-1).value):
            raise ctypes.WinError(ctypes.get_last_error())
        owned.append(value)
        return value

    def anonymous():
        reader, writer = w.HANDLE(), w.HANDLE()
        if not kernel.CreatePipe(ctypes.byref(reader), ctypes.byref(writer), None, 0):
            raise ctypes.WinError(ctypes.get_last_error())
        return checked_handle(reader.value), checked_handle(writer.value)

    def named(index):
        name = rf"\\.\pipe\theseus-identity-{os.getpid()}-{time.monotonic_ns()}-{index}"
        server = checked_handle(kernel.CreateNamedPipeW(name, 3, 8, 1, 4096, 4096, 0, None))
        client = checked_handle(kernel.CreateFileW(name, 0xc0000000, 0, None, 3, 0, None))
        return server, client

    def inherit(handle):
        duplicate = w.HANDLE()
        process = kernel.GetCurrentProcess()
        if not kernel.DuplicateHandle(process, handle, process, ctypes.byref(duplicate), 0, True, 2):
            raise ctypes.WinError(ctypes.get_last_error())
        return checked_handle(duplicate.value)

    argv = [sys.executable, str(Path(__file__).resolve()), "--child", query, str(receipt)]
    try:
        if scenario == "popen":
            child = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.DEVNULL)
        else:
            pair = named(0) if "named" in scenario and "anonymous" not in scenario else anonymous()
            if scenario.startswith("separate-"):
                second = named(1) if scenario == "separate-named" else anonymous()
                selected = pair[0], second[1]
            elif scenario == "duplicate-end":
                selected = pair[0], pair[0]
            else:
                selected = pair
            stdin, stdout = map(inherit, selected)
            # Valid stderr with no output: use an owned NUL handle, not either pipe.
            null = checked_handle(kernel.CreateFileW("NUL", 0x40000000, 3, None, 3, 0, None))
            stderr = inherit(null)
            startup = subprocess.STARTUPINFO()
            startup.dwFlags = subprocess.STARTF_USESTDHANDLES
            startup.hStdInput, startup.hStdOutput, startup.hStdError = stdin, stdout, stderr
            startup.lpAttributeList = {"handle_list": [stdin, stdout, stderr]}
            child = subprocess.Popen(argv, startupinfo=startup, close_fds=True)
        return child, owned, kernel
    except BaseException:
        for handle in reversed(owned):
            kernel.CloseHandle(handle)
        raise


def run_matrix(output):
    output.mkdir(parents=True, exist_ok=True)
    destination = output / "windows-pipe-identity.jsonl"
    started = time.monotonic()
    # Collection/admission budget plus <=1s join grace; the CI step bounds native syscalls.
    deadline = started + 55
    counts = {"completed": 0, "pending": 0, "timeout": 0, "not_run": 0, "error": 0}
    with tempfile.TemporaryDirectory(prefix="theseus-identity-") as temporary:
        with destination.open("x", encoding="utf-8") as stream:
            for index, (scenario, query) in enumerate((s, q) for s in SCENARIOS for q in QUERIES):
                cell = {"scenario": scenario, "query": query, "interpretation": "raw facts only"}

                def emit_fatal(record):
                    fatal = dict(cell)
                    fatal.update(record)
                    stream.write(json.dumps(fatal, sort_keys=True) + "\n")
                    stream.flush()

                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    cell["outcome"] = "not_run_budget_exhausted"
                    counts["not_run"] += 1
                else:
                    receipt = Path(temporary) / f"{index}.jsonl"
                    child = None
                    owned = []
                    try:
                        child, owned, kernel = launch_cell(scenario, query, receipt)
                        code, expired = wait_child(child, min(3, max(0.001, deadline - time.monotonic())),
                                                   emit_fatal)
                        cell["exit_code"] = code
                        cell["observations"] = read_receipt(receipt) if receipt.exists() else []
                        observations = cell["observations"]
                        if expired:
                            outcome = "timeout"
                        elif code == 0 and len(observations) == (2 if query == "compare" else 4):
                            outcome = "completed"
                        elif code == 2 and observations and observations[-1].get("pending"):
                            outcome = "pending"
                        else:
                            outcome = "error"
                        cell["outcome"] = outcome
                        counts[outcome] += 1
                    except Exception as error:
                        cell.update(outcome="error", error=f"{type(error).__name__}: {error}")
                        counts["error"] += 1
                    finally:
                        if child is not None:
                            if child.returncode is None:
                                terminate_and_join(child, emit_fatal)
                            for pipe in (child.stdin, child.stdout, child.stderr):
                                if pipe is not None:
                                    pipe.close()
                        for handle in reversed(owned):
                            kernel.CloseHandle(handle)
                stream.write(json.dumps(cell, sort_keys=True) + "\n")
                stream.flush()
    summary = {"scenarios": list(SCENARIOS), "queries": list(QUERIES), "cells": 48,
               "counts": counts, "pid": os.getpid(), "python": sys.version,
               "platform": sys.platform, "windows_version": list(sys.getwindowsversion()),
               "source_commit": os.environ.get("GITHUB_SHA"),
               "elapsed_seconds": time.monotonic() - started,
               "production_acceptance": False}
    (output / "windows-pipe-identity-summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    return 0 if counts["completed"] == 48 else 1


def self_test():
    import unittest

    class Bounds(unittest.TestCase):
        def test_pointer_bounds(self):
            self.assertEqual(bounded_name(1000, 32, 1004, 4), (4, 8))
            self.assertEqual(bounded_name(1000, 32, 1032, 0), (32, 32))
            for pointer, length in ((999, 2), (1031, 2), (1000, 3), (0, 0)):
                self.assertIsNone(bounded_name(1000, 32, pointer, length))

        def test_receipt_limit_and_json(self):
            with tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "receipt"
                path.write_bytes(b'{"fact":1}\n')
                self.assertEqual(read_receipt(path), [{"fact": 1}])
                path.write_bytes(b'x' * (MAX_CELL_BYTES + 1))
                with self.assertRaises(ValueError):
                    read_receipt(path)
                path.write_bytes(b'not json\n')
                with self.assertRaises(ValueError):
                    read_receipt(path)

        def test_fixed_matrix(self):
            self.assertEqual(len(SCENARIOS) * len(QUERIES), 48)
            self.assertLessEqual(MAX_TOTAL_BYTES, 5_000_000)

        def test_watchdog_actually_joins(self):
            child = subprocess.Popen([sys.executable, str(Path(__file__).resolve()),
                                      "--watchdog-child"], stdin=subprocess.DEVNULL,
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            code, expired = wait_child(child, 0.1)
            self.assertTrue(expired)
            self.assertIsNotNone(code)
            self.assertEqual(child.poll(), code)

        def test_fatal_paths_keep_owners_live(self):
            for mode in ("pending", "unjoined", "kill-error"):
                with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                    marker = Path(directory) / "owner-dropped"
                    child = subprocess.Popen([sys.executable, str(Path(__file__).resolve()),
                                              "--fatal-self-test", mode, str(marker)],
                                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                    code = child.wait(timeout=3)
                    self.assertEqual(code, 2)
                    self.assertFalse(marker.exists())

    return 0 if unittest.TextTestRunner().run(
        unittest.defaultTestLoader.loadTestsFromTestCase(Bounds)).wasSuccessful() else 1


def fatal_self_test(mode, marker):
    from types import SimpleNamespace

    class Owner:
        def __del__(self):
            Path(marker).write_text("owner dropped")

    owner = Owner()
    if mode == "pending":
        class BrokenReceipt:
            def open(self, *args, **kwargs):
                return self

            def __enter__(self):
                return self

            def __exit__(self, *args):
                Path(marker).write_text("receipt unwound")

            def write(self, value):
                raise OSError("injected receipt failure")

        global windows_api
        windows_api = lambda: (SimpleNamespace(GetStdHandle=lambda handle: handle),
                              SimpleNamespace(NtQueryInformationFile=lambda *args: 0x103))
        child_probe("internal", BrokenReceipt())
    else:
        class StubbornChild:
            def wait(self, timeout=None):
                if timeout is None:
                    raise AssertionError("unbounded wait")
                raise subprocess.TimeoutExpired("fixed-self-test", timeout)

            def kill(self):
                if mode == "kill-error":
                    raise OSError("injected kill failure")

        wait_child(StubbornChild(), 0.001)
    return 9 if owner is not None else 8


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--watchdog-child", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--fatal-self-test", nargs=2, help=argparse.SUPPRESS)
    parser.add_argument("--child", nargs=2, metavar=("QUERY", "RECEIPT"), help=argparse.SUPPRESS)
    parser.add_argument("--output", type=Path)
    arguments = parser.parse_args()
    if arguments.fatal_self_test:
        return fatal_self_test(*arguments.fatal_self_test)
    if arguments.watchdog_child:
        time.sleep(5)
        return 0
    if arguments.self_test:
        return self_test()
    if sys.platform != "win32":
        parser.error("native collection requires Windows; --self-test is portable")
    if arguments.child:
        query, receipt = arguments.child
        if query not in QUERIES:
            parser.error("unknown fixed query")
        return child_probe(query, Path(receipt))
    if arguments.output is None:
        parser.error("--output is required for native collection")
    return run_matrix(arguments.output)


if __name__ == "__main__":
    raise SystemExit(main())
