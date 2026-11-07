"""Pure planner/validation tests only; never load OS APIs or contact managers."""
import copy
from contextlib import ExitStack
import hashlib
import os
import importlib
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parent))


def manifest(windows=False):
    roles = ["THESEUS_NATIVE_HELPER_BINARY", "THESEUS_NATIVE_SERVICE_FIXTURE", "THESEUS_NATIVE_SERVICE_BASE", "THESEUS_NATIVE_SERVICE_TARGET", "THESEUS_NATIVE_INTEGRATION_CONTROLLER", "native_services_platforms"]
    common = [
        "native::native_upgrade_sut_transitions_from_running_and_preserves_data",
        "native::native_reinstall_compares_original_definition_reader_and_data",
        "native::native_installer_missing_and_leaked_registration_never_passes",
        "native::native_running_wrong_version_data_or_nonce_never_passes_readiness",
    ]
    names = common + ([
        "native::scm_own_process_clean_observes_config_status_readiness_and_delete_pending",
        "owners::scm_delete_success_remains_pending_until_original_handles_close_and_name_is_absent",
        "owners::scm_native_pipe_rejects_actual_helper_sid_before_challenge",
    ] if windows else [
        "native::launchd_user_clean_keeps_loaded_configuration_runtime_and_readiness_distinct",
        "owners::launchd_wrong_domain_and_replaced_plist_never_authorize_stop_or_bootout",
        "launchd_running_wrong_native_peer_never_passes_readiness",
    ])
    return {"schema": 1, "source_sha": "a" * 40,
            "build": {"source_sha": "a" * 40, "target": "x86_64-pc-windows-msvc" if windows else "aarch64-apple-darwin", "profile": "release", "features": ["native-integration-test-support"]},
            "artifacts": {role: {"path": "C:\\build\\" + role + ".exe" if windows else "/build/" + role, "sha256": "b" * 64, "length": 123, "source_sha": "a" * 40} for role in roles},
            "cases": [{"binary": "native_services_platforms", "name": name} for name in names]}


