# Theseus

Know what your tool leaves behind.

Theseus rehearses CLI installation, configuration, upgrade, runtime checks, and
uninstall in disposable Linux containers. A Ratatui workbench and headless CLI use
one scenario engine. Evidence includes commands, private logs, file fingerprints,
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

Create your own starter with `theseus init`. It refuses to overwrite an existing
recipe. Replace the failing placeholder commands **and** the `.` artifact paths
before running it. `theseus validate --recipe PATH` validates without Docker.

## Workbench controls

Launch with `theseus` or `theseus run --recipe PATH` to inspect commands and public
environment before execution. In preview, `p` cycles available profiles and `s`
cycles clean/upgrade/all; `r` starts the displayed selection. Arrows or `j/k` and
Page Up/Down scroll the preview. Preview never executes automatically.

During live or saved report viewing:

- `Tab` / `Enter` cycles scenario, stage, and evidence focus; `Esc` goes back.
- Arrows or `j/k` select a scenario, stage, or evidence record.
- Left/right in evidence focus switches Stages, Findings, Files, Commands, Logs.
- `/` edits a case-insensitive evidence filter; Enter applies it, Esc ends editing.
  Press `/` and Enter to clear it. Page Up/Down scrolls details.
- `c` requests cancellation. `q` exits when idle; during a run it cancels and waits
  up to five seconds for worker completion. If cleanup remains unresolved, exit 2
  identifies the report/journal and resources requiring inspection and recovery.

Resize preserves selections and uses stacked panels on narrow terminals. Logs
show one selected stream, at most 64 KiB; truncation is visible and terminal
control bytes are replaced. A finished session remains available for browsing;
launch a new session for another run. Raw mode and alternate screen are restored
on exit, errors, and panic. Interactive mode requires stdin and stdout terminals;
without them, use headless or JSON output.

## Recipe contract

See [the working recipe](examples/standalone/theseus.toml) and its `base/` and
`target/` installers. `theseus.toml` uses schema version 1, rejects unknown fields,
and defines a name, image, network (`none` by default or explicit `bridge`),
artifacts, profiles, and nonoverlapping absolute observation roots. Inputs resolve
inside the recipe project, are materialized and hashed, then copied into
`/inputs/base` and `/inputs/target`. There are no writable host source mounts,
host home/socket mounts, host namespaces, privileged mode, or forwarded ports.

Both `user` and `system` profiles run configure/exercise/checks as UID/GID
10001:10001. User installation/upgrade/uninstall uses that same identity; system
installation/upgrade/uninstall uses 0:0 **inside the disposable container**.
Steps are `/bin/sh -c` scripts with `/workspace` as working directory. Each step
is a new shell: files persist, shell variables and working-directory changes do
not. HOME, XDG directories and PATH are set explicitly. Runtime PATH defaults to
`/home/theseus/.local/bin:/usr/local/bin:/usr/bin:/bin`. Step/check `env` contains
public literal values; `pass_env` explicitly opts in named host variables. Values
from `pass_env` are not serialized as metadata, but applications can print secrets
into logs. No credentials are forwarded automatically and no general secret
redaction is promised.

The image must supply `/bin/sh`, `/usr/bin/env`, `sleep`, `mkdir`, `chown`, `chmod`,
and `tar`, with compatible UID/GID 10001 assignments. Image-declared volumes are
rejected. Ubuntu 24.04 is exercised by acceptance; other images must satisfy
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

Checks have unique IDs and named checkpoints. `command` expects a shell exit and
optional literal stdout substring. `executable` verifies PATH resolution, absolute
path, version argv, exit status, and optional literal output. `present` and
`absent` inspect paths (including dangling links); `preserved` compares an exact
path's content hash, kind, link, ownership and mode with configured evidence.
Preservation is valid at upgrade target_installed or uninstalled; use an explicit
command check for intentional migrations. File checks must fall within observed
roots. Hashes establish observed bytes, not arbitrary semantic correctness.

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
0700 directories and 0600 files: atomic `report.json` (schema 1), `journal.json`,
and separate bounded logs. No observed file contents are retained except explicit
command output; metadata/hashes and diffs are stored. Reports can be large (load
limit 64 MiB), and UI reloads parse saved metadata on progress notifications.

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
```

Regular tests include wide/narrow TestBackend behavior and actual Unix PTY
startup/error/restoration. Docker acceptance is opt-in locally and explicitly
required in CI; missing Docker is a failed prerequisite, never a passing skip.
Healthy and broken fixtures exercise all profiles/scenarios, stale PATH,
permissions, preservation, leftovers, baseline deletion, limits, and cancellation.

A separate real Phrourion 0.8.1 → 0.8.2 rehearsal passed all four matrix entries
and 32 declared checks, using actual binaries, controlled configuration and a
binary-copy install/remove recipe. This tests that declared cleanup contract;
it does not claim native Phrourion uninstall support. Synthetic fixtures remain
reproducible in this repository; real binary artifacts/reports are local evidence.

See [ROADMAP.md](ROADMAP.md) for future capabilities. License: [MIT](LICENSE).
