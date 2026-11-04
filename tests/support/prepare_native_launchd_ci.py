#!/usr/bin/env python3
"""CI-only native launchd User-domain fixture; --plan never contacts launchd.

Requires an already-authorized disposable GitHub-hosted macOS runner. Never uses
GUI/system/default domains, sudo, discovery, PID cleanup or reconstructed owners.
Every required named native test executes; missing fixtures/rights fail closed.
"""
import base64
from contextlib import ExitStack
import hashlib
import os
from pathlib import Path
import re
import stat
import struct
import subprocess
import sys

import native_platform_ci as common
from native_platform_ci import require

NATIVE_CASES = {
    "native::launchd_user_clean_keeps_loaded_configuration_runtime_and_readiness_distinct": 1,
    "owners::launchd_wrong_domain_and_replaced_plist_never_authorize_stop_or_bootout": 1,
    "launchd_running_wrong_native_peer_never_passes_readiness": 1,
}


def make_plan(manifest, token):
    return common.plan_for(manifest, token, windows=False, native_cases=NATIVE_CASES)


def execution_guard(env, *, execute, disposable, system):
    common.guard(env, execute=execute, disposable=disposable, system=system, windows=False)


def file_fingerprint(metadata):
    return list(b"\x01" + struct.pack(">QQQqqqq", metadata.st_size, metadata.st_dev, metadata.st_ino, metadata.st_mtime_ns // 1000000000, metadata.st_mtime_ns % 1000000000, metadata.st_ctime_ns // 1000000000, metadata.st_ctime_ns % 1000000000))


def validate_absence(name, uid, status, stdout, stderr):
    diagnostic = f'Could not find service "{name}" in domain for user: {uid}\n'
    require(1 <= status <= 255 and not stdout and stderr in (diagnostic, "Bad request.\n" + diagnostic), "exact qualified native User-domain label absence required; unknown is not absent")


def manager_receipt(plan, reservation, evidence):
    uid = evidence.get("uid")
    require(type(uid) is int and uid > 0, "actual non-root fixture principal required")
    require(evidence.get("domain_status") == 0 and evidence.get("domain_stdout", "").startswith(f"user/{uid} = {{") and evidence.get("domain_stderr") == "", "explicit native User domain qualification required")
    for field in ("os_version", "os_build"):
        value = evidence.get(field, "")
        require(isinstance(value, str) and 0 < len(value) <= 128 and not any(ord(c) < 32 for c in value) and "model" not in value.lower(), "measured native OS version/build required")
    fingerprint = evidence.get("tool_fingerprint", [])
    require(len(fingerprint) == 57 and fingerprint[0] == 1 and all(type(v) is int and 0 <= v <= 255 for v in fingerprint) and re.fullmatch(r"[a-f0-9]{64}", evidence.get("tool_sha256", "")), "retained actual launchctl tool pin required")
    validate_absence(reservation["name"], uid, evidence["absence_status"], evidence["absence_stdout"], evidence["absence_stderr"])
    receipt_sha = common.digest({"source_sha": plan["manifest"]["source_sha"], "reservation": reservation, "native": evidence})
    authority = {
        "kind": "launchd", "slot": "service", "domain": {"kind": "user", "uid": uid}, "label": reservation["name"],
        "definition_root": reservation["definition_root"], "exclusive_load_roots": [reservation["definition_root"]], "launchctl": "/bin/launchctl",
        "qualification": {"os_version": evidence["os_version"], "os_build": evidence["os_build"], "api_profile": "launchctl-qualified-v1", "receipt_sha256": receipt_sha, "launchctl_fingerprint": fingerprint,
                          "absence_profile": {"profile": "launchctl-user-no-service-v1", "exit_code": evidence["absence_status"], "stderr": evidence["absence_stderr"], "receipt_sha256": common.digest({"domain": f"user/{uid}", "label": reservation["name"], "status": evidence["absence_status"], "stdout": evidence["absence_stdout"], "stderr": evidence["absence_stderr"]})}},
        "managed_by": "installer", "sut_effects": ["registration", "configuration", "removal"], "allowed_actions": ["inspect", "start", "stop"],
    }
    return {"kind": "theseus_supplied_manager_receipt", "schema": 1, "manager": "launchd_user", "authority": authority, "manager_unique_name": f"user/{uid}/{reservation['name']}", "namespace_receipt": "theseus-native-launchd:" + receipt_sha,
            "native_fixture": {"root": reservation["root"], "ready_root": reservation["ready_root"], "cooperative_instance": reservation["instance"]}}


class ToolOwner:
    def __init__(self, path, stack):
        self.path = Path(path)
        self.artifacts = None
        require(self.path.resolve() == self.path, "native tool must have no symlink components")
        self.file = stack.enter_context(os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW), "rb"))
        metadata = os.fstat(self.file.fileno())
        require(stat.S_ISREG(metadata.st_mode) and metadata.st_uid == 0 and metadata.st_mode & 0o022 == 0, "root-owned immutable-to-fixture native tool required")
        self.fingerprint = file_fingerprint(metadata)
        self.sha256 = hashlib.sha256(self.file.read()).hexdigest()
        self.verify()

    def verify(self):
        require(file_fingerprint(os.fstat(self.file.fileno())) == self.fingerprint and file_fingerprint(self.path.stat()) == self.fingerprint, "original native tool identity changed")
        self.file.seek(0)
        require(hashlib.sha256(self.file.read()).hexdigest() == self.sha256 and file_fingerprint(os.fstat(self.file.fileno())) == self.fingerprint, "original native tool bytes changed")

    def run(self, arguments, env, log):
        if self.artifacts is not None:
            self.artifacts.verify("before native tool dispatch", str(log) + ".pins-before.json")
        self.verify()
        child = subprocess.Popen([str(self.path), *arguments], env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            stdout, stderr = child.communicate(timeout=20)
        except BaseException:
            child.terminate()
            try:
                stdout, stderr = child.communicate(timeout=10)
            except subprocess.TimeoutExpired:
                child.kill()
                stdout, stderr = child.communicate(timeout=10)
            common.write_new(log, {"command": [str(self.path), *arguments], "status": child.returncode, "interrupted_or_timed_out": True, "stdout_base64": base64.b64encode(stdout).decode(), "stderr_base64": base64.b64encode(stderr).decode(), "tool_sha256": self.sha256, "tool_fingerprint": self.fingerprint})
            raise
        common.write_new(log, {"command": [str(self.path), *arguments], "status": child.returncode, "stdout_base64": base64.b64encode(stdout).decode(), "stderr_base64": base64.b64encode(stderr).decode(), "tool_sha256": self.sha256, "tool_fingerprint": self.fingerprint})
        self.verify()
        if self.artifacts is not None:
            self.artifacts.verify("after native tool joined", str(log) + ".pins-after.json")
        require(len(stdout) + len(stderr) <= 1024 * 1024, "native output exceeds qualified bound (raw retained)")
        return child.returncode, stdout.decode("utf-8", "strict"), stderr.decode("utf-8", "strict")


def private_root(path, stack):
    path = Path(path)
    path.mkdir(mode=0o700)
    fd = os.open(path, os.O_DIRECTORY | os.O_NOFOLLOW)
    stack.callback(os.close, fd)
    original = os.fstat(fd)
    require(path.resolve() == path and original.st_uid == os.geteuid() and stat.S_IMODE(original.st_mode) == 0o700, "fresh canonical private fixture root required")
    return fd, {"path": str(path), "dev": original.st_dev, "inode": original.st_ino, "uid": original.st_uid, "mode": "0700"}


def operator(plan, source_env):
    env = common.qualified_env(plan, source_env)
    uid = os.geteuid()
    require(uid > 0, "native tests must run under the actual non-root explicit User domain; never elevates")
    root = Path(plan["root"])
    with ExitStack() as stack:
        root_fd, root_identity = private_root(root, stack)
        launchctl = ToolOwner("/bin/launchctl", stack)
        sw_vers = ToolOwner("/usr/bin/sw_vers", stack)
        version_status, version, version_error = sw_vers.run(["-productVersion"], env, root / "os-version.json")
        build_status, build, build_error = sw_vers.run(["-buildVersion"], env, root / "os-build.json")
        require(version_status == build_status == 0 and not version_error and not build_error, "actual native OS version/build measurement failed")
        domain_status, domain_out, domain_err = launchctl.run(["print", f"user/{uid}"], env, root / "user-domain.json")
        artifacts_root = root / "pinned-artifacts"
        private_root(artifacts_root, stack)
        artifacts, pins = common.pin_artifacts(plan, artifacts_root)
        env.update({key: artifacts[key] for key in common.BINARY_ENV})
        common.write_new(root / "source-build-artifacts.json", {"manifest": plan["manifest"], "artifacts": pins, "root": root_identity, "operator_uid": uid, "github": {key: source_env[key] for key in ("GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT", "GITHUB_SHA")}})
        artifact_guard = stack.enter_context(common.ArtifactGuard(artifacts, pins, artifacts_root))
        artifact_guard.verify("source/build evidence frozen", root / "artifacts-initial-guard.json")
        launchctl.artifacts = sw_vers.artifacts = artifact_guard
        list_status, inventory = common.run_logged([artifacts["native_services_platforms"], "--list", "--format=terse"], env, root / "tests-list.log", artifacts=artifact_guard)
        require(list_status == 0, "native precompiled test inventory failed")
        inventory = inventory.decode("utf-8", "strict").splitlines()
        results, all_absent = [], True
        try:
            for number, case in enumerate(plan["cases"]):
                command = common.test_command(artifacts[case["binary"]], case["name"], inventory)
                receipts, owners = [], []
                for item, reservation in enumerate(case["reservations"]):
                    for key in ("root", "ready_root", "definition_root"):
                        fd, identity = private_root(reservation[key], stack)
                        owners.append((fd, identity))
                    status, stdout, stderr = launchctl.run(["print", f"user/{uid}/{reservation['name']}"], env, root / f"case-{number}-{item}-absence-before.json")
                    evidence = {"uid": uid, "os_version": version.strip(), "os_build": build.strip(), "tool_fingerprint": launchctl.fingerprint, "tool_sha256": launchctl.sha256, "domain_status": domain_status, "domain_stdout": domain_out, "domain_stderr": domain_err, "absence_status": status, "absence_stdout": stdout, "absence_stderr": stderr, "root_owners": [identity for _, identity in owners[-3:]]}
                    receipt = manager_receipt(plan, reservation, evidence)
                    path = root / f"case-{number}-{item}-receipt.json"
                    common.write_new(path, receipt)
                    receipts.append(str(path))
                inventory_path = root / f"case-{number}-receipts.json"
                common.write_new(inventory_path, receipts)
                case_env = dict(env, THESEUS_NATIVE_LAUNCHD_RECEIPT_INVENTORY=str(inventory_path))
                status, output = common.run_logged(command, case_env, root / f"case-{number}.log", timeout=600, artifacts=artifact_guard)
                common.validate_test_result(status, output)
                for fd, identity in owners:
                    current = Path(identity["path"]).stat()
                    require((current.st_dev, current.st_ino) == (os.fstat(fd).st_dev, os.fstat(fd).st_ino), "original fixture root replaced; unknown namespace retained")
                results.append({"name": case["name"], "status": status, "receipts": receipts})
        finally:
            # Observation is limited to exact names reserved in this invocation.
            # The SUT owns all registration controls. An abandoned loaded job is
            # never adopted by a name lookup or booted out by this operator.
            observations = []
            for number, case in enumerate(plan["cases"]):
                for item, reservation in enumerate(case["reservations"]):
                    status, stdout, stderr = launchctl.run(["print", f"user/{uid}/{reservation['name']}"], env, root / f"case-{number}-{item}-absence-final.json")
                    try:
                        validate_absence(reservation["name"], uid, status, stdout, stderr)
                        absent = True
                    except ValueError:
                        absent = False
                        all_absent = False
                    observations.append({"name": reservation["name"], "absent": absent, "status": status})
            common.write_new(root / "cleanup.json", {"all_exact_reserved_names_absent": all_absent, "observations": observations, "registrations_mutated_by_operator": False, "roots_and_raw_logs_retained": True, "original_root_inode": os.fstat(root_fd).st_ino})
        require(all_absent, "native fixture registration remains or is unknown; preserved for disposable runner teardown, never adopted")
        common.write_new(root / "result.json", {"source_sha": plan["manifest"]["source_sha"], "cases": results, "native_manager": "launchd-user", "external_gui_ssh_reset": "Pending"})
    return str(root)


def main():
    common.cli(sys.modules[__name__])


if __name__ == "__main__":
    main()
