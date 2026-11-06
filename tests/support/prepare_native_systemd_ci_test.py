"""Pure qualification tests; never provision or contact any native manager."""
import copy
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch
from types import SimpleNamespace

SPEC = importlib.util.spec_from_file_location("prepare_native_systemd_ci", Path(__file__).with_name("prepare_native_systemd_ci.py"))
fixture = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(fixture)


def manifest():
    names = ["THESEUS_NATIVE_HELPER_BINARY", "THESEUS_NATIVE_SERVICE_FIXTURE", "THESEUS_NATIVE_SERVICE_BASE", "THESEUS_NATIVE_SERVICE_TARGET", "THESEUS_NATIVE_INTEGRATION_CONTROLLER", "THESEUS_NATIVE_DESKTOP_HELPER_BINARY", "THESEUS_NATIVE_DESKTOP_BROKER_BINARY", "native_services", "native_service_mcp", "native_desktop"]
    return {
        "schema": 1, "source_sha": "a" * 40,
        "build": {"source_sha": "a" * 40, "target": "x86_64-unknown-linux-gnu", "profile": "release", "features": ["native-integration-test-support"]},
        "artifacts": {name: {"path": f"/build/{name}", "sha256": "b" * 64, "length": 123, "source_sha": "a" * 40} for name in names},
        "cases": [{"binary": binary, "name": name} for binary, name in [
            ("native_services", "systemd_user_clean_installer_managed_service_uses_shared_product_path"),
            ("native_services", "systemd_installer_missing_service_is_failed_without_harness_repair"),
            ("native_services", "systemd_uninstaller_leaves_service_is_failed_before_and_after_provider_disposal"),
            ("native_services", "systemd_preexisting_or_wrong_owned_unit_is_untouched"),
            ("native_service_mcp", "systemd_supplied_cli_and_fresh_mcp_normalized_frontend_parity"),
            ("native_desktop", "source_combined_service_desktop_share_one_sequence_provider_and_lease"),
            ("native_desktop", "source_combined_ordinary_fresh_six_tool_mcp_keeps_one_shared_lease_and_saved_views"),
        ]],
    }


