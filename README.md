# Theseus

Know what your tool leaves behind.

Theseus rehearses CLI installation, configuration, upgrade, reinstall,
interrupted installation/upgrade, forward recovery, declared downgrade compatibility
or rejection, rollback after a declared failed or deliberately interrupted upgrade,
runtime checks and uninstall in disposable Linux containers.
A Ratatui workbench and headless CLI use one scenario engine. Evidence includes commands, private logs, file fingerprints,
declared check outcomes, and remaining files that need review.

Build on Linux with a current stable Rust toolchain:

```sh
cargo build --release --locked
cargo install --path . --locked
theseus --help
```

Docker CLI and a reachable Docker daemon are required for execution and cleanup.
Validation and saved report viewing work offline. Docker access is configured by
you; Theseus does not change daemon settings or user groups. Recipes are intended
for maintainer-controlled inputs. A container is not a hostile-code sandbox.

## Try a complete example

The included fixture installs real v1/v2 shell executables, configures them, checks
their identity and retained data, and removes the declared executable paths:

```sh
docker pull ubuntu:24.04
cd examples/standalone
theseus validate
theseus                       # preview first; press r explicitly to run
theseus run --headless         # all four user/system × clean/upgrade entries
theseus run --headless --profile user --scenario clean
```

You can also use `cargo run -- run --headless --recipe
examples/standalone/theseus.toml` from the repository root. Keep that command on one
line. An absent image fails preflight unless `run --pull` is explicitly supplied.
Theseus resolves and records the immutable image ID used by the run.

A target-only schema-2 example rehearses two invocations of the same installer:

```sh
theseus validate --recipe examples/reinstall/theseus.toml
theseus run --headless --recipe examples/reinstall/theseus.toml
theseus run --recipe examples/reinstall/theseus.toml  # preview, then r
```

Both user and system profiles declare stable binary and configuration scopes,
check runtime behavior after reinstall, and retain configuration/data on uninstall.
No base artifact is needed. A passing run establishes these declared observations
for two installer invocations; it does not prove universal idempotency or stability
across arbitrary repetitions, remote effects, or unobserved state.

Schema 3 provides a self-contained interruption example for both profiles:

```sh
theseus validate --recipe examples/interruption/theseus.toml
theseus run --headless --recipe examples/interruption/theseus.toml
theseus run --recipe examples/interruption/theseus.toml  # preview, then r
theseus run --headless --recipe examples/interruption/theseus.toml --profile user --scenario interrupted-upgrade
```

It copies a real target executable to `.partial`, starts a background writer,
reaches a cooperative barrier, and deliberately stops the whole container with
SIGKILL. Theseus verifies the stop, restarts the same container without setup,
input copying or chown, observes partial state, then runs explicit recovery. The
example verifies the writer stopped, promotes the partial executable, checks the
target version and retained data, and uninstalls.

Schema 4 provides two self-contained downgrade policies, each for both profiles:

```sh
theseus validate --recipe examples/downgrade/compatible.toml
theseus validate --recipe examples/downgrade/rejection.toml
theseus run --headless --recipe examples/downgrade/compatible.toml
theseus run --headless --recipe examples/downgrade/rejection.toml
theseus run --recipe examples/downgrade/compatible.toml  # preview, then r
theseus run --recipe examples/downgrade/rejection.toml   # preview, then r
```

Both start with `artifacts.target` (the newer 2.0.0 tool), configure a real format-2
database, then append a second record during the newer runtime exercise.
`artifacts.base` is the older requested 1.0.0 tool; these are declared roles,
not inferred version ordering. The compatible recipe installs that older tool
and requires its real reader to validate both records and calculate count 2 / sum
40. The rejection recipe explicitly asks its installer to refuse the newer data
before writing anything: actual exit 9 and the exact public marker must match,
then binary, recursive data, configuration and retained newer runtime checks must
pass. Both recipes uninstall the executable and verify retained settings/data.
They use only shell tools in the declared Ubuntu image, without credentials or
external application binaries. A passing recipe establishes its selected contracts
and observed scopes, not compatibility with arbitrary data or versions.

Schema 5 adds rollback after an ordinary declared failed upgrade. Both policies
are self-contained and run for user and system profiles:

```sh
theseus validate --recipe examples/rollback/binary-only.toml
theseus validate --recipe examples/rollback/restore-data.toml
theseus run --headless --recipe examples/rollback/binary-only.toml
theseus run --headless --recipe examples/rollback/restore-data.toml
theseus run --recipe examples/rollback/restore-data.toml  # preview, then r
```

The candidate changes the installed executable and migrates real data, prints the
declared failure marker, and exits 9. Explicit rollback restores the known-good
binary. `binary_only` retains migrated data and proves the old tool can read it;
`restore_data` additionally verifies a mapped in-container backup and restoration
of the entire declared data subtree. See the [rollback walkthrough](examples/rollback/README.md)
for timing, generated checks, failure evidence and limits.

Schema 6 adds rollback after a deliberately interrupted upgrade, again with both
policies and user/system profiles:

```sh
theseus validate --recipe examples/interrupted-rollback/binary-only.toml
theseus validate --recipe examples/interrupted-rollback/restore-data.toml
theseus run --headless --recipe examples/interrupted-rollback/binary-only.toml
theseus run --headless --recipe examples/interrupted-rollback/restore-data.toml
theseus run --recipe examples/interrupted-rollback/restore-data.toml  # preview, then r
```

The candidate changes the binary and data, starts a background writer and reaches
a cooperative barrier. Theseus verifies whole-container SIGKILL and same-ID
restart, observes actual Changed evidence, then authorizes explicit rollback.
`binary_only` proves the old tool can read retained partial data; `restore_data`
also proves mapped backup/restoration and cleanup. The [interrupted rollback
walkthrough](examples/interrupted-rollback/README.md) explains original versus
immediate references, typed runtime/reader evidence, authorization and limits.

Create your own starter with `theseus init`. It refuses to overwrite an existing
recipe. Replace the failing placeholder commands **and** the `.` artifact paths
before running it. `theseus validate --recipe PATH` validates without Docker.

## Workbench controls

Launch with `theseus` or `theseus run --recipe PATH` to inspect commands and public
environment before execution. In preview, `p` cycles available profiles and `s`
cycles the recipe’s declared scenarios and all; `r` starts the displayed selection.
Arrows or `j/k` and Page Up/Down scroll the preview. Preview never executes
automatically.

Interactive startup and ordinary idle exit show Theseus entering and retracing a
labyrinth with Ariadne's thread: six 900ms frames per scene (5.4 seconds each).
These scenes are decorative, with no observed lifecycle progress or success claim.
Space skips startup; `q` quits startup without starting a run. Space or `q` skips
exit. Run/selection input during startup is discarded. Global `--no-animation`
skips both scenes, for example `theseus --no-animation run --recipe PATH`.
TERM=dumb, terminals below 40 columns or 18 rows, and a resize below that size skip
without dwell. Headless, JSON, validate, init, help and version never animate.
Active quit starts cancellation immediately; signals, errors and cleanup deadlines
bypass the exit scene and preserve the actual outcome and terminal restoration.

During live or saved report viewing:

- `Tab` / `Enter` cycles scenario, stage, and evidence focus; `Esc` goes back.
- Arrows or `j/k` select a scenario, stage, or evidence record; Home/End selects
  the first/last record in the focused list.
- Left/right in evidence focus switches Stages, Findings, Files, Commands, Logs.
- `/` edits a case-insensitive evidence filter; Enter or Esc applies the draft.
  Press `/` and Enter to clear it. Matches include full stored record details.
- `[` / `]` loads the previous/next detail or log chunk. Page Up/Down scrolls
  inside the current chunk; it does not change the selected record.
- Ctrl-Home/Ctrl-End jumps to the first/last stored log chunk. `m` toggles a
  selected log between raw output and its full reference metadata, including its
  path; either mode change resets to the first chunk. Log selection is typed,
  even when stdout/stderr paths resemble one another.
- `c` requests cancellation and retains results for browsing. `q` exits when idle;
  during a run it cancels and waits up to five seconds for worker completion. If cleanup remains unresolved, exit 2
  identifies the report/journal and resources requiring inspection and recovery.
  SIGINT/SIGTERM follows the same exit path in preview, live, completed, and
  offline views.

Resize preserves selections and uses stacked panels on narrow terminals. Logs
show one selected stream, at most 64 KiB; truncation is visible and terminal
control bytes are replaced. A finished session remains available for browsing;
launch a new session for another run. Raw mode and alternate screen are restored
on exit, errors, and panic. Interactive mode requires stdin and stdout terminals;
without them, use headless or JSON output.

