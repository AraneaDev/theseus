## What changed?

<!-- Describe the user-visible change. -->

## Verification

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --locked --workspace --all-targets -- -D warnings`
- [ ] `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`
- [ ] Focused tests for the changed area (for example `cargo test --locked -p theseus --lib`)
- [ ] `python3 scripts/validate.py native` (or the affected `--stage`), when native behavior changes
- [ ] `python3 scripts/validate.py docker`, when Docker lifecycle behavior changes

## Release note

<!-- Explain whether this changes runtime behavior. -->