class Planners(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.mac = importlib.import_module("prepare_native_launchd_ci")
        cls.win = importlib.import_module("prepare_native_scm_ci")

    def test_full_default_cases_reserve_independent_roots_and_instances(self):
        for module, windows in [(self.mac, False), (self.win, True)]:
            plan = module.make_plan(manifest(windows), "abcd1234abcd1234")
            reservations = [r for c in plan["cases"] for r in c["reservations"]]
            self.assertEqual(len(reservations), 10)
            for key in ["root", "ready_root", "name", "instance"]:
                self.assertEqual(len({r[key] for r in reservations}), 10)
            self.assertTrue(all(r["root"] != r["ready_root"] for r in reservations))
            self.assertEqual(plan["manifest"]["source_sha"], "a" * 40)

    def test_plan_has_no_native_side_effects_and_execution_needs_both_guards(self):
        for module, windows in [(self.mac, False), (self.win, True)]:
            with tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "manifest.json"
                path.write_text(json.dumps(manifest(windows)))
                with patch.object(module, "operator", side_effect=AssertionError("native reached")), patch.object(sys, "argv", ["operator", "--manifest", str(path), "--plan"]), patch("builtins.print"):
                    module.main()
                with patch.dict(module.os.environ, {}, clear=True), patch.object(module, "operator", side_effect=AssertionError("native reached")), patch.object(sys, "argv", ["operator", "--manifest", str(path), "--execute", "--disposable-github-runner"]), self.assertRaises(ValueError):
                    module.main()

    def test_cli_fixed_token_makes_failure_evidence_path_known_without_discovery(self):
        import contextlib
        import io
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "manifest.json"
            path.write_text(json.dumps(manifest()))
            with patch.object(sys, "argv", ["operator", "--manifest", str(path), "--plan", "--token", "abcd1234abcd1234"]), contextlib.redirect_stdout(io.StringIO()) as output:
                self.mac.main()
            self.assertEqual(json.loads(output.getvalue())["root"], "/private/tmp/th-ci-abcd1234abcd1234")

    def test_hosted_runner_attestation_and_source_must_match(self):
        for module, runner_os, system in [(self.mac, "macOS", "Darwin"), (self.win, "Windows", "Windows")]:
            env = {"CI": "true", "GITHUB_ACTIONS": "true", "RUNNER_OS": runner_os, "RUNNER_ENVIRONMENT": "github-hosted", "GITHUB_RUN_ID": "123", "GITHUB_RUN_ATTEMPT": "1", "GITHUB_SHA": "a" * 40}
            module.execution_guard(env, execute=True, disposable=True, system=system)
            for key in env:
                bad = dict(env)
                bad.pop(key)
                with self.subTest(module=module.__name__, key=key), self.assertRaises(ValueError):
                    module.execution_guard(bad, execute=True, disposable=True, system=system)
            for execute, disposable in [(False, True), (True, False)]:
                with self.assertRaises(ValueError):
                    module.execution_guard(env, execute=execute, disposable=disposable, system=system)

    def test_missing_native_case_or_any_pin_never_becomes_optional(self):
        for module, windows in [(self.mac, False), (self.win, True)]:
            mutations = []
            for field in ["source_sha", "sha256", "length", "path"]:
                bad = manifest(windows)
                bad["artifacts"]["native_services_platforms"].pop(field)
                mutations.append(bad)
            for category in ["build", "artifacts"]:
                bad = manifest(windows)
                if category == "build":
                    bad[category]["source_sha"] = "c" * 40
                else:
                    bad[category]["native_services_platforms"]["source_sha"] = "c" * 40
                mutations.append(bad)
            bad = manifest(windows)
            bad["cases"].pop()
            mutations.append(bad)
            bad = manifest(windows)
            bad["cases"].append(copy.deepcopy(bad["cases"][0]))
            mutations.append(bad)
            bad = manifest(windows)
            bad["build"]["features"] = []
            mutations.append(bad)
            for bad in mutations:
                with self.assertRaises(ValueError):
                    module.make_plan(bad, "abcd1234abcd1234")

    def test_paths_cannot_alias_or_escape_and_target_must_match_platform(self):
        for module, windows in [(self.mac, False), (self.win, True)]:
            for path in (["relative", "C:\\build\\..\\escape.exe", "\\\\server\\share\\a.exe", "C:\\build\\a.exe:stream"] if windows else ["relative", "/build/../escape", "/build//exe", "/build/exe\x00"]):
                bad = manifest(windows)
                bad["artifacts"]["native_services_platforms"]["path"] = path
                with self.subTest(path=path), self.assertRaises(ValueError):
                    module.make_plan(bad, "abcd1234abcd1234")
            bad = manifest(windows)
            bad["artifacts"]["THESEUS_NATIVE_SERVICE_TARGET"]["path"] = bad["artifacts"]["THESEUS_NATIVE_SERVICE_BASE"]["path"]
            with self.assertRaises(ValueError):
                module.make_plan(bad, "abcd1234abcd1234")
            bad = manifest(windows)
            bad["build"]["target"] = "x86_64-unknown-linux-gnu"
            with self.assertRaises(ValueError):
                module.make_plan(bad, "abcd1234abcd1234")

    def test_exact_test_execution_refuses_missing_duplicates_and_ignored_success(self):
        from native_platform_ci import test_command, validate_test_result
        self.assertEqual(test_command("/bin/tests", "native::clean", ["native::clean: test"]), ["/bin/tests", "native::clean", "--exact", "--nocapture", "--test-threads=1"])
        for inventory in [[], ["native::clean_extra: test"], ["native::clean: test"] * 2]:
            with self.assertRaises(ValueError):
                test_command("/bin/tests", "native::clean", inventory)
        validate_test_result(0, b"test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out; finished in 1s\n")
        for status, output in [(0, b"test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured"), (0, b""), (1, b"1 passed; 0 failed; 0 ignored; 0 measured")]:
            with self.assertRaises(ValueError):
                validate_test_result(status, output)

    def test_launchd_qualification_requires_exact_user_absence_and_measured_tool(self):
        plan = self.mac.make_plan(manifest(), "abcd1234abcd1234")
        reservation = plan["cases"][0]["reservations"][0]
        evidence = {"uid": 501, "os_version": "15.0", "os_build": "24A1", "tool_fingerprint": [1] + [0] * 56, "tool_sha256": "c" * 64,
                    "domain_status": 0, "domain_stdout": "user/501 = {\n}\n", "domain_stderr": "", "absence_status": 113, "absence_stdout": "", "absence_stderr": 'Could not find service "' + reservation["name"] + '" in domain for user: 501\n'}
        receipt = self.mac.manager_receipt(plan, reservation, evidence)
        self.assertEqual(receipt["authority"]["domain"], {"kind": "user", "uid": 501})
        self.assertEqual(receipt["authority"]["exclusive_load_roots"], [reservation["definition_root"]])
        for key, value in [("uid", 0), ("absence_stdout", "unexpected"), ("absence_status", 0), ("absence_stderr", "No such process"), ("tool_fingerprint", []), ("domain_stdout", "gui/501 = {\n}\n"), ("os_build", "model")]:
            bad = dict(evidence, **{key: value})
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.mac.manager_receipt(plan, reservation, bad)

    def test_launchd_absence_accepts_exact_real_runner_uid_form_only(self):
        # Exact bytes retained from the real macOS 15.8.2 (24H232) runner, CI8 run 42302044978.
        name, uid = "theseus.ci.bc2bccaf8c83b2cf-00", 501
        real = 'Bad request.\nCould not find service "theseus.ci.bc2bccaf8c83b2cf-00" in domain for uid: 501\n'
        self.mac.validate_absence(name, uid, 113, "", real)
        self.mac.validate_absence(name, uid, 113, "", real.removeprefix("Bad request.\n"))
        for status, stdout, stderr in [
            (0, "", real), (113, "x", real), (256, "", real),
            (113, "", real.replace("uid: 501", "uid: 502")),
            (113, "", real.replace("bc2bccaf8c83b2cf-00", "bc2bccaf8c83b2cf-01")),
            (113, "", real.replace("domain for uid", "domain for gui")),
            (113, "", real.rstrip("\n")),
            (113, "", "Bad request.\nCould not find domain for uid: 501\n"),
            (113, "", real + "extra\n"),
        ]:
            with self.subTest(stderr=stderr, status=status), self.assertRaises(ValueError):
                self.mac.validate_absence(name, uid, status, stdout, stderr)

    def test_launchd_log_diagnostic_is_bounded_read_only_and_exact(self):
        args = self.mac.launchd_log_arguments(0, ["theseus.ci.4b149e0bbedf8e2b-00", "theseus.ci.4b149e0bbedf8e2b-01"], ["/private/tmp/th-ci-4b149e0bbedf8e2b"])
        self.assertEqual(args[0], "show")
        predicate = args[args.index("--predicate") + 1]
        self.assertIn('eventMessage CONTAINS "theseus.ci.4b149e0bbedf8e2b-00"', predicate)
        self.assertIn('eventMessage CONTAINS "/private/tmp/th-ci-4b149e0bbedf8e2b"', predicate)
        self.assertTrue(predicate.startswith('(process == "launchd" OR process == "launchctl") AND ('))
        for labels, roots in [([], ["/r"]), (['x" OR 1'], ["/r"]), (["theseus.ci.4b149e0bbedf8e2b-00"], ['/r" OR "']), (["theseus.ci.4b149e0bbedf8e2b-00"], ["relative"])]:
            with self.subTest(labels=labels, roots=roots), self.assertRaises(ValueError):
                self.mac.launchd_log_arguments(0, labels, roots)

    def test_scm_receipt_binds_actual_exact_path_acl_probes_and_original_manager(self):
        plan = self.win.make_plan(manifest(True), "abcd1234abcd1234")
        reservation = plan["cases"][0]["reservations"][0]
        helper = "S-1-5-21-10-20-30-1001"
        def acl(path, system, user):
            return {"path": path, "protected": True, "grants": [("S-1-5-18", system, 0), (helper, user, 0)]}
        workspace = reservation["root"] + "\\workspace"
        evidence = {"helper_sid": helper, "version": "10.0.20348", "absence_error": 1060,
                    "original_scm_handle_retained": True, "manager_access": 3,
                    "directory_acls": [acl(reservation[k], 0x1200a9, 0x1301bf) for k in ("root", "ready_root")],
                    "pipe_acl": acl(reservation["pipe"], 0x12019f, 0x12019f), "pipe_probe_closed": True, "pipe_rejects_remote_clients": True,
                    "install_path_probes": [dict(acl(workspace + suffix, system, user), kind=kind, original_deleted=True) for suffix, system, user, kind in [("\\reader.exe", 0x1200a9, 0x1301a9, "executable"), ("\\reader.service.json", 0x120089, 0x130189, "sidecar"), ("\\data", 0x120089, 0x1301bf, "data")]],
                    "workspace_probe_original_deleted": True}
        receipt = self.win.manager_receipt(plan, reservation, evidence)
        self.assertEqual(receipt["authority"]["preflight"]["helper_sid"], helper)
        self.assertEqual(receipt["authority"]["preflight"]["configured_account_sid"], "S-1-5-18")
        self.assertEqual(receipt["authority"]["preflight"]["executable"], workspace + "\\reader.exe")
        for key, value in [("helper_sid", "S-1-5-18"), ("absence_error", 5), ("original_scm_handle_retained", False), ("install_path_probes", []), ("workspace_probe_original_deleted", False), ("pipe_probe_closed", False)]:
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.win.manager_receipt(plan, reservation, dict(evidence, **{key: value}))
        bad = copy.deepcopy(evidence)
        bad["install_path_probes"][0]["grants"][1] = (helper, 0x1f01ff, 0)
        with self.assertRaises(ValueError):
            self.win.manager_receipt(plan, reservation, bad)

    def test_pinned_copy_rejects_tampered_or_hardlinked_roles(self):
        from native_platform_ci import pin_artifacts
        import hashlib
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            source.write_bytes(b"compiled-artifact")
            destination = root / "copies"
            destination.mkdir()
            plan = {"platform": "launchd", "manifest": {"artifacts": {"test": {"path": str(source), "sha256": "0" * 64, "length": 17}}}}
            with self.assertRaises(ValueError):
                pin_artifacts(plan, destination)
            destination.chmod(0o700)
            (destination / "test").unlink()
            plan["manifest"]["artifacts"]["test"]["sha256"] = hashlib.sha256(b"compiled-artifact").hexdigest()
            copied, _ = pin_artifacts(plan, destination)
            self.assertEqual(Path(copied["test"]).read_bytes(), b"compiled-artifact")
            destination.chmod(0o700)
            (destination / "test").unlink()
            alias = root / "alias"
            alias.hardlink_to(source)
            plan["manifest"]["artifacts"]["other"] = dict(plan["manifest"]["artifacts"]["test"], path=str(alias))
            with self.assertRaises(ValueError):
                pin_artifacts(plan, destination)

    def copied_batch(self, root):
        import native_platform_ci as common
        destination = root / "pinned-artifacts"
        destination.mkdir()
        pins = {}
        for role in ("native_services_platforms", "THESEUS_NATIVE_HELPER_BINARY", "THESEUS_NATIVE_INTEGRATION_CONTROLLER"):
            source = root / role
            value = ("reviewed-source-" + role).encode()
            source.write_bytes(value)
            pins[role] = {"path": str(source), "length": len(value), "sha256": hashlib.sha256(value).hexdigest(), "source_sha": "a" * 40}
        plan = {"platform": "launchd", "manifest": {"source_sha": "a" * 40, "artifacts": pins}}
        paths, evidence = common.pin_artifacts(plan, destination)
        return paths, evidence, common.ArtifactGuard(paths, evidence, destination)

    @staticmethod
    def replacement(path, value=None):
        path = Path(path)
        path.parent.chmod(0o700)
        swap = path.parent / "replacement"
        swap.write_bytes(path.read_bytes() if value is None else value)
        swap.chmod(0o500)
        swap.replace(path)
        path.parent.chmod(0o500)

    def test_replacement_after_list_refused_before_test_spawn_and_preserves_pins(self):
        import native_platform_ci as common
        for role in ("native_services_platforms", "THESEUS_NATIVE_HELPER_BINARY", "THESEUS_NATIVE_INTEGRATION_CONTROLLER"):
            with self.subTest(role=role), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                paths, pins, guard = self.copied_batch(root)
                source_evidence = copy.deepcopy(pins)
                class Listed:
                    def __init__(self, command, **kwargs):
                        kwargs["stdout"].write(b"native::clean: test\n")
                    def wait(self, timeout):
                        return 0
                with guard, patch.object(common.subprocess, "Popen", Listed):
                    status, output = common.run_logged([paths["native_services_platforms"], "--list"], {}, root / "list.log", artifacts=guard)
                    self.assertEqual((status, output), (0, b"native::clean: test\n"))
                    self.replacement(paths[role])
                    with patch.object(common.subprocess, "Popen", side_effect=AssertionError("replacement dispatched")), self.assertRaises(ValueError):
                        common.run_logged([paths["native_services_platforms"], "native::clean"], {}, root / "case.log", artifacts=guard)
                self.assertEqual(pins, source_evidence)
                self.assertEqual((root / "list.log").read_bytes(), b"native::clean: test\n")
                rejection = json.loads((root / "case.log.pins-before.json").read_text())
                self.assertEqual(rejection["status"], "refused")
                self.assertEqual(rejection["pins"], source_evidence)
                self.assertTrue(Path(paths[role]).is_file())

    def test_replacement_between_cases_refused_and_first_raw_result_retained(self):
        import native_platform_ci as common
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            paths, _, guard = self.copied_batch(root)
            class Passed:
                def __init__(self, command, **kwargs):
                    kwargs["stdout"].write(b"test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out;\n")
                def wait(self, timeout):
                    return 0
            with guard, patch.object(common.subprocess, "Popen", Passed):
                status, output = common.run_logged([paths["native_services_platforms"], "native::first"], {}, root / "first.log", artifacts=guard)
                common.validate_test_result(status, output)
                self.replacement(paths["THESEUS_NATIVE_INTEGRATION_CONTROLLER"], b"unreviewed-controller")
                with patch.object(common.subprocess, "Popen", side_effect=AssertionError("second case dispatched")), self.assertRaises(ValueError):
                    common.run_logged([paths["native_services_platforms"], "native::second"], {}, root / "second.log", artifacts=guard)
            self.assertEqual((root / "first.log").read_bytes(), output)
            self.assertEqual(json.loads((root / "first.log.pins-after.json").read_text())["status"], "verified")
            self.assertEqual(json.loads((root / "second.log.pins-before.json").read_text())["status"], "refused")

    def test_in_case_content_change_rejects_success_with_raw_log_retained(self):
        import native_platform_ci as common
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            paths, _, guard = self.copied_batch(root)
            class Changed:
                def __init__(self, command, **kwargs):
                    kwargs["stdout"].write(b"test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out;\n")
                    changed = Path(paths["THESEUS_NATIVE_HELPER_BINARY"])
                    changed.chmod(0o700)
                    changed.write_bytes(b"different-helper")
                    changed.chmod(0o500)
                def wait(self, timeout):
                    return 0
            with guard, patch.object(common.subprocess, "Popen", Changed), self.assertRaises(ValueError):
                common.run_logged([paths["native_services_platforms"], "native::first"], {}, root / "changed.log", artifacts=guard)
            self.assertIn(b"1 passed", (root / "changed.log").read_bytes())
            self.assertEqual(json.loads((root / "changed.log.pins-after.json").read_text())["status"], "refused")

    def test_mutable_missing_or_replaced_copy_directory_is_refused(self):
        import native_platform_ci as common
        for change in ("writable", "missing", "directory"):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                paths, _, guard = self.copied_batch(root)
                with guard:
                    if change == "writable":
                        Path(paths["native_services_platforms"]).chmod(0o700)
                    elif change == "missing":
                        Path(paths["native_services_platforms"]).parent.chmod(0o700)
                        Path(paths["native_services_platforms"]).unlink()
                        Path(paths["native_services_platforms"]).parent.chmod(0o500)
                    else:
                        original = root / "pinned-artifacts"
                        original.rename(root / "original-artifacts")
                        original.mkdir(mode=0o500)
                    with patch.object(common.subprocess, "Popen", side_effect=AssertionError("unknown artifact dispatched")), self.assertRaises(ValueError):
                        common.run_logged([paths["native_services_platforms"], "--list"], {}, root / "refusal.log", artifacts=guard)
                self.assertEqual(json.loads((root / "refusal.log.pins-before.json").read_text())["status"], "refused")

    def test_native_tool_dispatch_checks_copied_fixture_guards(self):
        import native_platform_ci as common
        with tempfile.TemporaryDirectory() as directory, ExitStack() as stack:
            root = Path(directory)
            paths, _, guard = self.copied_batch(root)
            stack.enter_context(guard)
            tool_path = root / "qualified-native-tool"
            tool_path.write_bytes(b"qualified-native-tool")
            tool_path.chmod(0o500)
            # Exercise real ToolOwner dispatch/verification with a retained local
            # file only; no platform executable or native API is invoked.
            tool = self.mac.ToolOwner.__new__(self.mac.ToolOwner)
            tool.path = tool_path
            tool.file = stack.enter_context(tool_path.open("rb"))
            tool.fingerprint = self.mac.file_fingerprint(os.fstat(tool.file.fileno()))
            tool.sha256 = hashlib.sha256(tool.file.read()).hexdigest()
            tool.artifacts = guard
            self.replacement(paths["THESEUS_NATIVE_HELPER_BINARY"])
            with patch.object(self.mac.subprocess, "Popen", side_effect=AssertionError("native tool dispatched")), self.assertRaises(ValueError):
                tool.run(["print", "user/501/theseus.ci.fixture"], {}, root / "native.json")
            self.assertEqual(json.loads((root / "native.json.pins-before.json").read_text())["status"], "refused")

    def test_windows_acl_is_protected_exact_two_distinct_principals(self):
        helper = "S-1-5-21-10-20-30-1001"
        good = [("S-1-5-18", 0x1200a9, 0), (helper, 0x1301a9, 0)]
        self.win.validate_acl(helper, "executable", True, good)
        for principal, protected, grants in [(helper, False, good), ("S-1-5-18", True, good), (helper, True, good + [("S-1-5-32-545", 0x1200a9, 0)]), (helper, True, [("S-1-5-18", 0x1200a9, 0), (helper, 0x1f01ff, 0)]), (helper, True, [("S-1-5-18", 0x1200a9, 0), (helper, 0x1301a9, 0x10)])]:
            with self.assertRaises(ValueError):
                self.win.validate_acl(principal, "executable", protected, grants)
        self.win.validate_absence(1060)
        for error in [0, 5, 1072, 123]:
            with self.assertRaises(ValueError):
                self.win.validate_absence(error)


if __name__ == "__main__":
    unittest.main()
