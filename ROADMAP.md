# Theseus roadmap

Theseus 0.12 retains the 0.11 shared Docker lifecycle engine, Ratatui workbench, headless
CLI, versioned recipes/reports, private bounded logs, user/system profiles,
clean/upgrade/reinstall, interrupted-install/upgrade, downgrade and failed-upgrade
rollback scenarios, interrupted-upgrade rollback, decorative startup/exit animations,
explicit assertions, cancellation, verified recovery and bounded MCP lifecycle
registration/discovery checks.
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
1, current source report writers use 7 and journal/ownership remain 1. These scoped
observations do not establish arbitrary method equivalence or package compatibility.

Scalable browsing in 0.8 adds typed evidence selection, bounded metadata/detail
and raw-log pages, full retained comparison differences and source fields,
filter-cache overflow navigation, Unicode-safe chunks and explicit capture/storage
labels. Coalesced read-only refreshes preserve input, cancellation and final-result
priority. Synthetic public CLI PTYs establish large-report browsing; real comparison
PTYs cover lifecycle integration. Report/comparison schemas, lifecycle outcomes and
capture/derivation budgets stay unchanged.

## Complete: preview editing (0.9)

- Standalone recipe and comparison suite previews now support bounded raw TOML
  editing, explicit validation and Save Copy to a new adjacent source. A saved
  source runs only after a fresh `r`; there is no autosave or source overwrite.

## Complete: local Claude Code plugins (0.10)

Schema 7 adds native plugin install/update/disable/enable/uninstall through the
shared Docker engine, with bounded typed JSON proofs, exact identity counts,
companion/settings preservation and source/cache component checks. Public CLI
acceptance passed `user`, `project` and `local` client scopes under Linux UID10001
with native `2.1.312 (Claude Code)`, SHA-256
`4f4178ce41d83e23341ebfbaecdbcf8ea6bf7baea0690025abe49eeec3a5d01d`
and image ID
`sha256:534baea6a22c03a63003dbc8dbe78fe34bc0d7e595d9a9dc9834884ff530eb55`.
Deliberate duplicate-list/settings/component faults, timeout/cancel and owned
cleanup are checked; real narrow live/offline PTYs exercise retained proof,
file digest, logs and terminal restoration. Tests use a local inert marketplace,
empty private guest HOME/config/project, network none and no forwarded environment.
Legacy commands appear as Skills; hooks are rehearsed manually. No credentials,
models, automatic client event dispatch or public marketplace are claimed.
Reports write schema 7; the new reader accepts 1 through 7, while older released
readers need not read 7. Journals/ownership/comparison stay version 1. Typed JSON
observations establish report consistency, not authenticated reports or independent
query replay. See [plugin fixtures](examples/plugins/README.md).

## Complete: MCP lifecycle and client registration (0.11)

- Standalone Clean/Upgrade and plugin install/update/disable/enable/remove reuse
  the shared engine, schema-8 contracts and conservative outcomes.
- Explicit modern `2026-07-28` and legacy `2025-11-25` stdio probes verify received
  identity and complete named tool inventory, with bounded I/O, shutdown and cleanup.
- Claude Code 2.1.312 standalone user/project/local and plugin user bind native
  target health separately; Codex CLI 0.166.0 global is configuration only, with
  native activation not exposed/tested. Every active checkpoint requires an
  independent typed probe; companion/settings preservation and exact source/argv
  bindings remain mandatory.
- Live/saved evidence separates client claim from protocol observations and raw
  paged logs. Reports write 8/read 1–8; journals/ownership/comparison remain 1.

See [MCP fixtures and explicit native acceptance](examples/mcp/README.md).

## Controller portability (0.12)

- Default push/PR CI requires full workspace clippy, all-target tests, optimized
  release builds and explicit native CLI/terminal controls on Ubuntu 24.04 x86-64,
  macOS 15 arm64 and Windows Server 2025 x86-64. Release acceptance requires these
  three native jobs and all eight real Ubuntu Docker suites on the same 0.12 source;
  consult the [source-specific CI results](https://github.com/AraneaDev/theseus/actions/workflows/ci.yml).
- Controller evidence covers retained filesystem identities, artifact/cache/storage,
  native process/pipes, bounded duplex capture, terminal restoration and owned
  cleanup. Existing report readers accept schemas 1–8; guest paths remain POSIX.
- Initial writable Windows evidence is local NTFS with file flush/atomic publication,
  not power-loss directory persistence. Native evidence records the actual OS build;
  the Windows runner has reported 26100, and pre26100 runtime remains unverified.
  The controlled blocking-close MODEL is separate from actual native cleanup proof.
- Linux Docker guests remain the execution environment. Native controller support
  does not establish native macOS/Windows guest, GUI, service or VM behavior, or any inherited-stdio
  behavior without the separate MCP acceptance gates.

## Next: native OS adapters (planned)

- Machine/VM adapters where containers cannot establish service/session fidelity.
- Native macOS and Windows environment adapters with OS-specific contracts;
  unavailable native acceptance remains explicitly pending.

## Shared-engine MCP interface and required acceptance gates

- `theseus serve` exposes exactly `recipe_inspect`, `run_start`, `run_list`,
  `run_status`, `run_cancel` and `run_evidence` over ordinary inherited pipes,
  using the shared engine, outcomes and report/journal contracts.
- Legacy `2025-11-25` and modern `2026-07-28` adapters, explicit launch roots,
  bounded frames/pages, process-local admission keys, polling and owned shutdown
  are implemented in source. Exceptional five-second shutdown exits 2 with
  incomplete cleanup; recovery remains explicit through the existing CLI.
- Required native preflight and actual-server suites remain in all three host
  jobs; the real server Docker suite joins the existing seven in one budgeted
  Linux job. `scripts/validate.py` shares those commands for local pre-push checks
  and records native OS, source/dirty-tree identity and artifact receipts.
- Release acceptance requires same-source Linux/macOS/Windows ordinary-pipe/binary
  suites in all three native jobs, with status recorded in the
  [source-specific CI results](https://github.com/AraneaDev/theseus/actions/workflows/ci.yml).
  Local Linux checks, cross-compilation and container tests cannot complete that
  three-host gate.

## Services and desktop applications (planned)

- Observe systemd units, launcher entries and application process lifecycles.

Theseus will continue to require explicit lifecycle contracts and distinguish
observations from assumptions. New interfaces must share the scenario engine.
