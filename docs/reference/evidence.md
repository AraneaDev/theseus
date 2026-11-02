# Evidence, outcomes and limits

Run a [first recipe](../get-started/first-run.md) or open an existing report.
Command paths below are relative to the recipe project containing
`.theseus-runs`, rather than necessarily the repository root. Evidence describes
the declared observed contract; recovery has its [own authorization
rules](../operate/recovery.md).

## Outcomes and exit codes

Reports distinguish failed expectations, incomplete observations, cancelled
work, and review findings. Precedence is cancelled → incomplete → failed →
passed. Review findings alone do not fail the declared contract; they are
counted prominently. New unclaimed files, deleted baseline files, and changed
baseline files require review. Unchanged baseline files are not leftovers.

| Exit | Meaning |
| --- | --- |
| 0 | Passed declared expectations (inspect review count); preview exit |
| 1 | Complete run with failed expectations |
| 2 | Invalid input, infrastructure failure, incomplete run, or unresolved forced quit |
| 130 | Cancelled run |

Closing a live/offline TUI returns the displayed run outcome. `report --json`
returns 0 when the saved report was read successfully.

## Capture and observation limits

Commands default to 120 seconds (maximum 3600). Snapshot defaults are 100000
entries and 1 GiB regular-file content per snapshot. Stored stdout/stderr are
independently capped at 16 MiB per stream per command while excess is drained;
truncation is recorded. Truncated check stdout is conservatively incomplete.
Acquisition errors, unsupported archives, symlinked observed-root ancestors and
observation overflow remain incomplete rather than silently passing.

## Saved reports and compatibility

Runs are saved under the recipe project's `.theseus-runs/<id>/`, using private
0700 directories and 0600 files: atomic `report.json` (schema 8), `journal.json`
(schema 1), and separate bounded logs. The new reader accepts report schemas 1
through 8; older released readers need not read schema 8. Schema 1 continues to
represent clean/upgrade evidence, while reinstall/comparison evidence requires
schema 2 or later. Interruption/proof/restart evidence requires schema 3.
Downgrade policy, checkpoints and rejection proof require schema 4. Rollback
policy, failure proof, Changed checks and backup metadata require schema 5.
Interrupted rollback and its typed command expectations require schema 6. Plugin
lifecycle and bounded JSON proofs require schema 7. MCP stdio and client
lifecycle contracts require schema 8. Recipe schemas 1 through 8 remain
supported. Recovery journals and ownership labels remain version 1. No observed
file contents are retained except explicit command output; metadata/hashes and
diffs are stored. Reports can be large (load limit 64 MiB). Live progress
schedules read-only refreshes with at most one read in flight and one coalesced
latest request; stale reads cannot replace final results. Input, cancellation
and the existing five-second active-quit deadline continue while a refresh
reads. Each refresh still parses bounded saved metadata; initial filtering may
scan all records.

## View saved evidence

```sh
theseus report .theseus-runs/RUN_ID            # offline TUI; no Docker
theseus report .theseus-runs/RUN_ID --json     # offline machine-readable evidence
theseus cleanup .theseus-runs/RUN_ID          # verified journaled resources only
```

## Log privacy and containment

Log references are confined to the run directory with bounded reads and no
symlink following. Treat all logs as sensitive local output. Recovery verifies
recorded container IDs and ownership labels, never a name prefix. An abrupt
crash between Docker resource creation and receiving its ID can precede
journaling. Inspect Docker separately in that exceptional window.

## Related documentation

- [Workbench](../guides/workbench.md): page retained text and logs.
- [Recipes](recipes.md): checks, references and observed roots.
- [MCP server](../agents/mcp-server.md): process-bound paging and reply limits.
- [Recovery](../operate/recovery.md): cancellation, incomplete cleanup and journals.

[Documentation index](../README.md) · [Project README](../../README.md)

