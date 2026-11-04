#!/usr/bin/env python3
"""CI-only real private systemd fixture. --plan/--help never contact managers.

The privileged operator must already run as root on a disposable GitHub-hosted
Linux runner. This script never elevates. One invocation provisions, qualifies,
runs explicitly pinned precompiled tests, collects evidence and joins original
owners. It cannot resume/clean up by PID or reconstruct a manager from a receipt.
"""
import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import platform
import pwd
import re
import shutil
import signal
import socket
import stat
import struct
import subprocess
import sys
import time
import xml.sax.saxutils

BINARY_ENV = (
    "THESEUS_NATIVE_HELPER_BINARY", "THESEUS_NATIVE_SERVICE_FIXTURE",
    "THESEUS_NATIVE_SERVICE_BASE", "THESEUS_NATIVE_SERVICE_TARGET",
    "THESEUS_NATIVE_INTEGRATION_CONTROLLER",
    "THESEUS_NATIVE_DESKTOP_HELPER_BINARY", "THESEUS_NATIVE_DESKTOP_BROKER_BINARY",
)
# Exact real-manager cases, including both three-scenario OwnedDesktop/real-service
# acceptance paths. Model/optional/arbitrary cases are never operator authority.
REQUIRED_CASES = {
    "native_services": (
        "systemd_user_clean_installer_managed_service_uses_shared_product_path",
        "systemd_installer_missing_service_is_failed_without_harness_repair",
        "systemd_uninstaller_leaves_service_is_failed_before_and_after_provider_disposal",
        "systemd_preexisting_or_wrong_owned_unit_is_untouched",
    ),
    "native_service_mcp": ("systemd_supplied_cli_and_fresh_mcp_normalized_frontend_parity",),
    "native_desktop": (
        "source_combined_service_desktop_share_one_sequence_provider_and_lease",
        "source_combined_ordinary_fresh_six_tool_mcp_keeps_one_shared_lease_and_saved_views",
    ),
}
FEATURES = ["unit_properties", "start_stop", "manager_reload"]
SDK_SYMBOLS = (
    "sd_bus_new", "sd_bus_set_address", "sd_bus_set_bus_client",
    "sd_bus_set_allow_interactive_authorization", "sd_bus_set_method_call_timeout",
    "sd_bus_start", "sd_bus_close_unref", "sd_bus_get_fd", "sd_bus_get_events",
    "sd_bus_process", "sd_bus_get_owner_creds", "sd_bus_creds_get_euid",
    "sd_bus_creds_unref", "sd_bus_message_new_method_call",
    "sd_bus_message_set_auto_start", "sd_bus_message_set_allow_interactive_authorization",
    "sd_bus_message_append_basic", "sd_bus_message_open_container",
    "sd_bus_message_close_container", "sd_bus_message_enter_container",
    "sd_bus_message_exit_container", "sd_bus_message_read_basic", "sd_bus_message_skip",
    "sd_bus_message_ref", "sd_bus_message_unref", "sd_bus_message_get_error",
    "sd_bus_call_async", "sd_bus_match_signal", "sd_bus_slot_unref",
)
SUPPORT_TARGET = "[Unit]\nDescription=Theseus private fixture anchor\nDefaultDependencies=no\n"
MANAGER = "org.freedesktop.systemd1"
OBJECT = "/org/freedesktop/systemd1"
IFACE = "org.freedesktop.systemd1.Manager"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def canonical_path(value):
    require(isinstance(value, str) and re.fullmatch(r"/[A-Za-z0-9_./-]+", value), "closed absolute ASCII path required")
    require(all(p not in ("", ".", "..") for p in value[1:].split("/")), "noncanonical path")
    return value


def execution_guard(env, *, execute, disposable, uid, system):
    require(execute and disposable, "provisioning requires --execute --disposable-github-runner")
    require(uid == 0 and system == "Linux", "already-privileged Linux operator required; this script never elevates")
    for key, expected in {"CI": "true", "GITHUB_ACTIONS": "true", "RUNNER_OS": "Linux", "RUNNER_ENVIRONMENT": "github-hosted"}.items():
        require(env.get(key) == expected, f"{key}={expected} required")
    for key in ("GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT"):
        require(re.fullmatch(r"[1-9][0-9]*", env.get(key, "")), f"{key} required")
    require(re.fullmatch(r"[a-f0-9]{40}|[a-f0-9]{64}", env.get("GITHUB_SHA", "")), "GITHUB_SHA required")


