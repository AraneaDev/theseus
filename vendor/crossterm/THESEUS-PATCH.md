# Theseus patch to crossterm 0.28.1

This directory was extracted from the exact crates.io archive locked by Theseus
commit `1b3240eeccc62b44befa27f7ec81efe5eba6f0e6`. Its SHA-256 is
`829d955a0bb380ef178a640b91779e3987da38c9aea133b20614cfed8cdea9c6`.
`THESEUS-PROVENANCE.json` retains all 75 upstream file hashes and separately
records each modified and added source file. Unmodified upstream files and the
original license retain their exact bytes. Edited upstream Rust files retain
CRLF line endings; the added Windows VT decoder uses LF. The Unix Mio patch is
unchanged. Windows event ingestion and the shared VT parser now also have the
narrow changes documented below.

The Unix Mio source previously returned the first parsed event or resize from a
readiness batch without retaining the remaining tokens. Returning from a TTY
read also discarded readiness for bytes beyond that read. Edge-triggered input
could then be stranded. The patch retains a deduplicated queue containing at
most the three registered tokens. It rotates retained TTY work behind other
sources and polls without waiting while work remains, so newly-ready signals
and wakeups can join each bounded round. Existing parser backlog is consumed
first, as upstream does; signal, wake-event and timeout types are unchanged.

The input descriptor can borrow blocking stdin. Before each read, FIONREAD
checks the currently available count; a read requests at most that count and
the original 1024-byte buffer size. It never changes the shared file status
flags. The intended crossterm input path has one serialized reader. A competing
external reader could invalidate this observation; this is not a universal
kernel nonblocking guarantee. The original parser's `more` hint remains tied to
a full 1024-byte chunk, retaining its existing escape-sequence chunking contract.

Zero availability, a zero-byte read, or WouldBlock retires that readiness;
Interrupted retains it. Other read errors are returned. Those branches prevent
a retained token from spinning on EOF or a persistent error, instead of the
upstream inner loop retrying forever. The patch introduces no EOF event, parser
rewrite, polling sleep, key resend, thread, library version or feature change.

Required first-party native controls are in
`crates/theseus-host/src/process/unix/terminal/events.rs`: concurrent resize plus
a single key, 2051 queued keys across reads, an incomplete CSI consumed before its final byte, and
new input after draining (including zero-timeout polling). They run the actual
library in an owned PTY. The unmodified archive source demonstrated missing
input/resize; the patched source passed the controls locally on Linux. This
does not identify the exact failed syscall of a separate hosted macOS run.
Actual macOS application resize/quit and all native application gates remain
required. The separate Windows legacy mode control still executes the unchanged
crossterm mode APIs and must demonstrate mode drift on adverse baselines.

The crate remains a third-party dependency, excluded from first-party workspace
membership exactly as its registry predecessor was. No upstream dev-dependency
graph is added to the workspace gates. Existing upstream lint warnings are left
unchanged; all first-party workspace targets remain subject to strict clippy.

## Windows VT character records

ConPTY's non-VT input mode consumes CSI 200~/201~ without preserving paste
boundaries. Theseus now enables ENABLE_VIRTUAL_TERMINAL_INPUT only inside its
existing exact-restoration terminal guard. Upstream crossterm raw-mode APIs are
unchanged, including the legacy negative control.

`src/event/source/windows.rs` retains ReadConsoleInput, native resize/mouse/key
handlers, and the existing poll deadline. Raw key-down records with virtual key
zero enter `source/windows_vt.rs`; raw key-up records are ignored. UTF-16 pairs
are decoded before UTF-8 parsing. Repeat counts are consumed one unit at a time
with at most one retained input record, preserving order and timeout checks.
A lone Escape resolves on actual queue exhaustion or before a native record,
without a delay threshold. ConPTY queues a complete parsed sequence under its
console lock; the required native fragmented-write control tests that assumption.
Incomplete CSI and paste survive poll timeouts. A native key interleaved into an
unfinished VT sequence errors rather than dispatching paste content as commands.

`src/event/sys/windows.rs` includes the existing VT grammar via a path alias.
A private parsed-event enum consumes complete query reports without changing
Windows' public/native event API or retaining skipped reports. The shared
`sys/unix/parse.rs` uses a cfg-specific internal-event import and its two raw-mode
queries use the existing public Result API. Unix gets exactly the same boolean
as before; Windows query errors propagate. No VT grammar was duplicated.

The new buffer is capped at 2 MiB, above Theseus' 1 MiB valid editor draft plus
both delimiters. Invalid UTF-16, malformed VT, and oversize sequences permanently
error the decoder; subsequent payload is never reclassified as command keys.
There is no additional event queue, thread, timeout, library version, or feature.

Focused decoder tests cover literal command/control/Unicode paste, incomplete
paste, standalone Escape/arrows/Ctrl-C, invalid UTF-16 and the size bound. Native
controls live in `crates/theseus-host/src/process/windows/terminal/events.rs`:
fixed raw records in legacy/VT modes, every delimiter split through ConPTY,
actual paste/arrow/control/Escape events, raw repeats/surrogate key-ups/native
fallback ordering, and exact mode restoration. The existing public preview
acceptance still proves paste outside the editor inert and saved Unicode bytes
exact. The added fragmented transport cases are Windows-specific; they do not
change Unix's existing lone-Escape/chunk boundary contract.

Primary behavior references:
- https://github.com/microsoft/terminal/blob/main/src/terminal/parser/InputStateMachineEngine.cpp
- https://github.com/microsoft/terminal/blob/main/src/host/inputBuffer.cpp
- https://github.com/microsoft/terminal/blob/main/src/host/VtInputThread.cpp

Local Linux decoder/parser tests cannot establish Windows runtime acceptance.
The native controls and all application/restoration gates must pass on the
actual supported Windows CI source before acceptance.

### Reviewed malformed-input and raw-release corrections

Inert virtual-key-zero releases are excluded from Escape resolution before
native-event ordering is considered. A fixed queued ESC-down/ESC-up/paste-opener
control requires exactly one literal `r\nq` Paste before the original ordered
repeat, surrogate-release and native-key controls.

The shared cursor, SGR/rxvt mouse and X10 coordinate conversions now use checked
subtraction. Invalid zero/below-offset coordinates return an error rather than
panicking or wrapping; valid coordinates are unchanged on Unix and Windows.
Existing checked mouse-button offsets remain. Fourteen independent malformed
cursor/mouse/button literals require InvalidData and continued rejection of
following r/q through the actual decoder, without panic shielding. These cases
are additional to the existing incomplete-paste, Unicode and size controls.