## Recipe contract

See the [standalone](examples/standalone/theseus.toml),
[reinstall](examples/reinstall/theseus.toml),
[interruption](examples/interruption/theseus.toml),
[compatible downgrade](examples/downgrade/compatible.toml),
[rejection](examples/downgrade/rejection.toml),
[rollback policies](examples/rollback/README.md), and
[interrupted rollback policies](examples/interrupted-rollback/README.md) and their installers.
Recipes reject unknown fields and define a name, image, network (`none` by default
or explicit `bridge`), artifacts, profiles, and nonoverlapping absolute observation
roots. Inputs resolve inside the recipe project, are materialized and hashed, then copied into
`/inputs/base` and `/inputs/target`. There are no writable host source mounts,
host home/socket mounts, host namespaces, privileged mode, or forwarded ports.

Schema version 1 retains clean/upgrade defaults and the original required steps.
It rejects `scenarios`, `reinstall`, `reinstalled`, and `unchanged` with a version-2
requirement. Schema version 2 requires a nonempty, unique, ordered declaration,
for example `scenarios = ["reinstall"]`. Default and `--scenario all` select that
list; requesting an undeclared scenario is an input error. Reinstall requires an
explicit `reinstall` step and at least one `unchanged` assertion for every selected
profile. There is no automatic fallback to `install_target`. A reinstall-only
recipe requires neither `artifacts.base`, `install_base`, nor `upgrade`.

Schema 3 adds `interrupted_install` and `interrupted_upgrade`, steps
`interrupt_install`/`recover_install` and `interrupt_upgrade`/`recover_upgrade`,
and checkpoints `interrupted`/`recovered`. Schemas 1/2 reject these identifiers.
Interruption steps must call the runner-supplied helper synchronously after the
intended partial mutation:

```sh
cp /inputs/target/sample-cli "$prefix/sample-cli.partial"
/bin/sh "$THESEUS_INTERRUPT_BARRIER"
```

The helper never returns normally. Do not background it or declare its private
variable in `env`/`pass_env`. The command timeout covers barrier setup, readiness,
and verified stop. Both scenarios require an interrupted filesystem assertion,
an explicit recovery command, and a recovered executable check with a nonempty
version/behavior expectation. Interrupted upgrade additionally requires an
explicit recovered data contract; use scoped unchanged/preserved assertions or a
command that reads real data and expects nonempty output.

Schema 4 adds `downgrade`, explicit `downgrade`/`exercise_downgraded` steps and
`downgraded`/`downgrade_rejected` checkpoints. Schemas 1/2/3 reject those features.
Each selected profile must declare `[profiles.NAME.downgrade]` with exactly one
policy. `expect = "compatible"` requires `data_checks` referencing real successful
`command` readers with nonempty expected output at `downgraded`, plus an old-version
`executable` check. `expect = "safe_rejection"` requires a literal `expected_exit`
from 1 through 125, nonempty `stdout_contains`, and separate `binary_checks` and
`data_checks` referencing `unchanged` assertions at `downgrade_rejected`. Binary
scopes must be actual files or symlinks; data scopes must exist and recursive
directory checks include added/removed children. Overlapping binary/data scopes
are rejected. A newer-version executable check is required before either attempt
and after rejection. All checkpoint checks must pass, including additional ones.

Schema 5 adds `rollback`, explicit `attempt_upgrade`, `rollback`, and
`exercise_rolled_back` steps, and the `known_good`, `backup_created`,
`upgrade_failed`, `rolled_back`, and `backup_removed` checkpoints. Earlier schemas
reject rollback features. Each profile declares `[profiles.NAME.rollback]` with
an exact expected failure (status 1..125 and nonempty literal stdout marker),
`change_checks` referencing `changed` at `upgrade_failed`, `binary_checks`
referencing file/symlink `unchanged` at `rolled_back`, `runtime_checks` referencing
successful old-version executable checks there, and `data_checks` referencing
successful actual data readers there. Runtime checks must match a `base_installed`
executable's name, resolved path, arguments and literal marker; that path must
also be a selected binary check. `binary_only` declares no backup. `restore_data`
requires explicit `backup`/`cleanup_backup` commands, a previously absent
`backup_root`, and 1..64 nonoverlapping source-to-relative-storage mappings.
Sources and storage must lie within observations and be separate from binaries.
The `__theseus_rollback_` check ID prefix is reserved for generated checks in
rollback-enabled recipes.