def make_plan(manifest, token):
    require(re.fullmatch(r"[a-f0-9]{16}", token), "16 lowercase hex reservation token required")
    require(manifest.get("schema") == 1, "manifest schema 1 required")
    require(re.fullmatch(r"[a-f0-9]{40}|[a-f0-9]{64}", manifest.get("source_sha", "")), "exact source SHA required")
    build = manifest.get("build", {})
    require(build.get("target") in ("x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"), "explicit Linux GNU target required")
    require(build.get("profile") in ("debug", "release") and build.get("source_sha") == manifest["source_sha"], "matching original source/build profile required")
    require(isinstance(build.get("features"), list) and "native-integration-test-support" in build["features"], "fixture build feature declaration required")
    artifacts = manifest.get("artifacts", {})
    require(set(artifacts) == {*BINARY_ENV, *REQUIRED_CASES}, "exact source owner and test artifact roles required")
    paths = set()
    for artifact in artifacts.values():
        require(set(artifact) == {"path", "sha256", "length", "source_sha"}, "exact artifact pin fields required")
        path = canonical_path(artifact.get("path"))
        require(path not in paths, "pinned artifact roles must not alias")
        paths.add(path)
        require(re.fullmatch(r"[a-f0-9]{64}", artifact.get("sha256", "")), "artifact SHA256 required")
        require(type(artifact.get("length")) is int and 0 < artifact["length"] <= 512 * 1024 * 1024, "bounded artifact length required")
        require(artifact.get("source_sha") == manifest["source_sha"], "exact matching artifact source required")
    cases = manifest.get("cases", [])
    require(isinstance(cases, list) and cases, "explicit native named cases required")
    seen = set()
    required = {(binary, name) for binary, names in REQUIRED_CASES.items() for name in names}
    for case in cases:
        require(isinstance(case, dict) and set(case) == {"binary", "name"}, "exact pinned case fields required")
        key = (case["binary"], case["name"])
        require(key in required and key not in seen, "exact non-model real-manager case/role required once")
        seen.add(key)
    require(seen == required, "all original real-manager and Combined cases are mandatory")
    root = "/tmp/th-sd-" + token
    env = {
        "PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "LANG": "C.UTF-8",
        "XDG_RUNTIME_DIR": root + "/runtime", "XDG_CONFIG_HOME": root + "/config",
        "XDG_CONFIG_DIRS": root + "/config-dirs", "XDG_DATA_HOME": root + "/data",
        "XDG_DATA_DIRS": root + "/data-dirs", "XDG_CACHE_HOME": root + "/cache",
        "TMPDIR": root + "/tmp", "SYSTEMD_UNIT_PATH": root + "/units:" + root + "/support-units",
        "SYSTEMD_GENERATOR_PATH": root + "/generators",
        "SYSTEMD_ENVIRONMENT_GENERATOR_PATH": root + "/environment-generators",
        "DBUS_SESSION_BUS_ADDRESS": "unix:path=" + root + "/runtime/bus",
        # systemd's user manager also opens a system-bus client for cgroup
        # notifications. Keep even that client on our owned private bus.
        "DBUS_SYSTEM_BUS_ADDRESS": "unix:path=" + root + "/runtime/bus",
        "SYSTEMD_LOG_TARGET": "console", "SYSTEMD_LOG_LEVEL": "info",
        "CI": "true", "GITHUB_ACTIONS": "true", "RUNNER_OS": "Linux",
        "RUNNER_ENVIRONMENT": "github-hosted",
    }
    name = "thsd_" + token
    command = [
        "/usr/bin/systemd-run", "--system", "--unit=theseus-private-" + token + ".service",
        "--uid=" + name, "--gid=" + name, "--property=Delegate=yes",
        "--property=KillMode=control-group", "--property=TimeoutStopSec=30s",
        "--property=RuntimeMaxSec=1800s", "--property=UMask=0077",
        "--service-type=exec", "--wait", "--collect", "--pipe",
        "/usr/bin/env", "-i", *[f"{k}={v}" for k, v in env.items()],
        "INVOCATION_ID=${INVOCATION_ID}",
        "THESEUS_CI_PRIVATE_SYSTEMD_WORKER=1", "/usr/bin/python3", root + "/operator.py",
        "--worker", "--manifest", root + "/worker.json",
    ]
    return {"root": root, "account": name, "service_command": command,
            "worker_environment": env, "manifest": manifest, "token": token}


