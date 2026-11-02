# Validation and acceptance scope

Run every command below from the repository root. See [installation and
controller qualification](../get-started/installation.md) for supported targets
and filesystem boundaries.

Release acceptance requires the same source to pass all three native host jobs
and all eight Linux Docker suites. Native inherited-stdio qualification must
cover both ordinary Rust launchers and CPython `Popen` / `CreatePipe` launchers,
alongside actual-binary suites. Every required launcher must pass; a passing
Rust launcher alone does not establish platform acceptance. Consult the
[source-specific CI
results](https://github.com/AraneaDev/theseus/actions/workflows/ci.yml).

Run the shared CI checks locally before pushing, without registering a runner or
installing a Git hook. Python 3, a native stable Rust toolchain with rustfmt and
clippy, and Node.js for the existing observer controls must be available. Use
Python 3.12 or newer on Windows and the checked Git-for-Windows Bash for the two
POSIX guest-script byte oracles.

| Native machine | Full local native validation |
| --- | --- |
| Linux | `python3 scripts/validate.py native` |
| macOS | `python3 scripts/validate.py native` |
| Windows (PowerShell) | `python scripts/validate.py native` |

Windows VT acceptance tests preserve all ten paste-delimiter splits as separate
`WriteConsoleInputW` batches into the real console reader, proving the first
batch was consumed without an event before submitting the second. A separate
required test sends the complete paste through the ConPTY pipe and checks the
literal Unicode/newline paste, controls, standalone Escape and mode restoration.
The public preview test also splits every payload boundary between complete paste
delimiters into two real pipe writes and checks that neither starts a run or quits.
On the Windows 26100 runner, the retained diagnostic showed that splitting the
raw pipe write after `ESC[2` lost that prefix before any native input record
reached the reader. These tests therefore make no claim that this ConHost
preserves VT sequences across arbitrary raw-pipe write boundaries; the diagnostic remains separate
from release acceptance.

## Docker acceptance

On Linux, also run `python3 scripts/validate.py docker` for all eight required
real Docker suites. This uses one trusted fixture build, one test precompile and
one shared 70-minute acceptance budget. Docker must be reachable; preparation
pulls `ubuntu:24.04` and records the exact image ID used for the MCP fixtures.
The server suite uses the real production `serve` binary over ordinary pipes. No
client, public MCP service, model or credentials are needed.

Full CI runs the three native host jobs and the Docker job in parallel. All four
jobs must pass on the same source. The shared Docker budget starts in the Docker
job itself.

## Receipts and required execution

The script prints its evidence directory (outside the source tree by default).
`--evidence /absolute/new-directory` chooses a retained location outside the
source tree. Receipts contain exact argv, exit codes, logs, compiler host, OS
version/build, source commit, tracked/untracked source-byte hash, dirty/diff
identity and pinned binary hashes. Missing tools/fixtures, artifact replacement,
source changes during validation, failed spawns and watchdog expiry fail
validation. Checked-in required case inventories in
`scripts/validation_cases.json` bind all eight Docker suites and filtered native
controls to named passing executions; zero-test or partial runs fail even when
Cargo returns 0. Not-run cases remain failures. CI invokes the same script
stages and uploads diagnostic logs even on failure. The additional Windows CI
observer uploads bounded in-flight close-model checkpoints; the native model
test itself runs locally too.

One machine validates its **native OS only**. Other native OS results are
recorded as `NOT_RUN`; cross-compilation and Linux containers do not establish
Windows or macOS runtime acceptance. Run the same source on those actual
operating systems for the required three-host gate. The [CI
results](https://github.com/AraneaDev/theseus/actions/workflows/ci.yml) record
that gate for each source revision.

## Trusted native fixtures

Application process tests require the trusted native fixture built by
`tests/support/build_host_fixture.py`. It copies the default-feature production
application before feature builds can replace Cargo output, then separately pins
the native host fixture, application test graph and supervised MCP fixture.
Cargo JSON artifact records and graph features identify each executable in an
owned temporary directory. `artifacts.json` records their
source/features/paths/hashes; CI rechecks all pinned hashes after workspace
tests so feature rebuilds cannot replace the binaries being exercised. Release
host features exclude test-support and the application test graph includes it.
Missing `THESEUS_HOST_TEST_FIXTURE` is a test failure. In PowerShell, set it
with `$env:THESEUS_HOST_TEST_FIXTURE = python
tests/support/build_host_fixture.py`. The fixture is a test tool and is not
installed as an application command.

CI explicitly runs Unix `pty_master_` mode controls, Windows actual-library
`conpty_legacy_crossterm` controls and the Windows
`conpty_backpressure_close_model` target with `--features test-support --lib` on
`theseus-host`, in addition to the native application targets. Ordinary cargo
test alone does not replace these required acceptance invocations. Python
version, guest byte-oracle output and raw guest-template bytes/counts/hashes are
retained. Windows preparers require Python 3.12 or newer. Two repository-owned
POSIX guest script oracles on Windows additionally require the checked
Git-for-Windows Bash; it is not used to launch Theseus or establish native host
behavior. Missing prerequisites fail. Default fixtures require no user
credentials, vendor clients, models, GUI session or nested VM; external
real-client tests have separate inputs.

## Native lifecycle boundaries

Cancellation registration is scoped to the active CLI/TUI run. Windows console
Ctrl-C and Ctrl-Break request cancellation; raw TUI Ctrl-C uses its existing key
handling. Console close, logoff, and shutdown still have Windows-imposed process
termination deadlines, so these events cannot guarantee completion of cleanup.
The TUI restores the terminal before joining outstanding engine work;
exceptional native cleanup can delay process return beyond the interactive quit
deadline. Cancellation bounds assume a healthy native kernel. If a kernel
operation does not resolve, its owned storage must remain retained; no hard
kernel-hang deadline or detached cleanup success is claimed. Host recovery
commands use POSIX shell quoting on Unix and PowerShell quoting on Windows.
Displayed non-Unicode paths may be lossy and are not exact copyable argv.

## Acceptance coverage

Regular tests include wide/narrow TestBackend behavior and actual Unix PTY
startup/error/restoration. Synthetic large-report public CLI PTYs reach records
beyond 65,535, filter-cache overflow, full Unicode detail, comparison difference
101 and logs beyond 64 KiB while checking offline Docker sentinels and unchanged
source bytes. These are UI fixtures, not a claim of a real 70,000-file
installation. Docker acceptance is opt-in locally and explicitly required in CI;
missing Docker is a failed prerequisite, never a passing skip. Healthy and
broken fixtures exercise all profiles/scenarios, stale PATH, permissions,
preservation, leftovers, baseline deletion, limits, and cancellation. Reinstall
acceptance covers both profiles, reset/duplicate settings, changed mode,
added/removed children, an installer rejecting an existing installation,
timeout, and cancellation during the second invocation. Interruption acceptance
covers the four profile/scenario combinations, actual partial bytes and retained
data, descendant stop, same-ID restart without setup or input recopy, invalid
readiness, early completion, recovery failures, transport errors, timeout and
cancellation before readiness, while stopped and during restart. Downgrade
acceptance covers both profiles and policies, real old-data readers, silent
reader omissions, lost records, changed binary/mode/link/configuration,
added/removed data children, wrong status/marker, unexpected success, missing
receipt/reference, transport failure, truncated output and timeout/cancellation.
Rollback acceptance covers both profiles and restoration policies, exact failure
and Changed evidence, complete mapped backup/restoration checks,
runtime/readers, source/backup corruption, baseline-root protection,
no-op/partial restoration, metadata/link/child changes, reference timing and
timeout/cancellation. Interrupted-rollback acceptance adds both
profiles/policies with same-ID stop and restart, original/immediate/fresh
reference boundaries, typed runtime/reader proof, faults that block restoration
and exact owned teardown. Default animation PTYs exercise frames, skips,
discarded input, opt-outs, active cancellation, signals, errors and terminal
restoration. All eight ignored Docker test binaries, including `mcp_docker` and
`mcp_server_docker`, are explicitly required in one CI job. Actual PTYs exercise
narrow live selection, new-report comparison visibility, offline viewing, and
terminal recovery.

## Separate real-artifact rehearsals

A separate real Phrourion 0.8.1 → 0.8.2 rehearsal passed all four matrix entries
and 32 declared checks, using actual binaries, controlled configuration and a
binary-copy install/remove recipe. This tests that declared cleanup contract; it
does not claim native Phrourion uninstall support. A same-artifact Phrourion
0.8.2 reinstall rehearsal also passed both profiles and 24 checks, including
stable binaries/configuration, runtime behavior, retention, and owned cleanup,
under that same binary-copy contract. Synthetic fixtures remain reproducible in
this repository; real binary artifacts/reports are local evidence.

## Related documentation

- [Installation](../get-started/installation.md): native architectures and host/guest distinctions.
- [MCP server](../agents/mcp-server.md): required ordinary inherited-pipe acceptance.
- [Evidence](../reference/evidence.md): interpreting report and command outcomes.
- [Roadmap](../../ROADMAP.md): capabilities outside the current contract.

[Documentation index](../README.md) · [Project README](../../README.md)

