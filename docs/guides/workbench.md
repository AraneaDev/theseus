# The workbench

Start from a [validated recipe](../get-started/first-run.md), or open a saved
run with `theseus report PATH`. Recipe paths in repository examples are relative
to the repository root. The same evidence and outcomes are available through the
[headless CLI](../get-started/first-run.md) and [MCP
server](../agents/mcp-server.md).

Launch with `theseus` or `theseus run --recipe PATH` to inspect commands and
public environment before execution. In preview, `p` cycles available profiles
and `s` cycles the recipe’s declared scenarios and all; `r` starts the displayed
selection. The preview marks the current selection in brackets and states it
on a `Will run:` line, and groups the fixed runtime identity, home, XDG and
`PATH` values under an `Environment` heading. Arrows or `j/k` and Page Up/Down
scroll the preview. Preview never executes automatically. A comparison suite
preview (`theseus compare --suite PATH`) lists its Selection, Stages, Scopes and
Variants (the baseline is marked) and states how many lifecycle runs and
comparison cells the declared suite implies, computed from the manifest alone.

The footer shows the current status followed by the keys that apply in the
current mode and focus; lower-priority hints are omitted when the terminal is
too narrow. Press `?` for a centred key reference listing every binding of the
current mode and focus, grouped as Navigate, Evidence, Logs, Run control and
Editing; `?` or Esc closes it, and no other key acts while it is open. In the
editor, the Save Copy name, confirmations and the `/` filter draft, `?` is
ordinary text. Setting `NO_COLOR` to any non-empty value (see
[no-color.org](https://no-color.org)) renders without colour; glyphs, bold,
reverse video and layout are unchanged. It is read once at startup. Scenario, stage and comparison rows start with a status glyph (✓
passed, ✗ failed, ! review, … incomplete or pending, – skipped), and the focused
selector is marked `▶` with a heavy border. Evidence tabs show record counts.
The scenario/stage column widens to fit the longest visible row when the
terminal has room, keeping at least 70 columns for evidence. During a run the
header shows a spinner, the running scenario and stage (from the latest engine
event while the saved report catches up) and the elapsed time; the scenario
holding the running stage shows `▸`. After the run the header shows the total
duration. Native evidence views use the same header, selector tabs and footer.

## Edit a preview

In a pristine recipe or comparison suite preview, `e` opens a raw TOML editor
before the first run. It opens at the top (`Ln 1 Col 1`) and preserves
comments, Unicode and exact source bytes. At 60 columns or wider it shows a
dim line-number gutter and colours TOML table headers, keys, strings and
comments; colouring is display only.
Ordinary `q`, `r`, `p`, `s` and `d` typed in the editor are text. Use arrows,
Home/End, Ctrl-Home/End, Enter, Tab and Backspace/Delete to edit; bracketed
paste inserts once. Ctrl-T validates the draft without saving or running it.
Ctrl-S opens Save Copy; enter a new basename and press Enter to save, or Esc to
return. Esc in the editor closes it while retaining an unsaved draft. The
preview then labels the displayed details as the accepted source and blocks `r`,
`p` and `s`. Press `d` to discard or `q` to quit: Enter confirms, Esc keeps the
draft. Ctrl-C and SIGINT/SIGTERM exit immediately without saving.

The editor accepts at most 1 MiB of UTF-8 source and rejects an oversized
insertion or paste as one operation. This limit applies to editing, not existing
recipe loading. Pathological combining or joined-glyph sequences may be replaced
by spaces of the same display width, with a `Display elided` notice; the raw
draft and saved bytes remain exact. Save Copy validates the whole draft and
creates only a new file adjacent to the accepted source in its canonical
directory, so relative artifact and variant paths keep their meaning. The
basename must be 1–128 ASCII bytes using letters, digits, `.`, `_` or `-` and
end in `.toml`. Existing files and symlinks are never overwritten. A failed save
retains the draft; if a failure occurs after creating a new file, the diagnostic
identifies that attempted adjacent path for inspection. A successful save adopts
the copy as the accepted source. Nothing is autosaved or run: press fresh `r` to
execute the saved source.

## Startup and exit scenes

Interactive startup and ordinary idle exit show Theseus entering and retracing a
labyrinth with Ariadne's thread: six 900ms frames per scene (5.4 seconds each).
These scenes are decorative, with no observed lifecycle progress or success
claim. Space skips startup; `q` quits startup without starting a run. Space or
`q` skips exit. Run/selection input during startup is discarded. Global
`--no-animation` skips both scenes, for example `theseus --no-animation run
--recipe PATH`. TERM=dumb, terminals below 40 columns or 18 rows, and a resize
below that size skip without dwell. Headless, JSON, validate, init, help and
version never animate. Active quit starts cancellation immediately; signals,
errors and cleanup deadlines bypass the exit scene and preserve the actual
outcome and terminal restoration.

## Browse live and saved evidence

During live or saved report viewing:

- A saved report opens on the first scenario and stage needing attention:
  the first failed one (by stage or check outcome), else the first incomplete
  one, else the first scenario with review findings; a report with none opens
  on the first row. A live run moves the selection the same way when it
  finishes, but only if you have not navigated during the run.
- `Tab` / `Enter` cycles scenario, stage, and evidence focus; `Esc` goes back.
- Arrows or `j/k` select a scenario, stage, or evidence record; Home/End selects
  the first/last record in the focused list.
- Left/right in evidence focus switches Stages, Findings, Files, Commands, Logs.
- `/` edits a case-insensitive evidence filter; Enter or Esc applies the draft.
  Press `/` and Enter to clear it. Matches include full stored record details.
  While a filter is active, matched text is highlighted (reverse video) in the
  visible record summaries and the current detail chunk only.
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
show one selected stream in bounded pageable chunks; stored and capture limits
are visible, and terminal control bytes are replaced. A finished session remains
available for browsing; launch a new session for another run. Raw mode and
alternate screen are restored on exit, errors, and panic. Interactive mode
requires stdin and stdout terminals; without them, use headless or JSON output.

## Paging and display bounds

Browsing uses bounded windows: at most 64 evidence summaries and 64 rows per
selector, one 16 KiB detail chunk, and at most 65,536 cached filter match or
checkpoint positions. Larger result sets remain reachable. Summaries may show an
explicit ellipsis; successive detail chunks expose all retained text, including
Unicode. The compatibility log mirror retains only the current bounded body. Log
APIs read at most 64 KiB at an explicit raw-byte offset; the UI reads 4 KiB plus
at most four UTF-8 guard bytes. The footer shows the actual byte range and
whether more bytes are stored. File identity/size/time checks invalidate changed
cached logs; these checks establish freshness, not authorship.

Pagination exposes stored evidence only. A last stored log chunk can still be
labelled capture truncated; a shortened file is incomplete current storage, and
unpublished appended bytes stay hidden. Capture limits and comparison-budget
omissions are unchanged and cannot be recovered by paging. Preview never
executes automatically. Pristine recipe and comparison previews support explicit
editing and Save Copy as described in [Edit a preview](#edit-a-preview).

## Related documentation

- [Evidence and limits](../reference/evidence.md): storage, capture and schema boundaries.
- [Comparison suites](comparisons.md): cell and evidence selection.
- [Recovery](../operate/recovery.md): cancellation and incomplete cleanup.

[Documentation index](../README.md) · [Project README](../../README.md)

