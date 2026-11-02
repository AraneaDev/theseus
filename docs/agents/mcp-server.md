# Use Theseus as an MCP server

Install the [native controller](../get-started/installation.md) and create
canonical project/evidence directories first. Launch commands are shell
examples; recipe paths in tool arguments are relative to the declared project
root. This page serves Theseus itself to an MCP client. To rehearse registration
of another server inside a Linux guest, use [MCP lifecycle
checks](../guides/mcp-lifecycle.md).

Release acceptance requires the same source to pass all three native host jobs
and all eight Linux Docker suites. Native inherited-stdio qualification must
cover both ordinary Rust launchers and CPython `Popen` / `CreatePipe` launchers,
alongside actual-binary suites. Every required launcher must pass; a passing
Rust launcher alone does not establish platform acceptance. Consult the
[source-specific CI
results](https://github.com/AraneaDev/theseus/actions/workflows/ci.yml).

`theseus serve` exposes the same lifecycle engine and outcomes over ordinary
stdin/stdout pipes. It has no model dependency or listening network service.
Launch it with explicit native filesystem roots:

```sh
theseus serve --project-root /absolute/project --output-root /absolute/evidence --max-run-seconds 3600
```

On Windows, supply native paths such as `C:\work\project` and
`C:\work\evidence`. Both roots are required, acquired once with strict no-follow
ancestry checks; create canonical directories first on systems with aliased
temporary ancestors. Tool recipe paths use portable relative `/` paths beneath
the project root. These roots constrain interface access under your OS
authority; recipe scripts still run according to their declared execution
contract. Startup may create the output root. Inspection, discovery, listing,
status and evidence reads never prepare Docker, pull an image or execute a
recipe.

The ordered tool inventory is exactly:

| Tool | Purpose |
| --- | --- |
| `recipe_inspect` | Read an overview or page sanitized recipe records. |
| `run_start` | Admit a digest-bound selection; acceptance is not run success. |
| `run_list` | Page this server process's admissions. |
| `run_status` | Poll cached state or request one saved report refresh. |
| `run_cancel` | Request cancellation of an admitted run. |
| `run_evidence` | Page saved report records and raw log bytes. |

Legacy MCP `2025-11-25` uses initialize/initialized. Each line below is a
separate JSON-RPC frame in one legacy session:

<!-- mcp-example:legacy --> ```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"local-client","version":"1"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"recipe_inspect","arguments":{"recipe_path":"theseus.toml"}}}
```

Modern MCP `2026-07-28` permits direct calls with version and client-capability
metadata on every request. These are request shapes: replace the all-zero
`server_instance`, `handle` prefix and `recipe_digest` with the actual values
returned by this process's inspection/admission before making dependent calls.
Use fresh RPC IDs; reuse the exact `request_key` and arguments only to retry the
same admission. The examples are parsed by the production wire/domain tests.

<!-- mcp-example:modern --> ```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}},"name":"recipe_inspect","arguments":{"recipe_path":"theseus.toml"}}}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}},"name":"run_start","arguments":{"server_instance":"00000000000000000000000000000000","request_key":"local-attempt-1","recipe_path":"theseus.toml","recipe_digest":"0000000000000000000000000000000000000000000000000000000000000000","profiles":["user"],"scenarios":["clean"],"pull":false}}}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}},"name":"run_list","arguments":{"server_instance":"00000000000000000000000000000000","limit":32}}}
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}},"name":"run_status","arguments":{"server_instance":"00000000000000000000000000000000","handle":"00000000000000000000000000000000:1","refresh":true}}}
{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}},"name":"run_cancel","arguments":{"server_instance":"00000000000000000000000000000000","handle":"00000000000000000000000000000000:1"}}}
{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}},"name":"run_evidence","arguments":{"server_instance":"00000000000000000000000000000000","handle":"00000000000000000000000000000000:1","selector":{"kind":"catalog","collection":"run"}}}}
```

Tool results carry `format_version:1`, `server_instance`, `ok` and either `data`
or a typed `error`, in `structuredContent` and the identical JSON text content.
Poll at the returned `poll_after_ms` (500). `finished` requires the engine
worker to return and join; inspect `engine_result.outcome` and counts separately
from state. Saved evidence is an exact report snapshot; its revision can differ
from historical engine results if evidence was subsequently replaced.

Recipe reads are capped at 1 MiB. Input and complete output frames are at most
65,536 bytes **including the newline**; duplicate keys and nesting deeper than
64 are rejected. RPC IDs are strings of at most 128 UTF-8 bytes or exact signed
64-bit integers; null and floating-point IDs are rejected. Four slots cover
processing, queued responses and partial writes. One query worker has a
30-second response deadline; a late worker remains owned until it returns. Only
one engine execution/finalizer is active; there is no run queue.

Catalog and run-list pages default to 32 items, accepting 1–32. Record/log pages
default to 4096 raw bytes, accepting 1–4096. Replies can shrink a page to fit
both copies of the encoded result. Follow opaque `next_cursor` values, repeating
the same selector, revision/digest and effective limit. A cursor is
process-bound, at most 256 ASCII bytes (v1 emits 188 lowercase hex characters).
Restart paging when the returned error requests it; pagination exposes retained
bytes and does not recover capture truncation. Reports keep their existing 64
MiB disk-read cap; parsed allocations and the engine's live report are
additional to that bound.

Inspection returns environment **names** and redacts literal/inherited values;
it never queries inherited values. Public recipe parse/validation diagnostics
omit source excerpts and values; local `theseus validate` provides detailed
diagnostics. Saved command records retain their existing public literal metadata
and raw logs can contain sensitive output. The recipe's `recipe_digest` is the
source digest: SHA-256 of exact source bytes, including newline representation.
It is not an immutable snapshot of artifact contents or environment values.
`run_start` rereads, checks the digest and passes the validated source to the
shared engine.

Admissions and idempotency are process-local: at most 128 keys, no eviction or
reuse until exit. Retrying the same key and canonical arguments recovers the
same handle; changing its arguments is `idempotency_conflict`. A lost response
leaves the client uncertain whether admission/execution began until that exact
retry or polling resolves it. Restarting the server loses the registry and
duplicate suppression; it does not establish crash-safe exactly-once execution
or import old reports as live jobs.

`run_cancel` cancels a run by handle. RPC cancellation concerns the outstanding
request and is best effort: after a start response's final newline, cancelling
that RPC ID does not cancel the run. Run deadlines request the existing engine
cancellation token; `--max-run-seconds` accepts 1–86400, default 3600. Input
EOF, transport failure, OS stop and output stalls shut down the process and
request active-run cancellation. Shutdown gets five seconds to join owned work;
unresolved work forces process exit **2**, which means **incomplete cleanup**,
not successful joining. Runs are not supported background jobs after server
exit. Inspect saved evidence and explicitly recover verified journaled resources
with:

```sh
theseus cleanup /absolute/evidence/REPORT_ID
```

Native ordinary-pipe and actual-binary acceptance on all three supported hosts
is required at the same source before claiming release acceptance; check the
[source-specific CI
results](https://github.com/AraneaDev/theseus/actions/workflows/ci.yml). Linux
Docker parity supplements these gates; it does not implement native
macOS/Windows guests, VMs, services or desktop apps.

## Related documentation

- [Recipes](../reference/recipes.md): the lifecycle engine and maintainer-controlled inputs.
- [Evidence and limits](../reference/evidence.md): saved outcomes, schemas and log privacy.
- [Recovery](../operate/recovery.md): inspect incomplete shutdown and journaled resources.
- [Validation](../contribute/validation.md): source-specific native stdio gates.

[Documentation index](../README.md) · [Project README](../../README.md)

