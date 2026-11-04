#!/usr/bin/python3
"""Explicit LOCAL TEST MODEL, never an actual systemd qualification.

Owns its private dbus-daemon and the direct child Popen handles started from the
limited fixture unit grammar. It never discovers an endpoint, contacts a user
manager, provisions an account, reattaches by PID, or executes through a shell.
See native_systemd_model_test.py and the private fixture model report.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import signal
import stat
import subprocess
import sys
import time
import xml.sax.saxutils

import dbus
import dbus.service
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

MANAGER = "org.freedesktop.systemd1.Manager"
UNIT = "org.freedesktop.systemd1.Unit"
SERVICE = "org.freedesktop.systemd1.Service"
PROPERTIES = "org.freedesktop.DBus.Properties"
NAME = "org.freedesktop.systemd1"
MANAGER_PATH = "/org/freedesktop/systemd1"
VERSION = "theseus-model-systemd-dbus-private-v1"
MAX_UNITS = 64
MAX_JOBS = 128


class ModelError(dbus.DBusException):
    _dbus_error_name = "org.freedesktop.systemd1.ModelRefused"


class NoSuchUnit(dbus.DBusException):
    _dbus_error_name = "org.freedesktop.systemd1.NoSuchUnit"


def unit_name(value):
    value = str(value)
    if not re.fullmatch(r"[A-Za-z0-9_.-]{1,119}\.service", value):
        raise ModelError("outside the model's exact service-name grammar")
    return value


def private_directory(path):
    if not path.is_absolute() or path != path.resolve():
        raise ValueError("explicit canonical absolute directory required")
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o077:
        raise ValueError("directory must be private, current-UID-owned and nonsymlink")


def parse_definition(raw):
    if len(raw) > 65536 or b"\0" in raw:
        raise ModelError("definition exceeds the supported bound")
    try:
        text = raw.decode("utf-8")
        section, entries, env = "", {}, []
        for raw_line in text.splitlines():
            line = raw_line.strip()
            if not line or line.startswith(("#", ";")):
                continue
            if line in ("[Unit]", "[Service]"):
                section = line
                continue
            if line.startswith("[") or line.endswith("\\") or "=" not in line:
                raise ValueError("unsupported section/directive")
            key, value = (part.strip() for part in line.split("=", 1))
            allowed = {"[Unit]": {"Description", "DefaultDependencies"},
                       "[Service]": {"Type", "Restart", "ExecStart", "WorkingDirectory", "Environment"}}
            if key not in allowed.get(section, set()) or (key in entries and key != "Environment"):
                raise ValueError("unsupported or duplicate directive")
            if key == "Environment":
                env.extend(shlex.split(value))
            else:
                entries[key] = value
        if entries.get("DefaultDependencies", "no") != "no" or entries.get("Type", "simple") != "simple" or entries.get("Restart", "no") != "no":
            raise ValueError("only no-dependency simple/no-restart fixture units are supported")
        argv = shlex.split(entries.get("ExecStart", ""))
        cwd = shlex.split(entries.get("WorkingDirectory", ""))
        if not argv or not argv[0].startswith("/") or len(argv) > 64 or len(cwd) != 1 or not cwd[0].startswith("/"):
            raise ValueError("exact absolute command and working directory required")
        if "$" in entries["ExecStart"] or "%" in text:
            raise ValueError("expansion/specifiers are unsupported")
        names = set()
        for assignment in env:
            name, separator, _ = assignment.partition("=")
            if not separator or not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", name) or name in names or name == "INVOCATION_ID":
                raise ValueError("invalid, duplicate or reserved environment entry")
            names.add(name)
        if len(env) > 64:
            raise ValueError("environment exceeds supported bound")
        return argv, cwd[0], env
    except (ValueError, UnicodeError) as error:
        raise ModelError(str(error)) from error


class UnitObject(dbus.service.Object):
    def __init__(self, manager, name, raw):
        self.manager, self.name = manager, name
        self.raw = raw
        self.argv, self.cwd, self.environment = parse_definition(raw)
        self.child = None
        self.children = []  # Retain every original Popen owner through teardown.
        self.invocation = bytes(16)
        self.result = "success"
        self.stopping = False
        self.path = MANAGER_PATH + "/unit/" + "".join(
            character if character.isascii() and character.isalnum() else f"_{ord(character):02x}"
            for character in name
        )
        super().__init__(manager.bus_connection, self.path)

    def running(self):
        return self.child is not None and self.child.poll() is None

    def properties(self, interface):
        active = self.running()
        if interface == UNIT:
            return {
                "Id": dbus.String(self.name, variant_level=1),
                "Names": dbus.Array([self.name], signature="s", variant_level=1),
                "LoadState": dbus.String("loaded", variant_level=1),
                "ActiveState": dbus.String("deactivating" if self.stopping else "active" if active else "inactive", variant_level=1),
                "SubState": dbus.String("stop-sigterm" if self.stopping else "running" if active else "dead", variant_level=1),
                "FragmentPath": dbus.String(str(self.manager.unit_root / self.name), variant_level=1),
                "DropInPaths": dbus.Array([], signature="s", variant_level=1),
                "InvocationID": dbus.ByteArray(self.invocation, variant_level=1),
                "NeedDaemonReload": dbus.Boolean(self.manager.definition(self.name) != self.raw, variant_level=1),
            }
        if interface == SERVICE:
            command = dbus.Struct((
                dbus.String(self.argv[0]), dbus.Array(self.argv, signature="s"), dbus.Boolean(False),
                dbus.UInt64(0), dbus.UInt64(0), dbus.UInt64(0), dbus.UInt64(0),
                dbus.UInt32(self.child.pid if active else 0), dbus.Int32(0), dbus.Int32(0),
            ), signature="sasbttttuii")
            return {
                "Type": dbus.String("simple", variant_level=1),
                "Restart": dbus.String("no", variant_level=1),
                "ExecStart": dbus.Array([command], signature="(sasbttttuii)", variant_level=1),
                "Environment": dbus.Array(self.environment, signature="s", variant_level=1),
                "WorkingDirectory": dbus.String(self.cwd, variant_level=1),
                "MainPID": dbus.UInt32(self.child.pid if active else 0, variant_level=1),
                "Result": dbus.String(self.result, variant_level=1),
            }
        raise ModelError("unsupported unit interface")

    @dbus.service.method(PROPERTIES, in_signature="ss", out_signature="v")
    def Get(self, interface, name):
        values = self.properties(str(interface))
        if str(name) not in values:
            raise ModelError("unsupported unit property")
        return values[str(name)]

    @dbus.service.method(PROPERTIES, in_signature="s", out_signature="a{sv}")
    def GetAll(self, interface):
        return self.properties(str(interface))


class ManagerObject(dbus.service.Object):
    def __init__(self, connection, unit_root, loop):
        self.bus_connection, self.unit_root, self.loop = connection, unit_root, loop
        self.root_fd = os.open(unit_root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        self.units = {}
        self.retired = []
        self.jobs = 0
        self.log_bytes = 0
        self.bus_name = dbus.service.BusName(NAME, connection, do_not_queue=True)
        super().__init__(connection, MANAGER_PATH)

    def log(self, event, **fields):
        line = json.dumps({"model_only": True, "event": event, **fields})
        limit = 4096 if event == "original_children_joined" else 3900
        if self.log_bytes + len(line) + 1 <= limit:
            print(line, flush=True)
            self.log_bytes += len(line) + 1

    def definition(self, name):
        unit_name(name)
        try:
            descriptor = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=self.root_fd)
        except FileNotFoundError:
            return None
        except OSError as error:
            raise ModelError("definition leaf is not an owned ordinary file") from error
        with os.fdopen(descriptor, "rb") as file:
            before = os.fstat(file.fileno())
            if not stat.S_ISREG(before.st_mode) or before.st_uid != os.geteuid() or before.st_size > 65536:
                raise ModelError("definition leaf is not a bounded current-UID ordinary file")
            data = file.read(65537)
            after = os.fstat(file.fileno())
            if (before.st_ino, before.st_size, before.st_mtime_ns) != (after.st_ino, after.st_size, after.st_mtime_ns) or len(data) > 65536:
                raise ModelError("definition changed during model read")
            parse_definition(data)
            return data

    def properties(self, interface):
        if interface != MANAGER:
            raise ModelError("unsupported manager interface")
        return {"Version": dbus.String(VERSION, variant_level=1),
                "UnitPath": dbus.Array([str(self.unit_root)], signature="s", variant_level=1)}

    @dbus.service.method(PROPERTIES, in_signature="ss", out_signature="v")
    def Get(self, interface, name):
        values = self.properties(str(interface))
        if str(name) not in values:
            raise ModelError("unsupported manager property")
        return values[str(name)]

    @dbus.service.method(PROPERTIES, in_signature="s", out_signature="a{sv}")
    def GetAll(self, interface):
        return self.properties(str(interface))

    @dbus.service.method(MANAGER, in_signature="", out_signature="")
    def Subscribe(self):
        pass

    @dbus.service.method(MANAGER, in_signature="", out_signature="")
    def Unsubscribe(self):
        pass

    @dbus.service.method(MANAGER, in_signature="s", out_signature="o")
    def GetUnit(self, name):
        name = unit_name(name)
        if name not in self.units:
            raise NoSuchUnit("model has no loaded exact unit")
        return dbus.ObjectPath(self.units[name].path)

    @dbus.service.method(MANAGER, in_signature="s", out_signature="o")
    def LoadUnit(self, name):
        name = unit_name(name)
        if name not in self.units:
            raw = self.definition(name)
            if raw is None:
                raise NoSuchUnit("model definition absent")
            if len(self.units) + len(self.retired) >= MAX_UNITS:
                raise ModelError("model unit bound reached")
            self.units[name] = UnitObject(self, name, raw)
            self.log("load", unit=name)
        return dbus.ObjectPath(self.units[name].path)

    @dbus.service.method(MANAGER, in_signature="asas", out_signature="a(ss)")
    def ListUnitFilesByPatterns(self, states, patterns):
        if states or len(patterns) != 1:
            raise ModelError("model requires one exact name and no state filter")
        name = unit_name(patterns[0])
        if self.definition(name) is None:
            return dbus.Array([], signature="(ss)")
        return dbus.Array([(str(self.unit_root / name), "static")], signature="(ss)")

    @dbus.service.signal(MANAGER, signature="b")
    def Reloading(self, reloading):
        pass

    @dbus.service.signal(MANAGER, signature="uoss")
    def JobRemoved(self, job_id, job_path, unit, result):
        pass

    @dbus.service.method(MANAGER, in_signature="", out_signature="")
    def Reload(self):
        updates = [(unit, self.definition(name)) for name, unit in self.units.items()]
        self.Reloading(True)
        for unit, raw in updates:
            if raw is None:
                if not unit.running():
                    self.units.pop(unit.name)
                    unit.remove_from_connection()
                    self.retired.append(unit)
            else:
                unit.argv, unit.cwd, unit.environment = parse_definition(raw)
                unit.raw = raw
        self.Reloading(False)
        self.log("reload")

    def job(self, name, mode):
        if mode != "fail":
            raise ModelError("only exact fail job mode is modeled")
        self.LoadUnit(name)
        if self.jobs >= MAX_JOBS:
            raise ModelError("model job bound reached")
        self.jobs += 1
        return self.units[str(name)], self.jobs, dbus.ObjectPath(f"{MANAGER_PATH}/job/{self.jobs}")

    def complete(self, identifier, path, unit, result):
        self.JobRemoved(dbus.UInt32(identifier), path, unit.name, result)
        self.log("job_removed", job_id=identifier, job=str(path), unit=unit.name, result=result)
        return False

    @dbus.service.method(MANAGER, in_signature="ss", out_signature="o")
    def StartUnit(self, name, mode):
        unit, identifier, path = self.job(name, mode)
        result = "done"
        if not unit.running():
            unit.invocation = hashlib.sha256(f"theseus-model:{identifier}:{name}".encode()).digest()[:16]
            environment = dict(entry.split("=", 1) for entry in unit.environment)
            environment["INVOCATION_ID"] = unit.invocation.hex()
            try:
                child = subprocess.Popen(unit.argv, cwd=unit.cwd, env=environment,
                                         stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                         close_fds=True, shell=False)
                unit.child = child
                unit.children.append(child)
                unit.result = "success"
            except OSError:
                unit.result, result = "resources", "failed"
        self.log("start", unit=unit.name, invocation_id=unit.invocation.hex(), result=result)
        GLib.idle_add(self.complete, identifier, path, unit, result)
        return path

    @dbus.service.method(MANAGER, in_signature="ss", out_signature="o")
    def StopUnit(self, name, mode):
        unit, identifier, path = self.job(name, mode)
        if unit.stopping:
            raise ModelError("original model stop remains pending")
        if not unit.running():
            GLib.idle_add(self.complete, identifier, path, unit, "done")
            return path
        unit.stopping = True
        unit.child.terminate()  # Original retained direct child owner, never a reported PID.
        deadline = time.monotonic() + 3

        def poll_original_child():
            if unit.child.poll() is not None:
                unit.child.wait()
                unit.stopping = False
                self.complete(identifier, path, unit, "done")
                return False
            now = time.monotonic()
            if now >= deadline + 3:
                self.complete(identifier, path, unit, "timeout")
                return False
            if now >= deadline:
                # Escalate only this original retained child; keep owner until join.
                unit.child.kill()
            return True

        GLib.timeout_add(10, poll_original_child)
        return path

    def cleanup(self):
        for unit in [*self.units.values(), *self.retired]:
            for child in unit.children:
                if child.poll() is None:
                    child.terminate()
                try:
                    child.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait(timeout=3)
        os.close(self.root_fd)
        self.log("original_children_joined")


def write_receipt(path, address_path, unit_root, manager):
    namespace = "theseus-model-private-namespace:" + hashlib.sha256(
        f"{VERSION}\0{address_path}\0{unit_root}\0{manager.bus_connection.get_unique_name()}".encode()
    ).hexdigest()
    digest = hashlib.sha256(namespace.encode()).hexdigest()
    receipt = {
        "kind": "theseus_model_manager_receipt", "schema": 1, "manager": "systemd_user",
        "authority": {
            "kind": "systemd", "slot": "service", "scope": "user",
            "bus_endpoint": {"kind": "unix_path", "path": str(address_path)},
            "qualification": {"manager": "systemd", "version": VERSION,
                              "api_profile": "systemd_dbus_private_v1",
                              "features": ["unit_properties", "start_stop", "manager_reload"],
                              "receipt_sha256": digest},
            "namespace": {"kind": "exclusive_owned_registration", "slot": "service",
                          "unit_load_roots": [str(unit_root)], "dropins": "forbidden", "provider_context": namespace},
            "manager_context": {"kind": "whole_private", "slot": "service", "sut_allowed_actions": ["daemon_reload"],
                                "rehearsal_allowed_actions": [], "provider_receipt_required": True},
            "expected_peer_uid": os.geteuid(), "expected_manager_uid": os.geteuid(),
            "definition_root": str(unit_root), "unit": "theseus-model.service", "managed_by": "installer",
            "sut_effects": ["registration", "configuration", "removal", "daemon_reload"],
            "allowed_actions": ["inspect", "start", "stop"],
        },
        "manager_unique_name": manager.bus_connection.get_unique_name(), "namespace_receipt": namespace,
    }
    temporary = path.with_name(path.name + ".pending")
    with temporary.open("x", encoding="utf-8") as file:
        json.dump(receipt, file, indent=2)
        file.flush()
        os.fsync(file.fileno())
    # Link publication refuses a preexisting receipt instead of overwriting it.
    os.link(temporary, path)
    temporary.unlink()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--address", required=True, help="exact unix:path=/private/socket, no fallback grammar")
    parser.add_argument("--unit-root", type=Path, required=True)
    parser.add_argument("--ready-receipt", type=Path, required=True)
    args = parser.parse_args()
    if not re.fullmatch(r"unix:path=/[A-Za-z0-9_./-]+", args.address):
        parser.error("one explicit closed unix:path address required")
    socket = Path(args.address[len("unix:path="):])
    if len(str(socket)) > 100 or socket != socket.resolve() or any(part in (".", "..") for part in str(socket).split("/")):
        parser.error("invalid closed UnixPath")
    private_directory(socket.parent)
    private_directory(args.unit_root)
    private_directory(args.ready_receipt.parent)
    if not args.ready_receipt.is_absolute() or args.ready_receipt != args.ready_receipt.resolve():
        parser.error("explicit canonical receipt required")
    for path in [socket, args.ready_receipt]:
        if os.path.lexists(path):
            parser.error("refusing to replace an existing socket/receipt")
    os.umask(0o077)
    config_path = socket.with_name(socket.name + ".model-dbus.conf")
    escaped = xml.sax.saxutils.escape(args.address)
    config = (f'<busconfig><type>session</type><listen>{escaped}</listen><auth>EXTERNAL</auth>'
              f'<policy context="default"><allow user="{os.geteuid()}"/><allow own="*"/>'
              '<allow send_destination="*"/><allow receive_sender="*"/></policy></busconfig>')
    bus_child, connection, manager = None, None, None
    loop = GLib.MainLoop()
    for signum in (signal.SIGTERM, signal.SIGINT):
        GLib.unix_signal_add(GLib.PRIORITY_DEFAULT, signum, lambda: (loop.quit(), False)[1])
    try:
        with config_path.open("x", encoding="utf-8") as file:
            file.write(config)
        bus_child = subprocess.Popen(["/usr/bin/dbus-daemon", "--nofork", "--nopidfile", f"--config-file={config_path}"],
                                     env={}, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=sys.stderr)
        deadline = time.monotonic() + 5
        while not socket.exists():
            if bus_child.poll() is not None or time.monotonic() >= deadline:
                raise RuntimeError("owned private model bus failed startup")
            time.sleep(.01)
        DBusGMainLoop(set_as_default=True)
        connection = dbus.bus.BusConnection(args.address)
        manager = ManagerObject(connection, args.unit_root, loop)
        write_receipt(args.ready_receipt, socket, args.unit_root, manager)
        manager.log("ready", version=VERSION, address=args.address, receipt=str(args.ready_receipt))
        # Absolute bounded model lifetime; callers retain and stop this child.
        GLib.timeout_add_seconds(300, lambda: (loop.quit(), False)[1])
        loop.run()
    finally:
        cleanup_error = None
        if manager is not None:
            try:
                manager.cleanup()
            except Exception as error:
                cleanup_error = error
        if connection is not None:
            connection.close()
        if bus_child is not None:
            if bus_child.poll() is None:
                bus_child.terminate()
            try:
                bus_child.wait(timeout=3)
            except subprocess.TimeoutExpired:
                bus_child.kill()
                bus_child.wait(timeout=3)
        config_path.unlink(missing_ok=True)
        if cleanup_error is not None:
            raise cleanup_error
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as error:
        print(f"LOCAL MODEL FAILED: {error}", file=sys.stderr)
        sys.exit(1)
