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
import re
import shutil
import signal
import socket
import stat
import struct
import subprocess
import sys
import time
import traceback

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
SUPPORT_TARGET = "[Unit]\nDescription=Theseus private fixture anchor\nDefaultDependencies=no\nRequires=dbus.socket dbus.service\nAfter=dbus.service\n"
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


def bus_command(root):
    canonical_path(root)
    return ["/usr/bin/dbus-daemon", "--config-file=" + root + "/dbus.conf",
            "--address=systemd:", "--nofork", "--nopidfile",
            "--systemd-activation", "--nosyslog"]


def support_units(root):
    canonical_path(root)
    # Real upstream dbus socket activation + READY=1, not a marker for an
    # externally spawned daemon. The initial target needs no already-live API.
    return {
        "theseus-empty.target": SUPPORT_TARGET,
        "dbus.socket": "[Unit]\nDescription=Theseus original bus socket\nDefaultDependencies=no\n[Socket]\nListenStream=" + root + "/runtime/bus\nSocketMode=0600\nService=dbus.service\nRemoveOnStop=yes\n",
        "dbus.service": "[Unit]\nDescription=Theseus original manager-owned bus\nDefaultDependencies=no\nRequires=dbus.socket\nAfter=dbus.socket\n[Service]\nType=notify\nNotifyAccess=main\nSockets=dbus.socket\nSlice=-.slice\nExecStart=" + " ".join(bus_command(root)) + "\nRestart=no\nTimeoutStartSec=30s\nTimeoutStopSec=10s\nKillMode=control-group\nStandardOutput=append:" + root + "/dbus.log\nStandardError=inherit\n",
    }


def managed_bus_proof(root, uid, manager_pid, manager_scope, observed, *, expected=None):
    """Validate actual observations under the retained original manager only."""
    canonical_path(root)
    canonical_path(manager_scope)
    service, sock = observed["service"], observed["socket"]
    require(uid > 0 and manager_pid > 0, "original manager identity required")
    require(observed["socket_peer_pid"] == manager_pid and observed["socket_peer_uid"] == uid,
            "socket creator must be original manager, not daemon/name lookup")
    require(observed["driver_uid"] == uid and observed["driver_pid"] == service["main_pid"]
            and service["main_pid"] > 0 and service["main_pid"] != manager_pid,
            "bus driver must be exact original managed daemon")
    for name, unit in (("dbus.service", service), ("dbus.socket", sock)):
        require(unit["id"] == name and unit["active"] == "active" and unit["sub"] == "running",
                "original managed bus unit not running")
        require(re.fullmatch(r"[a-f0-9]{32}", unit["invocation"]) and unit["invocation"] != "0" * 32,
                "original managed unit invocation required")
        require(unit["fragment"] == root + "/support-units/" + name and unit["dropins"] == [],
                "bus unit must be the exact closed support definition")
    require(service["control_group"] == manager_scope + "/dbus.service", "original managed daemon cgroup mismatch")
    require(service["arguments"] == bus_command(root), "original managed daemon argv mismatch")
    require(sock["listen"] == [["Stream", root + "/runtime/bus"]], "original bus listener mismatch")
    proof = {"kind": "original_manager_owned_bus_v1", "manager_pid": manager_pid,
             "socket_creator_pid": observed["socket_peer_pid"], "socket_creator_uid": observed["socket_peer_uid"],
             "daemon_pid": service["main_pid"], "daemon_uid": observed["driver_uid"],
             "service_invocation": service["invocation"], "socket_invocation": sock["invocation"],
             "control_group": service["control_group"], "service_fragment": service["fragment"],
             "socket_fragment": sock["fragment"], "arguments": service["arguments"], "listen": sock["listen"]}
    require(expected is None or proof == expected, "original managed bus identity changed; never adopt replacement")
    return proof


def scope_unpopulated(text):
    lines = text.splitlines()
    return [line for line in lines if line.startswith("populated ")] == ["populated 0"]


def manager_cleanup_complete(owner, empty):
    return bool(owner is not None and type(owner.get("joined_status")) is int and empty is True)