class Planning(unittest.TestCase):
    def test_initial_target_bootstraps_real_notify_bus_without_api_calls(self):
        root = "/tmp/th-sd-abcd1234abcd1234"
        units = fixture.support_units(root)
        self.assertEqual(set(units), {"theseus-empty.target", "dbus.socket", "dbus.service"})
        self.assertIn("Requires=dbus.socket dbus.service\n", units["theseus-empty.target"])
        self.assertIn("After=dbus.service\n", units["theseus-empty.target"])
        self.assertIn("ListenStream=" + root + "/runtime/bus\n", units["dbus.socket"])
        self.assertIn("SocketMode=0600\n", units["dbus.socket"])
        service = units["dbus.service"]
        for line in ["Type=notify", "NotifyAccess=main", "Sockets=dbus.socket", "Slice=-.slice", "Restart=no", "TimeoutStartSec=30s", "TimeoutStopSec=10s"]:
            self.assertIn(line + "\n", service)
        self.assertEqual(fixture.bus_command(root), ["/usr/bin/dbus-daemon", "--config-file=" + root + "/dbus.conf", "--address=systemd:", "--nofork", "--nopidfile", "--systemd-activation", "--nosyslog"])
        self.assertIn("ExecStart=" + " ".join(fixture.bus_command(root)) + "\n", service)
        for text in units.values():
            self.assertNotIn("RemainAfterExit", text)
            self.assertNotIn("/usr/lib/systemd/user", text)
        with self.assertRaises(ValueError):
            fixture.support_units("/tmp/private/../escape")

    def owned_bus_observation(self):
        root = "/tmp/th-sd-abcd1234abcd1234"
        scope = "/system.slice/theseus-private-abcd1234abcd1234.service/theseus-private-manager"
        return root, scope, {
            "socket_peer_pid": 18, "socket_peer_uid": 12345, "socket_peer_gid": 12345,
            "driver_pid": 19, "driver_uid": 12345,
            "service": {"id": "dbus.service", "active": "active", "sub": "running", "invocation": "a" * 32, "fragment": root + "/support-units/dbus.service", "dropins": [], "main_pid": 19, "control_group": scope + "/dbus.service", "arguments": fixture.bus_command(root)},
            "socket": {"id": "dbus.socket", "active": "active", "sub": "running", "invocation": "b" * 32, "fragment": root + "/support-units/dbus.socket", "dropins": [], "listen": [["Stream", root + "/runtime/bus"]]},
        }

    def test_managed_bus_roles_and_original_invocations_are_exact_and_sticky(self):
        root, scope, observed = self.owned_bus_observation()
        proof = fixture.managed_bus_proof(root, 12345, 18, scope, observed)
        self.assertEqual(proof["kind"], "original_manager_owned_bus_v1")
        self.assertEqual(proof["socket_creator_pid"], 18)
        self.assertEqual(proof["daemon_pid"], 19)
        self.assertEqual(proof, fixture.managed_bus_proof(root, 12345, 18, scope, observed, expected=proof))
        mutations = [("socket_peer_pid", 19), ("socket_peer_uid", 0), ("driver_pid", 20), ("driver_uid", 0)]
        for field, value in mutations:
            bad = copy.deepcopy(observed); bad[field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                fixture.managed_bus_proof(root, 12345, 18, scope, bad, expected=proof)
        for unit, field, value in [("service", "invocation", "c" * 32), ("socket", "invocation", "c" * 32), ("service", "main_pid", 20), ("service", "control_group", scope + "evil/dbus.service"), ("service", "arguments", ["/usr/bin/other"]), ("service", "fragment", "/usr/lib/systemd/user/dbus.service"), ("socket", "listen", [["Stream", "/tmp/ambient/bus"]]), ("service", "active", "inactive"), ("socket", "sub", "listening")]:
            bad = copy.deepcopy(observed); bad[unit][field] = value
            with self.subTest(unit=unit, field=field), self.assertRaises(ValueError):
                fixture.managed_bus_proof(root, 12345, 18, scope, bad, expected=proof)

    def test_cleanup_requires_original_join_and_empty_retained_scope(self):
        self.assertTrue(fixture.manager_cleanup_complete({"joined_status": 0}, True))
        self.assertTrue(fixture.manager_cleanup_complete({"joined_status": -9}, True))
        for owner, empty in [(None, True), ({}, True), ({"joined_status": None}, True), ({"joined_status": 0}, False)]:
            with self.subTest(owner=owner, empty=empty):
                self.assertFalse(fixture.manager_cleanup_complete(owner, empty))
        self.assertTrue(fixture.scope_unpopulated("populated 0\nfrozen 0\n"))
        for value in ["", "populated 1\n", "populated 0\npopulated 1\n", "populated 00\n"]:
            with self.subTest(value=value):
                self.assertFalse(fixture.scope_unpopulated(value))

    class FakePrivate:
        """Retained private channel double: (invocation, ActiveState) per unit."""
        def __init__(self, units=None, error=None):
            self.units, self.error = units or {}, error
            self.stops, self.reads = [], 0

        def wait_ready(self, deadline):
            fixture.remaining(deadline, 1)

        def get_unit(self, name, timeout):
            if self.error is not None:
                raise self.error
            if name not in self.units:
                raise fixture.PrivateBusError("org.freedesktop.systemd1.NoSuchUnit", name)
            return "/unit/" + name

        def unit_state(self, path, timeout):
            self.reads += 1
            value = self.units[path.rsplit("/", 1)[1]]
            if isinstance(value, Exception):
                raise value
            return value

        def stop_unit(self, name, mode, timeout):
            self.stops.append((name, mode))
            return "/job/1"

    def test_private_cleanup_never_stops_replaced_or_expired_original(self):
        root, scope, observed = self.owned_bus_observation()
        proof = fixture.managed_bus_proof(root, 12345, 18, scope, observed)
        private = self.FakePrivate()
        replaced = copy.deepcopy(observed)
        replaced["service"]["invocation"] = "c" * 32
        with self.assertRaises(ValueError):
            fixture.stop_original_bus(lambda: replaced, private, proof, fixture.time.monotonic() + 5)
        self.assertEqual(private.stops, [])
        with self.assertRaises(ValueError):
            fixture.stop_original_bus(lambda: observed, private, proof, fixture.time.monotonic() - 1)
        self.assertEqual(private.stops, [])

    def test_same_private_connection_survives_public_bus_stop_but_loss_never_settles(self):
        root, scope, observed = self.owned_bus_observation()
        proof = fixture.managed_bus_proof(root, 12345, 18, scope, observed)
        private = self.FakePrivate({"dbus.service": (proof["service_invocation"], "inactive"),
                                    "dbus.socket": (proof["socket_invocation"], "inactive")})
        result = fixture.stop_original_bus(lambda: observed, private, proof, fixture.time.monotonic() + 5)
        self.assertTrue(result["settled"])
        self.assertEqual(private.stops, [("dbus.socket", "replace")])
        self.assertEqual(private.reads, 2)
        lost = self.FakePrivate(error=fixture.PrivateBusError("org.freedesktop.DBus.Error.Disconnected", "lost"))
        with self.assertRaisesRegex(ValueError, "lost original"):
            fixture.stop_original_bus(lambda: observed, lost, proof, fixture.time.monotonic() + 5)
        changed = self.FakePrivate({"dbus.service": ("d" * 32, "inactive"), "dbus.socket": (proof["socket_invocation"], "inactive")})
        with self.assertRaisesRegex(ValueError, "invocation changed"):
            fixture.stop_original_bus(lambda: observed, changed, proof, fixture.time.monotonic() + 5)

    def test_private_channel_must_answer_for_the_same_original_units_before_cases(self):
        root, scope, observed = self.owned_bus_observation()
        same = self.FakePrivate({"dbus.service": (observed["service"]["invocation"], "active"),
                                 "dbus.socket": (observed["socket"]["invocation"], "active")})
        self.assertTrue(fixture.private_channel_matches(same, observed, fixture.time.monotonic() + 5))
        other = self.FakePrivate({"dbus.service": ("e" * 32, "active"), "dbus.socket": (observed["socket"]["invocation"], "active")})
        with self.assertRaisesRegex(ValueError, "disagrees"):
            fixture.private_channel_matches(other, observed, fixture.time.monotonic() + 5)
        silent = self.FakePrivate(error=fixture.PrivateBusError("org.freedesktop.DBus.Error.NoReply", "no reply"))
        with self.assertRaises(fixture.PrivateBusError):
            fixture.private_channel_matches(silent, observed, fixture.time.monotonic() + 5)
        with self.assertRaisesRegex(ValueError, "deadline expired"):
            fixture.private_channel_matches(same, observed, fixture.time.monotonic() - 1)

    def test_account_cleanup_requires_actual_original_scope_receipt(self):
        good = {"kind": "theseus_original_manager_cleanup_v1", "manager_started": True, "manager": {"joined_status": 0}, "manager_scope_empty": True, "errors": []}
        self.assertTrue(fixture.original_scope_settled(good))
        for field, value in [("kind", "old_cleanup"), ("manager", None), ("manager_scope_empty", False), ("errors", ["private connection lost"]), ("manager_started", None)]:
            bad = dict(good); bad[field] = value
            with self.subTest(field=field):
                self.assertFalse(fixture.original_scope_settled(bad))
        self.assertFalse(fixture.original_scope_settled(None))

    def test_initial_units_close_definitions_but_admit_kernel_derived_units(self):
        closed = [(n, "loaded", "active") for n in ("theseus-empty.target", "dbus.socket", "dbus.service", "init.scope", "-.slice", "app.slice")]
        kernel = [(n, "loaded", "active") for n in ("-.mount", "tmp.mount", "run-user-1001.mount", "dev-sda2.swap", "sys-devices-virtual-net-lo.device")]
        # CI4 journal: referenced but definition-less units stay loaded as not-found.
        missing = [(n, "not-found", "inactive") for n in ("blockdev@dev-sda16.target", "systemd-remount-fs.service", "shutdown.target")]
        self.assertTrue(fixture.initial_units_closed(closed + kernel + missing))
        for unexpected in ("default.target", "dbus-broker.service", "tmp.automount", "x.timer", "x.path", "session.scope", "theseus.mount.service"):
            with self.subTest(unit=unexpected):
                self.assertFalse(fixture.initial_units_closed(closed + kernel + [(unexpected, "loaded", "active")]))
                self.assertFalse(fixture.initial_units_closed(closed + [(unexpected, "loaded", "inactive")]))
                self.assertFalse(fixture.initial_units_closed(closed + [(unexpected, "not-found", "active")]))

    def test_stop_poll_accepts_null_or_collected_original_units(self):
        root, scope, observed = self.owned_bus_observation()
        proof = fixture.managed_bus_proof(root, 12345, 18, scope, observed)
        collected = fixture.PrivateBusError("org.freedesktop.DBus.Error.UnknownObject", "collected between GetUnit and reads")
        private = self.FakePrivate({"dbus.service": ("", "inactive"), "dbus.socket": collected})
        self.assertTrue(fixture.stop_original_bus(lambda: observed, private, proof, fixture.time.monotonic() + 5)["settled"])
        gone = self.FakePrivate({})
        self.assertTrue(fixture.stop_original_bus(lambda: observed, gone, proof, fixture.time.monotonic() + 5)["settled"])

    def test_worker_cleanup_reader_never_blocks_on_special_files(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "worker-cleanup.json"
            fixture.os.mkfifo(path)
            self.assertIsNone(fixture.read_worker_cleanup(path))
            path.unlink()
            path.write_text(json.dumps({"kind": "x", "pad": "y" * (fixture.WORKER_CLEANUP_LIMIT + 1)}))
            self.assertIsNone(fixture.read_worker_cleanup(path))

    def test_cleanup_failure_never_hides_primary_failure(self):
        try:
            try:
                raise ValueError("unexpected private bus failure")
            finally:
                fixture.require(False, "original native cleanup incomplete")
        except ValueError as error:
            text = fixture.exception_chain(error)
        self.assertLess(text.index("unexpected private bus failure"), text.index("original native cleanup incomplete"))
        self.assertEqual(fixture.exception_chain(ValueError("only")), "only")

    def test_manager_logs_reach_retained_journal_not_inaccessible_console(self):
        env = fixture.make_plan(manifest(), "abcd1234abcd1234")["worker_environment"]
        self.assertEqual(env["SYSTEMD_LOG_TARGET"], "journal")
        cases = fixture.artifact_environment(fixture.make_plan(manifest(), "abcd1234abcd1234"))
        self.assertEqual((cases["SYSTEMD_LOG_TARGET"], cases["SYSTEMD_LOG_LEVEL"]), ("console", "info"))
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "service-journal.log"
            observed = SimpleNamespace(stdout="systemd[17460]: Reached target\n", stderr="", returncode=0)
            with patch.object(fixture.subprocess, "run", return_value=observed) as run:
                fixture.retain_service_journal(path, "theseus-private-abcd1234abcd1234.service")
            self.assertEqual(run.call_args.args[0][-1], "--unit=theseus-private-abcd1234abcd1234.service")
            with patch.object(fixture.subprocess, "run", return_value=observed):
                fixture.retain_service_journal(path, "theseus-private-abcd1234abcd1234.service")  # existing: never raises
            self.assertIn("Reached target", path.read_text())
            missing = Path(directory) / "missing.log"
            with patch.object(fixture.subprocess, "run", side_effect=OSError("no journalctl")):
                fixture.retain_service_journal(missing, "theseus-private-abcd1234abcd1234.service")
            self.assertIn("journal unavailable", missing.read_text())

    def test_unreadable_worker_cleanup_is_uncertainty_not_a_lost_receipt(self):
        good = {"kind": "theseus_original_manager_cleanup_v1", "manager_started": True, "manager": {"joined_status": 0}, "manager_scope_empty": True, "errors": []}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "worker-cleanup.json"
            self.assertIsNone(fixture.read_worker_cleanup(path))
            path.write_text('{"kind": "theseus_original_manager_cleanup_v1", "manager_sc')
            self.assertIsNone(fixture.read_worker_cleanup(path))
            path.write_text("[]")
            self.assertIsNone(fixture.read_worker_cleanup(path))
            target = Path(directory) / "elsewhere.json"
            target.write_text(json.dumps(good))
            path.unlink()
            path.symlink_to(target)
            self.assertIsNone(fixture.read_worker_cleanup(path))
            path.unlink()
            path.write_text(json.dumps(good))
            self.assertEqual(fixture.read_worker_cleanup(path), good)
            self.assertTrue(fixture.original_scope_settled(fixture.read_worker_cleanup(path)))

    def test_pure_planning_imports_without_unix_pwd_module(self):
        # Windows has no pwd module. Exercise the actual import and pure plan;
        # keep the dependency mandatory for Linux execution before any effects.
        portable = importlib.util.module_from_spec(SPEC)
        with patch.dict("sys.modules", {"pwd": None}):
            try:
                SPEC.loader.exec_module(portable)
            except ModuleNotFoundError as error:
                self.fail(f"pure planning import requires a Unix-only dependency: {error}")
            plan = portable.make_plan(manifest(), "abcd1234abcd1234")
            self.assertEqual(plan["root"], "/tmp/th-sd-abcd1234abcd1234")
            self.assertEqual(len(plan["manifest"]["artifacts"]), 10)
            self.assertEqual(len(plan["manifest"]["cases"]), 7)
            with self.assertRaises(ModuleNotFoundError):
                portable.operator({}, {})
            with self.assertRaises(ModuleNotFoundError):
                portable.worker({})

    def test_real_combined_dispatch_has_exact_pinned_owners_and_bounded_case_budget(self):
        plan = fixture.make_plan(manifest(), "abcd1234abcd1234")
        self.assertEqual(len(plan["manifest"]["cases"]), 7)
        environment = fixture.artifact_environment(plan)
        self.assertEqual(environment["THESEUS_NATIVE_DESKTOP_HELPER_BINARY"], plan["root"] + "/artifacts/THESEUS_NATIVE_DESKTOP_HELPER_BINARY")
        self.assertEqual(environment["THESEUS_NATIVE_DESKTOP_BROKER_BINARY"], plan["root"] + "/artifacts/THESEUS_NATIVE_DESKTOP_BROKER_BINARY")
        self.assertEqual(environment["THESEUS_NATIVE_HELPER_BINARY"], plan["root"] + "/artifacts/THESEUS_NATIVE_HELPER_BINARY")
        self.assertIn("THESEUS_NATIVE_SYSTEMD_RECEIPT", environment)
        self.assertNotIn("THESEUS_NATIVE_SYSTEMD_MODEL_RECEIPT", environment)
        for case in plan["manifest"]["cases"]:
            bound = fixture.case_timeout(case)
            if case["binary"] == "native_desktop":
                self.assertGreaterEqual(bound, 360)
                self.assertLessEqual(bound, 450)
            else:
                self.assertEqual(bound, 300)

    def test_no_mandatory_role_case_or_source_binding_can_be_optional_or_aliased(self):
        good = manifest()
        mutations = []
        for role in good["artifacts"]:
            bad = copy.deepcopy(good); bad["artifacts"].pop(role); mutations.append(bad)
        for index in range(len(good["cases"])):
            bad = copy.deepcopy(good); bad["cases"].pop(index); mutations.append(bad)
        for name in ["private_model_combined_service_desktop_ordinary_cli_one_shared_lease", "unregistered_optional_case"]:
            bad = copy.deepcopy(good); bad["cases"][-1]["name"] = name; mutations.append(bad)
        bad = copy.deepcopy(good); bad["cases"][-1]["binary"] = "native_services"; mutations.append(bad)
        bad = copy.deepcopy(good); bad["cases"].append(copy.deepcopy(good["cases"][-1])); mutations.append(bad)
        bad = copy.deepcopy(good); bad["artifacts"]["native_desktop"]["path"] = good["artifacts"]["native_services"]["path"]; mutations.append(bad)
        bad = copy.deepcopy(good); bad["artifacts"]["native_desktop"]["source_sha"] = "c" * 40; mutations.append(bad)
        bad = copy.deepcopy(good); bad["build"]["source_sha"] = "c" * 40; mutations.append(bad)
        bad = copy.deepcopy(good); bad["artifacts"]["other_test"] = copy.deepcopy(good["artifacts"]["native_desktop"]); mutations.append(bad)
        for index, bad in enumerate(mutations):
            with self.subTest(mutation=index), self.assertRaises(ValueError):
                fixture.make_plan(bad, "abcd1234abcd1234")

    def test_cli_plan_and_non_ci_execution_never_reach_native_operations(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "pins.json"
            path.write_text(json.dumps(manifest()))
            with patch.object(fixture.subprocess, "Popen", side_effect=AssertionError("native process launched")), patch.object(fixture, "operator", side_effect=AssertionError("provisioning reached")), patch.object(fixture.sys, "argv", ["fixture", "--manifest", str(path), "--plan"]), contextlib.redirect_stdout(io.StringIO()) as output:
                fixture.main()
            planned = json.loads(output.getvalue())
            self.assertEqual(planned["manifest"]["source_sha"], "a" * 40)
            with patch.dict(fixture.os.environ, {}, clear=True), patch.object(fixture, "operator", side_effect=AssertionError("provisioning reached")), patch.object(fixture.sys, "argv", ["fixture", "--manifest", str(path), "--execute", "--disposable-github-runner"]), self.assertRaises(ValueError):
                fixture.main()

    def test_explicit_cli_token_selects_same_exact_plan_root_without_native_calls(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'pins.json'
            path.write_text(json.dumps(manifest()))
            with patch.object(fixture, 'operator', side_effect=AssertionError('native operator reached')), patch.object(fixture.sys, 'argv', ['fixture', '--manifest', str(path), '--plan', '--token', 'abcd1234abcd1234']), contextlib.redirect_stdout(io.StringIO()) as output:
                fixture.main()
            plan = json.loads(output.getvalue())
            self.assertEqual(plan['root'], '/tmp/th-sd-abcd1234abcd1234')
            self.assertEqual(plan['token'], 'abcd1234abcd1234')

    def test_plan_is_pure_and_explicitly_delegated(self):
        plan = fixture.make_plan(manifest(), "abcd1234abcd1234")
        command = plan["service_command"]
        self.assertEqual(command[:2], ["/usr/bin/systemd-run", "--system"])
        for option in ["--property=Delegate=yes", "--property=KillMode=control-group", "--wait", "--collect", "--pipe"]:
            self.assertIn(option, command)
        self.assertIn("--uid=thsd_abcd1234abcd1234", command)
        self.assertNotIn("--user", command)
        env = plan["worker_environment"]
        self.assertNotIn("HOME", env)
        self.assertEqual(env["SYSTEMD_UNIT_PATH"], plan["root"] + "/units:" + plan["root"] + "/support-units")
        self.assertIn("INVOCATION_ID=${INVOCATION_ID}", command)
        self.assertEqual(env["DBUS_SESSION_BUS_ADDRESS"], "unix:path=" + plan["root"] + "/runtime/bus")
        self.assertEqual(env["DBUS_SYSTEM_BUS_ADDRESS"], env["DBUS_SESSION_BUS_ADDRESS"])
        self.assertFalse(env["SYSTEMD_UNIT_PATH"].endswith(":"))
        for key in ["XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_CONFIG_DIRS", "XDG_DATA_HOME", "XDG_DATA_DIRS", "XDG_CACHE_HOME", "TMPDIR"]:
            self.assertTrue(env[key].startswith(plan["root"] + "/"))

    def test_execution_requires_hosted_linux_ci_and_both_guards(self):
        env = {"CI": "true", "GITHUB_ACTIONS": "true", "RUNNER_OS": "Linux", "RUNNER_ENVIRONMENT": "github-hosted", "GITHUB_RUN_ID": "123", "GITHUB_RUN_ATTEMPT": "1", "GITHUB_SHA": "a" * 40}
        fixture.execution_guard(env, execute=True, disposable=True, uid=0, system="Linux")
        for key in env:
            bad = dict(env)
            bad.pop(key)
            with self.subTest(missing=key), self.assertRaises(ValueError):
                fixture.execution_guard(bad, execute=True, disposable=True, uid=0, system="Linux")
        for overrides in [{"execute": False}, {"disposable": False}, {"uid": 1000}, {"system": "Darwin"}]:
            args = dict(execute=True, disposable=True, uid=0, system="Linux")
            args.update(overrides)
            with self.subTest(overrides=overrides), self.assertRaises(ValueError):
                fixture.execution_guard(env, **args)

    def test_rejects_ambiguous_artifact_inputs_and_missing_pins(self):
        for value in ["relative", "/build/../bin", "/build//bin", "/build/bin\x00"]:
            bad = manifest()
            bad["artifacts"]["native_services"]["path"] = value
            with self.subTest(path=value), self.assertRaises(ValueError):
                fixture.make_plan(bad, "abcd1234abcd1234")
        for field in ["sha256", "length", "path"]:
            bad = manifest()
            bad["artifacts"]["native_services"].pop(field)
            with self.subTest(field=field), self.assertRaises(ValueError):
                fixture.make_plan(bad, "abcd1234abcd1234")
        bad = manifest()
        bad["artifacts"].pop("THESEUS_NATIVE_HELPER_BINARY")
        with self.assertRaises(ValueError):
            fixture.make_plan(bad, "abcd1234abcd1234")

    def test_test_command_requires_an_exact_registered_non_model_case(self):
        self.assertEqual(fixture.test_command("/private/native_services", "systemd_user_clean", ["systemd_user_clean: test"]), ["/private/native_services", "systemd_user_clean", "--exact", "--nocapture", "--test-threads=1"])
        for names in [[], ["systemd_user_clean_extra: test"], ["systemd_user_clean: test", "systemd_user_clean: test"]]:
            with self.assertRaises(ValueError):
                fixture.test_command("/private/native_services", "systemd_user_clean", names)
        for name in ["systemd_model_clean", "local_model_service", "", "--ignored"]:
            bad = manifest()
            bad["cases"][0]["name"] = name
            with self.subTest(name=name), self.assertRaises(ValueError):
                fixture.make_plan(bad, "abcd1234abcd1234")

    def test_namespace_requires_actual_exact_private_unit_paths(self):
        plan = fixture.make_plan(manifest(), "abcd1234abcd1234")
        root = plan["root"]
        fixture.validate_live_namespace([root + "/units", root + "/support-units"], root)
        for paths in [[], ["/usr/lib/systemd/user"], [root + "/units", "/etc/systemd/user"], [root + "/units", root + "/units"], [root + "/units/../escape"], [root + "/unitsevil"]]:
            with self.subTest(paths=paths), self.assertRaises(ValueError):
                fixture.validate_live_namespace(paths, root)

    def test_receipt_binds_live_native_qualification_and_retained_owners(self):
        plan = fixture.make_plan(manifest(), "abcd1234abcd1234")
        evidence = {"version": "255.4-1ubuntu8", "unit_paths": [plan["root"] + "/units", plan["root"] + "/support-units"], "bus_peer_uid": 12345, "manager_uid": 12345, "manager_unique_name": ":1.7", "bus_pid": 19, "manager_pid": 18, "service_invocation_id": "a" * 32, "manager_features": "+PAM +LIBCRYPTSETUP", "no_other_namespace_writers": True}
        root, scope, observed = self.owned_bus_observation()
        evidence["bus_ownership"] = fixture.managed_bus_proof(root, 12345, 18, scope, observed)
        evidence["private_manager_peer"] = {"pid": 18, "uid": 12345, "gid": 12345}
        receipt = fixture.manager_receipt(plan, 12345, evidence)
        self.assertEqual(set(receipt), {"kind", "schema", "manager", "authority", "manager_unique_name", "namespace_receipt"})
        self.assertEqual(receipt["kind"], "theseus_supplied_manager_receipt")
        authority = receipt["authority"]
        self.assertEqual(authority["qualification"]["version"], evidence["version"])
        self.assertEqual(authority["expected_peer_uid"], 12345)
        self.assertEqual(authority["expected_manager_uid"], 12345)
        self.assertEqual(authority["namespace"]["unit_load_roots"], evidence["unit_paths"])
        self.assertEqual(authority["manager_context"]["kind"], "whole_private")
        for key, value in [("bus_peer_uid", 0), ("manager_uid", 0), ("version", "theseus-model"), ("manager_unique_name", "org.freedesktop.systemd1"), ("no_other_namespace_writers", False), ("service_invocation_id", "")]:
            bad = copy.deepcopy(evidence)
            bad[key] = value
            with self.subTest(field=key), self.assertRaises(ValueError):
                fixture.manager_receipt(plan, 12345, bad)
        for field in ["bus_ownership", "private_manager_peer"]:
            missing = copy.deepcopy(evidence); missing.pop(field)
            with self.subTest(missing=field), self.assertRaises(ValueError):
                fixture.manager_receipt(plan, 12345, missing)
        for field, value in [("kind", "external_popen"), ("daemon_pid", 17), ("socket_creator_pid", 19), ("service_invocation", "")]:
            bad = copy.deepcopy(evidence); bad["bus_ownership"][field] = value
            with self.subTest(bus_proof=field), self.assertRaises(ValueError):
                fixture.manager_receipt(plan, 12345, bad)
        changed = copy.deepcopy(evidence)
        changed["manager_pid"] = 20
        changed["bus_ownership"]["manager_pid"] = 20
        changed["bus_ownership"]["socket_creator_pid"] = 20
        changed["private_manager_peer"]["pid"] = 20
        self.assertNotEqual(receipt["namespace_receipt"], fixture.manager_receipt(plan, 12345, changed)["namespace_receipt"])

    def test_original_service_cleanup_refuses_live_or_replaced_instances(self):
        original = "a" * 32
        self.assertTrue(fixture.service_terminal({"LoadState": "not-found", "ActiveState": "inactive"}, None))
        self.assertTrue(fixture.service_terminal({"LoadState": "loaded", "ActiveState": "failed", "InvocationID": original}, original))
        for fields in [{}, {"LoadState": "loaded", "ActiveState": "active", "InvocationID": original}, {"LoadState": "loaded", "ActiveState": "failed", "InvocationID": "b" * 32}, {"LoadState": "loaded", "ActiveState": "inactive", "InvocationID": ""}]:
            with self.subTest(fields=fields):
                self.assertFalse(fixture.service_terminal(fields, original))


if __name__ == "__main__":
    unittest.main()