Schema 6 adds `interrupted_rollback`, selected with CLI
`--scenario interrupted-rollback`, and `[profiles.NAME.interrupted_rollback]`.
It declares the same `change_checks`, `binary_checks`, `runtime_checks`,
`data_checks` and `restoration` keys as rollback, with no `failure` field.
It reuses `interrupt_upgrade`, `rollback`, `exercise_rolled_back` and optional
`backup`/`cleanup_backup`. Changed checks belong to `interrupted`; no forward
`recover_upgrade` runs. A known-good actual reader is required before the attempt,
and required runtime/readers retain typed expected-exit, literal-match and
command/log evidence. Restored executable name/path/args/marker bind to the original;
known-good and restored reader scripts may differ. Ordinary and interrupted
rollback can coexist with different modes/check IDs; each entry gets its own owned
container and independent authorization.

Both `user` and `system` profiles run configure/exercise/checks as UID/GID
10001:10001. User installation/upgrade/reinstall/interruption/recovery/downgrade,
backup/rollback/cleanup and uninstall use that same identity; system mutation
commands use 0:0 **inside the disposable container**.
Steps are `/bin/sh -c` scripts with `/workspace` as working directory. Each step
is a new shell: files persist, shell variables and working-directory changes do
not. HOME, XDG directories and PATH are set explicitly. Runtime PATH defaults to
`/home/theseus/.local/bin:/usr/local/bin:/usr/bin:/bin`. Step/check `env` contains
public literal values; `pass_env` explicitly opts in named host variables. Values
from `pass_env` are not serialized as metadata. Command evidence records forwarded
names separately and omits overridden defaults; effective public literals remain
visible. Applications can print secrets into logs. No credentials are forwarded automatically and no general secret
redaction is promised.

The image must supply `/bin/sh`, `/usr/bin/env`, `sleep`, `mkdir`, `chown`, `chmod`,
`tar`, `cp`, `rm`, `cat`, `id`, and `awk`, with compatible UID/GID 10001
assignments. Interruption additionally requires GNU-compatible `mv -T` and
`dd iflag=nofollow,nonblock` for atomic readiness and bounded no-follow reads;
the example also uses `cmp`, `wc`, and `stat`. The downgrade examples also use
`head` and `wc`; rollback examples also use `readlink` and `sed`. Unsupported
tools fail closed as failed/incomplete evidence; Theseus installs no packages.
Image-declared volumes are rejected. Ubuntu 24.04 is exercised by acceptance; other images must satisfy
preflight and supported tar observations. Tests do not prove systemd, GUI, kernel,
macOS, or Windows behavior.

Clean executes install_target → target_installed checks → configure → configured
checks → exercise → uninstall → uninstalled checks. Upgrade executes install_base
→ base_installed checks → configure → configured checks → upgrade →
target_installed checks → exercise → uninstall → uninstalled checks. Each matrix
entry gets a fresh container/baseline. Snapshots follow mutations and occur before
and after checkpoint checks, so check-induced changes remain attributable. An
ordinary failed install still permits declared partial-install uninstall cleanup;
dependent forward mutations are skipped explicitly.

Reinstall executes install_target → target_installed checks → configure →
configured checks → exercise_before_reinstall → before_reinstall reference →
reinstall → reinstalled checks → exercise_after_reinstall → uninstall →
uninstalled checks. Both exercise stages use the same recipe command. The same
materialized target artifact is used twice inside one scenario container; there
is no second configure step. The comparison reference is captured after the first
exercise and earlier checks. `unchanged` uses the first reinstalled snapshot,
before checkpoint checks or the second exercise can mutate state. Those later
side effects remain visible in separate evidence. The configured reference used
by `preserved` stays independent of the before_reinstall reference.