def original_scope_settled(cleanup):
    return bool(isinstance(cleanup, dict) and cleanup.get("kind") == "theseus_original_manager_cleanup_v1"
                and cleanup.get("manager_scope_empty") is True and cleanup.get("errors") == []
                and (cleanup.get("manager_started") is False
                     or (cleanup.get("manager_started") is True
                         and manager_cleanup_complete(cleanup.get("manager"), True))))


# app.slice is synthesized by the user manager itself (CI4 journal: "Created
# slice app.slice") without any unit file in our closed paths.
CLOSED_INITIAL_UNITS = frozenset({"theseus-empty.target", "dbus.socket", "dbus.service", "init.scope", "-.slice", "app.slice"})
# A user manager synthesizes these from kernel mount/swap/udev state, never
# from a unit file; every definition-backed type stays exactly closed.
KERNEL_DERIVED_UNIT_SUFFIXES = (".mount", ".swap", ".device")
WORKER_CLEANUP_LIMIT = 64 * 1024


def initial_units_closed(rows):
    """rows: (name, load_state, active_state) from the manager's ListUnits.

    Definition-backed units stay exactly closed. Kernel-derived units and
    references whose definition was not found (CI4 journal: e.g.
    blockdev@dev-sda16.target, systemd-remount-fs.service) carry no
    definition or process and are admitted only while inactive.
    """
    return all(name in CLOSED_INITIAL_UNITS or name.endswith(KERNEL_DERIVED_UNIT_SUFFIXES)
               or (load == "not-found" and active == "inactive")
               for name, load, active in rows)


def read_worker_cleanup(path):
    """Worker-owned receipt; missing, malformed, special or redirected is uncertainty."""
    try:
        fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))
    except OSError:
        return None
    try:
        with os.fdopen(fd, "rb") as handle:
            if not stat.S_ISREG(os.fstat(handle.fileno()).st_mode):
                return None
            raw = handle.read(WORKER_CLEANUP_LIMIT + 1)
            if len(raw) > WORKER_CLEANUP_LIMIT:
                return None
            document = json.loads(raw)
    except (OSError, ValueError):
        return None
    return document if isinstance(document, dict) else None


def remaining(deadline, limit):
    interval = deadline - time.monotonic()
    require(interval > 0, "original manager operation deadline expired")
    return min(interval, limit)


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
        # Manager logs go to the journal (retained by the operator for this exact
        # unit only); manager.log stayed empty with the console target.
        "SYSTEMD_LOG_TARGET": "journal", "SYSTEMD_LOG_LEVEL": "debug",
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
    # Debug journal logging is manager evidence only; cases keep the previous
    # stderr-visible systemd tool logging.
    environment.update(SYSTEMD_LOG_TARGET="console", SYSTEMD_LOG_LEVEL="info")
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
    require(evidence["bus_pid"] > 0 and evidence["manager_pid"] > 0 and evidence["bus_pid"] != evidence["manager_pid"], "original manager and managed daemon identities required")
    proof, peer = evidence.get("bus_ownership"), evidence.get("private_manager_peer")
    require(isinstance(proof, dict) and proof.get("kind") == "original_manager_owned_bus_v1"
            and proof.get("manager_pid") == evidence["manager_pid"]
            and proof.get("socket_creator_pid") == evidence["manager_pid"]
            and proof.get("socket_creator_uid") == uid
            and proof.get("daemon_pid") == evidence["bus_pid"] and proof.get("daemon_uid") == uid,
            "original manager-owned bus role proof required")
    require(isinstance(peer, dict) and peer.get("pid") == evidence["manager_pid"] and peer.get("uid") == uid,
            "original private manager peer proof required")
    for field in ("service_invocation", "socket_invocation"):
        require(isinstance(proof.get(field), str) and re.fullmatch(r"[a-f0-9]{32}", proof[field])
                and proof[field] != "0" * 32, "original managed bus invocation required")
    require(proof.get("service_fragment") == plan["root"] + "/support-units/dbus.service"
            and proof.get("socket_fragment") == plan["root"] + "/support-units/dbus.socket"
            and proof.get("arguments") == bus_command(plan["root"])
            and proof.get("listen") == [["Stream", plan["root"] + "/runtime/bus"]],
            "exact closed bus definition proof required")
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
    import pwd  # Linux execution only; pure planning is portable.

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
            retain_service_journal(root / "service-journal.log", service_name)
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
        worker_cleanup = read_worker_cleanup(root / "worker-cleanup.json")
        managed_scope_settled = child is None or original_scope_settled(worker_cleanup)
        cleanup["original_managed_scope_settled"] = managed_scope_settled
        cleanup["account_removed"] = False
        if account_created and joined_service and managed_scope_settled:
            try:
                current_uid = pwd.getpwnam(plan["account"]).pw_uid
            except KeyError:
                current_uid = None
            cleanup["account_identity_retained"] = current_uid == uid
            if current_uid == uid:
                result = subprocess.run(["/usr/sbin/userdel", plan["account"]], check=False)
                cleanup["account_removed"] = result.returncode == 0
        write_new(root / "operator-cleanup.json", cleanup)
        os.close(root_fd)
        require(not account_created or cleanup.get("account_identity_retained") is not False, "original account identity lost; refusing account cleanup")
        require(not account_created or cleanup["account_removed"], "original account cleanup incomplete; inspect retained receipt")


