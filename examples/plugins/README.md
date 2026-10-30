# Disposable local plugin lifecycle

Stage an explicitly supplied native Linux ELF client without running it on the host:

```sh
python3 examples/plugins/prepare.py --client /explicit/path/to/claude \
  --client-version '2.1.312 (Claude Code)' --scope project \
  --output /tmp/new-plugin-workspace --image ubuntu:24.04
cargo run --offline -- run --headless --recipe /tmp/new-plugin-workspace/theseus.toml
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
as **Skills: 2**. The hook is executed manually from the cache and labeled as such.
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