Interrupted install executes before_interrupt reference → interrupt_install →
verified same-ID restart → interrupted checks → recover_install → recovered
checks → configure → configured checks → exercise_after_recovery → uninstall →
uninstalled checks. Interrupted upgrade executes install_base → base_installed
checks → configure → configured checks → exercise_before_interrupt →
before_interrupt reference → interrupt_upgrade → verified same-ID restart →
interrupted checks → recover_upgrade → recovered checks → exercise_after_recovery
→ uninstall → uninstalled checks. No observations run while stopped. Partial and
recovered snapshots precede their checkpoint checks; after-check snapshots retain
check side effects. At interrupted/recovered, `unchanged` compares before_interrupt;
`preserved` still compares configured and therefore applies only to interrupted
upgrade at these checkpoints. An ordinary interrupted install has not configured
anything yet. Recovery can intentionally migrate data using behavioral readers
rather than claiming byte equality.

Downgrade executes install_target → target_installed checks → configure →
configured checks → exercise_before_downgrade → before_downgrade reference →
explicit downgrade attempt → the declared policy checkpoint and checks →
exercise_downgraded (compatible) or exercise_after_rejection (the original newer
exercise command) → uninstall → uninstalled checks. The before_downgrade snapshot
includes earlier checks and newer exercise effects; `unchanged` compares it with
the first post-attempt checkpoint snapshot, before checkpoint commands can mutate
state. After-check snapshots keep those later effects visible. `preserved` still
compares the exact configured reference. No second configure, automatic fallback,
installer retry, restart, backup or restore occurs. A failed attempt or checkpoint
skips dependent runtime exercise; a running container still gets the ordinary
uninstall/final/owned cleanup tail.

Rollback executes install_base → base_installed checks → configure → configured
checks → exercise_before_upgrade → known_good checks → before_upgrade reference →
optional backup and backup_created checks → before_attempt reference →
attempt_upgrade → upgrade_failed checks → before_rollback reference → explicit
rollback → rolled_back checks → exercise_rolled_back → optional backup cleanup
and backup_removed checks → uninstall → final → owned teardown. The known-good
reference includes preceding runtime/check effects; `changed` compares the
immediate before_attempt reference, after backup/check effects. Before rollback,
fresh backup checks catch corruption caused by earlier checkpoint checks.
Restoration and binary equality compare before_upgrade, while `preserved` keeps
its configured reference. There is no second configure or automatic installer
fallback. See [the walkthrough](examples/rollback/README.md) for generated checks.

Checks have unique IDs and named checkpoints. `command` expects a shell exit and
optional literal stdout substring. An application exit is evaluated only after a
fresh in-container completion receipt is verified through a separate bounded
control operation; Docker control failures leave the application status unknown.
`executable` verifies PATH resolution, absolute
path, version argv, exit status, and optional literal output. `present` and
`absent` inspect paths (including dangling links); `preserved` compares an exact
path's content hash, kind, link, ownership and mode with configured evidence.
Preservation is valid at upgrade target_installed, reinstalled, uninstalled,
downgraded/downgrade_rejected, interrupted/recovered in interrupted upgrade, or
interrupted in interrupted rollback; use an explicit command check for intentional
migrations. File checks must fall within observed roots. Hashes establish observed bytes, not arbitrary semantic correctness.

`unchanged` is valid at `reinstalled` in schema 2 declaring reinstall, and at
`interrupted`/`recovered` in schema 3 declaring interruption scenarios, or at the
selected `downgraded`/`downgrade_rejected` checkpoint in schema 4 declaring downgrade,
or `rolled_back` in schema 5 declaring rollback or schema 6 declaring interrupted
rollback. `changed` is valid at ordinary rollback's `upgrade_failed` and
interrupted rollback's `interrupted`: observed additions, removals and
content/type/metadata changes pass; equality or absence at both ends fails.
Uncertain coverage or an unsafe ancestor remains incomplete. Unchanged compares a file’s hash, size, kind,
UID/GID, and mode; a directory compares its own
metadata and all observed descendants, including added/removed children. A
symlink compares its observed link target and metadata without following it.
For Unchanged, the declared path must exist in both complete snapshots. Missing
paths or changed entries fail; incomplete, ambiguous, overflowing, or unsafe observations remain
incomplete. Mtime, inode identity, ACLs, extended attributes, hidden remote effects,
and state outside the declared observed scopes are outside this contract.
Reports identify both comparison stages and retain changed-child evidence on the
comparison checkpoint. Use exact-path `preserved` for retention against configured state;
it does not recursively assert a directory’s children.

## Comparison suites

Compare declared installation methods or actual image IDs through the same engine:

```sh
theseus compare --suite examples/comparisons/methods.toml             # preview; r starts
theseus compare --suite examples/comparisons/methods.toml --headless
theseus comparison 'examples/comparisons/.theseus-comparisons/ID' --json
theseus cleanup 'examples/comparisons/.theseus-comparisons/ID'
```

The [self-contained comparison walkthrough](examples/comparisons/README.md) includes
copy/archive methods, two distinct actual image IDs on the same Ubuntu base,
validation/build commands, limits, controls, source logs and safe cleanup.
Variants run sequentially with fresh owned containers for the same selected matrix.
Scopes compare relative keys and all captured file metadata/content hashes and child
membership. Equivalent includes verified absence at both ends; Different makes the
suite Failed; Unknown evidence makes it Incomplete. Source lifecycle failures retain
their precedence even when scoped observations match. Inspect exact image IDs,
recipe/artifact digests and source reports; mutable tag names imply no ordering or
compatibility. These are declared observations, not universal installer equivalence.

Comparison manifests and saved artifacts use schema 1; source reports still write
schema 6. Offline viewing/JSON recomputes results and checks contained source digests
without Docker. JSON returns 0 for valid evidence regardless of suite outcome.
Derivation uses a shared conservative 32 MiB budget, including JSON escaping,
source summaries and diagnostics, before copying variable evidence. Limits also
cap differences at 10,000 per cell and 100,000 per suite. Exhaustion retains a
bounded prefix, makes the current and later cells Unknown and the suite Incomplete
(unless Cancelled takes precedence). Oversized optional source identities/artifacts
or errors are omitted atomically with a diagnostic; source lifecycle outcomes stay
accurate. Validated run IDs, declared recipe digests and contained recovery paths
remain available. Full source reports remain available for omitted evidence.
Legitimate large comparisons can reach this conservative byte limit before the
count caps. Private JSON writes stream with a 64 MiB limit and preserve the prior
file on serialization failure; comparison CLI JSON is staged within 32 MiB before
emission.
The comparison UI starts with cell selection; Tab switches between cells and
evidence records. Home/End reaches either list’s first/last item. The same filter,
chunk and local-scroll controls apply. Every retained difference and full source
field is reachable; source-report, recovery and full-JSON commands remain safely
quoted in selected detail records.

## Outcomes, limits, and recovery

Reports distinguish failed expectations, incomplete observations, cancelled work,
and review findings. Precedence is cancelled → incomplete → failed → passed.
Review findings alone do not fail the declared contract; they are counted
prominently. New unclaimed files, deleted baseline files, and changed baseline
files require review. Unchanged baseline files are not leftovers.

| Exit | Meaning |
| --- | --- |
| 0 | Passed declared expectations (inspect review count); preview exit |
| 1 | Complete run with failed expectations |
| 2 | Invalid input, infrastructure failure, incomplete run, or unresolved forced quit |
| 130 | Cancelled run |

Closing a live/offline TUI returns the displayed run outcome. `report --json`
returns 0 when the saved report was read successfully.

Headless SIGINT/SIGTERM requests cancellation and returns the saved partial report.
A verified planned interruption has outcome `interrupted`, no application exit
status, and independent readiness/stopped proof. Restart evidence records the
same container ID and independently verified running state. Exit 137 alone,
stdout markers, or an ordinary timeout cannot establish deliberate interruption.
Early ordinary completion (even exit 0) fails the barrier contract and skips
recovery. Missing/stale/symlink receipts, control errors, timeouts and unverified
restart are incomplete; recovery/data failures are failed expectations. Cancellation
wins, returns 130 and prevents later recovery. Running application failures still
permit declared uninstall; unavailable containers cannot provide uninstall evidence.

This models an abrupt whole-container stop at one cooperative pause point. It
does not establish host power-loss durability/fsync behavior, arbitrary installer
interleavings or service-manager behavior. Recovery is an
explicit forward application command; ordinary timeout/cancellation never retries
or recovers automatically. Rollback after an interrupted upgrade uses
a separate schema-6 scenario with explicit rollback and known-good proof.