def retain_service_journal(path, service_name):
    """Evidence only: read this exact unit's journal; never grants authority."""
    try:
        # --unit also includes PID 1's own UNIT= start/kill/timeout messages.
        observed = subprocess.run(["/usr/bin/journalctl", "--system", "--no-pager", "--output=short-monotonic",
                                   "--unit=" + service_name],
                                  capture_output=True, text=True, env={"PATH": "/usr/bin:/bin"}, timeout=30)
        text = observed.stdout + observed.stderr + f"\n[journalctl exit {observed.returncode}]\n"
    except (OSError, subprocess.SubprocessError) as error:
        text = f"[journal unavailable: {error}]\n"
    try:
        with Path(path).open("x") as handle:
            handle.write(text[-4 * 1024 * 1024:])
    except OSError as error:
        print(f"service journal not retained: {error}", file=sys.stderr)


def exception_chain(error):
    """Primary failure first; a later cleanup error must never hide it."""
    chain, seen = [], set()
    while error is not None and id(error) not in seen:
        seen.add(id(error))
        chain.append(error)
        error = error.__cause__ or error.__context__
    return "; caused after: ".join(str(e) for e in reversed(chain))


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


def pin_native_binary(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        identity = os.fstat(fd)
        require(stat.S_ISREG(identity.st_mode), "regular original native executable required")
        with os.fdopen(os.dup(fd), "rb") as binary:
            digest = hashlib.sha256(binary.read()).hexdigest()
        return fd, {"path": path, "dev": identity.st_dev, "inode": identity.st_ino,
                    "length": identity.st_size, "sha256": digest}
    except BaseException:
        os.close(fd)
        raise


def verify_managed_executable(pid, pin, command, control_group):
    # Observation of the original manager's already-bound MainPID, never a
    # process handle reconstructed for mutation or cleanup.
    executed = Path(f"/proc/{pid}/exe").stat()
    require((executed.st_dev, executed.st_ino, executed.st_size) == (pin["dev"], pin["inode"], pin["length"]),
            "original managed executable identity changed")
    raw = Path(f"/proc/{pid}/cmdline").read_bytes()
    require(raw == b"\0".join(arg.encode() for arg in command) + b"\0", "original managed executable argv changed")
    require(Path(f"/proc/{pid}/cgroup").read_text().splitlines() == ["0::" + control_group],
            "daemon is not in its original manager-owned cgroup")
    with Path(f"/proc/{pid}/exe").open("rb") as binary:
        require(hashlib.sha256(binary.read()).hexdigest() == pin["sha256"], "original managed executable bytes changed")


def connection_peer(connection):
    with socket.fromfd(connection.get_unix_fd(), socket.AF_UNIX, socket.SOCK_STREAM) as peer:
        return struct.unpack("3i", peer.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize("3i")))


