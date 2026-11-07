# Theseus roadmap

Last updated: **2026-11-06**. This is the maintained progress tracker for the
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
| Native machine/VM adapter integration | Local complete and independently reviewed; native OS CI and real supplied-target acceptance pending |
| Native services and desktop application observations | In progress: service scenarios and recovery source reviewed and locally tested; desktop Clean/Upgrade/Reinstall CLI and fresh MCP locally passed; native factory source review closed; combined CLI/MCP passed against the private model; native ordinary admission and final native acceptance remain in progress |

The latest accepted `main` is `d4e5304` (0.12). Native foundations,
client/file operations, adapter integration and the earlier service checkpoint
were committed through `2fb9c73`. Further service/desktop work continues on
`feat/native-adapter` for [PR #1](https://github.com/AraneaDev/theseus/pull/1).
The native work is not merged into `main`. Intermediate CI is deferred until the
combined acceptance batch; feature pushes use the PR workflow instead of
duplicating both push and PR runs.
Existing Docker workflows remain the supported execution
backend. A native controller is distinct from a native guest adapter.

### Work queue

1. Desktop Clean is locally verified through the actual CLI, fresh six-tool MCP,
   saved TUI and missing/leaked-entry negative cases; maintain these regressions.
2. Native fixture creator-ownership review is closed; required launchd/SCM/systemd
   acceptance is wired into the three-OS CI jobs, with execution still pending.
3. Native activation and ordinary/combined admission source reviews are closed;
   keep primary, manager-instance and descendant lifetime guarantees explicit.
4. Windows retained-entry retirement is implemented, locally checked and
   independently reviewed. Original parser handles settle before mutating SUT
   commands; failed or uncertain retirement stays quarantined. Native execution
   remains required in the combined CI batch.
5. Fresh local acceptance passes at `1f812a0`: all six standalone Source-owned
   desktop cases; combined CLI and fresh six-tool MCP each cover Clean, Upgrade
   and Reinstall against the private D-Bus model. The clean build pins 19 separate
   executable roles. Fresh original-input export, byte verification and production
   CLI validate/preview pass. These model checks do not qualify a real manager.
6. Run one combined CI batch, resolve failures and merge accepted work to `main`.

The [first combined CI run](https://github.com/AraneaDev/theseus/actions/runs/42224965159)
exposed a Windows planning import, JSON error-source retention and three macOS
test-fixture issues. Corrections pass focused local checks and the complete
default workspace at `a481a30`: 1,565 passing tests, no failures, with matching
pinned fixtures. The old-source Docker job was cancelled after collecting all
native failures to avoid repeating remaining CI minutes.

The [second combined CI run](https://github.com/AraneaDev/theseus/actions/runs/42230821459)
at `b68ea8d` is not acceptance. Windows stdio passed, then workspace lint found
an oversized `WorkerCompletion` variant; the original failure owner is now boxed
and full Windows/macOS cross-lint passes under default and all features (lint
only, not native execution). Linux passed runtime checks and all six compiled
desktop cases, then failed real-systemd setup before any manager case ran:
systemd v255 opens its public API only after its own managed `dbus.socket` and
`dbus.service` run, and the fixture had started an external bus daemon. The
corrected operator bootstraps those closed units under the original user
manager, binds manager/socket/daemon roles, stops only the verified original
bus over the retained private manager channel and deletes the temporary account
only after the held manager cgroup reports empty; uncertainty retains it. 22
pure operator and 42 validation-driver controls pass and the change is
independently reviewed; real-manager execution is still unproven. GitHub
cancelled macOS for arm64 runner capacity before assigning a runner, and the
old-source Docker job was cancelled.

The [third combined CI run](https://github.com/AraneaDev/theseus/actions/runs/42238957864)
at `566194f` passed all eight Docker suites and the six Linux desktop cases; it is
not acceptance because all three native jobs failed. Linux: the private manager
started its managed bus, then qualification failed about five seconds later and
the fail-closed cleanup error hid the primary exception; manager console logs
were also dropped. The operator now retains the primary traceback, prints the
whole exception chain, and keeps the exact unit's debug journal as evidence.
macOS (first native helper-runtime run): two saturated-output tests exceeded their
deadlines because the helper read one 4 KiB chunk per stream per worker tick; it
now drains up to 16 bounded reads per tick. Windows: an export test compared a
short 8.3 temp path with the export's canonical root.

The [fourth combined CI run](https://github.com/AraneaDev/theseus/actions/runs/42247956461)
at `f9f67b8` again passed all eight Docker suites and every earlier native gate,
and each native job reached further. Linux: the private manager bootstrapped its
own bus and public API within 53 ms, but the retained libdbus peer connection
authenticated and was never served; the private channel now uses libsystemd's
sd-bus client (as `systemctl` does) for stop/settle/exit only, exercised locally
against a dbus-python peer server, with read-only observation over the verified public
manager name. The real journal also showed the manager-synthesized `app.slice`
and inactive not-found references, which the initial closed-unit check now admits.
Windows ran its full workspace suite for the first time: the mandatory ledger lock
refuses reads through other handles (tests now read through the owner), launchd
origins are compared as POSIX literals, readiness deadlines compare instants
exactly, and several fixtures assumed POSIX paths, long temp names or
unprivileged handle semantics. macOS ran its six Source-owned desktop cases for
the first time; all six failed because the desktop fixture's provider dispatch
was compiled only on Linux. That is fixed and failed desktop steps now retain
their fixture evidence; macOS execution of the fix is pending.

The [fifth combined CI run](https://github.com/AraneaDev/theseus/actions/runs/42259726780)
at `10ece71` passed all eight Docker suites. Linux completed the first real
user-manager qualification and full original cleanup (managed bus stopped over
the retained private channel, manager joined 0, held cgroup `populated 0`,
temporary account removed). The first real-manager case then failed because a
real manager garbage-collects an inactive unreferenced unit immediately after
the installer's `LoadUnit`, so the product's `GetUnit` observation saw no
registration; service inspection now uses `LoadUnit` on the exact unit name.
macOS now completes prepare, broker bind, install, configure, owned activation,
close and uninstall; the source broker then exited 2 at stop because its settle
check judged owned (macOS original-primary) lifetimes by the legacy cleanup flag,
which the protocol never sets for them; owned lifetimes now use the explicit
owned retirement predicate, pending macOS execution (hidden run directories were
also dropped from the upload and are now retained under visible names). An owned
activation that never receives a receipt still cannot settle; no required case
reaches that path. Windows stopped at a new
evidence-copy control that followed a symlink without `O_NOFOLLOW`; links and
reparse points are now refused.

The [sixth combined CI run](https://github.com/AraneaDev/theseus/actions/runs/42270763738)
at `1f47516` passed all eight Docker suites, and macOS passed all six Source-owned
desktop cases natively for the first time. The following three owned
native-entry cases failed because broker ledger replay judged a close request by
the activation lifetime recorded at close acceptance rather than its current
projection, so the store refused the uninstall entry preparation; replay now
uses the same projection as queries, pending macOS execution. Linux again completed real
qualification and original cleanup; the first real case then failed because the
SDK reader required a 16-byte `InvocationID`, while real systemd reports a null
id as an empty array for never-started and not-found units. The reader now
accepts empty or exactly 16 bytes, and the private model now mirrors real
systemd (empty id before first start, transient not-found unit from `LoadUnit`);
all eight private-model service cases pass locally. Windows had two failures:
the same-size after-read check could miss a rewrite within one coarse NTFS
timestamp tick, so native file inspection now holds a deny-write handle for the
whole observation (a file another process holds open for writing is refused
rather than read); the shortcut fixture's field mismatch against the admitted
profile is not yet diagnosed, and the next run reports the differing field.

The [seventh combined CI run](https://github.com/AraneaDev/theseus/actions/runs/42294689119)
at `95586ff` passed all eight Docker suites. Windows confirmed that the inert test
shortcut has no target ID list (`GetPath` returns `S_FALSE` with an empty path);
the fixture now authors shortcuts through `IShellLinkW`/`IPersistFile::Save`
like real installers, and the next run records the exact saved bytes so the
closed admitted byte profile can be defined from evidence. Linux still refuses
the baseline inspection of the absent unit after all sixteen property reads;
the operator now records the real manager's view of an absent unit as evidence.
macOS failed an earlier client recovery test that had passed in three prior
runs; it now reports the actual recovered reply.

The [eighth combined CI run](https://github.com/AraneaDev/theseus/actions/runs/42302044978)
at `1e73965` passed all eight Docker suites. macOS passed every gate before the
real manager step, including all six Source-owned desktop cases, all three owned
native-entry cases and the client recovery test, then the first real launchd
qualification refused absence because macOS 15 reports `domain for uid: N`
rather than the assumed `domain for user: N`; both exact forms are now accepted
by the operator, protocol and host checks. Windows captured the exact 1,296-byte
Shell-saved shortcut; the admitted shortcut profile is now a closed layout built
from those bytes (exact header flags, size-checked target ID list proven by the
exact COM target, local-drive LinkInfo and an exact ExtraData allowlist). Linux
showed that real and model not-found units now agree, and that the real
installer stops after observing its freshly written unit; the operator now
records the real manager's view of a present never-started unit.

The [ninth combined CI run](https://github.com/AraneaDev/theseus/actions/runs/42309766740)
at `d93ca45` passed all eight Docker suites, and every Windows shortcut test now
passes natively against the closed profile. Windows next reached the catalog
adapter, where an SSH fixture used a short 8.3 temp path whose `~` the SSH option
grammar correctly refuses; the test now uses the canonical long-name root.
macOS passed real launchd qualification for the first time, then `launchctl
bootstrap` refused the first SUT plist with I/O error 5. User-domain plists now
declare exactly `LimitLoadToSessionType=Background` (required for the User
domain, refused elsewhere); this is the leading hypothesis and pending
confirmation, with launchd's own log retained as evidence. The Linux probe failed
on an untyped empty D-Bus array and now passes typed arrays.

The [tenth combined CI run](https://github.com/AraneaDev/theseus/actions/runs/42326310379)
at `7f1fa00` passed all eight Docker suites. The Linux probe finally showed the
real manager's view of a present unit: it reads path settings literally, so the
fixture's quoted `WorkingDirectory` became a non-absolute directory that the
product correctly refused, while the model's shell-style parsing had hidden it.
The renderer now writes an unquoted literal, the closed grammar admits only that
form and the model reads it literally; all eight private-model service cases pass.
macOS exposed a real timing defect: after an abort, `Immediate` supervision
recomputed the execution timeout including emergency cleanup time; the timeout is
now judged at the abort decision. Windows reached its six Source-owned desktop
cases for the first time; the desktop fixture now marks pinned images read-only
and uses the long-name temp root, as the service fixture already did.

The [eleventh combined CI run](https://github.com/AraneaDev/theseus/actions/runs/42330968176)
at `7f703ea` passed all eight Docker suites and, for the first time, the complete
Linux native job, including every real private systemd manager case with the
original managed bus, qualification and cleanup. macOS failed one supervision
test whose 150 ms deadline expired before a slow runner started its child; the
test now uses a 2 s deadline. Windows desktop cases still fail: removal now
deletes through an identity-checked delete-access handle, the controller image
comparison accepts only the exact verbatim form of a plain drive path, and the
evidence collector (whose pattern never matched libtest's same-line output) now
retains the native reports.

The [twelfth combined CI run](https://github.com/AraneaDev/theseus/actions/runs/42335353834)
at `3fe1cc5` passed all eight Docker suites and the complete Linux native job
again. On macOS the `Background` session type was confirmed: `launchctl bootstrap`
succeeded, and the product parser then refused the first real loaded-job print
(blank group separators, a separate `asid`, an exact property set and three new
fields); the parser now admits exactly that observed form, and a running job's
print remains unobserved. Windows desktop steps had succeeded but were scored as
failed because the recipe expected POSIX exit statuses; the Windows desktop setup
now names the Windows exit kind, keeping each expected code.

The [thirteenth combined CI run](https://github.com/AraneaDev/theseus/actions/runs/42339737863)
at `118e434` passed all eight Docker suites and the complete Linux native job for
the third time. Windows exposed a product defect: broker session validation
compared a stripped pipe suffix with the admitted header's full canonical pipe
path, so every Windows broker session was refused; the comparison now uses the
full validated path exactly. The macOS emergency-cleanup control's margins were
too tight for the duplex exchange window; it now uses a 4 s budget and an
emergency inspection that alone exceeds both the window and the budget.

The [fourteenth combined CI run](https://github.com/AraneaDev/theseus/actions/runs/42349360407)
at `5273464` passed all eight Docker suites and the complete Linux native job for
the fourth time; Windows passed its first Source-owned desktop case natively.
Windows deletion used legacy delete-on-close, leaving names delete-pending while
another owner still held the file, so later absence checks and reinstalls failed
with access denied; deletion now uses POSIX semantics after the same identity
proof. macOS completed real launchd install, configure and start for the first
time; the running-job print adds seven inert scalar rows that the closed parser
now admits exactly.

The [fifteenth combined CI run](https://github.com/AraneaDev/theseus/actions/runs/42353351670)
at `86e8f5b` passed all eight Docker suites and the complete Linux native job,
and Windows passed all six Source-owned desktop and all three owned native-entry
cases natively for the first time. The Windows SCM operator then refused its own
artifact pin because whole stat comparison included access time, which the pin
digest read updates; it now compares stable identity fields only. macOS's client
recovery control observed a truthful `NotStarted(CancelRequested)` recovery after
its own join cancelled the accepted create; the control now admits it only with
the matching durable ledger outcome and no physical file. Final corrected-source
native acceptance remains pending.

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
  fixtures and strict Linux/macOS/Windows compile checks. The integrated adapter
  also has a source-pinned native inventory and 33 validation controls.
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

## Local complete: supplied native machine/VM adapters

Checked tasks below are implemented, independently reviewed and locally qualified.
Native OS CI and real supplied-target acceptance remain separate gates.

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
- [x] Complete native examples and operator documentation, including target
  configuration, provider ownership, evidence limits and recovery procedures.
- [x] Complete independent integration review and resolve findings.
- [x] Resolve review findings in provider preparation/disposal ordering, retained
  panic ownership, terminal recovery consistency and local cleanup finalization;
  keep remote uncertainty separate from actual local-process status, bind recipe
  artifact facts to the helper catalog and retain unfinished filesystem owners
  without an unbounded pending-status wait.
- [x] Run the source-pinned local native inventory, fixture integrity checks and
  strict cross-platform compile checks; retain isolated environment failures and
  their exact targeted reruns separately.
- [ ] Run true Linux/macOS/Windows guest lifecycle acceptance when disposable
  targets and reset-provider authority are supplied.

**Current local evidence:** ordinary selected-engine scenarios use actual child
processes and compiled readers; an injected journal-publication failure verifies
that no provider invocation occurs and the bootstrapped workspace is unchanged.
These fixtures do not establish real SSH, VM reset or native guest acceptance.
All nine pinned runtime artifact hashes matched after final test-fixture and
formatting corrections. Review findings and targeted reruns are closed; the
integrated adapter remains unmerged pending the combined CI gates. See the
[native target guide](docs/guides/native-targets.md) and
[offline example](examples/native/README.md).

Native interruption, rollback and guest plugin/MCP checks require their own
contracts and evidence before being enabled; Docker support does not enable them
on native targets automatically. Theseus will not become a VM provisioner or use
the controller as an implicit installation target.

## In progress: services and desktop applications

The shared contract is reviewed. The first delivery is an installer-managed
systemd Clean scenario: the installer creates/configures/removes the service;
Theseus observes it and performs only declared Start/Stop rehearsals.

- [x] Review the service/desktop design and implementation sequence.
- [x] Implement the systemd native SDK backend with explicit manager binding;
  focused Host tests and strict Host lint checks pass locally.
- [x] Add schema2 contracts, helper/client wiring and shared service hooks;
  focused protocol/store/client/helper and contract checks pass.
- [x] Implement authenticated local-challenge readiness; bounded native socket
  checks pass, including wrong identity, nonce and application tuples.
- [x] Verify the compiled installer/application through CLI and six-tool MCP
  against an explicitly labelled private model, including saved TUI/MCP evidence.
  Eight CLI lifecycle/failure cases and the fresh MCP context pass locally.
- [ ] Exercise the compiled test installer and a real disposable systemd manager.
- [x] Independently review the systemd core, consumers and private-model slice;
  all core and consumer findings are closed. Real-manager acceptance stays pending.
- [x] Implement additive launchd/SCM contracts and shared helper integration,
  preserving the existing systemd and catalog wire formats. Focused shared and
  protocol tests and strict cross-platform compile checks pass locally.
- [x] Implement macOS peer-authenticated local socket and Windows first-instance
  named-pipe readiness source with retained pending I/O ownership.
- [x] Resolve independent platform adapter review findings, including original
  launchd file identity, native command results, SCM state and pipe admission,
  and retained Windows diagnostics. Native execution remains a separate gate.
- [x] Implement compiled native launchd/SCM installers and service applications;
  fixture contract tests and all-platform compilation pass locally.
- [x] Implement and review native receipt/setup fixture and exact Windows ACL
  policy source. Exercise the real native product paths in the acceptance gates
  below; pure policy tests do not establish actual SCM/ACL behavior.
- [x] Implement and review guarded macOS/Windows CI operator source, alongside
  the private-systemd operator. The copied artifact dispatch guards are reviewed;
  27 pure operator tests pass locally. Native execution is still required.
- [x] Implement and independently review retained original native registration
  owners for negative-case fixture teardown. Fifteen pure tests and macOS/Windows
  source checks pass; actual manager execution remains pending. Failed publication,
  revoked file authority and repeated finish errors retain original evidence;
  unknown leftovers never authorize cleanup reconstructed by registration name.
- [x] Wire required source-pinned native manager cases into the existing Linux,
  macOS and Windows CI jobs. Independent review is closed; 40 validation-driver
  controls verify artifact, feature, executed-case and source-drift boundaries.
  Linux requires all six desktop cases and both real-systemd combined CLI/MCP
  cases, with separately pinned source helper/broker and exact case/role sets.
  This is reviewed CI source wiring; the combined online batch remains pending.
- [ ] Qualify actual launchd format/domain and SCM rights, lifecycle and deletion
  behavior on native OS runners. Cross-compilation does not satisfy this gate.
- [x] Implement ordinary upgrade/reinstall and original-identity service recovery.
  Compiled private-model CLI Upgrade, Reinstall and both HarnessStopped traces
  pass locally. Original-identity recovery review is closed, with 16 focused
  recovery tests and seven schema1 recovery regressions passing. All four
  service review findings are closed; compiled Configure-failure cleanup passes
  for both runtime preconditions. Actual native OS acceptance remains open above.
- [x] Implement desktop Host session binding, original owned-process scopes and
  authenticated readiness/close mechanisms; 39 focused Linux tests pass,
  including fresh probes, cancellation/backpressure and escaped descendants.
  This is Host mechanism evidence; broker/product integration and native OS
  execution remain pending.
  macOS process-group cleanup cannot establish full descendant-tree settlement;
  required tree contracts refuse before launch and unknown descendant facts stay
  visible. Original-primary lifetime is a separate supported observation.
- [x] Integrate and independently review the durable session broker through the
  existing protocol/store, worker, Clean scenario and public evidence paths.
  Actual compiled Linux source fixtures pass CLI Clean, both missing/leaked-entry
  controls and a fresh six-tool MCP run, including their own saved report/TUI.
  Two actual IPC regressions verify partial-response scheduling, original
  deadlines, cancellation and terminal-cache behavior. This proves the explicitly
  labelled owned headless source path; native OS and GUI activation remain gates.
  A later clean-checkpoint run exposed legacy Configure replacing identical entry
  bytes. A fixture-only identity-preserving fix and pre-Prepare input snapshots
  pass focused controls; fresh final-source runtime/export validation is pending.
- [x] Implement and independently review Linux GIO, macOS AppKit and Windows
  Shell Host adapter source. All three review findings are closed; focused
  desktop tests and strict Linux/macOS/Windows Rust lint checks pass locally.
  Cross-checks do not establish native SDK execution or graphical activation.
- [x] Implement and independently review native activation through the durable
  broker and shared driver.
  The mode-specific router is implemented and its replay-deadline review finding
  is closed. Mac/Windows original-object polling and Linux original-child source
  are reviewed; actual SDK/session execution remains pending. The operational
  Linux provider-qualified bootstrap and stock SDK factory source are implemented
  and independently reviewed. The controller now retains the original authenticated
  admission before Acquire; this product seam and native ordinary/combined
  capability admission are independently reviewed. Missing original qualification
  still refuses. No actual SDK/graphical execution is claimed.
- [ ] Finish integrated acceptance of the explicit macOS owned-primary contract.
  Protocol/store and Host/broker source reviews are closed; product/report
  integration passes 61 focused integration and 26 recovery tests; its source
  reviews are closed. Primary/local retirement stays separate from unavailable
  tree authority. Fresh same-source compiled acceptance remains pending.
  Portable six-case Source-owned fixtures are required on all three native jobs.
- [ ] Accept genuine installed owned entries on macOS and Windows. Separate
  bundle/shortcut declarations and retained CF/COM adapters are source-reviewed;
  three required ordinary product cases let only the pinned SUT create/remove
  the real entry and second pinned executable. The Windows pre-SUT retirement
  barrier is implemented and independently reviewed; original parser handles
  retire before replacement, with durable quarantine on failure.
  Native execution is still pending.
- [ ] Qualify native SDK builds and platform test execution. The AppKit bridge
  requires a native macOS build with the Apple SDK. Real supplied graphical
  sessions remain a separate external acceptance row.
- [x] Implement desktop execution from standalone Clean to Upgrade, Reinstall and
  combined service/desktop scenarios, including explicit SUT generation handoff.
  Owned desktop Upgrade and Reinstall pass actual compiled CLI tests (27s/30s);
  fresh six-tool MCP covers all three ordinary scenarios and saved evidence/TUI.
  Shared generation/store/router source review is closed. Combined private-model
  CLI and fresh six-tool MCP each pass Clean, Upgrade and Reinstall (91s/186s),
  including one lease, saved evidence/TUI and release. The earlier failed runs
  remain retained: the fixture abandoned its original close socket after two
  seconds. A reviewed fixture-only fix preserves that connection; product
  deadlines, unknown-effect handling and release rules remain strict.
  Native ordinary/combined admission source is implemented and reviewed, with
  exact platform/lifetime/tuple checks and a separate six-capability declaration.
  Its focused checks pass, including 153 protocol tests. Actual native
  SDK/interactive execution remains a separate pending qualification gate.

The checked backend work includes private D-Bus primitive and ownership-model
checks. Actual systemd-manager acceptance remains pending. Local model checks now
exercise the full compiled product path; they do not qualify a real manager.


- [x] Define service registration/configuration, activation, runtime and declared
  readiness observations for systemd, launchd and Windows SCM.
- [x] Implement service adapter source and evidence validators with manager-specific
  ownership and cleanup; do not infer authority over a shared host process.
- [x] Define desktop entry validity, activation in an explicitly bound session,
  application readiness and owned process lifetime.
- [x] Implement and review native launcher/session observation source integrated
  with the shared engine and evidence views; native execution remains gated above.
- [ ] Add deterministic fixtures, documentation and real disposable-guest/session
  acceptance for each supported manager/desktop.

## Final acceptance and merge

- [x] Complete remaining implementation and independent reviews locally.
  The final original-recipe pin and Windows entry-retirement findings are closed.
  Workspace all-target/all-feature lint and formatting, focused protocol/helper
  tests, default shipping compilation and macOS/Windows metadata checks pass.
- [x] Run focused local checks during development; defer full repeated batches
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
