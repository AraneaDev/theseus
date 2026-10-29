# Rollback after an interrupted upgrade

These recipes need only Theseus, Docker and the local `ubuntu:24.04` image.
All application inputs are included shell scripts; no external application,
account, credential or downloaded installer is needed. From the repository root:

```sh
cargo build --release --locked
docker pull ubuntu:24.04
target/release/theseus validate --recipe examples/interrupted-rollback/binary-only.toml
target/release/theseus validate --recipe examples/interrupted-rollback/restore-data.toml
target/release/theseus run --headless --recipe examples/interrupted-rollback/binary-only.toml
target/release/theseus run --headless --recipe examples/interrupted-rollback/restore-data.toml
target/release/theseus run --recipe examples/interrupted-rollback/restore-data.toml
```

Each recipe runs both user and system profiles by default. Add `--profile user`
or `--profile system` to select one, and `--scenario interrupted-rollback` for an
explicit scenario selection. Interactive mode previews the barrier, kill/restart,
references, policy, mappings and commands; press `r` to run. Reports are saved
under this directory's `.theseus-runs/`. Reopen one from this directory with
`theseus report .theseus-runs/RUN_ID`, even without Docker. `--no-animation`
skips the decorative intro and outro; headless execution never animates.

## The application and deliberate stop

`artifacts.base` is the declared known-good 1.0.0 tool and `artifacts.target` is
its candidate 2.0.0 replacement. These roles are explicit, not inferred version
ordering. Configure creates format-1 records `alpha=11` and `beta=29`, a nested
child and a `current -> records` symlink. The real old tool validates both values,
the child and link, then calculates `reader records=2 sum=40` as runtime UID/GID
10001:10001.

The candidate replaces the installed executable, changes the data header to
format 2 and adds a child. It starts a background writer, then calls the
runner-provided barrier synchronously:

```sh
/bin/sh "$THESEUS_INTERRUPT_BARRIER"
```

The helper never returns normally. Theseus verifies its fresh nonce-bound
readiness, sends SIGKILL to the whole container and verifies that it stopped.
The interruption command has outcome `interrupted` and no normal application
exit status. A shell status such as 137, readiness text alone, early ordinary
completion, timeout or cancellation cannot authorize rollback.

Theseus restarts exactly the same container ID, verifies it running and observes
partial state before checks. It does not repeat setup, copy inputs, chown,
configure or run forward recovery. The example checks that the writer's byte
count stays fixed after restart and that the code after the barrier never ran.
The selected `changed` check proves actual data mutation.

## References and restoration policies

Configure, exercise and KnownGood checks finish before the immutable
`before_upgrade` restoration reference. For restore-data, backup and its checks
follow. A separate `before_interrupt` is captured immediately before the
candidate; Changed compares that reference to the first `interrupted` capture.
Backup/check side effects therefore cannot masquerade as candidate mutation.
Interrupted checks and their effects finish before a fresh `before_rollback`
observation revalidates backup readiness against original `before_upgrade`.
Stop/restart stages themselves have no snapshots; nothing is observed while
stopped.

Both policies explicitly copy back the original executable. Its full fingerprint
must match `before_upgrade`, and its actual version command must match the original
executable name, resolved path, arguments and literal output marker. A real old-tool
data reader must also pass. Required runtime and reader checks retain typed
`command_expectation` evidence: expected exit/marker, actual matched literal,
command/log references and, for runtime checks, the executable binding. A marker
or zero-return rollback command alone is insufficient.

| Policy | Data after rollback | Evidence |
| --- | --- | --- |
| `binary_only` | Format-2 records and added child remain | Original binary/runtime and actual old-tool reader prove compatibility with retained partial data |
| `restore_data` | Original format-1 subtree, without the added child | All binary/runtime/reader checks plus mapped backup, restoration and cleanup checks |

The old reader deliberately supports either header. Binary-only therefore claims
compatibility with this retained state, never restored data. Restore-data maps
`/home/theseus/.local/share/sample-cli` to `/workspace/rollback-backup/saved`,
where `stored = "saved"` is relative to the declared backup root. `cp -a` preserves
bytes, ownership, modes and symlink text inside this disposable container. No
backup contents are exported to the host; reports keep fingerprints, metadata,
comparisons and explicit command output.

## Mapped checks and cleanup authorization

The restore-data policy adds mandatory ordinary CheckResult records:

| Stage/checkpoint | Generated evidence |
| --- | --- |
| `backup` | `__theseus_rollback_root_absent` proves the entire backup root absent in original known-good state |
| `backup_created` | `__theseus_rollback_source_stable_data` and `__theseus_rollback_backup_matches_data` prove unchanged source and mapped backup equal original state |
| `interrupted` | `__theseus_rollback_backup_intact_data` proves backup still equals the original |
| `before_rollback` | `__theseus_rollback_backup_ready_data` repeats the mapping proof using the fresh observation after earlier checks |
| `rolled_back` | `__theseus_rollback_restored_data` and `__theseus_rollback_backup_retained_data` prove source restored and backup intact |
| `backup_removed` | `__theseus_rollback_backup_removed` proves the entire backup root absent again |

The suffix `data` comes from the recipe's scope ID. Mapped records retain source
and stored paths and comparison stages. Equality includes exact child sets,
kinds, content hashes, sizes, UID/GID, modes and link text without following links.
Every required and additional check must pass. Missing stop/restart proof, missing
observations, absent typed role evidence, corrupt backup, no-op/partial restore,
metadata/link mismatch or failing runtime/reader cannot establish verified
restoration. The UI's verified banner requires actual complete Passed evidence.

A preexisting backup root blocks backup without deleting it. Backup cleanup is
authorized only after absence was verified and backup actually starts, including
partial failure. Failed prerequisites skip forward mutations. Authorized cleanup
and uninstall may run only while the container is verified running; unavailable
containers are never restarted merely for cleanup. Cancellation removes the exact
owned resource through journaled teardown, rather than claiming uninstall checks.

Both profiles configure, exercise and run checks as 10001:10001. User mutations
use that identity; system install, backup, interruption, rollback, cleanup and
uninstall run as root inside the container. Data remains under `/home/theseus`
regardless of mutation HOME. Uninstall removes the declared executable; data
remains observed until exact owned-container teardown verifies ownership/removal.

## Limits and report semantics

The barrier is cooperative and declared, after synchronous sample mutations.
Application authors own quiescence and application-specific backup/restore.
Observed file equality does not prove database consistency, atomic snapshots,
surviving external-writer safety, arbitrary interruption timing, host backups or
host power-loss durability. There is no automatic old-installer fallback or retry.
Ordinary failed-upgrade rollback remains a separate scenario with a required
normal-exit failure expectation; this scenario has no `failure` field.

Recipe/report schema 6 adds interrupted rollback and typed role evidence. Readers
accept schemas 1 through 6; journal and ownership formats remain version 1.
Exit 0 means all declared expectations and cleanup passed, 1 means failed
expectations, 2 means invalid input/incomplete evidence and 130 means cancellation.
`report --json` returns 0 when reading succeeds, irrespective of the saved outcome;
inspect its `outcome` too. Offline reports with missing proof normalize to
Incomplete; stored check results remain inspectable.