def bus_unit_observation(dbus, connection, manager_object, *, bus_name, deadline=None):
    """Read-only facts over the public bus, addressed to the verified unique
    manager name; authority-bearing actions use only the private channel."""
    manager = dbus.Interface(manager_object, IFACE)
    def timeout():
        return 5 if deadline is None else remaining(deadline, 5)
    records = {}
    for name in ("dbus.service", "dbus.socket"):
        path = str(manager.GetUnit(name, timeout=timeout()))
        obj = connection.get_object(bus_name, path, introspect=False)
        properties = dbus.Interface(obj, "org.freedesktop.DBus.Properties")
        unit = properties.GetAll("org.freedesktop.systemd1.Unit", timeout=timeout())
        record = {"id": str(unit["Id"]), "active": str(unit["ActiveState"]), "sub": str(unit["SubState"]),
                  "invocation": bytes(unit["InvocationID"]).hex(), "fragment": str(unit["FragmentPath"]),
                  "dropins": [str(p) for p in unit["DropInPaths"]]}
        if name == "dbus.service":
            service = properties.GetAll("org.freedesktop.systemd1.Service", timeout=timeout())
            starts = service["ExecStart"]
            require(len(starts) == 1, "exact original bus ExecStart required")
            record.update(main_pid=int(service["MainPID"]), control_group=str(service["ControlGroup"]),
                          arguments=[str(a) for a in starts[0][1]])
        else:
            sock = properties.GetAll("org.freedesktop.systemd1.Socket", timeout=timeout())
            record["listen"] = [[str(kind), str(address)] for kind, address in sock["Listen"]]
        records[name.split(".")[1]] = record
    return records


class PrivateBusError(Exception):
    def __init__(self, name, message):
        super().__init__(f"{name}: {message}")
        self.name = name

    def get_dbus_name(self):
        return self.name


