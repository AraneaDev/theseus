# Theseus patch to crossterm 0.28.1

This directory was extracted from the exact crates.io archive locked by Theseus
commit `1b3240eeccc62b44befa27f7ec81efe5eba6f0e6`. Its SHA-256 is
`829d955a0bb380ef178a640b91779e3987da38c9aea133b20614cfed8cdea9c6`.
`THESEUS-PROVENANCE.json` records every upstream file hash and the modified file
hash. The original license and bytes, including CRLF line endings, are retained.
Only `src/event/source/unix/mio.rs` changes upstream code. In particular, all
Windows code remains byte-for-byte identical to that archive.

The Unix Mio source previously returned the first parsed event or resize from a
readiness batch without retaining the remaining tokens. Returning from a TTY
read also discarded readiness for bytes beyond that read. Edge-triggered input
could then be stranded. The patch retains a deduplicated queue containing at
most the three registered tokens. It rotates retained TTY work behind other
sources and polls without waiting while work remains, so newly-ready signals
and wakeups can join each bounded round. Existing parser backlog is consumed
first, as upstream does; parser, signal, wake-event and timeout types are unchanged.

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
required. The separate Windows legacy control executes the unchanged Windows
crossterm sequence and must demonstrate mode drift on adverse baselines.

The crate remains a third-party dependency, excluded from first-party workspace
membership exactly as its registry predecessor was. No upstream dev-dependency
graph is added to the workspace gates. Existing upstream lint warnings are left
unchanged; all first-party workspace targets remain subject to strict clippy.
