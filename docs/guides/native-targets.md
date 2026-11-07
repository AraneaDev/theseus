# Supplied native targets

The native catalog adapter is under development in [draft PR #1](https://github.com/AraneaDev/theseus/pull/1).
It runs clean, upgrade and reinstall through the shared engine on an explicitly
supplied disposable target. Real SSH, VM reset and native guest acceptance are
still pending; see the [roadmap](../../ROADMAP.md) for the current gates.

## Preview a recipe

From the repository root:

```sh
theseus validate --recipe examples/native/recipe.json
theseus run --headless --preview --recipe examples/native/recipe.json
```

The [example recipe](../../examples/native/recipe.json) is an offline template.
Its zero digests and one-byte artifact lengths are placeholders. Replace them
with the configured helper catalog digest and actual base/target reader lengths
and SHA-256 digests before execution. The literal checks must match your reader's
actual output, including newlines. Preview does not authenticate, reset a target
or create a helper process.
Before acquisition, execution checks the recipe's base/target artifact facts
against the helper's catalog commitment. An unchanged catalog digest cannot
authorize different expected reader bytes.

Native recipes select fixed catalog names rather than shell commands. The helper
configuration supplies the immutable executable, arguments and replacement
environment for exactly these programs:

| Purpose | Catalog ID |
| --- | --- |
| Install base reader | `install_base` |
| Install or upgrade target reader | `install_target` |
| Configure application data | `configure` |
| Exercise the installed reader | `exercise` |
| Remove the application | `uninstall` |
| Verify base, data and target | `check_base`, `check_data`, `check_target` |

The installed reader is `reader` on Linux/macOS and `reader.exe` on Windows;
`app` and `data` are the other declared direct leaves. Windows recipes use
`windows_exit` statuses. POSIX recipes distinguish exit codes from signals.
Execution uses the current helper account; Docker user/system profiles do not
apply. Native interruption, downgrade, rollback, plugin checks and comparison
execution remain unsupported until their own contracts are implemented.

## Supply launch and provider authority

Copy [launch.example.json](../../examples/native/launch.example.json) to a private
location and replace every placeholder. This file contains explicit runtime
authority and must be supplied separately from the recipe or saved report.
Keep its bytes unchanged when recovering a run: the journal binds its digest.

The production transport requires an explicitly installed OpenSSH 10.x executable
and exact version output, a preconfigured named SSH subsystem, a dedicated
private key and pinned host keys. Theseus checks the effective option profile;
it does not configure the SSH server or discover machines. Runtime material paths
must be absolute and use the launcher's restricted literal alphabet; whitespace
and expansion characters are rejected. Windows path separators are normalized
for OpenSSH option values.

The external provider must implement exclusive ownership before preparing or
resetting a target. Theseus appends two arguments to its configured argv:
`invoke` or `query`, followed by the full invocation as one JSON argument.
There is no shell joining. The invocation binds the ownership ID, provider,
invocation ID, action, target, baseline, launch fingerprint and prior resource.
The provider returns a JSON receipt with `provider`, `invocation_id`,
`invocation_digest`, `binding`, `manifest_digest`, `workspace_marker`,
`reset_epoch` and `receipt`. `invocation_digest` is the lowercase SHA-256 of the
exact invocation JSON argument bytes, binding the entire original context.
The reset epoch is a 64-digit lowercase hex digest; disposal must advance it.
A query may return JSON `null` when the exact original invocation remains unknown.

Receipts are provider claims checked against the requested identity. The provider
must enforce exclusivity durably and reject conflicting or uncertain ownership.
After an unknown response, Theseus queries the original invocation rather than
repeating a reset. Theseus does not provision VMs or use its controller machine
as an implicit installation target.

Target provisioning must also bootstrap the helper's private ledger/workspace
with the native profile and matching catalog. The supplied provisioning tool can
use the store's `bootstrap_native` API; the serving binary has no implicit
bootstrap command. Configure the SSH subsystem to start
`theseus-native-helper serve --config /absolute/helper.json` with fixed arguments.
Helper configuration schema 2 binds `native_catalog_v1`, its capabilities,
provider epoch, the base/target artifact facts and all eight catalog programs.

## Execute and inspect

After provisioning and validating your disposable target and provider:

```sh
theseus run --headless --recipe /absolute/native-recipe.json --native-target /private/launch.json
theseus report /path/to/run --json
theseus cleanup /path/to/run --native-target /private/launch.json
```

Omit `--headless` to use the workbench. Native artifacts are
`native-report.json` and `native-journal.json`; they do not masquerade as Docker
reports. Preserve the whole run directory, including its original acquisition
intent store. Mixing artifact families is rejected.

Cleanup verifies the original intent and launch binding before recovery. It
queries original operations, requests cancellation/release where authorized and
records provider disposal separately from local process settlement. It never
replays installation commands. Historical output that was not retained stays
unavailable. A prior controller's unproved process cleanup remains unknown even
after remote disposal, so cleanup can remain incomplete.

The MCP server accepts the same launch ceiling at startup with
`theseus serve --project-root PROJECT --output-root OUTPUT --native-target /private/launch.json`.
Tool calls cannot replace that target. For native `run_start`, omit Docker
profiles and pull options; scenarios and output limits may only narrow the recipe.

## Evidence and limits

Each Run and file inspection is bounded to 30 seconds. Recipe limits may narrow
these ceilings, with at most 128 operations and 16 KiB captured output per command.
Native file hashing streams at most 64 MiB. An uncertain file lookup does not
prove absence. Replaced or unproved workspace identity revokes further catalog
effects, including checks and uninstall; recovery and provider disposal retain
their separate authority.

Local inert fixtures exercise real child processes and compiled readers, but
they do not qualify a real SSH connection, reboot/reset provider or guest OS.
Native Linux/macOS/Windows CI and the existing Docker suites remain required
before merge. Service managers and interactive desktop sessions have additional
pending acceptance gates.