Downgrade chooses its branch from the declared policy. A compatible attempt
must return zero; a rejection policy must return its exact declared status with
verified bounded stdout. Unexpected success, wrong status or marker fails. A
missing completion receipt, Docker transport/control failure, truncated output,
timeout or cancellation cannot establish refusal, even if text contains the
marker. An `expected_rejection` command outcome records the real status and marker
proof, but the scenario passes only with all required binary/data/runtime checks.
Live and offline UI distinguishes “Compatible downgrade passed” from “Declared
rejection checks passed” and shows actual status, marker verification, scopes and
comparison stages. Safe rejection only establishes these observed scopes; it does
not prove arbitrary installer safety. Downgrade does not implement rollback:
schema 5 supplies its separate explicit restoration contract.

Rollback authorizes reversal only after the exact declared application failure is
proved by a normal completion receipt, bounded complete stdout and a matching
literal marker, plus all selected Changed checks. Unexpected exit 0, wrong status
or marker fails. Missing receipt, transport errors or truncated stdout cannot
authorize rollback; timeout/cancellation does not trigger it. An `expected_failure`
command records the real nonzero status and marker proof. A scenario can pass
only after all required binary/runtime/data checks, generated backup checks where
applicable, backup cleanup, uninstall and owned teardown also pass. A zero-return
rollback command alone cannot establish restored data. Theseus verifies observed
bytes/metadata; application authors own quiescence and application-specific backup
and restoration, including database consistency. No host backup/export, atomic
snapshot or power-loss durability is claimed.

Interrupted rollback follows the same known-good/backup prefix, captures
`before_interrupt` immediately before `interrupt_upgrade`, verifies stop and
same-ID restart, captures Interrupted and its checks, then captures fresh
`before_rollback` before the shared rollback/cleanup tail. Actual Changed compares
`before_interrupt` to `interrupted`; restoration and fresh mapped backup readiness
compare original `before_upgrade`. No observation is claimed while stopped.
Verified stop/restart, complete observations, actual mutation, every prerequisite
and fresh backup readiness are all required. Missing proof is Incomplete; known
predicate mismatch is Failed; cancellation retains precedence. A prior successful
backup check cannot authorize restoration after later check-induced corruption.
The backup root is never deleted if it predated the run; cleanup is authorized only
when absent-root verification passed and backup actually started. See the
[walkthrough](examples/interrupted-rollback/README.md) for the mapped check list.
This declared cooperative barrier does not establish arbitrary fault timing or
host/database durability; authors own quiescence and database consistency.

Timeout/cancellation removes the owned scenario container, including descendants;
it cannot promise application uninstall observations after that removal. If the
daemon is unavailable, separately bounded cleanup may fail and leave journaled
resources. There is no global Docker prune.

Commands default to 120 seconds (maximum 3600). Snapshot defaults are 100000
entries and 1 GiB regular-file content per snapshot. Stored stdout/stderr are
independently capped at 16 MiB per stream per command while excess is drained;
truncation is recorded. Truncated check stdout is conservatively incomplete.
Acquisition errors, unsupported archives, symlinked observed-root ancestors and
observation overflow remain incomplete rather than silently passing.

Runs are saved under the recipe project's `.theseus-runs/<id>/`, using private
0700 directories and 0600 files: atomic `report.json` (schema 6), `journal.json`
(schema 1), and separate bounded logs. Readers accept report schemas 1 through 6; schema 1
continues to represent clean/upgrade evidence, while reinstall/comparison evidence
requires schema 2 or later. Interruption/proof/restart evidence requires schema 3.
Downgrade policy, checkpoints and rejection proof require schema 4. Rollback
policy, failure proof, Changed checks and backup metadata require schema 5.
Interrupted rollback and its typed command expectations require schema 6. Recovery
journals and ownership labels remain version 1. No observed file contents are retained except explicit command output; metadata/hashes and diffs are stored.
Reports can be large (load limit 64 MiB). Live progress schedules read-only
refreshes with at most one read in flight and one coalesced latest request; stale
reads cannot replace final results. Input, cancellation and the existing five-second
active-quit deadline continue while a refresh reads. Each refresh still parses
bounded saved metadata; initial filtering may scan all records.

```sh
theseus report .theseus-runs/RUN_ID            # offline TUI; no Docker
theseus report .theseus-runs/RUN_ID --json     # offline machine-readable evidence
theseus cleanup .theseus-runs/RUN_ID          # verified journaled resources only
```

