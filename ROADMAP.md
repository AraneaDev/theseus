# Theseus roadmap

Last updated: **2026-11-03**. This is the maintained progress tracker for the
remaining roadmap. Update the relevant checklist, current work and acceptance
status whenever a milestone is implemented, reviewed, tested or merged.

A checked item is complete **at the scope stated**. “Local complete” means
implemented, reviewed and locally verified; it does not mean merged, released or
accepted on every operating system. Unchecked acceptance gates remain required.
In the active implementation section, checked source tasks have focused local
checks; independent review and combined acceptance are separate unchecked gates.

## Current position

| Area | Status |
| --- | --- |
| Standalone lifecycle engine, CLI and TUI | Complete and merged |
| Claude Code plugins and MCP lifecycle checks | Complete and merged |
| Linux/macOS/Windows controllers and shared-engine MCP server | Complete and merged; same-source native and Docker CI passed |
| README and documentation restructure | Complete and merged |
| Native helper protocol, host primitives, durable store and runtime | Local complete; final cross-platform acceptance pending |
| Typed native helper client | Local complete after review fixes; final cross-platform acceptance pending |
| Native helper file operations | Local complete and reviewed; native macOS/Windows acceptance pending |
| Native machine/VM adapter integration | In progress: ordinary scenarios, MCP execution and public recovery implemented locally; review fixes and final qualification remain |
| Native services and desktop application observations | Planned |

