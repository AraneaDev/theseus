# Comparison suites

Install [Theseus and Docker](../get-started/installation.md), then start from
the repository root for the commands below. The existing [comparison
walkthrough](../../examples/comparisons/README.md) supplies the complete
fixtures.

Compare declared installation methods or actual image IDs through the same
engine:

```sh
theseus compare --suite examples/comparisons/methods.toml             # preview; r starts
theseus compare --suite examples/comparisons/methods.toml --headless
theseus comparison 'examples/comparisons/.theseus-comparisons/ID' --json
theseus cleanup 'examples/comparisons/.theseus-comparisons/ID'
```

The [self-contained comparison
walkthrough](../../examples/comparisons/README.md) includes copy/archive
methods, two distinct actual image IDs on the same Ubuntu base, validation/build
commands, limits, controls, source logs and safe cleanup. Variants run
sequentially with fresh owned containers for the same selected matrix. Scopes
compare relative keys and all captured file metadata/content hashes and child
membership. Equivalent includes verified absence at both ends; Different makes
the suite Failed; Unknown evidence makes it Incomplete. Source lifecycle
failures retain their precedence even when scoped observations match. Inspect
exact image IDs, recipe/artifact digests and source reports; mutable tag names
imply no ordering or compatibility. These are declared observations, not
universal installer equivalence.

Comparison manifests and saved artifacts use schema 1; source reports write
schema 8. Offline viewing/JSON recomputes results and checks contained source
digests without Docker. JSON returns 0 for valid evidence regardless of suite
outcome. Derivation uses a shared conservative 32 MiB budget, including JSON
escaping, source summaries and diagnostics, before copying variable evidence.
Limits also cap differences at 10,000 per cell and 100,000 per suite. Exhaustion
retains a bounded prefix, makes the current and later cells Unknown and the
suite Incomplete (unless Cancelled takes precedence). Oversized optional source
identities/artifacts or errors are omitted atomically with a diagnostic; source
lifecycle outcomes stay accurate. Validated run IDs, declared recipe digests and
contained recovery paths remain available. Full source reports remain available
for omitted evidence. Legitimate large comparisons can reach this conservative
byte limit before the count caps. Private JSON writes stream with a 64 MiB limit
and preserve the prior file on serialization failure; comparison CLI JSON is
staged within 32 MiB before emission. The comparison UI starts with cell
selection; Tab switches between cells and evidence records. Home/End reaches
either list’s first/last item. The same filter, chunk and local-scroll controls
apply. Every retained difference and full source field is reachable;
source-report, recovery and full-JSON commands remain safely quoted in selected
detail records.

## Related documentation

- [Workbench](workbench.md): cell focus, filters and full-field navigation.
- [Recipe reference](../reference/recipes.md): shared scenario execution.
- [Evidence and limits](../reference/evidence.md): source outcomes and retained bytes.
- [Recovery](../operate/recovery.md): cleanup by verified journals.

[Documentation index](../README.md) · [Project README](../../README.md)

