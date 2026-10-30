# Supplied native MCP lifecycle fixtures

Prepare an explicit native Linux ELF client without launching it on the host:

```sh
python3 examples/mcp/prepare.py --client /explicit/path/to/claude \
  --client-version '2.1.312 (Claude Code)' --adapter claude-code-2.1.312 \
  --delivery standalone --scope user --scenario upgrade \
  --protocol 2026-07-28 --output /tmp/new-mcp-workspace --image ubuntu:24.04
cargo run --offline -- --no-animation validate --recipe /tmp/new-mcp-workspace/theseus.toml
cargo run --offline -- --no-animation run --headless \
  --recipe /tmp/new-mcp-workspace/theseus.toml --profile user --scenario upgrade
```

The output must be a new directory outside checkout `examples/` and `tests/`.
Preparation copies only the explicit ELF and fixture assets, records SHA-256,
size, expected version, image and provenance in `input-manifest.json`, and never
searches PATH, runs the client, copies configuration or downloads dependencies.
Version is expected until the exact guest version command succeeds. The ELF magic
check is an input guard, not verification of vendor provenance or executable ABI.
Keep native binaries and generated workspaces private and uncommitted.

Each generated recipe declares one scenario. Supported combinations are:

| Adapter | Delivery / scope | Scenarios | Native claim |
|---|---|---|---|
| `claude-code-2.1.312` | standalone user/project/local | clean, upgrade | NativeHealth |
| `claude-code-2.1.312` | plugin user | plugin_lifecycle | NativeHealth |
| `codex-cli-0.166.0` | standalone global | clean, upgrade | ConfigurationOnly |

