# Rollback after a declared failed upgrade

These recipes require only Theseus, Docker and the local `ubuntu:24.04` image.
All application inputs are included shell scripts; there is no account,
credential, external application or downloaded installer.

From the repository root:

```sh
cargo build --release --locked
docker pull ubuntu:24.04
target/release/theseus validate --recipe examples/rollback/binary-only.toml
target/release/theseus validate --recipe examples/rollback/restore-data.toml
target/release/theseus run --headless --recipe examples/rollback/binary-only.toml
target/release/theseus run --headless --recipe examples/rollback/restore-data.toml
target/release/theseus run --recipe examples/rollback/restore-data.toml
```

Each run selects both profiles by default. Add `--profile user` or
`--profile system` to select one. Interactive mode previews the selected policy,
failure expectation, scope mapping, generated checks and commands; press `r` to
run. `--scenario rollback` selects the single declared scenario explicitly.
Reports are saved under this directory's `.theseus-runs/`; reopen one with
`theseus report .theseus-runs/RUN_ID` from this directory, even without Docker.

## What the application does

`artifacts.base` is the declared known-good 1.0.0 tool, and `artifacts.target` is
the candidate 2.0.0 tool. These roles are explicit, with no inferred version order.
The base installer copies a real executable to the profile's binary directory.
Configure creates format-1 records `alpha=11` and `beta=29`, a nested child, and a
`current -> records` symlink. The actual old tool reads the records, validates
both values and child/link contents, and prints `reader records=2 sum=40`.

The candidate synchronously replaces the executable, changes `format=1` to
`format=2`, adds a child, prints `upgrade failed after migration`, and exits 9.
The old reader deliberately supports either format header. Neither a marker by
itself nor a nonzero Docker exit establishes this failure: Theseus requires the
normal application completion receipt, exact status 9, complete bounded stdout,
and the literal marker. The `candidate-changed-state` Changed check independently
proves mutation against `before_attempt`. Unexpected success, wrong status/marker,
missing receipt, transport failure, truncation, timeout or cancellation cannot
authorize rollback.

Both policies explicitly copy the original binary back, require its fingerprint
to match `before_upgrade`, execute its bound original version check, and run the
actual old data reader. The runtime check matches the BaseInstalled executable's
name, resolved path, arguments and literal marker, and its path is the selected
Unchanged binary path. A zero-return rollback command is insufficient.

| Policy | Data after rollback | Evidence |
| --- | --- | --- |
| `binary_only` | Migrated format-2 data and added child remain | Original binary/runtime and real old-tool reader; no data-restoration claim |
| `restore_data` | Original format-1 subtree, without the added child | All binary/runtime/reader checks plus independent mapped backup and restored-scope equality |

The restore-data backup maps source `/home/theseus/.local/share/sample-cli` to
`/workspace/rollback-backup/saved` (`stored = "saved"` relative to `backup_root`).
The recipe's `cp -a` preserves bytes, ownership, modes and link text inside this
single disposable container. No file-content backup is exported to the host.
The report records fingerprints, metadata, comparisons and explicit command logs.

## References and generated checks

Known-good timing matters: configure, exercise and KnownGood checks finish before
`before_upgrade` captures the restoration reference. Backup and BackupCreated
checks follow; then the separate `before_attempt` captures the immediate
pre-candidate state. Changed compares against that reference, so backup/check
side effects cannot masquerade as candidate mutation. UpgradeFailed checks and
their side effects finish before a fresh `before_rollback` observation validates
backup readiness. The original `before_upgrade` remains the restoration reference.
`preserved`, if declared, would still compare configured state.

Theseus adds normal CheckResult records for each restore-data requirement:

| Stage/checkpoint | Mandatory generated evidence |
| --- | --- |
| Before backup starts (`backup` stage) | `__theseus_rollback_root_absent` proves the entire backup root absent in before_upgrade |
| `backup_created` | `__theseus_rollback_source_stable_data` and `__theseus_rollback_backup_matches_data`: unchanged source and mapped backup equal known-good |
| `upgrade_failed` | `__theseus_rollback_backup_intact_data`: backup still equals original known-good |
| `before_rollback` | `__theseus_rollback_backup_ready_data`: fresh backup mapping proof after earlier checks |
| `rolled_back` | `__theseus_rollback_restored_data` and `__theseus_rollback_backup_retained_data`: source restored and backup intact |
| `backup_removed` | `__theseus_rollback_backup_removed`: entire backup root absent again |

Check IDs use the declared scope ID `data`; authors cannot use the reserved
`__theseus_rollback_` prefix. Mapped records identify both source and stored paths,
plus comparison stages. Equality includes the subtree's exact child set, kinds,
hashes, sizes, UID/GID, modes and symlink text without following links. All selected
and additional checks must pass. Missing/corrupt backup blocks restoration;
missing children, wrong metadata, no-op restore or a failing actual reader cannot
produce restored-data success. A backup root present beforehand fails without
running backup or deleting that preexisting root. Cleanup is authorized only once
known absence is verified and backup actually starts, including partial failure.

## Identity, cleanup and limits

Both profiles configure, exercise and check behavior as UID/GID 10001:10001.
User mutations also use 10001:10001; system install, backup, attempt, rollback,
backup cleanup and uninstall use root inside the container. Application data
always resides under `/home/theseus`, independent of the mutation shell's HOME.
Uninstall removes only the declared executable; retained data remains observed
until the exact owned container is removed. Journaled teardown verifies ownership.

The shell commands use `set -eu` and propagate failed copy/test operations. These
restore commands are specific to this sample application. Its mutations are
synchronous and quiesced. Real application authors must arrange quiescence and
application-specific backup/restore; observed file equality does not prove an
atomic or database-consistent snapshot, surviving-writer safety, host backup,
or power-loss durability. There is no generic host restore, automatic old-installer
fallback, retry or container restart in this scenario.

Recipe/report schema 5 supplies rollback, Changed, failure proof and backup
metadata. Earlier recipes retain their semantics, and readers accept reports
1/2/3/4/5; journals and ownership remain version 1. Headless/TUI outcomes use exit
0 for all declared expectations passed (including cleanup), 1 for failed
expectations, 2 for invalid input/incomplete evidence, and 130 for cancellation.
`report --json` returns 0 when it successfully reads a report, irrespective of the
saved run outcome. Inspect the saved outcome and review findings as well.

This milestone handles an ordinary declared failed upgrade. Rollback after an
interrupted upgrade remains required next; the interruption examples currently
perform explicit forward recovery.
