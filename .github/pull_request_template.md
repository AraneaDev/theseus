## What changed?

<!-- Describe the user-visible change. -->

## Verification

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --locked --workspace --all-targets -- -D warnings`
- [ ] `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`
- [ ] Focused tests for the changed area (for example `cargo test --locked -p theseus --lib`;
      the root library tests need the native fixture, so first run
      `export THESEUS_HOST_TEST_FIXTURE=$(python3 tests/support/build_host_fixture.py)`)
- [ ] `python3 scripts/validate.py native`, when native behavior changes. To run only the affected
      stage, build the fixture first into the same evidence directory (outside the source tree):
      `python3 scripts/validate.py native --stage fixture --evidence <dir>`, then
      `python3 scripts/validate.py native --stage <stage> --evidence <dir>`
- [ ] `python3 scripts/validate.py docker`, when Docker lifecycle behavior changes

## Release note

<!-- Explain whether this changes runtime behavior. -->
