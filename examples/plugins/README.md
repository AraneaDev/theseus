# Disposable local plugin lifecycle

Stage an explicitly supplied native Linux ELF client without running it on the host:

```sh
python3 examples/plugins/prepare.py --client /explicit/path/to/claude \
  --client-version '2.1.312 (Claude Code)' --scope project \
  --output /tmp/new-plugin-workspace --image ubuntu:24.04
cargo run --offline -- --no-animation validate --recipe /tmp/new-plugin-workspace/theseus.toml
cargo run --offline -- --no-animation run --headless \
  --recipe /tmp/new-plugin-workspace/theseus.toml --profile user --scenario plugin-lifecycle
```

The output directory must be new and outside the checkout’s `examples/` and
`tests/` source trees; use a private temporary or ignored output directory. `--scope` defaults to
`user`; `project` and `local` also use Linux profile `user` (UID:GID 10001:10001).
The helper copies only the supplied ELF, hashes its streamed bytes, makes the
staged copy executable, and records the supplied version as **expected until guest
verification** in `input-manifest.json`. It never discovers or executes an installed
client, reads client configuration, copies credentials, or downloads anything.
The default image must already be locally available. Generated inputs stay in the
new private output directory; never commit client binaries or generated workspaces.

The public engine uses network `none` and disposable guest home/config/project
paths. Static templates check exact target identity cardinality, scope, versions,
enabled state, source/cache paths, an independently installed user-scope companion,
and two unrelated nested settings at every checkpoint. Uninstall proves absence
from inventory, installed registrations, and settings while preserving the
companion. Cache leftovers do not invalidate removal of registrations.

The inert v1/v2 marketplaces contain a skill, legacy hello command, and target
hook. Component checks compare exact source and cache declarations. `plugin
details` is retained as text; modern clients count one skill plus one legacy command
as **Skills (2)**. The hook is executed manually from the cache and labeled as such.
These observations do not establish model invocation or client event dispatch.

`tests/fixtures/plugins/replay-client` is an explicit stateful shell test double.
Only the test harness replaces the prepared dummy ELF with it, updates provenance
and hashes, and labels reports **SYNTHETIC**. Replay Docker tests prove fixture and
engine behavior, not native Claude compatibility:

```sh
python3 tests/support/plugin_prepare_test.py
cargo test --offline --test plugin_examples
cargo test --offline --test plugin_lifecycle -- --ignored --test-threads=1
```

Actual native acceptance requires both explicit inputs and never falls back to
the replay client. Missing inputs fail the ignored test with a prerequisite
explanation; run these only when the native client and cached image are available:

```sh
THESEUS_PLUGIN_CLIENT=/explicit/path/to/claude \
THESEUS_PLUGIN_CLIENT_VERSION='2.1.312 (Claude Code)' \
  cargo test --offline --test plugin_cli --test plugin_tui -- --ignored --test-threads=1 --nocapture
```

The harness stages three separate scoped workspaces, validates and runs the public
CLI, reloads saved schema-7 JSON, checks the exact file SHA/size and guest version,
UID10001/no forwarding/network none and exact owned Docker absence. Guest-only
test alterations append one target record to actual native inventory while retaining
all original entries, remove a setting or component after successful native update,
or wait before update for timeout/cancel. Duplicate inventory is deliberate test
corruption, not a Claude defect. Fault recipes reuse the same immutable prepared
input within their owned workspace. Actual 80-column live/offline PTYs inspect
scope/version/file digest, JSON counts, component/manual-hook labels and bounded
logs, including active cancellation, offline immutability and terminal restoration.
Each attempt retains reports, journals, command arguments, logs, raw PTY bytes and
screen text in ignored `docs/superpowers/reviews/2026-10-30-plugin-lifecycle/` output.

Observed native acceptance: `2.1.312 (Claude Code)`, ELF `293592432` bytes,
SHA-256 `4f4178ce41d83e23341ebfbaecdbcf8ea6bf7baea0690025abe49eeec3a5d01d`,
Ubuntu image ID
`sha256:534baea6a22c03a63003dbc8dbe78fe34bc0d7e595d9a9dc9834884ff530eb55`.
The native binary executes only in disposable Docker guests. No authentication,
model/session calls, public marketplace, automatic client hook dispatch or host
client configuration is exercised. Typed saved observations prove ordinary report
consistency; they are not authenticated attestations or independent JSON query
replay. The new reader supports report schemas 1 through 7; older released readers
need not support 7. Journal/ownership/comparison schemas remain 1.

Preparation preserves supplied ELF bytes and emits generated text as UTF-8/LF.
Windows preparation requires Python 3.12+ and checks regular, nonreparse source
identity before/after opening; Unix retains no-follow acquisition. The guest copies
the client to `/home/theseus/.local/bin/theseus-client`, compares it with the staged
input, and applies explicit execute permission in its user-owned destination.
