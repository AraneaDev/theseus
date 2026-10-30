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
plugins, and binds `${CLAUDE_PLUGIN_ROOT}` to the actual `readFromFolder` source rather than the
cache. The example installs the plugin companion first and asserts target index 1
with its exact ID guard. Unexpected native reordering fails closed; the fixture
does not promise order-independent inventory processing. Other assertions may
prove another guarded index without changing the adapter.

Native health capture has its own file bound and timeout; a nonzero native status
never becomes absence proof. Guest production uses shell/core tools only, Linux
UID:GID 10001:10001 and network none. The image must already provide the native
client's runtime libraries and core tools; preparation does not install them.
Native commands, config observations and fixture requests establish no credentials,
model use, tools/call, host configuration fidelity or companion activation.

`tests/support/mcp.rs` is explicitly SYNTHETIC: raw native-shaped documents and
McpSession-generated response evidence exercise contract and engine behavior.
The matrix test supplies an inert ELF-header byte fixture that is never executed.
Neither test satisfies vendor-native acceptance. Actual supplied-client acceptance
and presentation are a separate task, with guest version/artifact proof and owned
container cleanup required. Retained reports check internal consistency of finite
observations and invocation bindings, not authenticated reconstruction of raw logs.
