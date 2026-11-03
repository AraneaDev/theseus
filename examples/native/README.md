# Native catalog template

See the [supplied native targets guide](../../docs/guides/native-targets.md).

`recipe.json` validates and previews offline. Its digests, lengths and output
literals must be replaced with your actual helper catalog and artifact facts.
`launch.example.json` documents the separate private launch configuration; its
placeholder executable paths and endpoint do not authorize a usable target.

These examples do not provision a machine, install an SSH subsystem or implement
a reset/disposal provider. Real supplied-target acceptance is still pending.
