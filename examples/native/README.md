# Native catalog template

See the [supplied native targets guide](../../docs/guides/native-targets.md).

`recipe.json` validates and previews offline. Its digests, lengths and output
literals must be replaced with your actual helper catalog and artifact facts.
`launch.example.json` documents the separate private launch configuration; its
placeholder executable paths and endpoint do not authorize a usable target.

These examples do not provision a machine, install an SSH subsystem or implement
a reset/disposal provider. Real supplied-target acceptance is still pending.

## Complete generated service and desktop examples

The JSON templates above are explanatory. Complete integration examples come
from the compiled fixture's retained case directory, using its real Base/Target,
controller, helper and provider artifacts. Native CI generates these cases on
Linux, macOS and Windows; desktop source cases also include the broker config.
The native manager evidence archive retains the case directories.

Use the `native-artifacts.json` from the same validation batch:

```sh
python3 tests/support/export_native_examples.py \
  --fixture-root /absolute/retained-case \
  --artifacts /absolute/validation-evidence/native-artifacts.json \
  --output /absolute/new-example-directory
python3 tests/support/export_native_examples.py --verify /absolute/new-example-directory
theseus validate --recipe /absolute/new-example-directory/recipe.json
theseus run --headless --preview --recipe /absolute/new-example-directory/recipe.json
```

The fixture retains exact original inputs under `example-inputs/<graph-sha256>`
before provider Prepare can update instance configuration. The exporter selects
that unchanged graph's snapshot, verifies its original pins and preserves literal
paths. Invalid snapshots fail closed; old changed contexts without one remain
unexportable.

The exporter starts no helper, provider or application. It checks the original
graph/config pins, actual compiled artifact bytes, source metadata and recipe
Base/Target commitments before writing a new private directory. It preserves the
complete recipe, config, launch, graph and provider template byte for byte, plus
`broker.json` when declared. `examples.json` records source and file hashes.
Before provider preparation, fixture constructors save exact input bytes under
`example-inputs/<sha256-of-graph.json>/` in the original private case directory.
The exporter selects that snapshot using the unchanged current graph, verifies
its file hashes and original graph pins, and records its provenance. Service
and final Combined graphs retain separate snapshots. Config and broker files
may then reflect provider preparation without changing the exported original
inputs. Actual compiled artifacts are still verified at their original paths.
Older cases without a snapshot require unchanged direct inputs; mutated inputs
are refused. No paths or commitments are rewritten for export.

Integrity verification works without the original target or executable files.
Offline recipe validation and preview do not establish native execution.

Absolute paths and authority stay committed to the original case. Execution uses
a fresh supplied-target reservation with matching paths, manager/session and
provider authority; changing those values requires regenerating the manifests.
Source-owned headless examples use the separately compiled fixture controller;
production configuration loaders continue to reject that session profile.