def validate_live_namespace(paths, root):
    require(isinstance(paths, list) and 1 <= len(paths) <= 8 and len(set(paths)) == len(paths), "bounded exact UnitPath required")
    # SYSTEMD_UNIT_PATH must replace the entire search path: no default roots,
    # automatically imported unit directories, generators, or drop-in locations.
    require(paths == [root + "/units", root + "/support-units"], "live UnitPath must equal only the private definition/support roots")
    for path in paths:
        canonical_path(path)


def test_command(binary, name, inventory):
    require(inventory.count(name + ": test") == 1, "required exact test not registered exactly once")
    return [binary, name, "--exact", "--nocapture", "--test-threads=1"]


def case_timeout(case):
    require(isinstance(case, dict) and set(case) == {"binary", "name"}, "exact case required")
    require(case["binary"] in REQUIRED_CASES and case["name"] in REQUIRED_CASES[case["binary"]], "allowlisted real-manager case required")
    # Each Combined test covers Clean/Upgrade/Reinstall. Captured model timing
    # was 91s/186s; 450s permits real-manager/artifact variance within existing
    # outer fixture/CI bounds. Existing service-only budgets stay 300s.
    return 450 if case["binary"] == "native_desktop" else 300


def artifact_environment(plan):
    environment = dict(plan["worker_environment"])
    environment["THESEUS_NATIVE_SYSTEMD_RECEIPT"] = plan["root"] + "/manager-receipt.json"
    for name in BINARY_ENV:
        require(name in plan["manifest"]["artifacts"], "required original binary role absent")
        environment[name] = plan["root"] + "/artifacts/" + name
    return environment


