# Synthetic stateful plugin replay

`replay-client` is a POSIX shell test double, not a native Claude executable. The
production preparation helper rejects it because it is not ELF. The test harness
first prepares a never-executed dummy ELF and then explicitly substitutes this
file, recording its digest/size and synthetic provenance in the input manifest.
Reports, command stderr, and retained test logs identify synthetic behavior.

The double accepts only fixture CLI forms. Mutations persist settings, installed
registrations, cache copies, and a stored inventory array under the disposable
`CLAUDE_CONFIG_DIR/synthetic-replay`. `list --json` reads this stored inventory.
All three plugin scopes are modeled independently from Linux execution identity.

`THESEUS_REPLAY_FAULT` selects: `duplicate-target`, `wrong-scope`, `stale-version`,
`remove-companion`, `delete-setting`, `missing-component`, `wrong-hook`,
`fail-install`, `fail-update`, `fail-disable`, `fail-enable`, `fail-uninstall`,
`malformed-list`, `duplicate-key-list`, `invalid-utf8-list`, `truncate-list`, or
`hang-update`. Non-mutation output faults start after update, so base checks run.
`THESEUS_REPLAY_VERSION` explicitly replaces version stdout for adapter tests.
Its test-only token `__synthetic_version_with_nul__` emits the declared version
followed by a raw NUL, proving validation precedes shell substitution.
No credentials, installed configuration, network, model call, or native-client
acceptance is involved. Guest commands require only POSIX shell and coreutils.
