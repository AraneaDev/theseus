"""Closed source-pinned CI planner and retained-process driver; no import effects.

Only --execute callers after hosted-runner attestation may use runtime helpers.
Receipts are evidence, never resumable manager/process cleanup authority.
"""
import argparse
from contextlib import ExitStack
import copy
import hashlib
import json
import os
from pathlib import Path, PurePosixPath, PureWindowsPath
import platform
import re
import secrets
import stat
import subprocess

BINARY_ENV = ("THESEUS_NATIVE_HELPER_BINARY", "THESEUS_NATIVE_SERVICE_FIXTURE", "THESEUS_NATIVE_SERVICE_BASE", "THESEUS_NATIVE_SERVICE_TARGET", "THESEUS_NATIVE_INTEGRATION_CONTROLLER")
COMMON_CASES = {
    "native::native_upgrade_sut_transitions_from_running_and_preserves_data": 1,
    "native::native_reinstall_compares_original_definition_reader_and_data": 1,
    "native::native_installer_missing_and_leaked_registration_never_passes": 2,
    "native::native_running_wrong_version_data_or_nonce_never_passes_readiness": 3,
}
GRAMMAR_CASES = {
    "native_supplied_receipt_requires_explicit_platform_layout_and_instance": 0,
    "native_supplied_receipt_preserves_exact_launchd_authority_and_rejects_implicit_inputs": 0,
    "native_acl_policy_refuses_inherited_extra_or_content_write_executable_grants": 0,
}
SHA = r"[a-f0-9]{40}|[a-f0-9]{64}"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def path_value(value, windows):
    require(isinstance(value, str) and len(value) <= 4096, "explicit platform absolute path required")
    if windows:
        require(re.fullmatch(r"[A-Za-z]:\\[A-Za-z0-9_ .\\-]+", value), "closed local Windows drive path required")
        parts = value[3:].split("\\")
        require(all(p and p not in (".", "..") and not p.endswith((".", " ")) for p in parts), "noncanonical Windows path")
        require(all(p.split(".")[0].upper() not in {"CON", "PRN", "AUX", "NUL", *[f"COM{i}" for i in range(1, 10)], *[f"LPT{i}" for i in range(1, 10)]} for p in parts), "reserved Windows device path")
    else:
        require(re.fullmatch(r"/[A-Za-z0-9_./-]+", value), "closed absolute ASCII path required")
        require(all(p and p not in (".", "..") for p in value[1:].split("/")), "noncanonical path")
    return value


def guard(env, *, execute, disposable, system, windows):
    require(execute and disposable, "requires --execute --disposable-github-runner")
    require(system == ("Windows" if windows else "Darwin"), "native host OS required; never elevates")
    for key, expected in {"CI": "true", "GITHUB_ACTIONS": "true", "RUNNER_OS": "Windows" if windows else "macOS", "RUNNER_ENVIRONMENT": "github-hosted"}.items():
        require(env.get(key) == expected, f"{key}={expected} required")
    for key in ("GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT"):
        require(re.fullmatch(r"[1-9][0-9]*", env.get(key, "")), f"{key} required")
    require(re.fullmatch(SHA, env.get("GITHUB_SHA", "")), "exact GitHub source SHA required")


def plan_for(manifest, token, *, windows, native_cases):
    require(isinstance(token, str) and re.fullmatch(r"[a-f0-9]{16}", token), "16 lowercase hex reservation token required")
    require(manifest.get("schema") == 1 and re.fullmatch(SHA, manifest.get("source_sha", "")), "manifest schema/exact source SHA required")
    source = manifest["source_sha"]
    build = manifest.get("build", {})
    targets = ("x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc") if windows else ("x86_64-apple-darwin", "aarch64-apple-darwin")
    require(build.get("target") in targets and build.get("profile") in ("debug", "release") and build.get("source_sha") == source, "exact matching native build source/target/profile required")
    require(isinstance(build.get("features"), list) and "native-integration-test-support" in build["features"] and len(build["features"]) == len(set(build["features"])), "explicit native fixture feature set required")
    artifacts = manifest.get("artifacts", {})
    require(set(artifacts) == {*BINARY_ENV, "native_services_platforms"}, "exact native artifact roles required")
    paths = set()
    for pin in artifacts.values():
        path = path_value(pin.get("path"), windows)
        key = path.lower() if windows else path
        require(key not in paths, "artifact roles must not alias")
        paths.add(key)
        require(re.fullmatch(r"[a-f0-9]{64}", pin.get("sha256", "")) and type(pin.get("length")) is int and 0 < pin["length"] <= 512 * 1024 * 1024 and pin.get("source_sha") == source, "exact artifact source/length/SHA256 pins required")
    cases = manifest.get("cases", [])
    required = dict(COMMON_CASES, **native_cases)
    names = [case.get("name") for case in cases]
    require(len(names) == len(set(names)) and set(required).issubset(names) and set(names).issubset(required.keys() | GRAMMAR_CASES.keys()), "complete exact native case set required; no model/optional/duplicate cases")
    root = ("C:\\theseus-ci-" if windows else "/private/tmp/th-ci-") + token
    join = PureWindowsPath if windows else PurePosixPath
    planned = []
    index = 0
    for case in cases:
        require(case.get("binary") == "native_services_platforms", "explicit native_services_platforms executable required")
        reservations = []
        for _ in range(required.get(case["name"], 0)):
            suffix = token + f"-{index:02d}"
            # Each test fixture needs two empty independent roots. Definition
            # roots are separate so exact fixture root emptiness remains valid.
            reservations.append({"root": str(join(root) / ("fixture-" + str(index))), "ready_root": str(join(root) / ("ready-" + str(index))), "definition_root": str(join(root) / ("definitions-" + str(index))), "name": "theseus.ci." + suffix, "instance": "theseus-ci-original-" + suffix, "pipe": "\\\\.\\pipe\\theseus.ci." + suffix})
            index += 1
        planned.append(dict(case, reservations=reservations))
    return {"root": root, "token": token, "manifest": manifest, "cases": planned, "platform": "scm" if windows else "launchd"}