def manager_receipt(plan, uid, evidence):
    validate_live_namespace(evidence["unit_paths"], plan["root"])
    require(uid > 0 and evidence["bus_peer_uid"] == uid and evidence["manager_uid"] == uid, "original fixture bus and manager UID mismatch")
    version = evidence["version"]
    require(isinstance(version, str) and 0 < len(version) <= 256 and "model" not in version.lower(), "actual native systemd Version required")
    require(re.fullmatch(r":[0-9]+\.[0-9]+", evidence["manager_unique_name"]), "live D-Bus unique manager owner required")
    require(re.fullmatch(r"[a-f0-9]{32}", evidence["service_invocation_id"]), "retained original transient service invocation required")
    require(evidence.get("no_other_namespace_writers") is True, "exclusive fresh UID namespace required")
    require(evidence["bus_pid"] > 0 and evidence["manager_pid"] > 0 and evidence["bus_pid"] != evidence["manager_pid"], "original distinct Popen identities required")
    digest = hashlib.sha256(json.dumps(evidence, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    namespace = "theseus-real-private-systemd:" + plan["token"] + ":" + digest
    authority = {
        "kind": "systemd", "slot": "service", "scope": "user",
        "bus_endpoint": {"kind": "unix_path", "path": plan["root"] + "/runtime/bus"},
        "qualification": {"manager": "systemd", "version": version, "api_profile": "systemd_dbus_private_v1", "features": FEATURES, "receipt_sha256": digest},
        "namespace": {"kind": "exclusive_owned_registration", "slot": "service", "unit_load_roots": evidence["unit_paths"], "dropins": "forbidden", "provider_context": namespace},
        "manager_context": {"kind": "whole_private", "slot": "service", "sut_allowed_actions": ["daemon_reload"], "rehearsal_allowed_actions": [], "provider_receipt_required": True},
        "expected_peer_uid": uid, "expected_manager_uid": uid,
        "definition_root": plan["root"] + "/units", "unit": "theseus-private.service",
        "managed_by": "installer", "sut_effects": ["registration", "configuration", "removal", "daemon_reload"], "allowed_actions": ["inspect", "start", "stop"],
    }
    return {"kind": "theseus_supplied_manager_receipt", "schema": 1, "manager": "systemd_user", "authority": authority, "manager_unique_name": evidence["manager_unique_name"], "namespace_receipt": namespace}


def write_new(path, data):
    with Path(path).open("x", encoding="utf-8") as handle:
        json.dump(data, handle, indent=2, sort_keys=True)
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())


def private_directory(path, uid):
    st = Path(path).lstat()
    require(stat.S_ISDIR(st.st_mode) and st.st_uid == uid and stat.S_IMODE(st.st_mode) == 0o700, f"exact owned 0700 root required: {path}")
    require(Path(path).resolve() == Path(path), "symlink-free fixture root required")
    return {"path": str(path), "dev": st.st_dev, "inode": st.st_ino, "uid": st.st_uid, "mode": "0700"}


def pinned_copy(source, target, pin, uid, gid):
    require(source.resolve() == source and source.is_file(), "regular canonical source artifact required")
    shutil.copyfile(source, target)
    raw = target.read_bytes()
    require(len(raw) == pin["length"] and hashlib.sha256(raw).hexdigest() == pin["sha256"], "copied artifact differs from exact source pin")
    target.chmod(0o500)
    os.chown(target, uid, gid)


def operator(plan, env):
    require(plan["manifest"]["source_sha"] == env["GITHUB_SHA"], "artifact source SHA must match exact GitHub source")
    for path in ("/usr/bin/systemd-run", "/usr/bin/systemctl", "/usr/sbin/useradd", "/usr/sbin/userdel", "/usr/bin/python3", "/usr/bin/dbus-daemon", "/usr/lib/systemd/systemd"):
        require(Path(path).is_file(), f"mandatory CI dependency absent: {path}")
    # Dependency import before creating any account/service. No bus connection.
    subprocess.run(["/usr/bin/python3", "-c", "import dbus; import ctypes; ctypes.CDLL('libsystemd.so.0')"], check=True, env={"PATH": "/usr/bin:/bin"})
    root = Path(plan["root"])
    root.mkdir(mode=0o700)  # exclusive reservation: never adopt an existing tree
    root_fd = os.open(root, os.O_DIRECTORY | os.O_NOFOLLOW)
    account_created = False
    joined_service = True
    child = None
    uid = None
    interrupted = []
    old_handlers = {}
    try:
        subprocess.run(["/usr/sbin/useradd", "--system", "--user-group", "--no-create-home", "--home-dir", "/nonexistent/" + plan["account"], "--shell", "/usr/sbin/nologin", plan["account"]], check=True)
        account_created = True
        account = pwd.getpwnam(plan["account"])
        uid, gid = account.pw_uid, account.pw_gid
        require(uid != 0 and account.pw_name == plan["account"], "dedicated temporary fixture account required")
        os.chown(root, uid, gid)
        roots = [root]
        for name in ("runtime", "config", "config-dirs", "data", "data-dirs", "cache", "tmp", "units", "support-units", "generators", "environment-generators", "artifacts"):
            directory = root / name
            directory.mkdir(mode=0o700)
            os.chown(directory, uid, gid)
            roots.append(directory)
        directories = [private_directory(directory, uid) for directory in roots]
        for name, pin in plan["manifest"]["artifacts"].items():
            pinned_copy(Path(pin["path"]), root / "artifacts" / name, pin, uid, gid)
        shutil.copyfile(Path(__file__).resolve(), root / "operator.py")
        (root / "operator.py").chmod(0o555)
        worker_manifest = {"plan": plan, "uid": uid, "gid": gid, "directories": directories, "github": {k: env[k] for k in ("GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT", "GITHUB_SHA")}}
        write_new(root / "worker.json", worker_manifest)
        os.chown(root / "worker.json", uid, gid)
        (root / "worker.json").chmod(0o400)
        # Only observe signals here. The original service is bounded by its own
        # RuntimeMaxSec; do not kill/reconstruct a service using a discovered PID.
        for signum in (signal.SIGINT, signal.SIGTERM):
            old_handlers[signum] = signal.signal(signum, lambda value, frame: interrupted.append(value))
        with (root / "service.log").open("x") as log:
            child = subprocess.Popen(plan["service_command"], stdout=log, stderr=subprocess.STDOUT, env={"PATH": "/usr/sbin:/usr/bin:/sbin:/bin"})
            joined_service = False
            # Keep the original systemd-run --wait owner until the unit settles.
            status = child.wait()
            # Exit of the client alone does not prove the service stopped if
            # its system-bus connection failed. Observe only our exact unit;
            # never discover another manager/service or mutate a name lookup.
            service_name = "theseus-private-" + plan["token"] + ".service"
            observed = subprocess.run(["/usr/bin/systemctl", "--system", "show", service_name, "--property=LoadState", "--property=ActiveState", "--property=InvocationID"], capture_output=True, text=True, env={"PATH": "/usr/bin:/bin"}, timeout=15)
            fields = dict(line.split("=", 1) for line in observed.stdout.splitlines() if "=" in line)
            write_new(root / "original-service-terminal.json", {"unit": service_name, "status": observed.returncode, "fields": fields, "stderr": observed.stderr})
            owner_path = root / "worker-owner.json"
            owner = json.loads(owner_path.read_text()) if owner_path.is_file() else None
            joined_service = service_terminal(fields, owner["service_invocation_id"] if owner else None)
        require(status == 0 and not interrupted, "real private manager native acceptance failed/interrupted; retained service.log")
        require((root / "result.json").is_file(), "worker did not produce native execution evidence")
        return root
    finally:
        for signum, handler in old_handlers.items():
            signal.signal(signum, handler)
        cleanup = {"account_created": account_created, "fixture_uid": uid, "original_launcher_pid": child.pid if child else None, "original_launcher_joined": joined_service, "interrupted": interrupted, "root_original_inode": os.fstat(root_fd).st_ino, "root_retained": str(root)}
        if account_created and joined_service:
            current = pwd.getpwnam(plan["account"])
            require(current.pw_uid == uid, "original account identity lost; refusing account cleanup")
            result = subprocess.run(["/usr/sbin/userdel", plan["account"]], check=False)
            cleanup["account_removed"] = result.returncode == 0
        else:
            cleanup["account_removed"] = False
        write_new(root / "operator-cleanup.json", cleanup)
        os.close(root_fd)
        require(not account_created or cleanup["account_removed"], "original account cleanup incomplete; inspect retained receipt")


def join_original(child):
    if child is None:
        return None
    if child.poll() is None:
        child.terminate()
    try:
        status = child.wait(timeout=15)
    except subprocess.TimeoutExpired:
        child.kill()
        status = child.wait(timeout=15)
    return {"original_pid": child.pid, "joined_status": status}


def native_child(command, **kwargs):
    """Pin actual executable bytes and verify this original child's exec inode."""
    child = None
    fd = os.open(command[0], os.O_RDONLY | os.O_NOFOLLOW)
    try:
        with os.fdopen(fd, "rb", closefd=False) as binary:
            identity = os.fstat(fd)
            digest = hashlib.sha256(binary.read()).hexdigest()
        child = subprocess.Popen(command, **kwargs)
        require(child.poll() is None, "original native executable exited during setup")
        # Read only this retained child's executable identity; this never
        # reconstructs a process owner or grants PID-based mutation authority.
        executed = Path(f"/proc/{child.pid}/exe").stat()
        require((executed.st_dev, executed.st_ino) == (identity.st_dev, identity.st_ino), "native executable replaced between pin and original exec")
        return child, {"path": command[0], "dev": identity.st_dev, "inode": identity.st_ino, "length": identity.st_size, "sha256": digest}
    except BaseException:
        join_original(child)
        raise
    finally:
        os.close(fd)


def service_terminal(fields, original_invocation):
    if fields.get("LoadState") == "not-found" and fields.get("ActiveState") == "inactive" and not fields.get("InvocationID"):
        return True
    return bool(original_invocation and fields.get("InvocationID") == original_invocation and fields.get("ActiveState") in ("inactive", "failed"))


def delegated_cgroups(token):
    entries = Path("/proc/self/cgroup").read_text().splitlines()
    unified = [line[3:] for line in entries if line.startswith("0::/")]
    require(len(unified) == 1, "delegated unified cgroup v2 required")
    base = Path("/sys/fs/cgroup") / unified[0].lstrip("/")
    require(base.name == "theseus-private-" + token + ".service", "worker must inhabit its original exact fixture service cgroup")
    require(os.access(base / "cgroup.procs", os.W_OK) and os.access(base / "cgroup.subtree_control", os.W_OK), "transient service must delegate its cgroup to the fixture UID")
    owner = base / "theseus-fixture-owner"
    manager = base / "theseus-private-manager"
    owner.mkdir()
    manager.mkdir()
    # Empty delegated parent before enabling controllers; keep bus/test harness
    # outside the private manager subtree (cgroup v2 no-internal-process rule).
    (owner / "cgroup.procs").write_text(str(os.getpid()))
    controllers = (base / "cgroup.controllers").read_text().split()
    if controllers:
        (base / "cgroup.subtree_control").write_text(" ".join("+" + c for c in controllers))
    return base, owner, manager, controllers


def worker(document):
    require(os.environ.get("THESEUS_CI_PRIVATE_SYSTEMD_WORKER") == "1", "internal fixture worker guard absent")
    plan = document["plan"]
    require(make_plan(plan["manifest"], plan["token"]) == plan, "worker plan altered")
    uid = document["uid"]
    require(os.geteuid() == uid and uid > 0 and pwd.getpwuid(uid).pw_name == plan["account"], "worker requires original dedicated fixture UID")
    execution_guard({**os.environ, **document["github"]}, execute=True, disposable=True, uid=0, system=platform.system())
    invocation = os.environ.get("INVOCATION_ID", "")
    # The native service manager expands the literal ${INVOCATION_ID} argument
    # before env -i runs; this preserves only the original generated identifier.
    require(re.fullmatch(r"[a-f0-9]{32}", invocation), "original service INVOCATION_ID required")
    root = Path(plan["root"])
    require(os.environ.get("XDG_RUNTIME_DIR") == str(root / "runtime"), "private runtime context required")
    for key, value in plan["worker_environment"].items():
        require(os.environ.get(key) == value, "worker replacement environment mismatch: " + key)
    os.umask(0o077)
    for directory in document["directories"]:
        require(private_directory(directory["path"], uid) == directory, "retained original directory identity changed")
    for directory in ("units", "support-units", "generators", "environment-generators"):
        require(not list((root / directory).iterdir()), "initial private unit/generator roots must be empty")
    require(not (root / "runtime/bus").exists(), "refusing preexisting bus")
    write_new(root / "worker-owner.json", {"service_invocation_id": invocation, "fixture_uid": uid, "worker_pid": os.getpid(), "service": "theseus-private-" + plan["token"] + ".service"})
    import dbus  # operator-only native Python D-Bus SDK, never product fallback
    bus_child = manager_child = None
    connection = None
    logs = []
    cleanup = {}
    old_handlers = {}
    try:
        for signum in (signal.SIGTERM, signal.SIGINT):
            def interrupt(value, frame):
                raise InterruptedError(f"fixture received signal {value}")
            old_handlers[signum] = signal.signal(signum, interrupt)
        base, owner, manager_cgroup, controllers = delegated_cgroups(plan["token"])
        support_path = root / "support-units/theseus-empty.target"
        with support_path.open("x") as support:
            support.write(SUPPORT_TARGET)
        support_path.chmod(0o400)
        address = plan["worker_environment"]["DBUS_SESSION_BUS_ADDRESS"]
        config = '<busconfig><type>session</type><listen>' + xml.sax.saxutils.escape(address) + '</listen><auth>EXTERNAL</auth><policy context="default"><allow user="' + str(uid) + '"/><allow own="*"/><allow send_destination="*"/><allow receive_sender="*"/></policy></busconfig>'
        (root / "dbus.conf").write_text(config)
        bus_log = (root / "dbus.log").open("x")
        logs.append(bus_log)
        bus_child, bus_binary = native_child(["/usr/bin/dbus-daemon", "--nofork", "--nopidfile", "--config-file=" + str(root / "dbus.conf")], stdout=bus_log, stderr=subprocess.STDOUT, env=plan["worker_environment"])
        deadline = time.monotonic() + 30
        while not (root / "runtime/bus").exists():
            require(bus_child.poll() is None and time.monotonic() < deadline, "original private bus failed to start")
            time.sleep(0.05)
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as peer:
            peer.connect(str(root / "runtime/bus"))
            peer_pid, peer_uid, peer_gid = struct.unpack("3i", peer.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize("3i")))
        require(peer_uid == uid and peer_pid == bus_child.pid and bus_child.poll() is None, "bus socket does not belong to original fixture Popen")
        connection = dbus.bus.BusConnection(address)
        daemon = dbus.Interface(connection.get_object("org.freedesktop.DBus", "/org/freedesktop/DBus", introspect=False), "org.freedesktop.DBus")
        manager_log = (root / "manager.log").open("x")
        logs.append(manager_log)
        def enter_manager_cgroup():
            (manager_cgroup / "cgroup.procs").write_text(str(os.getpid()))
        manager_child, manager_binary = native_child(["/usr/lib/systemd/systemd", "--user", "--unit=theseus-empty.target"], stdout=manager_log, stderr=subprocess.STDOUT, preexec_fn=enter_manager_cgroup, env=plan["worker_environment"])
        deadline = time.monotonic() + 45
        while True:
            require(manager_child.poll() is None and time.monotonic() < deadline, "original native user manager did not acquire its private bus name")
            try:
                unique = str(daemon.GetNameOwner(MANAGER, timeout=2))
                break
            except dbus.DBusException as error:
                require(error.get_dbus_name() == "org.freedesktop.DBus.Error.NameHasNoOwner", "unexpected private bus failure")
                time.sleep(0.05)
        manager_uid = int(daemon.GetConnectionUnixUser(unique, timeout=5))
        manager_pid = int(daemon.GetConnectionUnixProcessID(unique, timeout=5))
        require(manager_pid == manager_child.pid and manager_child.poll() is None, "manager owner must be original retained Popen")
        obj = connection.get_object(unique, OBJECT, introspect=False)
        properties = dbus.Interface(obj, "org.freedesktop.DBus.Properties")
        version = str(properties.Get(IFACE, "Version", timeout=5))
        unit_paths = [str(p) for p in properties.Get(IFACE, "UnitPath", timeout=5)]
        validate_live_namespace(unit_paths, str(root))
        actual_features = str(properties.Get(IFACE, "Features", timeout=5))
        introspection = str(dbus.Interface(obj, "org.freedesktop.DBus.Introspectable").Introspect(timeout=5))
        for method in ("StartUnit", "StopUnit", "Reload", "GetUnit", "ListUnits"):
            require(f'name="{method}"' in introspection, "native manager API missing: " + method)
        initial_units = [str(row[0]) for row in dbus.Interface(obj, IFACE).ListUnits(timeout=5)]
        require(set(initial_units).issubset({"theseus-empty.target", "init.scope", "-.slice"}), "new whole-private manager loaded an unexpected unit")
        require(not list((root / "units").iterdir()), "unit namespace changed before qualification")
        sdk = ctypes.CDLL("libsystemd.so.0")
        for symbol in SDK_SYMBOLS:
            require(hasattr(sdk, symbol), "required native SDK symbol unavailable: " + symbol)
        sdk_mappings = [line for line in Path("/proc/self/maps").read_text().splitlines() if "libsystemd.so" in line]
        sdk_paths = sorted({line.split()[-1] for line in sdk_mappings})
        require(sdk_paths and all(path.startswith("/") for path in sdk_paths), "actual native SDK mapping required")
        evidence = {
            "version": version, "unit_paths": unit_paths, "bus_peer_uid": peer_uid,
            "bus_peer_gid": peer_gid, "manager_uid": manager_uid, "manager_unique_name": unique,
            "bus_pid": bus_child.pid, "manager_pid": manager_pid, "service_invocation_id": invocation,
            "manager_features": actual_features, "os": platform.uname()._asdict(),
            "initial_units": initial_units, "support_target_sha256": hashlib.sha256(support_path.read_bytes()).hexdigest(),
            "os_release": Path("/etc/os-release").read_text(),
            "native_sdk_mappings": sdk_mappings, "native_sdk_symbols": SDK_SYMBOLS,
            "native_sdk_libraries": [{"path": path, "sha256": hashlib.sha256(Path(path).read_bytes()).hexdigest(), "length": Path(path).stat().st_size} for path in sdk_paths],
            "build": plan["manifest"]["build"], "source_sha": plan["manifest"]["source_sha"],
            "artifact_pins": plan["manifest"]["artifacts"], "github": document["github"],
            "directory_owners": document["directories"], "cgroup": str(base), "manager_cgroup": str(manager_cgroup), "owner_cgroup": str(owner), "delegated_controllers": controllers,
            "no_other_namespace_writers": True,
            "exclusivity_basis": "fresh root-created nonlogin UID; original private 0700 roots; one delegated service; only supplied pinned tests and their original descendants",
            "manager_binary": manager_binary, "dbus_binary": bus_binary,
        }
        write_new(root / "qualification.json", evidence)
        receipt = manager_receipt(plan, uid, evidence)
        write_new(root / "manager-receipt.json", receipt)
        test_env = artifact_environment(plan)
        results = []
        for index, case in enumerate(plan["manifest"]["cases"]):
            require(bus_child.poll() is None and manager_child.poll() is None, "lost original manager/bus during acceptance")
            require(str(daemon.GetNameOwner(MANAGER, timeout=5)) == unique, "original manager bus owner replaced")
            binary = str(root / "artifacts" / case["binary"])
            inventory = subprocess.run([binary, "--list"], env=test_env, capture_output=True, text=True, timeout=30, check=True)
            (root / f"inventory-{index}.log").write_text(inventory.stdout + inventory.stderr)
            command = test_command(binary, case["name"], inventory.stdout.splitlines())
            output_path = root / f"case-{index}.log"
            with output_path.open("x") as output:
                test_child = subprocess.Popen(command, env=test_env, stdout=output, stderr=subprocess.STDOUT)
                try:
                    status = test_child.wait(timeout=case_timeout(case))
                finally:
                    joined = join_original(test_child)
            text = output_path.read_text()
            require(status == 0 and "running 1 test" in text and "1 passed; 0 failed; 0 ignored" in text, "mandatory native case failed or did not execute: " + case["name"])
            results.append({"case": case, "command": command, "status": status, "owner": joined, "log_sha256": hashlib.sha256(output_path.read_bytes()).hexdigest()})
        write_new(root / "result.json", {"kind": "theseus_real_systemd_ci_acceptance", "schema": 1, "qualification_sha256": hashlib.sha256((root / "qualification.json").read_bytes()).hexdigest(), "cases": results})
    finally:
        errors = []
        # Do not allow a second polite cancellation to interrupt original-owner
        # joins. The original system service still has its bounded stop policy.
        for signum in old_handlers:
            signal.signal(signum, signal.SIG_IGN)
        if connection is not None:
            try:
                connection.close()
            except Exception as error:
                errors.append(str(error))
        # Native user manager Exit/Stop calls are unnecessary: terminate only the
        # actual original owned Popen, then let the original service's KillMode
        # settle its delegated subtree. No PID lookup, attach, killpg, or scan.
        for name, child in (("manager", manager_child), ("bus", bus_child)):
            try:
                cleanup[name] = join_original(child)
            except Exception as error:
                errors.append(name + ": " + str(error))
        cleanup["errors"] = errors
        for log in logs:
            log.close()
        write_new(root / "worker-cleanup.json", cleanup)
        for signum, handler in old_handlers.items():
            signal.signal(signum, handler)
        require(not errors, "original native cleanup incomplete; retained worker-cleanup.json")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, required=True, help="source/build/artifact SHA256 pins and exact native named cases")
    parser.add_argument("--plan", action="store_true", help="print a pure plan; never provision or contact managers")
    parser.add_argument("--execute", action="store_true", help="provision only on guarded CI; run precompiled tests and clean original owners")
    parser.add_argument("--disposable-github-runner", action="store_true", help="explicitly attest exclusive disposable GitHub-hosted runner authority")
    parser.add_argument("--worker", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--token", help="explicit fresh 16-hex reservation, shared by plan and execute")
    args = parser.parse_args()
    document = json.loads(args.manifest.read_text())
    if args.worker:
        worker(document)
        return
    require(not (args.plan and args.execute), "--plan and --execute are exclusive")
    if not args.execute:
        print(json.dumps(make_plan(document, args.token if args.token is not None else "0000000000000000"), indent=2))
        return
    execution_guard(os.environ, execute=args.execute, disposable=args.disposable_github_runner, uid=os.geteuid(), system=platform.system())
    plan = make_plan(document, args.token if args.token is not None else os.urandom(8).hex())
    print("retained fixture evidence: " + plan["root"], flush=True)
    operator(plan, os.environ)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print(f"mandatory real systemd CI fixture failed: {error}", file=sys.stderr)
        sys.exit(1)
