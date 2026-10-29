# Theseus roadmap

Theseus 0.5 ships a shared Docker lifecycle engine, Ratatui workbench, headless
CLI, versioned recipes/reports, private bounded logs, user/system profiles,
clean/upgrade/reinstall, interrupted-install/upgrade, downgrade and failed-upgrade
rollback scenarios, explicit assertions, cancellation and verified recovery.
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
proof/comparison browsing. Schema-4 evidence remains readable. Recipe schemas
1/2/3/4 remain supported, and journal/ownership versions remain 1.
These observations establish the declared policy and scopes, not arbitrary
version compatibility or automatic restoration.

Schema 5 adds rollback after an ordinary declared failed upgrade. A known-good
base runs and reads real data before the candidate changes observed state and
returns an exact declared failure. Explicit rollback restores the binary and
proves the bound old runtime and actual data reader. `binary_only` retains data;
`restore_data` adds complete source-to-stored backup comparisons, fresh backup
validation before restoration, restored scopes and backup cleanup. Both profiles
pass real Docker and live/offline narrow PTY acceptance. Reports write schema 5
and read schemas 1/2/3/4/5. These are observed scoped contracts, not generic host
backup/restore, atomic snapshots or database-consistency guarantees.

The following capabilities are planned, not available features.

## Additional lifecycle scenarios

- **Required next: rollback after an interrupted upgrade.** Combine verified
  interruption and same-ID restart with the known-good backup/rollback contract.
  The current interruption scenarios run explicit forward recovery; schema-5
  rollback handles an ordinary declared failure. Interrupted rollback is not shipped.
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