def test_command(binary, name, inventory):
    require(inventory.count(name + ": test") == 1, "required exact test not registered exactly once")
    return [binary, name, "--exact", "--nocapture", "--test-threads=1"]


def validate_test_result(status, output):
    require(status == 0 and re.search(rb"test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; [0-9]+ filtered out;", output), "required native test failed, absent, ignored or did not emit exact one-test completion")


def write_new(path, value):
    with Path(path).open("x", encoding="utf-8") as stream:
        json.dump(value, stream, sort_keys=True, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())


def run_logged(command, env, path, timeout=30, *, artifacts):
    """Join only the original Popen. Never discover or signal by PID/name."""
    with Path(path).open("xb") as log:
        artifacts.verify("before dispatch", str(path) + ".pins-before.json", executable=command[0])
        child = subprocess.Popen(command, env=env, stdout=log, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
        try:
            status = child.wait(timeout=timeout)
        except BaseException:
            child.terminate()
            try:
                child.wait(timeout=10)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=10)
            raise
    raw = Path(path).read_bytes()
    artifacts.verify("after original child joined", str(path) + ".pins-after.json", executable=command[0])
    require(len(raw) <= 32 * 1024 * 1024, "raw native log exceeds evidence bound (retained)")
    return status, raw


def qualified_env(plan, source_env):
    require(source_env.get("GITHUB_SHA") == plan["manifest"]["source_sha"], "manifest source must equal exact checked-out GitHub SHA")
    windows = plan["platform"] == "scm"
    actual = platform.machine().lower()
    expected = "aarch64" if plan["manifest"]["build"]["target"].startswith("aarch64") else "x86_64"
    require(actual in ({"aarch64", "arm64"} if expected == "aarch64" else {"x86_64", "amd64"}), "artifact target differs from actual runner architecture")
    if windows:
        # Windows process creation and TLS need the real OS root; do not invent it.
        systemroot = path_value(source_env.get("SystemRoot"), True)
        env = {"SystemRoot": systemroot, "WINDIR": systemroot, "PATH": systemroot + "\\System32", "TEMP": plan["root"], "TMP": plan["root"]}
    else:
        env = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LANG": "C", "TMPDIR": plan["root"]}
    env.update({key: source_env[key] for key in ("CI", "GITHUB_ACTIONS", "RUNNER_OS", "RUNNER_ENVIRONMENT", "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT", "GITHUB_SHA")})
    return env



def object_identity(metadata):
    return {"dev": metadata.st_dev, "inode": metadata.st_ino, "mode": metadata.st_mode,
            "uid": metadata.st_uid, "gid": metadata.st_gid, "length": metadata.st_size,
            "modified_ns": metadata.st_mtime_ns, "changed_ns": metadata.st_ctime_ns,
            "attributes": getattr(metadata, "st_file_attributes", 0)}


def path_identity(path):
    """What `path` currently names. POSIX keeps lstat (a link is its own
    object). Windows path stat and handle fstat are filled by different APIs
    (CI16/CI17), so observe the path through a fresh handle of the same API."""
    if os.name != "nt":
        return object_identity(Path(path).lstat())
    # The fresh open would follow a reparse point, so refuse one at the name.
    require(not (getattr(Path(path).lstat(), "st_file_attributes", 0) & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)),
            "copied artifact path became a link or reparse point")
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_BINARY", 0))
    try:
        return object_identity(os.fstat(descriptor))
    finally:
        os.close(descriptor)


