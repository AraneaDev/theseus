"""Independent PRIVATE design arithmetic/oracle; never imported by Rust tests.

Uses Python's compact ASCII JSON solely to spell the frozen tables independently
of the future Rust codec. Output is a checked-in private design artifact, not
native or runtime acceptance. Re-run manually after a reviewed schema change.
"""
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).parent
ID = "f" * 32
DIGEST = "f" * 64
U64 = (1 << 64) - 1


def compact(value):
    return json.dumps(value, separators=(",", ":"), ensure_ascii=True).encode("ascii")


def record(index, entry):
    return compact({"schema": 1, "index": index, "record": entry})


def header(os):
    return {"kind": "header", "profile": "trusted_fixture_v1", "binding": ID,
            "manifest_digest": DIGEST, "workspace_marker": ID, "os": os,
            "durability": "file_flushed_process_crash"}


outcomes = []
for cleanup, statuses in [
    ("posix_leader_reaped_group_termination_requested", [
        {"kind": "posix_exit", "code": 255},
        {"kind": "posix_signal", "signal": 255}]),
    ("windows_process_exited_job_empty", [
        {"kind": "windows_exit", "code": (1 << 32) - 1}]),
]:
    for status in statuses:
        for reason in ["natural", "cancel_requested", "deadline"]:
            outcomes.append({"kind": "run_completed", "reason": reason,
                             "status": status, "cleanup": cleanup,
                             "stdout_bytes": U64, "stderr_bytes": U64,
                             "truncated": False})
    for phase in (["may_run"] if cleanup.startswith("posix") else
                  ["may_run", "created_suspended"]):
        outcomes.append({"kind": "launch_failed", "phase": phase,
                         "code": "invalid_catalog", "cleanup": cleanup})
outcomes += [
    {"kind": "not_started", "reason": "cancel_requested"},
    {"kind": "not_started", "reason": "deadline"},
    {"kind": "file_create_failed", "code": "already_exists"},
    {"kind": "file_create_failed", "code": "permission"},
    {"kind": "file_created", "length": 4096, "digest": DIGEST},
    {"kind": "launch_failed", "phase": "never_created",
     "code": "invalid_catalog", "cleanup": "no_child_created"},
]

entries = {
    "header": [(0, header(os)) for os in ["linux", "macos", "windows"]],
    "lease_reserved": [(1, {"kind": "lease_reserved", "intent": ID, "lease": ID})],
    "operation_accepted": [(256, {"kind": "operation_accepted", "lease": ID,
                                "operation": ID, "request_digest": DIGEST,
                                "operation_kind": kind})
                           for kind in ["create_file", "run"]],
    "operation_finished": [(257, {"kind": "operation_finished", "lease": ID,
                                  "operation": ID, "outcome": outcome})
                           for outcome in outcomes],
    "lease_closed": [(258, {"kind": "lease_closed", "lease": ID,
                             "disposition": disposition}) for disposition in [
        {"kind": "awaiting_external_reset"},
        {"kind": "quarantined", "reason": "helper_restarted_with_unfinished_operation"},
        {"kind": "quarantined", "reason": "ledger_write_uncertain"},
        {"kind": "quarantined", "reason": "process_state_unproved"},
        {"kind": "quarantined", "reason": "file_effect_uncertain"}]],
}
maxima = {}
for kind, variants in entries.items():
    body = max((record(index, entry) for index, entry in variants), key=len)
    maxima[kind] = {"body_bytes": len(body), "framed_bytes": len(body) + 36,
                    "maximizing_body_ascii": body.decode("ascii")}
history_bound = (maxima["header"]["framed_bytes"] +
                 maxima["lease_reserved"]["framed_bytes"] +
                 128 * (maxima["operation_accepted"]["framed_bytes"] +
                        maxima["operation_finished"]["framed_bytes"]) +
                 maxima["lease_closed"]["framed_bytes"])
assert maxima["operation_finished"]["framed_bytes"] <= 4096
assert maxima["lease_closed"]["framed_bytes"] <= 8192
assert history_bound <= 1048576

requests = [
    {"kind": "run", "lease": "2" * 32, "operation": "3" * 32,
     "program": "probe_success"},
    {"kind": "create_file", "lease": "2" * 32, "operation": "4" * 32,
     "path": "probe.bin", "bytes": "00ff"},
]
request_oracles = []
for request in requests:
    raw = compact(request)
    request_oracles.append({"request_ascii": raw.decode("ascii"),
        "domain_hex": b"theseus-native-request-v2\0".hex(),
        "digest_hex": hashlib.sha256(b"theseus-native-request-v2\0" + raw).hexdigest()})
