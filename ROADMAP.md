# Theseus roadmap

Theseus 0.4 ships a shared Docker lifecycle engine, Ratatui workbench, headless
CLI, versioned recipes/reports, private bounded logs, user/system profiles,
clean/upgrade/reinstall, interrupted-install/upgrade and downgrade scenarios, explicit
assertions, cancellation and verified recovery.
Self-contained fixtures establish these declared container contracts. A separate
real Phrourion binary-copy rehearsal covers clean/upgrade and reinstall.
Reinstall uses explicit schema-2 declarations and stable path/subtree
contracts across two installer invocations, with report-2 evidence and version-1
recovery journals. Both profiles pass real healthy/broken Docker acceptance and
live/offline PTY comparison browsing. This observes declared stability rather
than proving universal idempotency.

Schema 3 adds explicit interruption and forward recovery: a nonce-bound
cooperative barrier, verified whole-container SIGKILL, same-ID restart without
setup/input recopy, partial-state assertions, explicit recovery and runtime/data
contracts. Both profiles pass real Docker acceptance and live/offline PTY proof
and comparison browsing. Reports retain this schema-3 evidence; recovery journals
remain version 1. This establishes the declared container observations,
not host power-loss durability or arbitrary fault timing.

Schema 4 adds explicit per-profile compatible downgrade or safe rejection. Both
start from the newer target, configure/exercise real data, then attempt the older
base without reconfiguration. Compatible contracts require an actual older-version
data reader. Declared refusal requires exact application status/marker evidence,
unchanged binary and recursive data scopes, and retained newer runtime checks.
Both profiles pass healthy/broken real Docker acceptance and live/offline PTY
proof/comparison browsing. Reports write schema 4 and read schemas 1/2/3/4;
recipe schemas 1/2/3 remain supported, and journal/ownership versions remain 1.
These observations establish the declared policy and scopes, not arbitrary
version compatibility or automatic restoration.

The following capabilities are planned, not available features.

## Additional lifecycle scenarios

- **Required next: rollback to a declared known-good state.** Verify the failure
  point, invoke an explicit rollback command, and verify restored runtime and
  data. Distinguish binary reversal from data restoration; restoration claims
  require scoped backup/restore evidence, including failed and later interrupted
  upgrades. Rerunning an older installer alone cannot prove data restoration.
  Forward recovery and downgrade do not implement rollback.
- Comparisons between installation methods and image versions.
- More scalable metadata/log paging for large reports and preview editing.

## Claude Code plugins

- Install, update, disable and remove plugins in a disposable host configuration.
- Verify plugin inventory, hooks, skills, commands and configuration changes.
- Detect duplicate/stale registrations while preserving unrelated settings.
- Record tested client versions and distinguish local checks from authenticated
  model-backed checks that need explicitly supplied credentials.

## MCP lifecycle and client registration

- Rehearse standalone and plugin-provided MCP server installations.
- Support explicit client configuration adapters, startup/handshake/discovery
  checks, registration preservation and duplicate detection.
- Expose recipe inspection, run creation, status, cancellation and evidence through
  an MCP interface using the same engine and outcome semantics.

## Services, desktop applications and other operating systems

- Machine/VM adapters where containers cannot establish service/session fidelity.
- Observe systemd units, launcher entries and application process lifecycles.
- Native macOS and Windows environment adapters with OS-specific contracts.

Theseus will continue to require explicit lifecycle contracts and distinguish
observations from assumptions. New interfaces must share the scenario engine.
