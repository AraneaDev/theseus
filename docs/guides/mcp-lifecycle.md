# MCP lifecycle and client registration

This schema-8 capability was introduced in Theseus 0.11. It rehearses external
client/server registration inside disposable Linux guests; it is separate from
[using Theseus as an MCP server](../agents/mcp-server.md). Run repository
commands from the repository root and prepare the explicitly supplied native
client files described below.

Schema 8 adds `mcp_stdio` checks and an optional profile `mcp` lifecycle
contract. Standalone servers reuse `clean` and `upgrade`; plugin-provided
servers reuse `plugin_lifecycle`. Each contract selects one scenario. Native
registration and health remain separate from a typed stdio probe that checks
actual received server identity and the complete named tool inventory. No
`tools/call`, model, authentication, host-client execution or HTTP transport is
involved.

| Version-bound adapter | Delivery / client scope | Native claim |
|---|---|---|
| `claude-code-2.1.312` | standalone user/project/local; plugin user | Target alias/source native health |
| `codex-cli-0.166.0` | standalone global in private `CODEX_HOME` | Configuration only; native activation not exposed/tested |

Both adapters require independent discovery at each active checkpoint. Codex
list/get JSON establishes configuration, never Connected or activation. Claude
health may use modern discovery while the separate typed probe explicitly uses
legacy; legacy probe coverage does not establish native-client legacy use.
Claude project examples declare a named `probe` approval in disposable private
user settings. The pinned client otherwise reports Pending approval; this tests
a preapproved fixture, not the interactive approval or workspace trust flow.
Disabled/removed checkpoints prove successful native inactivity/absence and do
not run a typed server probe. Existing plugin component proofs remain required.

Prepare an explicit Linux ELF using [the MCP
fixtures](../../examples/mcp/README.md). The preparer copies that file and
fixture assets, records SHA-256/size/expected version and never searches PATH,
reads host settings or downloads a client. Guest `--version` must independently
match the adapter. All lifecycle commands run through the shared Docker engine
under UID:GID 10001:10001, network none, private HOME/config/project and empty
`pass_env`. Images must already provide runtime libraries and shell/core tools;
no guest interpreter or SDK is installed.

The supported protocol profiles are explicit `2026-07-28` (`server/discover`,
then `tools/list`) and `2025-11-25` (`initialize`, fully written
`notifications/initialized`, then `tools/list`). There is no negotiation or
fallback. Modern requests carry per-request protocol metadata; startup validates
modern server-info metadata, tools capability and supported version. Both
profiles reject duplicate JSON keys, excessive nesting, unexpected/duplicate
responses, wrong ID types, repeated cursors and mismatched identity/inventory.
Discovery follows bounded pagination to a terminal page and compares exact
unique names.

| Probe bound | Limit |
|---|---|
| Total execution, including preparation/shutdown/receipt | 3–120 seconds; default 15, final 2 reserved |
| Emergency owned cleanup | Separate maximum 3 seconds |
| Newline-delimited frame / stdout / frames | 65,536 bytes / 262,144 bytes / 256 |
| Tool pages / tools / cursor | 8 / 128 / 256 UTF-8 bytes |
| Outgoing bytes / outstanding request / pending frame | 16,384 / 1 / 1 |
| Captured stdout / stderr | min(run log limit, 262,144 / 65,536 bytes) |
| Retained serialized proof / diagnostic / JSON nesting | 1 MiB / 512 UTF-8 bytes / 64 containers |

The pure `McpSession` state machine does not launch anything. The Docker adapter
uses interactive `exec -i` without a TTY, the existing nonblocking process
supervisor and nonce completion receipt. EOF closes the direct server;
cancellation, backpressure, deadlines and failed shutdown remain bounded. Forced
termination never invents a normal application exit. Exact owned-ID/label checks
precede emergency removal, and strict missing-target inspection establishes
absence. Whole-container teardown is authoritative containment: graceful
completion does not prove that every detached guest descendant already exited.
An unavailable container suppresses later guest steps/checks. Protocol
mismatches are Failed; acquisition, capture, shutdown and cleanup gaps are
Incomplete; cancellation keeps precedence.

Registration checks consume complete native JSON with bounded scalar/absence/
identity-count assertions. The v1 adapters reserve `companion` with `/bin/true`
and no args as a configuration preservation witness, plus supported unrelated
settings. That inert companion can fail native health while the target succeeds.
Claude target scope maps must be unique; Codex global is a private config home,
not a CLI scope flag. Plugin registration binds the exact marketplace-qualified
PluginContract ID, a global count, a uniquely guarded inventory index and native
`readFromFolder`. Supported source-bearing launches are literal
`${CLAUDE_PLUGIN_ROOT}/<normalized relative code path>` in executable argv0, or
script argv1 with argv0 exactly `/bin/sh`. Other layouts use generic checks.
Inventory reordering fails the declared index assertion; no order-independent
registry or exhaustive unrelated-alias audit is promised.

Live and saved evidence show the client claim/scope independently from received
protocol identity/tools, shutdown and issues. Raw response bodies remain
separate paged logs. Retained response proof is reparsed for consistency;
reports are not authenticated attestations. Schema 8 is required for MCP
checks/contracts; recipe schemas 1–7 retain their semantics and plugin contracts
remain valid in 7 and 8. Reports write 8 and read 1–8;
journal/ownership/comparison schemas stay 1.

Native acceptance uses explicit vendor files only, with actual guest versions,
artifact/image/source/argv manifests, independent parsing of saved responses and
strict absence for every owned ID. The opt-in harness tests both eras, all
matrix scopes, upgrades, plugin disable/enable/removal, preservation and
deliberate registration/protocol/deadline/cancel/shutdown faults. Missing native
inputs remain clearly ignored/unrun. Required Linux CI runs self-contained
Docker transport acceptance without downloading native clients. The 0.12
controller matrix is separate from native macOS/Windows guest adapters. The
[Theseus MCP service](../agents/mcp-server.md) uses the same engine through its
own interface and required native acceptance; these guest-client probes do not
establish that acceptance.

## Related documentation

- [MCP fixture walkthrough](../../examples/mcp/README.md): prepare version-bound inputs.
- [Plugin lifecycle](plugins.md): the plugin delivery contract.
- [Recipe reference](../reference/recipes.md): scenarios, profiles and typed checks.
- [Validation](../contribute/validation.md): self-contained versus opt-in native-client acceptance.

[Documentation index](../README.md) · [Project README](../../README.md)

