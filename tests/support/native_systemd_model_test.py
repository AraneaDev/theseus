"""Protocol model tests; these do not qualify an actual systemd manager."""
import json
from pathlib import Path
import subprocess
import tempfile
import time
import unittest

import dbus
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

DBusGMainLoop(set_as_default=True)

MANAGER = "org.freedesktop.systemd1.Manager"
UNIT = "org.freedesktop.systemd1.Unit"
SERVICE = "org.freedesktop.systemd1.Service"


class ModelProtocolTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="th-model-", dir="/tmp")
        self.root = Path(self.directory.name)
        self.units = self.root / "units"
        self.units.mkdir(mode=0o700)
        self.receipt = self.root / "receipt.json"
        self.process = subprocess.Popen([
            "/usr/bin/python3", str(Path(__file__).with_name("native_systemd_model.py")),
            "--address", f"unix:path={self.root / 'bus'}", "--unit-root", str(self.units),
            "--ready-receipt", str(self.receipt),
        ], env={}, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        deadline = time.monotonic() + 5
        while not self.receipt.exists() and self.process.poll() is None and time.monotonic() < deadline:
            time.sleep(.02)
        if not self.receipt.exists():
            self.process.terminate()
            out, err = self.process.communicate(timeout=5)
            self.directory.cleanup()
            self.fail(f"model did not become ready: {out!r} {err!r}")
        self.qualification = json.loads(self.receipt.read_text())
        self.bus = dbus.bus.BusConnection(f"unix:path={self.root / 'bus'}")
        self.manager = self.bus.get_object("org.freedesktop.systemd1", "/org/freedesktop/systemd1")

    def tearDown(self):
        self.bus.close()
        self.process.terminate()
        out, err = self.process.communicate(timeout=6)
        self.assertEqual(self.process.returncode, 0, (out, err))
        self.directory.cleanup()

    def call(self, member, *args):
        return self.manager.get_dbus_method(member, MANAGER)(*args, timeout=3)

    def get(self, obj, interface, key):
        return obj.get_dbus_method("Get", "org.freedesktop.DBus.Properties")(interface, key)

    def await_events(self, events, count):
        deadline = time.monotonic() + 3
        context = GLib.MainContext.default()
        while len(events) < count and time.monotonic() < deadline:
            while context.pending():
                context.iteration(False)
            time.sleep(.01)
        self.assertEqual(len(events), count, events)

    def test_exact_reload_and_job_removed_signal_protocol(self):
        reloading, jobs = [], []
        self.bus.add_signal_receiver(lambda value: reloading.append(bool(value)),
                                     signal_name="Reloading", dbus_interface=MANAGER,
                                     bus_name=self.qualification["manager_unique_name"])
        self.bus.add_signal_receiver(lambda *values: jobs.append(values),
                                     signal_name="JobRemoved", dbus_interface=MANAGER,
                                     bus_name=self.qualification["manager_unique_name"])
        self.call("Subscribe")
        self.define()
        self.call("Reload")
        self.await_events(reloading, 2)
        self.assertEqual(reloading, [True, False])
        self.call("LoadUnit", "fixture.service")
        start = self.call("StartUnit", "fixture.service", "fail")
        self.await_events(jobs, 1)
        stop = self.call("StopUnit", "fixture.service", "fail")
        self.await_events(jobs, 2)
        for event, expected_path in zip(jobs, [start, stop]):
            identifier, path, name, result = event
            self.assertIsInstance(identifier, dbus.UInt32)
            self.assertIsInstance(path, dbus.ObjectPath)
            self.assertEqual(str(path), str(expected_path))
            self.assertEqual(int(identifier), int(str(path).rsplit("/", 1)[1]))
            self.assertEqual(str(name), "fixture.service")
            self.assertEqual(str(result), "done")

    def test_supervisor_joins_original_running_child_and_bus(self):
        self.define()
        path = self.call("LoadUnit", "fixture.service")
        obj = self.bus.get_object("org.freedesktop.systemd1", path)
        self.call("StartUnit", "fixture.service", "fail")
        observed_pid = int(self.get(obj, SERVICE, "MainPID"))
        self.assertGreater(observed_pid, 0)
        # pidfd is a read-only test observation; no signaling or reattachment.
        descriptor = __import__("os").pidfd_open(observed_pid)
        try:
            self.process.terminate()
            out, err = self.process.communicate(timeout=6)
            self.assertEqual(self.process.returncode, 0, (out, err))
            self.assertIn(b'"event": "original_children_joined"', out)
            import select
            poller = select.poll()
            poller.register(descriptor, select.POLLIN)
            self.assertTrue(poller.poll(100), "original modeled service remains alive")
            with self.assertRaises(dbus.DBusException):
                dbus.bus.BusConnection(f"unix:path={self.root / 'bus'}")
        finally:
            __import__("os").close(descriptor)

    def define(self):
        output = str(self.root / "invocation")
        command = f'/usr/bin/python3 -c "import os,time;open(\\\"{output}\\\",\\\"w\\\").write(os.environ[\\\"INVOCATION_ID\\\"]);time.sleep(60)"'
        (self.units / "fixture.service").write_text(
            "[Unit]\nDescription=Model test\nDefaultDependencies=no\n[Service]\n"
            f"Type=simple\nRestart=no\nExecStart={command}\nWorkingDirectory={self.root}\n"
        )

    def test_loaded_configuration_and_owned_child_invocation(self):
        self.assertEqual(self.qualification["kind"], "theseus_model_manager_receipt")
        self.assertTrue(self.qualification["authority"]["qualification"]["version"].startswith("theseus-model-"))
        self.assertEqual(self.get(self.manager, MANAGER, "UnitPath"), [str(self.units)])
        with self.assertRaises(dbus.DBusException):
            self.call("GetUnit", "fixture.service")
        self.define()
        self.call("Reload")
        unit_path = self.call("LoadUnit", "fixture.service")
        obj = self.bus.get_object("org.freedesktop.systemd1", unit_path)
        self.assertEqual(str(self.get(obj, UNIT, "ActiveState")), "inactive")
        self.assertEqual(str(self.get(obj, UNIT, "FragmentPath")), str(self.units / "fixture.service"))
        self.assertEqual(str(self.get(obj, SERVICE, "ExecStart")[0][0]), "/usr/bin/python3")
        self.call("StartUnit", "fixture.service", "fail")
        deadline = time.monotonic() + 3
        while not (self.root / "invocation").exists() and time.monotonic() < deadline:
            time.sleep(.01)
        invocation = bytes(self.get(obj, UNIT, "InvocationID")).hex()
        self.assertEqual((self.root / "invocation").read_text(), invocation)
        self.assertEqual(str(self.get(obj, UNIT, "ActiveState")), "active")
        self.assertGreater(int(self.get(obj, SERVICE, "MainPID")), 0)
        self.call("StopUnit", "fixture.service", "fail")
        deadline = time.monotonic() + 3
        while str(self.get(obj, UNIT, "ActiveState")) != "inactive" and time.monotonic() < deadline:
            time.sleep(.01)
        self.assertEqual(int(self.get(obj, SERVICE, "MainPID")), 0)
        (self.units / "fixture.service").unlink()
        self.call("Reload")
        with self.assertRaises(dbus.DBusException) as error:
            self.call("GetUnit", "fixture.service")
        self.assertEqual(error.exception.get_dbus_name(), "org.freedesktop.systemd1.NoSuchUnit")

    def test_changed_definition_requires_reload_and_unsafe_names_fail(self):
        self.define()
        path = self.call("LoadUnit", "fixture.service")
        obj = self.bus.get_object("org.freedesktop.systemd1", path)
        definition = self.units / "fixture.service"
        definition.write_text(definition.read_text().replace("Description=Model test", "Description=Changed"))
        self.assertTrue(self.get(obj, UNIT, "NeedDaemonReload"))
        self.call("Reload")
        self.assertFalse(self.get(obj, UNIT, "NeedDaemonReload"))
        for name in ["../escape.service", "wild*.service", "wrong.target"]:
            with self.assertRaises(dbus.DBusException):
                self.call("LoadUnit", name)
        definition.unlink()
        definition.symlink_to("/etc/passwd")
        with self.assertRaises(dbus.DBusException):
            self.call("Reload")


if __name__ == "__main__":
    unittest.main()