Browsing in 0.8 uses bounded windows: at most 64 evidence summaries and 64 rows
per selector, one 16 KiB detail chunk, and at most 65,536 cached filter match or
checkpoint positions. Larger result sets remain reachable. Summaries may show an
explicit ellipsis; successive detail chunks expose all retained text, including
Unicode. The compatibility log mirror retains only the current bounded body.
Log APIs read at most 64 KiB at an explicit raw-byte offset; the UI reads 4 KiB
plus at most four UTF-8 guard bytes. The footer shows the actual byte range and
whether more bytes are stored. File identity/size/time checks invalidate changed
cached logs; these checks establish freshness, not authorship.

Pagination exposes stored evidence only. A last stored log chunk can still be
labelled capture truncated; a shortened file is incomplete current storage, and
unpublished appended bytes stay hidden. Capture limits and comparison-budget
omissions are unchanged and cannot be recovered by paging. Preview remains
read-only; interactive preview editing is deferred.

Log references are confined to the run directory with bounded reads and no
symlink following. Treat all logs as sensitive local output. Recovery verifies
recorded container IDs and ownership labels, never a name prefix. An abrupt crash
between Docker resource creation and receiving its ID can precede journaling.
Inspect Docker separately in that exceptional window.

## Validation and scope

```sh
cargo fmt --check
cargo clippy --offline --locked --all-targets -- -D warnings
cargo test --offline --locked
cargo build --offline --locked --release
cargo test --offline --locked --test docker_lifecycle --test interruption_lifecycle --test downgrade_lifecycle --test rollback_lifecycle --test interrupted_rollback_lifecycle --test comparison_docker -- --ignored --test-threads=1
```

Regular tests include wide/narrow TestBackend behavior and actual Unix PTY
startup/error/restoration. Synthetic large-report public CLI PTYs reach records
beyond 65,535, filter-cache overflow, full Unicode detail, comparison difference
101 and logs beyond 64 KiB while checking offline Docker sentinels and unchanged
source bytes. These are UI fixtures, not a claim of a real 70,000-file installation.
Docker acceptance is opt-in locally and explicitly
required in CI; missing Docker is a failed prerequisite, never a passing skip.
Healthy and broken fixtures exercise all profiles/scenarios, stale PATH,
permissions, preservation, leftovers, baseline deletion, limits, and cancellation.
Reinstall acceptance covers both profiles, reset/duplicate settings, changed mode,
added/removed children, an installer rejecting an existing installation, timeout,
and cancellation during the second invocation. Interruption acceptance covers the
four profile/scenario combinations, actual partial bytes and retained data, descendant stop, same-ID restart without setup or
input recopy, invalid readiness, early completion, recovery failures, transport
errors, timeout and cancellation before readiness, while stopped and during restart.
Downgrade acceptance covers both profiles and policies, real old-data readers,
silent reader omissions, lost records, changed binary/mode/link/configuration,
added/removed data children, wrong status/marker, unexpected success, missing
receipt/reference, transport failure, truncated output and timeout/cancellation.
Rollback acceptance covers both profiles and restoration policies, exact failure
and Changed evidence, complete mapped backup/restoration checks, runtime/readers,
source/backup corruption, baseline-root protection, no-op/partial restoration,
metadata/link/child changes, reference timing and timeout/cancellation.
Interrupted-rollback acceptance adds both profiles/policies with same-ID stop and
restart, original/immediate/fresh reference boundaries, typed runtime/reader proof,
faults that block restoration and exact owned teardown. Default animation PTYs
exercise frames, skips, discarded input, opt-outs, active cancellation, signals,
errors and terminal restoration. All six ignored Docker test binaries are
explicitly required in CI. Actual PTYs exercise narrow live selection, new-report
comparison visibility, offline viewing, and terminal recovery.

A separate real Phrourion 0.8.1 → 0.8.2 rehearsal passed all four matrix entries
and 32 declared checks, using actual binaries, controlled configuration and a
binary-copy install/remove recipe. This tests that declared cleanup contract;
it does not claim native Phrourion uninstall support. A same-artifact Phrourion
0.8.2 reinstall rehearsal also passed both profiles and 24 checks, including
stable binaries/configuration, runtime behavior, retention, and owned cleanup,
under that same binary-copy contract. Synthetic fixtures remain
reproducible in this repository; real binary artifacts/reports are local evidence.

See [ROADMAP.md](ROADMAP.md) for future capabilities. License: [MIT](LICENSE).
