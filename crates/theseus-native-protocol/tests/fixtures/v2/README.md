# Independent protocol V2 fixtures

These static JSON bodies and binary frames were authored with Python from the frozen field tables, independently of the Rust implementation. Rust tests must read the materialized files; they must never invoke the reference scripts to derive expected bytes.

`manifest.json` records exact body/frame lengths and SHA-256 values. `build-corpus.py` preserves provenance; `reference-calculation.py` and `protocol-v2-size-and-oracles.json` preserve the independently calculated width bounds, request-domain digests and canonical header oracle. `round1-guard-traces.json` preserves the nine external guard traces. `not-started-correlations.json` distinguishes both mutation kinds and both no-attempt reasons.

Wire fixtures exercise schema shapes independently and do not generally form a valid session sequence. Ledger fixtures exercise canonical record shapes independently and do not generally form a valid history. These artifacts establish no runtime effects, native identity, persistence, cleanup, or execution authority.
