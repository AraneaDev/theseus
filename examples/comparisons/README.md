# Declared method and image comparisons

Both suites install the same included `comparison-cli 1.0.0` shell application,
configure and exercise it, remove its binary and retain settings, for both user
and system profiles. `copy.toml` copies the executable; `archive.toml` extracts
its deterministic USTAR archive. Both inputs contain exactly the same executable
bytes. No downloads, credentials or host configuration changes are used.

From the repository root, with Theseus 0.8 installed and Docker available:

```sh
docker pull ubuntu:24.04
theseus validate --recipe examples/comparisons/copy.toml
theseus validate --recipe examples/comparisons/archive.toml
theseus validate --recipe examples/comparisons/image-marker.toml
theseus compare --suite examples/comparisons/methods.toml          # preview; r starts
theseus compare --suite examples/comparisons/methods.toml --headless

docker build --pull=false --network=none -f examples/comparisons/Dockerfile.marker -t theseus-comparison:marker examples/comparisons
theseus compare --suite examples/comparisons/images.toml --headless
```

The second suite compares two actual image IDs: Ubuntu 24.04 and a derived image
with `/theseus-comparison-image-marker` added. It demonstrates image identity
comparison on the same distro base; it does not demonstrate different distro
releases. The marker is outside the application scopes. Theseus records the exact
resolved image IDs, recipe digests and artifact digests; inspect these rather than
inferring identity from mutable tag names. The recipe image must already exist
unless `compare --pull` is explicitly requested.

Suites save private evidence beneath this example's `.theseus-comparisons/ID`.
Use the directory printed by the command:

```sh
theseus comparison 'examples/comparisons/.theseus-comparisons/ID'         # offline
theseus comparison 'examples/comparisons/.theseus-comparisons/ID' --json  # no Docker
theseus cleanup 'examples/comparisons/.theseus-comparisons/ID'           # recorded owned resources only
theseus report 'examples/comparisons/.theseus-comparisons/ID/sources/copy/RUN_ID' # source commands/logs
```

The suite view gives exact source report paths. In interactive preview, r starts,
q exits, and the manifest fixes profiles/scenarios; p/s do not edit it. Live and
offline Up/Down or j/k select cells initially. Tab switches to evidence-record
selection and back; Home/End selects the first/last item in either list. `/` edits
a filter applied by Enter/Esc; `[`/`]` loads detail chunks, and PgUp/PgDn scrolls
inside the current chunk. Active q cancels
the current source, prevents later variants, and bypasses the exit animation;
c also requests cancellation. Existing animation opt-outs and Space skips apply.
Offline r cannot rerun. Source logs remain private local output; inspect them as
potentially sensitive data.

Each suite compares 12 cells: two profile-specific scopes at three exact stages
for both profiles. Equivalent compares complete scoped metadata/content hashes
and child membership after rebasing relative paths. Both verified absent is
Equivalent (for example binaries after uninstall). Different describes an observed
scoped difference and makes the suite Failed even if both source lifecycles pass.
These fixtures explicitly retain the settings file; the surrounding unclaimed
settings directory still appears as a source review finding. Equality does not
suppress lifecycle review findings, so inspect source reports as well as cells.
Unknown means evidence cannot establish a comparison, including an absent stage;
it makes the suite Incomplete. A failed/incomplete/cancelled lifecycle cannot be
turned into success by equivalent scopes. Offline JSON exits 0 for valid evidence,
including Failed suites; inspect `outcome` and cells.

Manifest schema 1 denies unknown fields; recipe paths are relative to the manifest.
Names are 1–128 characters; IDs are 1–64 ASCII letters/digits/underscore/hyphen.
Select 2–16 variants, 1–32 scopes, 1–32 unique exact stage names (each at most 128
bytes), nonempty unique profiles/scenarios supported by every recipe, at most
8192 cells and a 1 MiB manifest. Scopes are normalized absolute non-root paths,
explicit per profile, each covered by one observed root without overlapping roots.
Unknown stage names remain Unknown rather than being skipped. Evidence bounds are
10000 differences per cell and 100000 per suite, plus a shared conservative
32 MiB derived-report budget that accounts for JSON escaping before copying
metadata, diagnostics and source summaries. Byte exhaustion retains a bounded
prefix and makes the current and later cells Unknown, with an Incomplete suite
(Cancelled still takes precedence). Oversized optional source identities, artifacts
or errors are omitted together with an explicit diagnostic while source lifecycle
outcomes stay accurate; validated run IDs, declared recipe digests and contained
recovery paths survive. Full source reports retain the original evidence. Large
valid suites may become Incomplete before reaching count caps. JSON file writes
stream up to 64 MiB; failed serialization leaves the last valid file and cleanup
usable. CLI comparison JSON stages a complete document within 32 MiB. The UI
pages through every retained difference and complete stored text; summaries
may show an explicit ellipsis. It projects at most 64 record summaries and one
16 KiB detail chunk, with at most 65,536 cached filter positions and bounded
rescanning beyond that cache. Paging cannot recover budget-omitted or uncaptured
evidence. JSON also retains the complete bounded stored evidence. Saved comparison
schema is 1;
source lifecycle schema stays 6 and journals/ownership stay 1.

These observations cover these scopes, stages, inputs and image IDs. They do not
prove arbitrary installer equivalence, remote effects, package compatibility,
image ordering, or universal application behavior. The derived marker deliberately
does not change the observed app. For a Different demonstration, append a shell
comment to one candidate's installed executable; runtime can still pass while the
binary hash differs. An unobserved marker alone should remain Equivalent.

Acceptance creates a unique temporary copy of this example, a uniquely tagged
marker image, and only journaled owned containers. It removes that image tag and
checks actual container absence; it never prunes. For manual runs, remove the
marker image only when finished with your own tag:

```sh
docker image rm theseus-comparison:marker
cargo test --test comparison_docker -- --ignored --test-threads=1 --nocapture
```

The ignored tests require local `ubuntu:24.04`; missing Docker/image is a failed
prerequisite. Unique `/tmp/theseus-comparison-task4-*` directories retain raw CLI,
Docker inspection, source reports, PTY transcripts, reconstructed screens, exit
statuses and before/after termios. Tests cover equivalent copy/archive and actual
distinct IDs, deliberate differences, failed sources, per-profile cancellation,
preview/start/live/offline selection, animation controls and terminal restoration.
