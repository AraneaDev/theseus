# Theseus

Know what your tool leaves behind.

Theseus rehearses CLI installation, configuration, upgrade, reinstall,
interrupted installation/upgrade, forward recovery, runtime checks, and uninstall in disposable Linux containers. A Ratatui workbench and
headless CLI use one scenario engine. Evidence includes commands, private logs, file fingerprints,
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

Create your own starter with `theseus init`. It refuses to overwrite an existing
recipe. Replace the failing placeholder commands **and** the `.` artifact paths
before running it. `theseus validate --recipe PATH` validates without Docker.

## Workbench controls

Launch with `theseus` or `theseus run --recipe PATH` to inspect commands and public
environment before execution. In preview, `p` cycles available profiles and `s`
cycles the recipe’s declared scenarios and all; `r` starts the displayed selection.
Arrows or `j/k` and Page Up/Down scroll the preview. Preview never executes
automatically.

During live or saved report viewing:

- `Tab` / `Enter` cycles scenario, stage, and evidence focus; `Esc` goes back.
- Arrows or `j/k` select a scenario, stage, or evidence record.
- Left/right in evidence focus switches Stages, Findings, Files, Commands, Logs.
- `/` edits a case-insensitive evidence filter; Enter applies it, Esc ends editing.
  Press `/` and Enter to clear it. Page Up/Down scrolls details.
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

See [the working recipe](examples/standalone/theseus.toml) and its `base/` and
`target/` installers, or the [reinstall recipe](examples/reinstall/theseus.toml) and
[interruption recipe](examples/interruption/theseus.toml).
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

Both `user` and `system` profiles run configure/exercise/checks as UID/GID
10001:10001. User installation/upgrade/reinstall/interruption/recovery/uninstall
uses that same identity; system installer commands use 0:0 **inside the disposable
container**.
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
the example also uses `cmp`, `wc`, and `stat`. Unsupported tools fail closed as
failed/incomplete interruption evidence; Theseus installs no packages.
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

Checks have unique IDs and named checkpoints. `command` expects a shell exit and
optional literal stdout substring. An application exit is evaluated only after a
fresh in-container completion receipt is verified through a separate bounded
control operation; Docker control failures leave the application status unknown.
`executable` verifies PATH resolution, absolute
path, version argv, exit status, and optional literal output. `present` and
`absent` inspect paths (including dangling links); `preserved` compares an exact
path's content hash, kind, link, ownership and mode with configured evidence.
Preservation is valid at upgrade target_installed, reinstalled, uninstalled,
or interrupted/recovered in interrupted upgrade;
use an explicit command check for intentional migrations. File checks must fall
within observed roots. Hashes establish observed bytes, not arbitrary semantic correctness.

`unchanged` is valid at `reinstalled` in schema 2 declaring reinstall, and at
`interrupted`/`recovered` in schema 3 declaring interruption scenarios. A file
compares its hash, size, kind, UID/GID, and mode; a directory compares its own
metadata and all observed descendants, including added/removed children. A
symlink compares its observed link target and metadata without following it.
The declared path must exist in both complete snapshots. Missing paths or changed
entries fail; incomplete, ambiguous, overflowing, or unsafe observations remain
incomplete. Mtime, inode identity, ACLs, extended attributes, hidden remote effects,
and state outside the declared observed scopes are outside this contract.
Reports identify both comparison stages and retain changed-child evidence on the
reinstall stage. Use exact-path `preserved` for retention against configured state;
it does not recursively assert a directory’s children.

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
interleavings, service-manager behavior, downgrade or rollback. Recovery is an
explicit forward application command; ordinary timeout/cancellation never retries
or recovers automatically.

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
0700 directories and 0600 files: atomic `report.json` (schema 3), `journal.json`
(schema 1), and separate bounded logs. Readers accept report schemas 1, 2 and 3; schema 1
continues to represent clean/upgrade evidence, while reinstall/comparison evidence
requires schema 2 or later. Interruption/proof/restart evidence requires schema 3.
Recovery journals remain version 1. No observed file contents are retained except explicit command output; metadata/hashes and diffs are stored.
Reports can be large (load limit 64 MiB), and UI reloads parse saved metadata on progress notifications.

```sh
theseus report .theseus-runs/RUN_ID            # offline TUI; no Docker
theseus report .theseus-runs/RUN_ID --json     # offline machine-readable evidence
theseus cleanup .theseus-runs/RUN_ID          # verified journaled resources only
```

Log references are confined to the run directory with bounded reads and no
symlink following. Treat all logs as sensitive local output. Recovery verifies
recorded container IDs and ownership labels, never a name prefix. An abrupt crash
between Docker resource creation and receiving its ID can precede journaling.
Inspect Docker separately in that exceptional window.

## Validation and scope

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
cargo test --locked --test docker_lifecycle -- --ignored --test-threads=1
cargo test --locked --test interruption_lifecycle -- --ignored --test-threads=1
```

Regular tests include wide/narrow TestBackend behavior and actual Unix PTY
startup/error/restoration. Docker acceptance is opt-in locally and explicitly
required in CI; missing Docker is a failed prerequisite, never a passing skip.
Healthy and broken fixtures exercise all profiles/scenarios, stale PATH,
permissions, preservation, leftovers, baseline deletion, limits, and cancellation.
Reinstall acceptance covers both profiles, reset/duplicate settings, changed mode,
added/removed children, an installer rejecting an existing installation, timeout,
and cancellation during the second invocation. Interruption acceptance covers the
four profile/scenario combinations, actual partial bytes and retained data, descendant stop, same-ID restart without setup or
input recopy, invalid readiness, early completion, recovery failures, transport
errors, timeout and cancellation before readiness, while stopped and during restart.
Both ignored Docker test binaries are explicitly required in CI. Actual PTYs exercise narrow live
selection, new-report comparison visibility, offline viewing, and terminal recovery.

A separate real Phrourion 0.8.1 → 0.8.2 rehearsal passed all four matrix entries
and 32 declared checks, using actual binaries, controlled configuration and a
binary-copy install/remove recipe. This tests that declared cleanup contract;
it does not claim native Phrourion uninstall support. A same-artifact Phrourion
0.8.2 reinstall rehearsal also passed both profiles and 24 checks, including
stable binaries/configuration, runtime behavior, retention, and owned cleanup,
under that same binary-copy contract. Synthetic fixtures remain
reproducible in this repository; real binary artifacts/reports are local evidence.

See [ROADMAP.md](ROADMAP.md) for future capabilities. License: [MIT](LICENSE).