header_body = record(0, header("windows"))
header_frame = len(header_body).to_bytes(4, "big") + header_body + hashlib.sha256(header_body).digest()
artifact = {"outcome_branch_coverage": outcomes, "purpose": "private independent schema arithmetic; no runtime/native proof",
            "record_maxima": maxima, "conservative_full_history_bytes": history_bound,
            "finish_reservation_bytes": 4096, "close_reservation_bytes": 8192,
            "request_digest_oracles": request_oracles,
            "literal_header_body_ascii": header_body.decode("ascii"),
            "literal_header_digest_hex": hashlib.sha256(header_body).hexdigest(),
            "literal_header_frame_hex": header_frame.hex()}
leaf = "A" * 64
principal = {"kind": "windows", "token_user_sid": "S-1-" + "9" * 252}
programs = [letter + "z" * 31 for letter in "abcdefgh"]
large_requests = [
    {"kind": "describe"},
    *[{"kind": kind, "intent": ID, "binding": ID, "manifest_digest": DIGEST}
      for kind in ["acquire", "query_acquire"]],
    {"kind": "create_file", "lease": ID, "operation": ID, "path": leaf, "bytes": "ff" * 4096},
    {"kind": "read_file", "lease": ID, "path": leaf, "offset": U64 - 4096, "length": 4096},
    {"kind": "inspect_file", "lease": ID, "path": leaf},
    {"kind": "run", "lease": ID, "operation": ID, "program": "a" + "z" * 31},
    *[{"kind": kind, "lease": ID, "operation": ID} for kind in ["query_operation", "cancel"]],
    *[{"kind": kind, "lease": ID} for kind in ["release", "query_lease"]],
]
large_results = [
    {"kind": "description", "profile": "trusted_fixture_v1", "authority": "explicit_fixture_only",
     "binding": ID, "manifest_digest": DIGEST, "workspace_marker": ID, "os": "windows",
     "helper_principal": principal, "programs": programs},
    {"kind": "file_bytes", "lease": ID, "path": leaf, "offset": U64 - 4096,
     "bytes": "ff" * 4096, "eof": False},
    {"kind": "file_info", "lease": ID, "path": leaf, "length": 4096, "digest": DIGEST},
    {"kind": "refused", "code": "awaiting_external_reset"},
]
for outcome in outcomes:
    large_results.append({"kind": "operation", "lease": ID, "operation": ID,
                          "state": {"kind": "finished", "outcome": outcome}})
for state in [
    {"kind": "held", "intent": ID},
    {"kind": "awaiting_external_reset", "intent": ID},
    {"kind": "quarantined", "intent": ID,
     "reason": "helper_restarted_with_unfinished_operation"},
]:
    large_results.append({"kind": "lease", "lease": ID, "state": state})
    large_results.append({"kind": "acquired", "intent": ID, "binding": ID,
                          "manifest_digest": DIGEST,
                          "state": {"kind": "lease", "lease": ID, "state": state}})
messages = {
    "hello": [{"kind": "hello", "role": "controller", "protocol_min": 2, "protocol_max": 2}],
    "request": [{"kind": "request", "request": value} for value in large_requests],
    "response": [{"kind": "response", "reply_to": U64 - 1, "result": value} for value in large_results],
    "output": [{"kind": "output", "lease": ID, "operation": ID, "channel": "stderr",
                "offset": U64 - 4096, "bytes": "ff" * 4096}],
    "log": [{"kind": "log", "lease": ID, "operation": ID, "code": "output_truncated"}],
    "closed": [{"kind": "closed", "code": "sequence_exhausted"}],
}
wire_bounds = {}
for kind, variants in messages.items():
    max_body = max(len(compact({"version": 2, "session": ID,
                                "sequence": 0 if kind == "hello" else (U64 if kind == "closed" else U64 - 1),
                                "message": message})) for message in variants)
    assert max_body <= 65532
    wire_bounds[kind] = {"body_bytes": max_body, "framed_bytes": max_body + 4}
artifact["wire_message_width_bounds"] = wire_bounds
(ROOT / "protocol-v2-size-and-oracles.json").write_text(json.dumps(artifact, indent=2) + "\n")
print(json.dumps({"record_maxima": {k: {x: v[x] for x in ["body_bytes", "framed_bytes"]}
                                         for k, v in maxima.items()},
                  "conservative_full_history_bytes": history_bound}, indent=2))
