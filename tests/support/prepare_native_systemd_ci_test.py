"""Pure qualification tests; never provision or contact any native manager."""
import copy
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("prepare_native_systemd_ci", Path(__file__).with_name("prepare_native_systemd_ci.py"))
fixture = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(fixture)


def manifest():
    names = [*fixture.BINARY_ENV, "native_services", "native_service_mcp"]
    return {
        "schema": 1, "source_sha": "a" * 40,
        "build": {"target": "x86_64-unknown-linux-gnu", "profile": "release", "features": ["native-integration-test-support"]},
        "artifacts": {name: {"path": f"/build/{name}", "sha256": "b" * 64, "length": 123} for name in names},
        "cases": [{"binary": "native_services", "name": "systemd_user_clean_installer_managed_service_uses_shared_product_path"}],
    }


class Planning(unittest.TestCase):
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
        evidence = {"version": "255.4-1ubuntu8", "unit_paths": [plan["root"] + "/units", plan["root"] + "/support-units"], "bus_peer_uid": 12345, "manager_uid": 12345, "manager_unique_name": ":1.7", "bus_pid": 17, "manager_pid": 18, "service_invocation_id": "a" * 32, "manager_features": "+PAM +LIBCRYPTSETUP", "no_other_namespace_writers": True}
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
        changed = dict(evidence, manager_pid=19)
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
