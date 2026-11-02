# Local Claude Code plugin lifecycles

Plugin lifecycle support was introduced in Theseus 0.10. Prepare a supplied
Linux native client and a reachable Docker daemon as described in
[installation](../get-started/installation.md). Commands below run from the
repository root; the preparer writes a separate disposable project.

Schema 7 adds `plugin_lifecycle`: install v1, update to v2, disable, enable and
uninstall, with exact inventory and preservation checks at all five checkpoints.
Linux profile `user` (UID:GID 10001:10001) runs each client scope `user`,
`project` or `local`; client scope is separate from the operating-system
profile.

Prepare a new workspace from an explicitly supplied native Linux ELF client:

```sh
python3 examples/plugins/prepare.py --client /explicit/path/to/claude \
  --client-version '2.1.312 (Claude Code)' --scope project \
  --output /tmp/new-plugin-project --image ubuntu:24.04
cargo run --offline -- --no-animation validate --recipe /tmp/new-plugin-project/theseus.toml
cargo run --offline -- --no-animation run --headless \
  --recipe /tmp/new-plugin-project/theseus.toml --profile user --scenario plugin-lifecycle
```

The image must already be available locally. The helper streams only the
supplied ELF into a new private directory outside the checkout's `examples/` and
`tests/` trees, retaining its SHA-256 and size. It never discovers or executes a
host client. The supplied version remains expected until the guest `--version`
check passes. See [the plugin example](../../examples/plugins/README.md) for
staging and acceptance.

Each run has network `none`, no forwarded environment and disposable guest
HOME/config/project directories. The inert local marketplace installs a target
and an independent companion, preserving two unrelated settings through removal.
Checks bind target identity cardinality, scope, version, enabled state and
source/cache paths. Removal checks inventory, installed records and settings;
old cache directories may remain. Component proofs inspect source and cache
manifests, skill/legacy-command files and hook declarations. The cache hook is
invoked **manually**; native details list the skill and legacy command together
as `Skills (2)`. These observations establish neither model invocation nor
automatic client hook dispatch, authenticated behavior, host fidelity or
arbitrary marketplace compatibility. No credentials, model/session calls or
public marketplace are used.

Actual public CLI acceptance passed all three scopes with guest client `2.1.312
(Claude Code)`, native file size `293592432`, SHA-256
`4f4178ce41d83e23341ebfbaecdbcf8ea6bf7baea0690025abe49eeec3a5d01d`, and Ubuntu
image ID
`sha256:534baea6a22c03a63003dbc8dbe78fe34bc0d7e595d9a9dc9834884ff530eb55`.
Deliberately corrupted native list output, lost settings and a removed component
produced Failed reports after successful native updates; timeout/cancel stopped
later client commands and retained owned cleanup. List corruption is a test
fault, not a Claude defect. Real 80-column live/offline PTYs cover explicit
start, scope/version/file digest, JSON identity counts, component/manual-hook
labels, bounded logs, active cancellation and terminal restoration. Binary
inputs and reports remain ignored local evidence.

Schema-7 `json_command` checks parse one bounded complete JSON document and
reject duplicate keys, invalid UTF-8 and excessive nesting. Finite RFC-6901
pointer assertions support scalar equality, absence with an existing parent, and
exact identity counts with selected scalar values. Incomplete capture cannot
pass. Saved typed proofs are consistency-checked against retained command
observations; they are ordinary report data, not authenticated attestations or
independent query replay over the original JSON. Raw output remains in bounded
logs.

## Related documentation

- [Plugin example](../../examples/plugins/README.md): staging and acceptance commands.
- [MCP lifecycle](mcp-lifecycle.md): plugin-provided MCP registrations and protocol probes.
- [Recipes](../reference/recipes.md): guest environment and schema contracts.

[Documentation index](../README.md) · [Project README](../../README.md)