class PrivateManager:
    """Retained original peer-to-peer channel to the manager's private socket.

    Uses libsystemd's sd-bus client (as systemctl does for this socket): CI4
    proved a libdbus client authenticates but its calls are never received.
    No Hello, no name lookup; it survives shutdown of the managed public bus.
    """

    class Error(ctypes.Structure):
        _fields_ = [("name", ctypes.c_char_p), ("message", ctypes.c_char_p), ("need_free", ctypes.c_int)]

    def __init__(self, address, sdk=None):
        self.sdk = sdk if sdk is not None else ctypes.CDLL("libsystemd.so.0")
        self.libc = ctypes.CDLL(None)
        for name in ("sd_bus_new", "sd_bus_set_address", "sd_bus_start", "sd_bus_get_fd", "sd_bus_is_ready",
                     "sd_bus_process", "sd_bus_wait",
                     "sd_bus_set_method_call_timeout", "sd_bus_call_method", "sd_bus_get_property",
                     "sd_bus_get_property_string", "sd_bus_message_read", "sd_bus_message_read_array"):
            getattr(self.sdk, name).restype = ctypes.c_int
        self.sdk.sd_bus_set_method_call_timeout.argtypes = [ctypes.c_void_p, ctypes.c_uint64]
        self.sdk.sd_bus_is_ready.argtypes = [ctypes.c_void_p]
        self.sdk.sd_bus_process.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
        self.sdk.sd_bus_wait.argtypes = [ctypes.c_void_p, ctypes.c_uint64]
        self.sdk.sd_bus_message_read_array.argtypes = [ctypes.c_void_p, ctypes.c_char, ctypes.POINTER(ctypes.c_void_p), ctypes.POINTER(ctypes.c_size_t)]
        self.sdk.sd_bus_message_unref.restype = ctypes.c_void_p
        self.sdk.sd_bus_message_unref.argtypes = [ctypes.c_void_p]
        self.sdk.sd_bus_error_free.argtypes = [ctypes.POINTER(PrivateManager.Error)]
        self.sdk.sd_bus_flush_close_unref.restype = ctypes.c_void_p
        self.sdk.sd_bus_flush_close_unref.argtypes = [ctypes.c_void_p]
        self.libc.free.argtypes = [ctypes.c_void_p]
        self.bus = ctypes.c_void_p()
        self.check(self.sdk.sd_bus_new(ctypes.byref(self.bus)), "sd_bus_new")
        try:
            self.check(self.sdk.sd_bus_set_address(self.bus, address.encode()), "sd_bus_set_address")
            self.check(self.sdk.sd_bus_start(self.bus), "sd_bus_start")
        except BaseException:
            self.close()
            raise

    def check(self, result, what, error=None):
        if result >= 0:
            return result
        name = error.name.decode() if error is not None and error.name else "errno." + str(-result)
        message = error.message.decode() if error is not None and error.message else os.strerror(-result)
        raise PrivateBusError(name, what + ": " + message)

    def wait_ready(self, deadline):
        """Bound authentication by our deadline, not sd-bus's own auth timeout."""
        while self.check(self.sdk.sd_bus_is_ready(self.bus), "sd_bus_is_ready") == 0:
            if self.check(self.sdk.sd_bus_process(self.bus, None), "sd_bus_process") == 0:
                self.check(self.sdk.sd_bus_wait(self.bus, max(1, int(remaining(deadline, 1) * 1_000_000))), "sd_bus_wait")
            remaining(deadline, 1)

    def fd(self):
        return self.check(self.sdk.sd_bus_get_fd(self.bus), "sd_bus_get_fd")

    def peer(self):
        with socket.socket(fileno=os.dup(self.fd())) as peer:
            return struct.unpack("3i", peer.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize("3i")))

    def _timeout(self, seconds):
        self.check(self.sdk.sd_bus_set_method_call_timeout(self.bus, max(1, int(seconds * 1_000_000))), "timeout")

    def _call(self, member, signature, args, timeout, read=None):
        self._timeout(timeout)
        error, reply = PrivateManager.Error(), ctypes.c_void_p()
        try:
            self.check(self.sdk.sd_bus_call_method(self.bus, None, OBJECT.encode(), IFACE.encode(), member.encode(),
                                                   ctypes.byref(error), ctypes.byref(reply),
                                                   signature.encode() if signature else None, *args), member, error)
            if read is None:
                return None
            value = ctypes.c_char_p()
            result = self.check(self.sdk.sd_bus_message_read(reply, read.encode(), ctypes.byref(value)), member + " reply")
            if result == 0 or value.value is None:
                raise PrivateBusError("theseus.EmptyReply", member + " reply carried no " + read)
            return value.value.decode()
        finally:
            self.sdk.sd_bus_error_free(ctypes.byref(error))
            if reply:
                self.sdk.sd_bus_message_unref(reply)

    def get_unit(self, name, timeout):
        return self._call("GetUnit", "s", [ctypes.c_char_p(name.encode())], timeout, read="o")

    def stop_unit(self, name, mode, timeout):
        return self._call("StopUnit", "ss", [ctypes.c_char_p(name.encode()), ctypes.c_char_p(mode.encode())], timeout, read="o")

    def exit(self, timeout):
        self._call("Exit", None, [], timeout)

    def unit_state(self, path, timeout):
        """(InvocationID hex, ActiveState) of the unit object, read privately."""
        iface = b"org.freedesktop.systemd1.Unit"
        self._timeout(timeout)
        error, reply = PrivateManager.Error(), ctypes.c_void_p()
        try:
            self.check(self.sdk.sd_bus_get_property(self.bus, None, path.encode(), iface, b"InvocationID",
                                                    ctypes.byref(error), ctypes.byref(reply), b"ay"), "InvocationID", error)
            data, size = ctypes.c_void_p(), ctypes.c_size_t()
            self.check(self.sdk.sd_bus_message_read_array(reply, b"y", ctypes.byref(data), ctypes.byref(size)), "InvocationID array")
            invocation = ctypes.string_at(data, size.value).hex() if size.value else ""
        finally:
            self.sdk.sd_bus_error_free(ctypes.byref(error))
            if reply:
                self.sdk.sd_bus_message_unref(reply)
        self._timeout(timeout)
        error, value = PrivateManager.Error(), ctypes.c_char_p()
        try:
            self.check(self.sdk.sd_bus_get_property_string(self.bus, None, path.encode(), iface, b"ActiveState",
                                                           ctypes.byref(error), ctypes.byref(value)), "ActiveState", error)
            state = (value.value or b"").decode()
        finally:
            self.sdk.sd_bus_error_free(ctypes.byref(error))
            if value:
                self.libc.free(ctypes.cast(value, ctypes.c_void_p))
        return invocation, state

    def close(self):
        if self.bus:
            self.sdk.sd_bus_flush_close_unref(self.bus)
            self.bus = ctypes.c_void_p()