def same_object(left, right):
    return (left["dev"], left["inode"]) == (right["dev"], right["inode"])


class ArtifactGuard:
    """Retained copied objects, rechecked at every consumption boundary.

    Checks are source qualification under the disposable-runner contract. They
    do not promise atomic pathname execution against a hostile concurrent writer.
    A failed check preserves originals/replacements and never repairs/adopts them.
    """
    def __init__(self, paths, pins, directory, native=None):
        self.paths, self.pins = dict(paths), copy.deepcopy(pins)
        self.directory, self.native = Path(directory), native
        self.stack = ExitStack()
        self.files, self.metadata, self.native_ids = {}, {}, {}
        try:
            require(len(self.pins) == len(self.paths) and {pin["role"] for pin in self.pins} == set(self.paths), "exact copied role inventory required")
            require(self.directory.resolve() == self.directory, "copy directory canonical identity lost")
            roots = [pin["copy_root"] for pin in self.pins]
            require(roots and all(root == roots[0] for root in roots), "one original copied directory required")
            if native:
                self.directory_handle = native.open_frozen(self.directory, directory=True)
                self.stack.callback(native.close, self.directory_handle)
                self.native_directory_id = native.file_identity(self.directory_handle)
                native.acl_receipt(self.directory_handle, "directory")
                current = object_identity(self.directory.lstat())
            else:
                fd = os.open(self.directory, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
                self.stack.callback(os.close, fd)
                self.directory_handle = fd
                current = object_identity(os.fstat(fd))
            require(same_object(current, roots[0]), "original copied directory replaced")
            self.directory_identity = current
            for pin in self.pins:
                role, path = pin["role"], Path(pin["copy"])
                require(str(path) == self.paths[role] and path.parent == self.directory and path.resolve() == path, "exact copied artifact path required")
                if native:
                    file = native.open_frozen_file(path)
                else:
                    file = os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW), "rb")
                self.files[role] = self.stack.enter_context(file)
                metadata = object_identity(os.fstat(file.fileno()))
                require(same_object(metadata, pin["copied_object"]), "original copied artifact replaced before retention")
                self.metadata[role] = metadata
                if native:
                    self.native_ids[role] = native.file_identity(native.file_handle(file))
            self.verify("initial frozen object retention")
        except BaseException:
            self.stack.close()
            raise

    def __enter__(self):
        return self

    def __exit__(self, *error):
        self.stack.close()

    def verify(self, boundary, evidence_path=None, executable=None):
        evidence = {"boundary": boundary, "pins": self.pins, "observed": [], "status": "refused"}
        try:
            require(executable is None or executable in self.paths.values(), "dispatch executable not in retained exact artifact roles")
            require(self.directory.resolve() == self.directory, "copied directory canonical identity lost")
            current_root = object_identity(self.directory.lstat())
            evidence["directory"] = current_root
            require(stat.S_ISDIR(current_root["mode"]) and same_object(current_root, self.directory_identity), "original copied directory missing or replaced")
            if self.native:
                require(self.native.file_identity(self.directory_handle) == self.native_directory_id, "original native copied directory identity changed")
                evidence["directory_acl"] = self.native.acl_receipt(self.directory_handle, "directory")
            else:
                require(same_object(object_identity(os.fstat(self.directory_handle)), current_root) and stat.S_IMODE(current_root["mode"]) == 0o500 and current_root["uid"] == os.geteuid(), "original copied directory must remain privately frozen 0500")
            require({entry.name for entry in self.directory.iterdir()} == {Path(path).name for path in self.paths.values()}, "copied artifact directory has missing or unknown leaves")
            for pin in self.pins:
                role, file, path = pin["role"], self.files[pin["role"]], Path(pin["copy"])
                require(path.resolve() == path, "copied artifact canonical identity lost")
                held, current = object_identity(os.fstat(file.fileno())), path_identity(path)
                observed = {"role": role, "held": held, "path": current}
                evidence["observed"].append(observed)
                require(stat.S_ISREG(held["mode"]) and current == held == self.metadata[role], f"copied artifact missing, replaced or metadata changed: {observed} expected {self.metadata[role]}")
                if self.native:
                    handle = self.native.file_handle(file)
                    require(self.native.file_identity(handle) == self.native_ids[role] and held["attributes"] & 1, "copied native artifact must remain original and readonly")
                    observed["acl"] = self.native.acl_receipt(handle, "executable")
                else:
                    require(stat.S_IMODE(held["mode"]) == 0o500 and held["uid"] == os.geteuid(), "copied executable must remain privately frozen 0500")
                file.seek(0)
                hasher, count = hashlib.sha256(), 0
                while chunk := file.read(65536):
                    count += len(chunk)
                    require(count <= pin["length"], "copied artifact grew while checking")
                    hasher.update(chunk)
                observed["sha256"], observed["length"] = hasher.hexdigest(), count
                require(count == pin["length"] and hasher.hexdigest() == pin["sha256"] and object_identity(os.fstat(file.fileno())) == held and object_identity(path.lstat()) == held, "copied artifact bytes differ from original exact source pin")
            evidence["status"] = "verified"
        except (OSError, ValueError) as error:
            evidence["error"] = str(error)
            raise ValueError("copied artifact guard refused at " + boundary + ": " + str(error)) from error
        finally:
            if evidence_path is not None:
                write_new(evidence_path, evidence)


