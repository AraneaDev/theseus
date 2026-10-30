# Theseus roadmap

Theseus 0.7 ships a shared Docker lifecycle engine, Ratatui workbench, headless
CLI, versioned recipes/reports, private bounded logs, user/system profiles,
clean/upgrade/reinstall, interrupted-install/upgrade, downgrade and failed-upgrade
rollback scenarios, interrupted-upgrade rollback, decorative startup/exit animations,
explicit assertions, cancellation and verified recovery.
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
pass real Docker and live/offline narrow PTY acceptance. Schema-5 evidence remains
readable. These are observed scoped contracts, not generic host
backup/restore, atomic snapshots or database-consistency guarantees.

Schema 6 adds explicit rollback after a deliberately interrupted upgrade, with
both `binary_only` and `restore_data` policies for both profiles. A cooperative
nonce-bound barrier precedes verified whole-container SIGKILL and same-ID restart
without setup/input recopy. Changed evidence uses the immediate `before_interrupt`
reference; original `before_upgrade` remains immutable for binary/restoration and
fresh mapped-backup readiness checks. Bound original runtime and actual old-tool
readers retain typed expected-exit/literal-match and command/log evidence. Ordinary
failed-upgrade rollback keeps its separate normal-failure contract. Both policies
pass real Docker and live/offline narrow PTYs, including missing-proof, corrupt
backup, no-op/partial restore, metadata/reader/runtime faults and cancellation.
Reports write schema 6 and read schemas 1 through 6; journals and ownership stay
version 1. These are scoped cooperative observations with author-managed
quiescence, not arbitrary fault timing, database consistency or host durability.

Interactive startup and ordinary idle exit animate Theseus entering and retracing
the labyrinth along Ariadne's thread. Each default scene has six 900ms frames
(5.4 seconds); Space skips startup and Space/q skips exit. Global `--no-animation`,
TERM=dumb and small terminals skip scenes; headless/JSON/validation never animate.
Scenes are decorative and make no lifecycle-progress or success claim. Active
quit cancels immediately, and signals/errors/cleanup deadlines bypass exit scenes.
Actual normal and active Docker PTYs verify controls and terminal restoration.

Comparison suites add explicit baseline variants, profiles/scenarios, exact stages
and application scopes through the existing engine. Self-contained copy/archive
and two distinct image-ID fixtures cover both profiles, deliberate scoped differences,
failed sources, cancellation, owned cleanup and offline JSON. The image fixture is
Ubuntu 24.04 plus a content-marker derivative, not two distro releases. Real PTYs
cover preview with explicit start, live/offline selection, active cancellation and
terminal restoration. Equivalent/Different/Unknown retain source lifecycle outcome
precedence and exact image/recipe/artifact identity. Comparison artifacts use schema
1, source report writers remain 6 and journal/ownership remain 1. These scoped
observations do not establish arbitrary method equivalence or package compatibility.

The following capabilities are planned, not available features.

## Next: scalable browsing

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