def private_channel_matches(private, observed, deadline):
    """Before any case: the retained private channel answers and names the
    same original manager units and invocations as the public observation."""
    private.wait_ready(deadline)
    for key, name in (("service", "dbus.service"), ("socket", "dbus.socket")):
        invocation, state = private.unit_state(private.get_unit(name, remaining(deadline, 5)), remaining(deadline, 5))
        require(invocation == observed[key]["invocation"] and state == "active",
                "retained private manager channel disagrees with original managed bus")
    return True


def stop_original_bus(observe, private, proof, deadline):
    """Only the same authenticated original private connection can settle it."""
    remaining(deadline, 5)
    units = observe()
    require(units["service"]["invocation"] == proof["service_invocation"]
            and units["service"]["main_pid"] == proof["daemon_pid"]
            and units["service"]["control_group"] == proof["control_group"]
            and units["service"]["fragment"] == proof["service_fragment"]
            and units["service"]["arguments"] == proof["arguments"]
            and units["service"]["dropins"] == []
            and units["socket"]["invocation"] == proof["socket_invocation"]
            and units["socket"]["fragment"] == proof["socket_fragment"]
            and units["socket"]["listen"] == proof["listen"]
            and units["socket"]["dropins"] == [],
            "original managed bus replaced before cleanup; refusing name-based stop")
    private.stop_unit("dbus.socket", "replace", remaining(deadline, 5))
    # Stop of the required socket also stops the service. Public bus disconnect
    # says nothing; observe the original units over our retained private channel.
    while True:
        require(time.monotonic() < deadline, "original managed bus stop deadline")
        settled = True
        for name, invocation in (("dbus.service", proof["service_invocation"]), ("dbus.socket", proof["socket_invocation"])):
            try:
                current, state = private.unit_state(private.get_unit(name, remaining(deadline, 2)), remaining(deadline, 2))
                # A null id is sent as an empty array; never a different id.
                require(current in (invocation, "", "0" * 32), "managed bus invocation changed during stop")
                settled = settled and state in ("inactive", "failed")
            except PrivateBusError as error:
                # Collected after stop (before or between GetUnit and the reads).
                require(error.get_dbus_name() in ("org.freedesktop.systemd1.NoSuchUnit", "org.freedesktop.DBus.Error.UnknownObject"),
                        "lost original private cleanup observation")
        if settled:
            return {"original_service_invocation": proof["service_invocation"], "original_socket_invocation": proof["socket_invocation"], "settled": True}
        time.sleep(0.05)
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
    import pwd  # Linux execution only; pure planning is portable.

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
    manager_child = None
    connection = private_connection = None
    manager_cgroup = None
    scope_fd = events_fd = bus_binary_fd = None
    bus_proof = None
    logs = []
    cleanup = {"kind": "theseus_original_manager_cleanup_v1"}
    old_handlers = {}
    try:
        for signum in (signal.SIGTERM, signal.SIGINT):
            def interrupt(value, frame):
                raise InterruptedError(f"fixture received signal {value}")
            old_handlers[signum] = signal.signal(signum, interrupt)
        base, owner, manager_cgroup, controllers = delegated_cgroups(plan["token"])
        scope_fd = os.open(manager_cgroup, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        events_fd = os.open("cgroup.events", os.O_RDONLY | os.O_NOFOLLOW, dir_fd=scope_fd)
        scope_identity = os.fstat(scope_fd)
        definitions = support_units(str(root))
        for name, contents in definitions.items():
            path = root / "support-units" / name
            with path.open("x") as support:
                support.write(contents)
            path.chmod(0o400)
        address = plan["worker_environment"]["DBUS_SESSION_BUS_ADDRESS"]
        config = '<busconfig><type>session</type><listen>systemd:</listen><auth>EXTERNAL</auth><policy context="default"><allow user="' + str(uid) + '"/><allow own="*"/><allow send_destination="*"/><allow receive_sender="*"/></policy></busconfig>'
        with (root / "dbus.conf").open("x") as config_file:
            config_file.write(config)
        (root / "dbus.conf").chmod(0o400)
        bus_binary_fd, bus_binary = pin_native_binary(bus_command(str(root))[0])
        manager_log = (root / "manager.log").open("x")
        logs.append(manager_log)
        def enter_manager_cgroup():
            (manager_cgroup / "cgroup.procs").write_text(str(os.getpid()))
        manager_child, manager_binary = native_child(["/usr/lib/systemd/systemd", "--user", "--unit=theseus-empty.target"], stdout=manager_log, stderr=subprocess.STDOUT, preexec_fn=enter_manager_cgroup, env=plan["worker_environment"])
        deadline = time.monotonic() + 45
        # Retain a separate original peer-to-peer manager channel. It performs
        # no Hello/name lookup and survives shutdown of the managed public bus.
        while not (root / "runtime/systemd/private").exists():
            require(manager_child.poll() is None and time.monotonic() < deadline, "original manager private API failed to start")
            time.sleep(0.05)
        private_connection = PrivateManager("unix:path=" + str(root / "runtime/systemd/private"))
        private_peer_pid, private_peer_uid, private_peer_gid = private_connection.peer()
        require(private_peer_pid == manager_child.pid and private_peer_uid == uid,
                "private API must be exact original manager peer")
        while not (root / "runtime/bus").exists():
            require(manager_child.poll() is None and time.monotonic() < deadline, "original managed bus socket failed to start")
            time.sleep(0.05)
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as peer:
            peer.settimeout(max(0.001, deadline - time.monotonic()))
            peer.connect(str(root / "runtime/bus"))
            peer_pid, peer_uid, peer_gid = struct.unpack("3i", peer.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize("3i")))
        require(peer_uid == uid and peer_pid == manager_child.pid and manager_child.poll() is None,
                "bus listening socket must be created by original manager")
        connection = dbus.bus.BusConnection(address)
        actual_peer_pid, actual_peer_uid, _ = connection_peer(connection)
        require((actual_peer_pid, actual_peer_uid) == (peer_pid, peer_uid), "actual bus connection peer changed")
        daemon = dbus.Interface(connection.get_object("org.freedesktop.DBus", "/org/freedesktop/DBus", introspect=False), "org.freedesktop.DBus")
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

        def observe_units(deadline=None):
            return bus_unit_observation(dbus, connection, obj, bus_name=unique, deadline=deadline)

        units = observe_units()
        observed = {**units, "socket_peer_pid": peer_pid, "socket_peer_uid": peer_uid,
                    "socket_peer_gid": peer_gid,
                    "driver_pid": int(daemon.GetConnectionUnixProcessID("org.freedesktop.DBus", timeout=5)),
                    "driver_uid": int(daemon.GetConnectionUnixUser("org.freedesktop.DBus", timeout=5))}
        manager_scope = "/" + str(manager_cgroup.relative_to("/sys/fs/cgroup"))
        bus_proof = managed_bus_proof(str(root), uid, manager_child.pid, manager_scope, observed)
        verify_managed_executable(bus_proof["daemon_pid"], bus_binary, bus_command(str(root)), bus_proof["control_group"])
        private_channel_matches(private_connection, units, time.monotonic() + 10)
        properties = dbus.Interface(obj, "org.freedesktop.DBus.Properties")
        version = str(properties.Get(IFACE, "Version", timeout=5))
        unit_paths = [str(p) for p in properties.Get(IFACE, "UnitPath", timeout=5)]
        validate_live_namespace(unit_paths, str(root))
        actual_features = str(properties.Get(IFACE, "Features", timeout=5))
        introspection = str(dbus.Interface(obj, "org.freedesktop.DBus.Introspectable").Introspect(timeout=5))
        for method in ("StartUnit", "StopUnit", "Reload", "GetUnit", "ListUnits"):
            require(f'name="{method}"' in introspection, "native manager API missing: " + method)
        initial_rows = [(str(row[0]), str(row[2]), str(row[3])) for row in dbus.Interface(obj, IFACE).ListUnits(timeout=5)]
        initial_units = [row[0] for row in initial_rows]
        require(initial_units_closed(initial_rows), "new whole-private manager loaded an unexpected unit")
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
            "bus_pid": bus_proof["daemon_pid"], "manager_pid": manager_pid, "service_invocation_id": invocation,
            "bus_ownership": bus_proof, "private_manager_peer": {"pid": private_peer_pid, "uid": private_peer_uid, "gid": private_peer_gid},
            "manager_features": actual_features, "os": platform.uname()._asdict(),
            "initial_units": initial_units, "support_units_sha256": {name: hashlib.sha256((root / "support-units" / name).read_bytes()).hexdigest() for name in definitions},
            "bus_config_sha256": hashlib.sha256((root / "dbus.conf").read_bytes()).hexdigest(),
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
            require(manager_child.poll() is None, "lost original manager during acceptance")
            current = observe_units()
            observed.update(current)
            observed["driver_pid"] = int(daemon.GetConnectionUnixProcessID("org.freedesktop.DBus", timeout=5))
            observed["driver_uid"] = int(daemon.GetConnectionUnixUser("org.freedesktop.DBus", timeout=5))
            managed_bus_proof(str(root), uid, manager_child.pid, manager_scope, observed, expected=bus_proof)
            verify_managed_executable(bus_proof["daemon_pid"], bus_binary, bus_command(str(root)), bus_proof["control_group"])
            for name, contents in definitions.items():
                require((root / "support-units" / name).read_text() == contents, "closed support definition changed")
            require((root / "dbus.conf").read_text() == config, "original bus config changed")
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
    except BaseException:
        cleanup["primary_error"] = traceback.format_exc()[-16384:]
        print("original worker failure:\n" + cleanup["primary_error"], file=sys.stderr, flush=True)
        raise
    finally:
        errors = []
        # Do not allow a second polite cancellation to interrupt original-owner
        # joins. The original system service still has its bounded stop policy.
        for signum in old_handlers:
            signal.signal(signum, signal.SIG_IGN)
        # Stop only the exact already-bound unit instances on the original
        # private connection. Never reconnect after loss or mutate a found PID.
        if bus_proof is not None and private_connection is not None:
            try:
                cleanup_deadline = time.monotonic() + 20
                cleanup["bus"] = stop_original_bus(lambda: observe_units(deadline=cleanup_deadline),
                                                   private_connection, bus_proof, cleanup_deadline)
                private_connection.exit(5)
                manager_child.wait(timeout=5)
            except Exception as error:
                errors.append("original managed bus shutdown: " + str(error))
        for channel in (connection, private_connection):
            if channel is not None:
                try:
                    channel.close()
                except Exception as error:
                    errors.append(str(error))
        try:
            cleanup["manager"] = join_original(manager_child)
        except Exception as error:
            errors.append("manager: " + str(error))
        cleanup["manager_scope_empty"] = manager_child is None and events_fd is None
        if events_fd is not None:
            try:
                # This is the held original manager subtree, not the sibling
                # worker's containing delegated scope or a path reconstructed
                # after exit. Missing/unreadable is uncertainty, never empty.
                current_identity = os.fstat(scope_fd)
                require((current_identity.st_dev, current_identity.st_ino) == (scope_identity.st_dev, scope_identity.st_ino), "original manager scope identity lost")
                cleanup["manager_scope_identity"] = {"path": str(manager_cgroup), "dev": scope_identity.st_dev, "inode": scope_identity.st_ino}
                text = os.pread(events_fd, 4096, 0).decode("ascii")
                cleanup["manager_scope_events"] = text
                cleanup["manager_scope_empty"] = scope_unpopulated(text)
            except Exception as error:
                errors.append("original manager subtree: " + str(error))
        if manager_child is not None and not manager_cleanup_complete(cleanup.get("manager"), cleanup["manager_scope_empty"]):
            errors.append("original manager subtree remains unknown or populated")
        cleanup["errors"] = errors
        cleanup["manager_started"] = manager_child is not None
        for fd in (events_fd, scope_fd, bus_binary_fd):
            if fd is not None:
                os.close(fd)
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
        print(f"mandatory real systemd CI fixture failed: {exception_chain(error)}", file=sys.stderr)
        sys.exit(1)