Both explicit protocol choices, `2026-07-28` and `2025-11-25`, have distinct v1/v2
server identities and tool inventories (`1.0.0`/`hello_v1`, `2.0.0`/`hello_v2`).
The fixture responds in the era of each process’s actual startup request. The
independent typed probe selects its declared era without negotiation or fallback;
Claude native health can independently use modern discovery even when the typed
probe selects legacy. A legacy typed result does not claim native-client legacy
use. Codex global means private `CODEX_HOME`, without a
native CLI scope flag. Codex uses native add/upsert, list/get JSON and remove;
**native activation is not exposed/tested**, and no Connected claim is made.
Every active checkpoint independently probes the actual stdio response bytes.
Claude project fixtures explicitly approve only `probe` through
`enabledMcpjsonServers` in the disposable guest's private user `settings.json`.
Without that seed, the actual 2.1.312 client reports Pending approval and the
NativeHealth check fails. This rehearses a declared preapproved configuration;
it does not test an interactive approval prompt or workspace trust. The setting
is documented in the [official settings reference](https://code.claude.com/docs/en/settings-reference#enabledmcpjsonservers)
and was confirmed by a same-client guest A/B health check.
Disabled and removed checkpoints do not launch the server through a typed probe.
The small fixture server accepts the simple string/integer request IDs used by
these version-bound clients; it is not a general-purpose MCP implementation.

The v1 lifecycle adapter reserves standalone identity `companion` with exact
`/bin/true` command and an empty args array, in Claude user scope or Codex private
global scope. It preserves Claude's supported `hasCompletedOnboarding = true`
setting and absence of that companion in project/local maps. The companion is
intentionally inert: preservation is a configuration claim, and its native health
can report failure while the native list command still exits zero. Only the bound
target alias/source/Connected observation (or successful target absence) determines
the target health receipt. Codex preserves the exact three-line supported prefix
`hide_agent_reasoning = true`, `[tui]`, `disable_paste_burst = true`; unfamiliar
formatting fails closed. These conventions are enforced at every checkpoint.
Use generic schema-8 JsonCommand and McpStdio checks without a lifecycle adapter
for custom preservation conventions.

Claude registration embeds complete private `.claude.json` and `.mcp.json`
documents using shell `printf` plus `cat`; no guest JSON/TOML normalizer is used.
The selected target must be absent from other applicable scope maps. Exact argv
means scalar command, each args index and absence at the next index. Unknown
`.mcp.json` keys are excluded from preservation because the native client can
normalize them. Plugin delivery retains every ordinary PluginContract proof from
`examples/plugins`, adds native plugin-list cardinality matched by ID alone, and
detects only the declared target, rather than arbitrary aliases across unrelated
plugins, and binds `${CLAUDE_PLUGIN_ROOT}` to the actual `readFromFolder` source
rather than the cache. V1 supports a source-bearing code path only as executable argv0
`${CLAUDE_PLUGIN_ROOT}/<relative code path>`, or script argv1 with argv0 exactly
`/bin/sh`. The relative code path must be nonempty and have no empty, `.` or `..`
components. A placeholder only in data or another argument, a fixed external
launcher, and other interpreter layouts do not establish this binding; use generic
checks for those layouts. Disabled registration retains the same requirement and
must resolve to the last active concrete launch. The identity-only global count
and unique indexed ID guard both select the exact PluginContract ID, including
marketplace, so a same-name plugin in another marketplace cannot substitute.
The example installs the plugin companion first and asserts target index 1
with its exact ID guard. Unexpected native reordering fails closed; the fixture
does not promise order-independent inventory processing. Other assertions may
prove another guarded index without changing the adapter.

Native health capture has its own file bound and timeout; a nonzero native status
never becomes absence proof. Guest production uses shell/core tools only, Linux
UID:GID 10001:10001 and network none. The image must already provide the native
client's runtime libraries and core tools; preparation does not install them.
The supplied ELF is staged byte-for-byte in the input manifest, then copied inside
the guest to `/home/theseus/.local/bin/theseus-client`, compared with the input,
and explicitly made executable. Recipes do not chmod root-owned `/inputs` or infer
executable modes from a host extension. Preparation emits owned guest text as UTF-8
with LF; arbitrary ELF/archive bytes are never normalized. Windows preparation
requires Python 3.12+ for native file identity. Unix acquisition uses no-follow
opens; Windows rejects reparse points and checks native identity before and after
opening the retained source, without claiming atomic no-follow acquisition.

Native commands, config observations and fixture requests establish no credentials,
model use, tools/call, host configuration fidelity or companion activation.

`tests/support/mcp.rs` is explicitly SYNTHETIC: raw native-shaped documents and
McpSession-generated response evidence exercise contract and engine behavior.
The matrix test supplies an inert ELF-header byte fixture that is never executed.
Neither test satisfies vendor-native acceptance. Actual supplied-client acceptance is opt-in and separate from this synthetic matrix.

Supply both exact native ELF paths and an already prepared immutable image ID:

```sh
export THESEUS_MCP_CLAUDE_CLIENT=/explicit/path/to/claude
export THESEUS_MCP_CODEX_CLIENT=/explicit/path/to/codex
export THESEUS_MCP_TEST_IMAGE=sha256:534baea6a22c03a63003dbc8dbe78fe34bc0d7e595d9a9dc9834884ff530eb55
cargo test --offline --locked --test mcp_lifecycle -- --ignored --nocapture
cargo test --offline --locked --test mcp_tui -- --ignored --nocapture
```

Without these inputs tests stay ignored/unrun; explicit invocation fails clearly,
with no client discovery, download or replay substitution. The CLI harness covers
18 native combinations: standalone Clean/Upgrade in all listed scopes and plugin
user lifecycle, each with modern and legacy direct probes. Guest versions and
artifact hashes bind the actual supplied files. Independent host verification parses
persisted response bytes and compares `hello_v1`/`hello_v2`, rather than trusting
request traces or Connected. Deliberate duplicate/stale registration, setting loss,
wrong inventory, malformed response, ignored EOF, blocked input, timeout and cancel
faults follow successful native base/update observations. Test-only direct faults
use explicit probe environment; native health is checked separately. Cancellation
requires a unique target-v2 marker containing the actual Theseus request ID and
UID10001 on that direct probe's own stderr, then forbids later guest actions.

Evidence under ignored `.superpowers` contains exact source/public-binary digests,
client size/SHA-256/actual guest version, image ID, preparation/CLI argv, input
manifests, native health output, independent response facts, journals and strict
missing-target inspection for every owned ID. Native binaries and private generated
workspaces are never committed. The public PTY harness checks current selected
detail cells in live/offline/cancel sessions, actual narrow/wide resizing, bounded
raw logs/metadata, explicit native claim boundaries, terminal restoration, a Docker
sentinel and unchanged offline reports. This is a text-cell oracle, not physical or
human PNG review. Required Linux CI runs self-contained transport fixtures using
its explicitly prepared image ID; vendor-native tests require supplied artifacts.

Retained reports check internal consistency of finite observations and invocation
bindings, not authenticated reconstruction of raw logs. MCP recipe/report fields
require schema 8; recipes 1–7 retain semantics and reports 1–8 remain readable.
The shared-engine Theseus MCP interface and controller/native OS portability are
separate roadmap milestones.

Keep the preparation script and private `examples/_client_input.py` helper in
the same checkout; the command may run from another working directory.
