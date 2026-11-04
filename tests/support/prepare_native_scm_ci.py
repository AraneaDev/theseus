#!/usr/bin/env python3
"""CI-only native Windows SCM fixture; --plan never loads native Windows APIs.

Runs with the existing non-SYSTEM helper token on a disposable hosted runner.
Never elevates, impersonates, weakens SystemOnly policy, enumerates services, or
cleans up by PID/name. Missing native rights are hard failures with raw evidence.
"""
from contextlib import ExitStack
import ctypes
import os
from pathlib import Path
import re
import sys

import native_platform_ci as common
from native_platform_ci import require

NATIVE_CASES = {
    "native::scm_own_process_clean_observes_config_status_readiness_and_delete_pending": 1,
    "owners::scm_delete_success_remains_pending_until_original_handles_close_and_name_is_absent": 1,
    "owners::scm_native_pipe_rejects_actual_helper_sid_before_challenge": 1,
}
MASKS = {"directory": (0x1200a9, 0x1301bf), "executable": (0x1200a9, 0x1301a9), "data": (0x120089, 0x1301bf), "sidecar": (0x120089, 0x130189), "pipe": (0x12019f, 0x12019f)}


def make_plan(manifest, token):
    return common.plan_for(manifest, token, windows=True, native_cases=NATIVE_CASES)


def execution_guard(env, *, execute, disposable, system):
    common.guard(env, execute=execute, disposable=disposable, system=system, windows=True)


def validate_acl(helper, kind, protected, grants):
    require(isinstance(helper, str) and re.fullmatch(r"S-1-(?:[0-9]+-)*[0-9]+", helper) and helper != "S-1-5-18", "actual distinct helper SID required")
    require(kind in MASKS and protected and len(grants) == 2, "protected exact two-principal DACL required")
    system, user = MASKS[kind]
    require(sorted(grants) == sorted([("S-1-5-18", system, 0), (helper, user, 0)]), "inherited, extra, missing or excessive ACL grant forbidden")


def validate_absence(error):
    require(error == 1060, "only ERROR_SERVICE_DOES_NOT_EXIST proves exact SCM name absent; denied/delete-pending is unknown")