The latest accepted `main` is `d4e5304` (0.12). Later native foundations and the
client/file operations are committed through `8dd1986` and published with the
tracker in [draft PR #1](https://github.com/AraneaDev/theseus/pull/1).
The adapter integration remains local and uncommitted; neither is merged into
`main`. Intermediate CI is deferred until the combined acceptance batch.
Existing Docker workflows remain the supported execution
backend. A native controller is distinct from a native guest adapter.

### Work queue

1. Resolve native adapter review findings: original provider-query ordering,
   retained failure ownership, recovery consistency, cleanup finalization,
   artifact binding and bounded pending filesystem observations.
2. Complete scoped re-review and source-pinned local gates for CLI/TUI/MCP,
   helper transport, provider fixtures and public recovery.
3. Finish native operator documentation and example verification.
4. Implement service observations, then desktop/session observations, with
   deterministic fixtures and documentation for each supported platform.
5. Run the combined CI batch, resolve failures and merge accepted work to `main`.

Keep this queue and the checklists below current as work lands. Record source
implementation, review, local verification, CI acceptance and merge separately.
Do not estimate a completion percentage from checkbox counts: tasks differ
substantially in size. No new CI run or feature push is needed for tracker edits.

## Completed and merged

- [x] Shared lifecycle engine with standalone headless CLI and Ratatui workbench.
- [x] User/system profiles, clean install, upgrade, configuration/runtime checks
  and uninstall in owned disposable Linux containers.
- [x] Reinstall with declared path/subtree stability checks.
- [x] Cooperative interruption, same-container forward recovery and retained
  evidence for interrupted install/upgrade.
- [x] Declared compatible downgrade or safe rejection, including actual data-reader
  checks and preservation requirements.
- [x] Rollback after ordinary failed upgrade and interrupted upgrade, with
  `binary_only` and `restore_data` policies.
- [x] Private bounded logs, explicit assertions, cancellation, ownership-verified
  cleanup and retained reports/recovery journals.
- [x] Comparison suites with declared baseline variants, lifecycle outcome
  precedence and live/offline evidence browsing.
- [x] Scalable paged evidence/log browsing, Unicode-safe detail and capture limits.
- [x] Preview editing with explicit validation and Save Copy; execution requires
  a fresh explicit start.
- [x] Theseus/Ariadne startup and exit animations, skip controls, small-terminal
  fallback and terminal-restoration checks.
- [x] Local Claude Code plugin install/update/disable/enable/uninstall rehearsal
  using an inert marketplace and explicit native client version.
- [x] MCP lifecycle probing and version-bound client registration checks, with
  separate protocol observations and client claims.
- [x] Native Linux, macOS and Windows **controller** support and required CI jobs.
- [x] Shared-engine `theseus serve`: `recipe_inspect`, `run_start`, `run_list`,
  `run_status`, `run_cancel` and `run_evidence` over ordinary inherited pipes.
- [x] MCP framing, paging, explicit launch roots, cancellation and owned shutdown.
- [x] Same-source acceptance of all three controller OS jobs and all eight Docker
  suites at `d4e5304`: [accepted CI run](https://github.com/AraneaDev/theseus/actions/runs/41550085377).
- [x] README/docs restructure with task-based guides and reference pages.

Delivered behavior is limited to declared observations. Container acceptance
does not establish arbitrary application compatibility, host power-loss
persistence, native guest behavior or a hostile-code sandbox. Current Docker
reports write schema 8/read 1–8; recovery journals and ownership remain version 1,
and comparison artifacts remain version 1.

## In progress: native adapter foundations

These are private helper/client foundations, not a second scenario engine or an
installation interface for the controller machine.

- [x] **Protocol foundation:** bounded frames, typed identity/handshake, explicit
  v2 messages and request/reply correlation; preserve the existing v1 interface.
- [x] **Protocol recovery rules:** session guards, immutable preflight binding,
  operation IDs/digests, bounded stream evidence and durable ledger replay.
- [x] **Host primitives:** locked ledger handles, effective native principal facts
  and retained native process/pipe ownership.
- [x] **Durable helper store:** explicit bootstrap, workspace marker binding,
  append/sync ordering, committed acquisition intents and uncertainty poisoning.
- [x] **Helper runtime:** explicit configuration/program catalog, owned native Run,
  responsive cached Query/Cancel, bounded output and truthful shutdown/cleanup.
- [x] **Typed client:** exact preflight, ordinary request plus reserved Cancel,
  bounded pipe pump, owned stop/join and failed-start/reconnect owner retention.
- [x] **Client recovery checks:** same-intent acquisition recovery, original-ID
  operation queries, no automatic Run replay, observed lease continuity and
  stream/terminal consistency across reconnects.
- [x] **Local qualification:** reviewed helper/client source, actual Linux pipe
  fixtures and strict Linux/macOS/Windows compile checks. Latest client checks:
  32 unit + 2 preflight + 9 native Linux tests, and 29 validation controls.
- [ ] **Native macOS/Windows execution:** qualify the new helper/client on both
  actual OS runners in the final combined CI batch. Cross-compilation is not
  native execution evidence.
- [ ] **Integrate to main:** merge reviewed native work after the combined gates.

## Local complete: bounded native helper file operations

Implementation checkmarks in this section describe reviewed, locally qualified
source. All 14 helper and 9 client pipe cases passed on Linux; the final fixture
ordering correction passed its targeted rerun. Strict compile checks passed for
Linux/macOS/Windows. Actual native macOS/Windows execution remains pending the
combined CI batch; neither compilation nor fixture boundary gates replace it.

- [x] Retain the validated workspace directory authority for all file requests.
- [x] Implement existing typed `CreateFile`, `ReadFile` and `InspectFile` requests
  for bounded regular leaf files; no implicit overwrite or recursive access.
- [x] Commit acceptance before mutation; publish completion only after actual
  file observations and durable ledger completion.
- [x] Preserve partial/uncertain effects and quarantine rather than infer success
  or a safe retry from a transport/filesystem error.
- [x] Serialize file work with native Run/cleanup on the existing worker; keep
  cached Query/Cancel responsive and refuse conflicting work before filesystem I/O.
- [x] Extend the typed client and retain explicit Unsupported behavior from older
  helpers; support selection must be explicit.
- [x] Add focused failure/order/ownership tests and required native cases on all
  three OS jobs; complete review and local qualification before the final batch.

## In progress: supplied native machine/VM adapters

Checked tasks below are implemented with focused local checks. Integration
review, full local qualification and external acceptance remain separate gates.

- [x] define explicit target/helper/capability bindings and supported native
  command, path, principal and termination contracts.
- [x] add bounded transport for an explicitly supplied disposable target, with
  explicit endpoint/identity authority and no automatic host discovery.
- [x] bind an external reset/disposal provider; record the target/reset identity
  and refuse automatic reuse without the required matching receipt.
- [x] integrate through the shared engine: first clean/upgrade/reinstall, with
  unsupported native scenarios refused before reservation.
- [x] durably publish controller intent before reservation and acquired identity
  before setup; retain original operation identities through recovery.
- [x] introduce explicit native recipe/report/journal representations and
  validators without inventing Docker image/container/UID fields.
- [x] route CLI/TUI execution and preview
  through the selected environment; provide native saved-report/evidence views.
- [x] add MCP native recipe validation,
  read-only inspection, status and bounded evidence browsing.
- [x] Wire MCP native execution into the existing owned worker and admission
  lifecycle; retain unresolved ownership and block further admission.
- [x] Add public native recovery/cleanup with explicit launch authority and
  original intent verification; query/cancel/release without replaying Run.
- [x] Define the initial native comparison boundary: reject native variants
  before suite creation; native comparison execution remains unsupported.
- [x] add deterministic helper, compiled
  reader, transport/provider and publication-failure fixtures. Local selected
  engine tests exercise clean/upgrade/reinstall on fresh inert targets.
- [ ] Complete native examples and operator documentation, including target
  configuration, provider ownership, evidence limits and recovery procedures.
- [ ] Complete independent integration review and resolve findings.
- [ ] Resolve review findings in provider preparation/disposal ordering, retained
  panic ownership, terminal recovery consistency and local cleanup finalization;
  keep remote uncertainty separate from actual local-process status, bind recipe
  artifact facts to the helper catalog and retain unfinished filesystem owners
  without an unbounded pending-status wait.
- [ ] Run the complete source-pinned local test inventory, fixture integrity
  checks and required cross-platform compile checks for the integrated adapter.
- [ ] Run true Linux/macOS/Windows guest lifecycle acceptance when disposable
  targets and reset-provider authority are supplied.

**Current local evidence:** ordinary selected-engine scenarios use actual child
processes and compiled readers; an injected journal-publication failure verifies
that no provider invocation occurs and the bootstrapped workspace is unchanged.
These fixtures do not establish real SSH, VM reset or native guest acceptance.
The integrated adapter remains unmerged and is awaiting independent review and
the combined validation gates.

Native interruption, rollback and guest plugin/MCP checks require their own
contracts and evidence before being enabled; Docker support does not enable them
on native targets automatically. Theseus will not become a VM provisioner or use
the controller as an implicit installation target.

## Remaining: services and desktop applications

- [ ] Define service registration/configuration, activation, runtime and declared
  readiness observations for systemd, launchd and Windows SCM.
- [ ] Implement service adapters and evidence validators with manager-specific
  ownership and cleanup; do not infer authority over a shared host process.
- [ ] Define desktop entry validity, activation in an explicitly bound session,
  application readiness and owned process lifetime.
- [ ] Implement native launcher/session observations and integrate them with the
  shared engine and evidence views.
- [ ] Add deterministic fixtures, documentation and real disposable-guest/session
  acceptance for each supported manager/desktop.

## Final acceptance and merge

- [ ] Complete remaining implementation and independent reviews locally.
- [ ] Run focused local checks during development; defer full repeated batches
  until the implementation is ready.
- [ ] Run one combined same-source Linux/macOS/Windows native CI batch plus all
  eight existing Docker suites; investigate failures before rerunning.
- [ ] Verify release builds, required named tests, documentation links and retained
  source/artifact/cleanup evidence.
- [ ] Merge accepted work into `main`, update this tracker and document the exact
  supported scope and remaining externally blocked acceptance.

**Explicitly pending external acceptance:** supplied-machine SSH lifecycle,
VM reboot/reset, systemd/launchd/SCM and interactive Linux/macOS/Windows desktop
sessions. Adapters can be implemented with these tests pending when no target is
available, as agreed. Pending tests must remain visible; they are not successful
or skipped acceptance. Native hosted CI remains required by default.

## Documentation and validation

See [documentation](docs/README.md), [recipe contracts](docs/reference/recipes.md),
[evidence formats and limits](docs/reference/evidence.md),
[recovery](docs/operate/recovery.md), [MCP server](docs/agents/mcp-server.md),
and [validation gates](docs/contribute/validation.md).
Design specifications, implementation plans and working review records remain
private and ignored; this public tracker records scope and progress.