def stable_identity(st):
    """Object identity without access time: reading for the pin digest updates
    st_atime on Windows (CI15), which must not read as a change."""
    return (st.st_dev, st.st_ino, st.st_size, st.st_mtime_ns, st.st_ctime_ns, st.st_mode, st.st_nlink, st.st_uid)


def reopened_identity(path, flags):
    """The path's current object, observed through a fresh handle: on Windows
    path stat and handle fstat may be filled by different APIs (CI16)."""
    descriptor = os.open(path, flags)
    try:
        return stable_identity(os.fstat(descriptor))
    finally:
        os.close(descriptor)


def pin_artifacts(plan, destination, secure=None):
    """Copy exclusive leaves; retain original source descriptors through copying."""
    result, identities, original_files = {}, [], set()
    copy_root = object_identity(Path(destination).lstat())
    require(stat.S_ISDIR(copy_root["mode"]) and Path(destination).resolve() == Path(destination), "original canonical copy directory required")
    for role, pin in plan["manifest"]["artifacts"].items():
        source = Path(pin["path"])
        require(source.resolve() == source and source.is_file(), "canonical no-symlink source artifact required")
        flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_BINARY", 0)
        with os.fdopen(os.open(source, flags), "rb") as handle:
            before = os.fstat(handle.fileno())
            require(stat.S_ISREG(before.st_mode) and before.st_size == pin["length"], "source artifact metadata differs")
            identity = (before.st_dev, before.st_ino)
            require(identity not in original_files, "artifact roles alias the same original file object")
            original_files.add(identity)
            target = Path(destination) / (role + (".exe" if plan["platform"] == "scm" else ""))
            h = hashlib.sha256()
            count = 0
            with target.open("xb") as output:
                while chunk := handle.read(65536):
                    count += len(chunk)
                    require(count <= pin["length"], "artifact grew during pin")
                    h.update(chunk)
                    output.write(chunk)
                output.flush()
                os.fsync(output.fileno())
                copied_identity = object_identity(os.fstat(output.fileno()))
            after = os.fstat(handle.fileno())
            current = reopened_identity(source, flags)
            require((before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns) == (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns) and current == stable_identity(after), f"source artifact changed during pin: {current} != {stable_identity(after)}")
            require(count == pin["length"] and h.hexdigest() == pin["sha256"], "artifact digest differs from exact pin")
            if secure:
                secure(target, "executable")
            # Match the fixture frozen executable policy: mode 0500 on Unix,
            # and readonly attribute plus exact executable DACL on Windows.
            target.chmod(0o500)
            require(same_object(copied_identity, object_identity(target.lstat())), "copied artifact replaced before freeze")
            result[role] = str(target)
            identities.append({"role": role, "source": str(source), "source_dev": before.st_dev, "source_inode": before.st_ino, "copy": str(target), "copied_object": copied_identity, "copy_root": copy_root, **pin})
    if plan["platform"] != "scm":
        Path(destination).chmod(0o500)
    return result, identities


def cli(module):
    parser = argparse.ArgumentParser(description=module.__doc__)
    parser.add_argument("--manifest", required=True)
    parser.add_argument("--plan", action="store_true")
    parser.add_argument("--token", help="explicit fresh 16-hex reservation; same plan/execute token makes exact evidence path known on failure")
    parser.add_argument("--execute", action="store_true")
    parser.add_argument("--disposable-github-runner", action="store_true")
    args = parser.parse_args()
    require(args.plan != args.execute, "select exactly one of --plan or --execute")
    raw = Path(args.manifest).read_bytes()
    require(len(raw) <= 65536, "bounded source pin manifest required")
    plan = module.make_plan(json.loads(raw), args.token if args.token is not None else secrets.token_hex(8))
    if args.plan:
        print(json.dumps(plan, indent=2, sort_keys=True))
        return
    module.execution_guard(os.environ, execute=args.execute, disposable=args.disposable_github_runner, system=platform.system())
    print(module.operator(plan, os.environ))