def manager_receipt(plan, reservation, evidence):
    helper = evidence["helper_sid"]
    require(helper != "S-1-5-18" and re.fullmatch(r"S-1-(?:[0-9]+-)*[0-9]+", helper), "distinct measured helper SID required")
    require(re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", evidence.get("version", "")), "measured RtlGetVersion build required")
    validate_absence(evidence["absence_error"])
    require(evidence.get("original_scm_handle_retained") is True and evidence.get("manager_access") == 3, "original local SCM connect/create handle required; missing rights fail")
    require(evidence.get("pipe_rejects_remote_clients") is True and evidence.get("pipe_probe_closed") is True, "original local-only named-pipe ACL probe required")
    paths = evidence.get("directory_acls", [])
    require({entry["path"] for entry in paths} == {reservation["root"], reservation["ready_root"]} and len(paths) == 2, "both exact fixture root ACL receipts required")
    for entry in paths:
        validate_acl(helper, "directory", entry["protected"], [tuple(g) for g in entry["grants"]])
    validate_acl(helper, "pipe", evidence["pipe_acl"]["protected"], [tuple(g) for g in evidence["pipe_acl"]["grants"]])
    workspace = reservation["root"] + "\\workspace"
    probes = evidence.get("install_path_probes", [])
    expected = {workspace + "\\reader.exe": "executable", workspace + "\\reader.service.json": "sidecar", workspace + "\\data": "data"}
    require(len(probes) == 3 and {probe["path"] for probe in probes} == set(expected) and evidence.get("workspace_probe_original_deleted") is True, "exact native install-path ACL probes and joined original probe cleanup required")
    for probe in probes:
        require(probe.get("kind") == expected[probe["path"]] and probe.get("original_deleted") is True, "original exact-path probe kind/cleanup required")
        validate_acl(helper, probe["kind"], probe["protected"], [tuple(g) for g in probe["grants"]])
    exact = common.digest({"source_sha": plan["manifest"]["source_sha"], "reservation": reservation, "native": evidence})
    workspace = reservation["root"] + "\\workspace"
    authority = {
        "kind": "scm", "slot": "service", "context": "local_machine", "service_name": reservation["name"],
        "qualification": {"manager": "windows_scm", "version": evidence["version"], "api_profile": "scm_own_process_v1", "features": ["query_config", "query_status", "start_stop", "original_handle"], "receipt_sha256": exact},
        "allowed_access": 0x35, "allow_local_system": True,
        "preflight": {"helper_sid": helper, "configured_account_sid": "S-1-5-18", "executable": workspace + "\\reader.exe", "definition": workspace + "\\reader.service.json", "data": workspace + "\\data", "pipe": reservation["pipe"],
                      "executable_and_definition_read_execute": True, "data_declared_modify_only": True, "pipe_system_and_helper_only": True, "pipe_rejects_remote_clients": True, "no_other_acl_grants": True, "exact_access_receipt": exact},
        "managed_by": "installer", "sut_effects": ["registration", "configuration", "removal"], "allowed_actions": ["inspect", "start", "stop"],
    }
    return {"kind": "theseus_supplied_manager_receipt", "schema": 1, "manager": "windows_scm", "authority": authority, "manager_unique_name": "local-machine/" + reservation["name"], "namespace_receipt": "theseus-native-scm:" + exact,
            "native_fixture": {"root": reservation["root"], "ready_root": reservation["ready_root"], "cooperative_instance": reservation["instance"]}}


class Native:
    """Typed Windows ABI, loaded only behind execute+hosted-runner guard."""
    def __init__(self, stack):
        from ctypes import wintypes as w
        self.w = w
        self.adv = ctypes.WinDLL("advapi32", use_last_error=True)
        self.kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        self.ntdll = ctypes.WinDLL("ntdll", use_last_error=True)
        self.stack = stack
        self._bind(self.adv, "OpenSCManagerW", [w.LPCWSTR, w.LPCWSTR, w.DWORD], w.HANDLE)
        self._bind(self.adv, "OpenServiceW", [w.HANDLE, w.LPCWSTR, w.DWORD], w.HANDLE)
        self._bind(self.adv, "CloseServiceHandle", [w.HANDLE], w.BOOL)
        self._bind(self.kernel, "GetCurrentProcess", [], w.HANDLE)
        self._bind(self.kernel, "CloseHandle", [w.HANDLE], w.BOOL)
        self._bind(self.kernel, "LocalFree", [w.HLOCAL], w.HLOCAL)
        self._bind(self.adv, "OpenProcessToken", [w.HANDLE, w.DWORD, ctypes.POINTER(w.HANDLE)], w.BOOL)
        self._bind(self.adv, "GetTokenInformation", [w.HANDLE, ctypes.c_int, ctypes.c_void_p, w.DWORD, ctypes.POINTER(w.DWORD)], w.BOOL)
        self._bind(self.adv, "ConvertSidToStringSidW", [ctypes.c_void_p, ctypes.POINTER(w.LPWSTR)], w.BOOL)
        self._bind(self.adv, "ConvertStringSecurityDescriptorToSecurityDescriptorW", [w.LPCWSTR, w.DWORD, ctypes.POINTER(ctypes.c_void_p), ctypes.POINTER(w.DWORD)], w.BOOL)
        self._bind(self.adv, "GetSecurityDescriptorDacl", [ctypes.c_void_p, ctypes.POINTER(w.BOOL), ctypes.POINTER(ctypes.c_void_p), ctypes.POINTER(w.BOOL)], w.BOOL)
        self._bind(self.adv, "SetSecurityInfo", [w.HANDLE, ctypes.c_int, w.DWORD, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p], w.DWORD)
        self._bind(self.adv, "GetSecurityInfo", [w.HANDLE, ctypes.c_int, w.DWORD, ctypes.c_void_p, ctypes.c_void_p, ctypes.POINTER(ctypes.c_void_p), ctypes.c_void_p, ctypes.POINTER(ctypes.c_void_p)], w.DWORD)
        self._bind(self.adv, "GetSecurityDescriptorControl", [ctypes.c_void_p, ctypes.POINTER(w.WORD), ctypes.POINTER(w.DWORD)], w.BOOL)
        self._bind(self.adv, "GetAce", [ctypes.c_void_p, w.DWORD, ctypes.POINTER(ctypes.c_void_p)], w.BOOL)
        self._bind(self.adv, "IsValidSid", [ctypes.c_void_p], w.BOOL)
        self._bind(self.adv, "GetLengthSid", [ctypes.c_void_p], w.DWORD)
        self._bind(self.kernel, "CreateFileW", [w.LPCWSTR, w.DWORD, w.DWORD, ctypes.c_void_p, w.DWORD, w.DWORD, w.HANDLE], w.HANDLE)
        self._bind(self.kernel, "CreateNamedPipeW", [w.LPCWSTR, w.DWORD, w.DWORD, w.DWORD, w.DWORD, w.DWORD, w.DWORD, ctypes.c_void_p], w.HANDLE)
        self._bind(self.kernel, "SetFileInformationByHandle", [w.HANDLE, ctypes.c_int, ctypes.c_void_p, w.DWORD], w.BOOL)
        self._bind(self.kernel, "GetFileInformationByHandleEx", [w.HANDLE, ctypes.c_int, ctypes.c_void_p, w.DWORD], w.BOOL)
        self._bind(self.ntdll, "RtlGetVersion", [ctypes.c_void_p], ctypes.c_long)
        self.manager = self.adv.OpenSCManagerW(None, None, 3)
        self.check(self.manager, "OpenSCManager local connect/create (no UAC/elevation)")
        stack.callback(self.close_service, self.manager)
        self.helper_sid = self.current_sid()
        require(self.helper_sid != "S-1-5-18", "SYSTEM helper substitution forbidden; existing distinct helper token required")
        self.version = self.os_version()

    @staticmethod
    def _bind(library, name, args, result):
        function = getattr(library, name)
        function.argtypes, function.restype = args, result

    @staticmethod
    def check(value, operation):
        if not value:
            raise OSError(ctypes.get_last_error(), operation)
        return value

    def close_service(self, handle):
        self.check(self.adv.CloseServiceHandle(handle), "close original SCM handle")

    def close(self, handle):
        self.check(self.kernel.CloseHandle(handle), "close original native handle")

    def sid_text(self, raw):
        require(bool(self.adv.IsValidSid(raw)), "measured native SID invalid")
        text = self.w.LPWSTR()
        self.check(self.adv.ConvertSidToStringSidW(raw, ctypes.byref(text)), "convert measured SID")
        try:
            return ctypes.wstring_at(text)
        finally:
            self.kernel.LocalFree(ctypes.cast(text, ctypes.c_void_p))

    def current_sid(self):
        token = self.w.HANDLE()
        self.check(self.adv.OpenProcessToken(self.kernel.GetCurrentProcess(), 8, ctypes.byref(token)), "open original process token")
        try:
            length = self.w.DWORD()
            self.adv.GetTokenInformation(token, 1, None, 0, ctypes.byref(length))
            require(0 < length.value <= 65536 and ctypes.get_last_error() == 122, "bounded TokenUser measurement required")
            buffer = ctypes.create_string_buffer(length.value)
            self.check(self.adv.GetTokenInformation(token, 1, buffer, length, ctypes.byref(length)), "measure actual helper TokenUser")
            return self.sid_text(ctypes.c_void_p.from_buffer(buffer).value)
        finally:
            self.close(token)

    def os_version(self):
        class Version(ctypes.Structure):
            _fields_ = [("size", self.w.DWORD), ("major", self.w.DWORD), ("minor", self.w.DWORD), ("build", self.w.DWORD), ("platform", self.w.DWORD), ("service_pack", self.w.WCHAR * 128)]
        version = Version()
        version.size = ctypes.sizeof(version)
        require(self.ntdll.RtlGetVersion(ctypes.byref(version)) == 0, "native OS version query failed")
        return f"{version.major}.{version.minor}.{version.build}"

    def absence(self, name):
        ctypes.set_last_error(0)
        original = self.adv.OpenServiceW(self.manager, name, 0x35)
        if original:
            self.close_service(original)
            raise ValueError("exact reserved name already present; never adopt/control/delete it")
        error = ctypes.get_last_error()
        validate_absence(error)
        return error

    def descriptor(self, kind):
        system, helper = MASKS[kind]
        text = f"D:P(A;;0x{system:08x};;;S-1-5-18)(A;;0x{helper:08x};;;{self.helper_sid})"
        raw = ctypes.c_void_p()
        self.check(self.adv.ConvertStringSecurityDescriptorToSecurityDescriptorW(text, 1, ctypes.byref(raw), None), "create exact protected security descriptor")
        return raw

    def acl_receipt(self, handle, kind):
        acl, descriptor = ctypes.c_void_p(), ctypes.c_void_p()
        error = self.adv.GetSecurityInfo(handle, 1, 4, None, None, ctypes.byref(acl), None, ctypes.byref(descriptor))
        require(error == 0 and acl.value and descriptor.value, f"native original object DACL query failed: {error}")
        try:
            control, revision = self.w.WORD(), self.w.DWORD()
            self.check(self.adv.GetSecurityDescriptorControl(descriptor, ctypes.byref(control), ctypes.byref(revision)), "query DACL protection")
            header = ctypes.string_at(acl, 8)
            size, count = int.from_bytes(header[2:4], "little"), int.from_bytes(header[4:6], "little")
            require(count == 2 and 8 <= size <= 65535, "bounded exact two ACE native DACL required")
            grants = []
            for index in range(count):
                ace = ctypes.c_void_p()
                self.check(self.adv.GetAce(acl, index, ctypes.byref(ace)), "read original DACL ACE")
                require(acl.value + 8 <= ace.value <= acl.value + size - 16, "ACE outside original ACL")
                raw = ctypes.string_at(ace, 16)
                ace_size = int.from_bytes(raw[2:4], "little")
                sid_size = 8 + 4 * raw[9]
                require(raw[0] == 0 and ace_size >= 16 and ace.value + ace_size <= acl.value + size and sid_size <= ace_size - 8, "only bounded access-allowed SID ACE admitted")
                sid = ace.value + 8
                require(self.adv.GetLengthSid(sid) == sid_size, "native SID length differs")
                grants.append((self.sid_text(sid), int.from_bytes(raw[4:8], "little"), raw[1]))
            protected = bool(control.value & 0x1000)
            validate_acl(self.helper_sid, kind, protected, grants)
            return {"protected": protected, "grants": grants}
        finally:
            self.kernel.LocalFree(descriptor)

    def file_identity(self, handle):
        class Id(ctypes.Structure):
            _fields_ = [("volume", ctypes.c_ulonglong), ("id", ctypes.c_ubyte * 16)]
        class Tag(ctypes.Structure):
            _fields_ = [("attributes", self.w.DWORD), ("tag", self.w.DWORD)]
        identity, attributes = Id(), Tag()
        self.check(self.kernel.GetFileInformationByHandleEx(handle, 18, ctypes.byref(identity), ctypes.sizeof(identity)), "measure original 128-bit file identity")
        self.check(self.kernel.GetFileInformationByHandleEx(handle, 9, ctypes.byref(attributes), ctypes.sizeof(attributes)), "measure original file reparse attributes")
        require(not attributes.attributes & 0x400 and attributes.tag == 0, "reparse object refused")
        return {"volume": identity.volume, "file_id": bytes(identity.id).hex(), "directory": bool(attributes.attributes & 0x10)}

    def open_path(self, path, writable_acl=False, delete=False):
        require(Path(path).resolve() == Path(path), "canonical no-reparse path required")
        # OPEN_REPARSE_POINT + BACKUP_SEMANTICS; no content-write authority.
        handle = self.kernel.CreateFileW(str(path), 0x20080 | (0x40000 if writable_acl else 0) | (0x10000 if delete else 0), 7, None, 3, 0x02200000, None)
        require(handle != ctypes.c_void_p(-1).value and handle is not None, f"open exact native object failed: {ctypes.get_last_error()}")
        return handle

    @staticmethod
    def file_handle(file):
        import msvcrt
        return msvcrt.get_osfhandle(file.fileno())

    def open_frozen(self, path, directory=False):
        require(Path(path).resolve() == Path(path), "canonical frozen native object required")
        # Keep native no-share-write/no-share-delete protection for these exact
        # retained copies/directories. This does not promise atomic hostile-
        # writer pathname execution or grant cleanup authority over replacements.
        access = 0x20080 if directory else 0x80020080
        handle = self.kernel.CreateFileW(str(path), access, 1, None, 3, 0x02200000, None)
        require(handle not in (None, ctypes.c_void_p(-1).value), f"cannot retain frozen native artifact: {ctypes.get_last_error()}")
        try:
            require(self.file_identity(handle)["directory"] == directory, "frozen native object type differs")
        except BaseException:
            self.close(handle)
            raise
        return handle

    def open_frozen_file(self, path):
        import msvcrt
        handle = self.open_frozen(path)
        try:
            fd = msvcrt.open_osfhandle(handle, os.O_RDONLY | os.O_BINARY)
        except BaseException:
            self.close(handle)
            raise
        # The CRT descriptor owns this original HANDLE from this point onward.
        return os.fdopen(fd, "rb")

    def secure_created(self, path, kind, retain=True):
        # Call only immediately after exclusive creation under our private root.
        original = self.open_path(path, writable_acl=True, delete=not retain)
        try:
            identity = self.file_identity(original)
            require(identity["directory"] == (kind == "directory"), "created native object type differs")
            descriptor = self.descriptor(kind)
            try:
                present, defaulted, acl = self.w.BOOL(), self.w.BOOL(), ctypes.c_void_p()
                self.check(self.adv.GetSecurityDescriptorDacl(descriptor, ctypes.byref(present), ctypes.byref(acl), ctypes.byref(defaulted)), "get created exact DACL")
                require(present.value and acl.value, "exact non-null DACL required")
                error = self.adv.SetSecurityInfo(original, 1, 0x80000004, None, None, acl, None)
                require(error == 0, f"cannot establish exact protected fixture ACL: {error}; no policy weakening")
            finally:
                self.kernel.LocalFree(descriptor)
            receipt = dict(self.acl_receipt(original, kind), path=str(path), identity=identity)
            current = self.open_path(path)
            try:
                require(self.file_identity(current) == identity, "created native object replaced during DACL setup")
            finally:
                self.close(current)
            if retain:
                self.stack.callback(self.close, original)
            return original, receipt
        except BaseException:
            self.close(original)
            raise

    def private_root(self, path, retain=True):
        Path(path).mkdir()  # exclusive reservation; never repair/adopt existing tree
        return self.secure_created(path, "directory", retain=retain)

    def delete_original_probe(self, original, receipt):
        # Close only after requesting deletion on the retained original HANDLE.
        # A replaced path is preserved; there is never path-based removal.
        current = self.open_path(receipt["path"])
        try:
            require(self.file_identity(current) == self.file_identity(original) == receipt["identity"], "original native probe replaced; refusing cleanup")
            disposition = ctypes.c_ubyte(1)
            self.check(self.kernel.SetFileInformationByHandle(original, 4, ctypes.byref(disposition), ctypes.sizeof(disposition)), "delete exact original native probe by HANDLE")
        finally:
            self.close(current)

    def install_path_probes(self, reservation):
        workspace = Path(reservation["root"]) / "workspace"
        directory, directory_receipt = self.private_root(workspace, retain=False)
        probes = []
        try:
            for leaf, kind in [("reader.exe", "executable"), ("reader.service.json", "sidecar"), ("data", "data")]:
                path = workspace / leaf
                with path.open("xb"):
                    pass
                original, receipt = self.secure_created(path, kind, retain=False)
                try:
                    self.delete_original_probe(original, receipt)
                finally:
                    self.close(original)
                require(not path.exists(), "original native ACL probe deletion did not settle")
                probes.append(dict(receipt, kind=kind, original_deleted=True))
            self.delete_original_probe(directory, directory_receipt)
        finally:
            self.close(directory)
        require(not workspace.exists() and not any(Path(reservation["root"]).iterdir()), "exact fixture root must be empty after original ACL probes join")
        return probes

    def pipe_probe(self, name):
        class Attributes(ctypes.Structure):
            _fields_ = [("length", self.w.DWORD), ("descriptor", ctypes.c_void_p), ("inherit", self.w.BOOL)]
        descriptor = self.descriptor("pipe")
        try:
            attributes = Attributes(ctypes.sizeof(Attributes), descriptor, False)
            # First-instance, overlapped, duplex, PIPE_REJECT_REMOTE_CLIENTS.
            handle = self.kernel.CreateNamedPipeW(name, 0x40080003, 8, 1, 4096, 4096, 0, ctypes.byref(attributes))
            require(handle not in (None, ctypes.c_void_p(-1).value), f"exact local pipe probe failed: {ctypes.get_last_error()}")
            try:
                receipt = self.acl_receipt(handle, "pipe")
            finally:
                self.close(handle)
            return receipt
        finally:
            self.kernel.LocalFree(descriptor)


def operator(plan, source_env):
    env = common.qualified_env(plan, source_env)
    root = Path(plan["root"])
    with ExitStack() as stack:
        native = Native(stack)
        root_handle, root_acl = native.private_root(root)
        artifacts_root = root / "pinned-artifacts"
        native.private_root(artifacts_root)
        artifacts, pins = common.pin_artifacts(plan, artifacts_root, secure=native.secure_created)
        env.update({key: artifacts[key] for key in common.BINARY_ENV})
        common.write_new(root / "source-build-artifacts.json", {"manifest": plan["manifest"], "artifacts": pins, "root_acl": root_acl, "helper_sid": native.helper_sid, "windows_version": native.version, "github": {key: source_env[key] for key in ("GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT", "GITHUB_SHA")}})
        artifact_guard = stack.enter_context(common.ArtifactGuard(artifacts, pins, artifacts_root, native=native))
        artifact_guard.verify("source/build evidence frozen", root / "artifacts-initial-guard.json")
        list_status, inventory = common.run_logged([artifacts["native_services_platforms"], "--list", "--format=terse"], env, root / "tests-list.log", artifacts=artifact_guard)
        require(list_status == 0, "native precompiled test inventory failed")
        inventory = inventory.decode("utf-8", "strict").splitlines()
        results, all_absent = [], True
        try:
            for number, case in enumerate(plan["cases"]):
                command = common.test_command(artifacts[case["binary"]], case["name"], inventory)
                receipts, owners = [], []
                for item, reservation in enumerate(case["reservations"]):
                    directory_acls = []
                    for key in ("root", "ready_root"):
                        owner, acl = native.private_root(reservation[key])
                        owners.append((owner, acl))
                        directory_acls.append(acl)
                    absence_error = native.absence(reservation["name"])
                    install_probes = native.install_path_probes(reservation)
                    pipe_acl = native.pipe_probe(reservation["pipe"])
                    evidence = {"helper_sid": native.helper_sid, "version": native.version, "absence_error": absence_error, "original_scm_handle_retained": True, "manager_access": 3, "directory_acls": directory_acls, "install_path_probes": install_probes, "workspace_probe_original_deleted": True, "pipe_acl": pipe_acl, "pipe_rejects_remote_clients": True, "pipe_probe_closed": True}
                    common.write_new(root / f"case-{number}-{item}-native-evidence.json", evidence)
                    receipt = manager_receipt(plan, reservation, evidence)
                    path = root / f"case-{number}-{item}-receipt.json"
                    common.write_new(path, receipt)
                    receipts.append(str(path))
                inventory_path = root / f"case-{number}-receipts.json"
                common.write_new(inventory_path, receipts)
                case_env = dict(env, THESEUS_NATIVE_SCM_RECEIPT_INVENTORY=str(inventory_path))
                status, output = common.run_logged(command, case_env, root / f"case-{number}.log", timeout=600, artifacts=artifact_guard)
                common.validate_test_result(status, output)
                for original, acl in owners:
                    current = native.open_path(acl["path"])
                    try:
                        require(native.file_identity(current) == native.file_identity(original) == acl["identity"], "original fixture root replaced; no cleanup authority")
                    finally:
                        native.close(current)
                results.append({"name": case["name"], "status": status, "receipts": receipts})
        finally:
            observations = []
            for case in plan["cases"]:
                for reservation in case["reservations"]:
                    try:
                        error = native.absence(reservation["name"])
                        observations.append({"name": reservation["name"], "error": error, "absent": True})
                    except (ValueError, OSError) as error:
                        all_absent = False
                        observations.append({"name": reservation["name"], "error": str(error), "absent": False})
            common.write_new(root / "cleanup.json", {"all_exact_reserved_names_absent": all_absent, "observations": observations, "service_controls_by_operator": False, "roots_and_raw_logs_retained": True, "original_root_identity": native.file_identity(root_handle)})
        require(all_absent, "SCM fixture service remains or is unknown; preserved for disposable runner teardown, never adopted")
        common.write_new(root / "result.json", {"source_sha": plan["manifest"]["source_sha"], "cases": results, "native_manager": "windows-scm", "external_gui_ssh_reset": "Pending"})
    return str(root)


def main():
    common.cli(sys.modules[__name__])


if __name__ == "__main__":
    main()
