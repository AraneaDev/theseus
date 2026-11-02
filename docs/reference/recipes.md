# Recipe reference

Use [your first run](../get-started/first-run.md) before writing a recipe. This
reference defines what each declaration establishes. Commands and `examples/`
paths below assume the repository root unless a command explicitly changes
directory.

See the [standalone](../../examples/standalone/theseus.toml),
[reinstall](../../examples/reinstall/theseus.toml),
[interruption](../../examples/interruption/theseus.toml), [compatible
downgrade](../../examples/downgrade/compatible.toml),
[rejection](../../examples/downgrade/rejection.toml), [rollback
policies](../../examples/rollback/README.md), and [interrupted rollback
policies](../../examples/interrupted-rollback/README.md) and their installers.
Recipes reject unknown fields and define a name, image, network (`none` by
default or explicit `bridge`), artifacts, profiles, and nonoverlapping absolute
observation roots. Inputs resolve inside the recipe project, are materialized
and hashed, then copied into `/inputs/base` and `/inputs/target`. There are no
writable host source mounts, host home/socket mounts, host namespaces,
privileged mode, or forwarded ports.

## Schema versions and scenario selection

Schema version 1 retains clean/upgrade defaults and the original required steps.
It rejects `scenarios`, `reinstall`, `reinstalled`, and `unchanged` with a
version-2 requirement. Schema version 2 requires a nonempty, unique, ordered
declaration, for example `scenarios = ["reinstall"]`. Default and `--scenario
all` select that list; requesting an undeclared scenario is an input error.
Reinstall requires an explicit `reinstall` step and at least one `unchanged`
assertion for every selected profile. There is no automatic fallback to
`install_target`. A reinstall-only recipe requires neither `artifacts.base`,
`install_base`, nor `upgrade`.

Schema 3 adds `interrupted_install` and `interrupted_upgrade`, steps
`interrupt_install`/`recover_install` and `interrupt_upgrade`/`recover_upgrade`,
and checkpoints `interrupted`/`recovered`. Schemas 1/2 reject these identifiers.
Interruption steps must call the runner-supplied helper synchronously after the
intended partial mutation:

```sh
cp /inputs/target/sample-cli "$prefix/sample-cli.partial"
/bin/sh "$THESEUS_INTERRUPT_BARRIER"
```

The helper never returns normally. Do not background it or declare its private
variable in `env`/`pass_env`. The command timeout covers barrier setup,
readiness, and verified stop. Both scenarios require an interrupted filesystem
assertion, an explicit recovery command, and a recovered executable check with a
nonempty version/behavior expectation. Interrupted upgrade additionally requires
an explicit recovered data contract; use scoped unchanged/preserved assertions
or a command that reads real data and expects nonempty output.

Schema 4 adds `downgrade`, explicit `downgrade`/`exercise_downgraded` steps and
`downgraded`/`downgrade_rejected` checkpoints. Schemas 1/2/3 reject those
features. Each selected profile must declare `[profiles.NAME.downgrade]` with
exactly one policy. `expect = "compatible"` requires `data_checks` referencing
real successful `command` readers with nonempty expected output at `downgraded`,
plus an old-version `executable` check. `expect = "safe_rejection"` requires a
literal `expected_exit` from 1 through 125, nonempty `stdout_contains`, and
separate `binary_checks` and `data_checks` referencing `unchanged` assertions at
`downgrade_rejected`. Binary scopes must be actual files or symlinks; data
scopes must exist and recursive directory checks include added/removed children.
Overlapping binary/data scopes are rejected. A newer-version executable check is
required before either attempt and after rejection. All checkpoint checks must
pass, including additional ones.

Schema 5 adds `rollback`, explicit `attempt_upgrade`, `rollback`, and
`exercise_rolled_back` steps, and the `known_good`, `backup_created`,
`upgrade_failed`, `rolled_back`, and `backup_removed` checkpoints. Earlier
schemas reject rollback features. Each profile declares
`[profiles.NAME.rollback]` with an exact expected failure (status 1..125 and
nonempty literal stdout marker), `change_checks` referencing `changed` at
`upgrade_failed`, `binary_checks` referencing file/symlink `unchanged` at
`rolled_back`, `runtime_checks` referencing successful old-version executable
checks there, and `data_checks` referencing successful actual data readers
there. Runtime checks must match a `base_installed` executable's name, resolved
path, arguments and literal marker; that path must also be a selected binary
check. `binary_only` declares no backup. `restore_data` requires explicit
`backup`/`cleanup_backup` commands, a previously absent `backup_root`, and 1..64
nonoverlapping source-to-relative-storage mappings. Sources and storage must lie
within observations and be separate from binaries. The `__theseus_rollback_`
check ID prefix is reserved for generated checks in rollback-enabled recipes.

Schema 6 adds `interrupted_rollback`, selected with CLI `--scenario
interrupted-rollback`, and `[profiles.NAME.interrupted_rollback]`. It declares
the same `change_checks`, `binary_checks`, `runtime_checks`, `data_checks` and
`restoration` keys as rollback, with no `failure` field. It reuses
`interrupt_upgrade`, `rollback`, `exercise_rolled_back` and optional
`backup`/`cleanup_backup`. Changed checks belong to `interrupted`; no forward
`recover_upgrade` runs. A known-good actual reader is required before the
attempt, and required runtime/readers retain typed expected-exit, literal-match
and command/log evidence. Restored executable name/path/args/marker bind to the
original; known-good and restored reader scripts may differ. Ordinary and
interrupted rollback can coexist with different modes/check IDs; each entry gets
its own owned container and independent authorization.

## Inputs and execution

Both `user` and `system` profiles run configure/exercise/checks as UID/GID
10001:10001. User
installation/upgrade/reinstall/interruption/recovery/downgrade,
backup/rollback/cleanup and uninstall use that same identity; system mutation
commands use 0:0 **inside the disposable container**. Steps are `/bin/sh -c`
scripts with `/workspace` as working directory. Each step is a new shell: files
persist, shell variables and working-directory changes do not. HOME, XDG
directories and PATH are set explicitly. Runtime PATH defaults to
`/home/theseus/.local/bin:/usr/local/bin:/usr/bin:/bin`. Step/check `env`
contains public literal values; `pass_env` explicitly opts in named host
variables. Values from `pass_env` are not serialized as metadata. Command
evidence records forwarded names separately and omits overridden defaults;
effective public literals remain visible. Applications can print secrets into
logs. No credentials are forwarded automatically and no general secret redaction
is promised.

Materialization preserves artifact bytes and sorted POSIX `/` manifest
references. Unix controllers preserve source execute bits when staging files.
Windows staging uses explicit guest modes: files 0444 and directories 0755,
without inferring executability from `.exe`, `.sh` or other extensions. Only the
owned `/inputs/base` and `/inputs/target` roots are normalized after copying;
archive contents stay unchanged. Inputs are root-owned. A recipe that needs
executables copies or extracts into its user-owned guest directory and applies
its declared `chmod`, as the copy and archive comparison examples do.

The image must supply `/bin/sh`, `/usr/bin/env`, `sleep`, `mkdir`, `chown`,
`chmod`, `tar`, `cp`, `rm`, `cat`, `id`, and `awk` (plus `find` for Windows
input normalization), with compatible UID/GID 10001 assignments. Interruption
additionally requires GNU-compatible `mv -T` and `dd iflag=nofollow,nonblock`
for atomic readiness and bounded no-follow reads; the example also uses `cmp`,
`wc`, and `stat`. The downgrade examples also use `head` and `wc`; rollback
examples also use `readlink` and `sed`. Unsupported tools fail closed as
failed/incomplete evidence; Theseus installs no packages. Image-declared volumes
are rejected. Ubuntu 24.04 is exercised by acceptance; other images must satisfy
preflight and supported tar observations. These Linux guest tests do not prove
systemd, GUI, other guest kernels, or native macOS/Windows guest behavior;
native controller tests have the separate [installation and controller
scope](../get-started/installation.md#native-controllers-and-release-qualification).

## Lifecycle ordering

Clean executes install_target → target_installed checks → configure → configured
checks → exercise → uninstall → uninstalled checks. Upgrade executes
install_base → base_installed checks → configure → configured checks → upgrade →
target_installed checks → exercise → uninstall → uninstalled checks. Each matrix
entry gets a fresh container/baseline. Snapshots follow mutations and occur
before and after checkpoint checks, so check-induced changes remain
attributable. An ordinary failed install still permits declared partial-install
uninstall cleanup; dependent forward mutations are skipped explicitly.

Reinstall executes install_target → target_installed checks → configure →
configured checks → exercise_before_reinstall → before_reinstall reference →
reinstall → reinstalled checks → exercise_after_reinstall → uninstall →
uninstalled checks. Both exercise stages use the same recipe command. The same
materialized target artifact is used twice inside one scenario container; there
is no second configure step. The comparison reference is captured after the
first exercise and earlier checks. `unchanged` uses the first reinstalled
snapshot, before checkpoint checks or the second exercise can mutate state.
Those later side effects remain visible in separate evidence. The configured
reference used by `preserved` stays independent of the before_reinstall
reference.

Interrupted install executes before_interrupt reference → interrupt_install →
verified same-ID restart → interrupted checks → recover_install → recovered
checks → configure → configured checks → exercise_after_recovery → uninstall →
uninstalled checks. Interrupted upgrade executes install_base → base_installed
checks → configure → configured checks → exercise_before_interrupt →
before_interrupt reference → interrupt_upgrade → verified same-ID restart →
interrupted checks → recover_upgrade → recovered checks →
exercise_after_recovery → uninstall → uninstalled checks. No observations run
while stopped. Partial and recovered snapshots precede their checkpoint checks;
after-check snapshots retain check side effects. At interrupted/recovered,
`unchanged` compares before_interrupt; `preserved` still compares configured and
therefore applies only to interrupted upgrade at these checkpoints. An ordinary
interrupted install has not configured anything yet. Recovery can intentionally
migrate data using behavioral readers rather than claiming byte equality.

Downgrade executes install_target → target_installed checks → configure →
configured checks → exercise_before_downgrade → before_downgrade reference →
explicit downgrade attempt → the declared policy checkpoint and checks →
exercise_downgraded (compatible) or exercise_after_rejection (the original newer
exercise command) → uninstall → uninstalled checks. The before_downgrade
snapshot includes earlier checks and newer exercise effects; `unchanged`
compares it with the first post-attempt checkpoint snapshot, before checkpoint
commands can mutate state. After-check snapshots keep those later effects
visible. `preserved` still compares the exact configured reference. No second
configure, automatic fallback, installer retry, restart, backup or restore
occurs. A failed attempt or checkpoint skips dependent runtime exercise; a
running container still gets the ordinary uninstall/final/owned cleanup tail.

Rollback executes install_base → base_installed checks → configure → configured
checks → exercise_before_upgrade → known_good checks → before_upgrade reference
→ optional backup and backup_created checks → before_attempt reference →
attempt_upgrade → upgrade_failed checks → before_rollback reference → explicit
rollback → rolled_back checks → exercise_rolled_back → optional backup cleanup
and backup_removed checks → uninstall → final → owned teardown. The known-good
reference includes preceding runtime/check effects; `changed` compares the
immediate before_attempt reference, after backup/check effects. Before rollback,
fresh backup checks catch corruption caused by earlier checkpoint checks.
Restoration and binary equality compare before_upgrade, while `preserved` keeps
its configured reference. There is no second configure or automatic installer
fallback. See [the walkthrough](../../examples/rollback/README.md) for generated
checks.

## Check semantics

Checks have unique IDs and named checkpoints. `command` expects a shell exit and
optional literal stdout substring. An application exit is evaluated only after a
fresh in-container completion receipt is verified through a separate bounded
control operation; Docker control failures leave the application status unknown.
`executable` verifies PATH resolution, absolute path, version argv, exit status,
and optional literal output. `present` and `absent` inspect paths (including
dangling links); `preserved` compares an exact path's content hash, kind, link,
ownership and mode with configured evidence. Preservation is valid at upgrade
target_installed, reinstalled, uninstalled, downgraded/downgrade_rejected,
interrupted/recovered in interrupted upgrade, or interrupted in interrupted
rollback; use an explicit command check for intentional migrations. File checks
must fall within observed roots. Hashes establish observed bytes, not arbitrary
semantic correctness.

`unchanged` is valid at `reinstalled` in schema 2 declaring reinstall, and at
`interrupted`/`recovered` in schema 3 declaring interruption scenarios, or at
the selected `downgraded`/`downgrade_rejected` checkpoint in schema 4 declaring
downgrade, or `rolled_back` in schema 5 declaring rollback or schema 6 declaring
interrupted rollback. `changed` is valid at ordinary rollback's `upgrade_failed`
and interrupted rollback's `interrupted`: observed additions, removals and
content/type/metadata changes pass; equality or absence at both ends fails.
Uncertain coverage or an unsafe ancestor remains incomplete. Unchanged compares
a file’s hash, size, kind, UID/GID, and mode; a directory compares its own
metadata and all observed descendants, including added/removed children. A
symlink compares its observed link target and metadata without following it. For
Unchanged, the declared path must exist in both complete snapshots. Missing
paths or changed entries fail; incomplete, ambiguous, overflowing, or unsafe
observations remain incomplete. Mtime, inode identity, ACLs, extended
attributes, hidden remote effects, and state outside the declared observed
scopes are outside this contract. Reports identify both comparison stages and
retain changed-child evidence on the comparison checkpoint. Use exact-path
`preserved` for retention against configured state; it does not recursively
assert a directory’s children.

## Lifecycle examples

### Reinstall

A target-only schema-2 example rehearses two invocations of the same installer:

```sh
theseus validate --recipe examples/reinstall/theseus.toml
theseus run --headless --recipe examples/reinstall/theseus.toml
theseus run --recipe examples/reinstall/theseus.toml  # preview, then r
```

Both user and system profiles declare stable binary and configuration scopes,
check runtime behavior after reinstall, and retain configuration/data on
uninstall. No base artifact is needed. A passing run establishes these declared
observations for two installer invocations; it does not prove universal
idempotency or stability across arbitrary repetitions, remote effects, or
unobserved state.

### Interruption

Schema 3 provides a self-contained interruption example for both profiles:

```sh
theseus validate --recipe examples/interruption/theseus.toml
theseus run --headless --recipe examples/interruption/theseus.toml
theseus run --recipe examples/interruption/theseus.toml  # preview, then r
theseus run --headless --recipe examples/interruption/theseus.toml --profile user --scenario interrupted-upgrade
```

It copies a real target executable to `.partial`, starts a background writer,
reaches a cooperative barrier, and deliberately stops the whole container with
SIGKILL. Theseus verifies the stop, restarts the same container without setup,
input copying or chown, observes partial state, then runs explicit recovery. The
example verifies the writer stopped, promotes the partial executable, checks the
target version and retained data, and uninstalls.

### Downgrade

Schema 4 provides two self-contained downgrade policies, each for both profiles:

```sh
theseus validate --recipe examples/downgrade/compatible.toml
theseus validate --recipe examples/downgrade/rejection.toml
theseus run --headless --recipe examples/downgrade/compatible.toml
theseus run --headless --recipe examples/downgrade/rejection.toml
theseus run --recipe examples/downgrade/compatible.toml  # preview, then r
theseus run --recipe examples/downgrade/rejection.toml   # preview, then r
```

Both start with `artifacts.target` (the newer 2.0.0 tool), configure a real
format-2 database, then append a second record during the newer runtime
exercise. `artifacts.base` is the older requested 1.0.0 tool; these are declared
roles, not inferred version ordering. The compatible recipe installs that older
tool and requires its real reader to validate both records and calculate count 2
/ sum 40. The rejection recipe explicitly asks its installer to refuse the newer
data before writing anything: actual exit 9 and the exact public marker must
match, then binary, recursive data, configuration and retained newer runtime
checks must pass. Both recipes uninstall the executable and verify retained
settings/data. They use only shell tools in the declared Ubuntu image, without
credentials or external application binaries. A passing recipe establishes its
selected contracts and observed scopes, not compatibility with arbitrary data or
versions.

### Rollback

Schema 5 adds rollback after an ordinary declared failed upgrade. Both policies
are self-contained and run for user and system profiles:

```sh
theseus validate --recipe examples/rollback/binary-only.toml
theseus validate --recipe examples/rollback/restore-data.toml
theseus run --headless --recipe examples/rollback/binary-only.toml
theseus run --headless --recipe examples/rollback/restore-data.toml
theseus run --recipe examples/rollback/restore-data.toml  # preview, then r
```

The candidate changes the installed executable and migrates real data, prints
the declared failure marker, and exits 9. Explicit rollback restores the
known-good binary. `binary_only` retains migrated data and proves the old tool
can read it; `restore_data` additionally verifies a mapped in-container backup
and restoration of the entire declared data subtree. See the [rollback
walkthrough](../../examples/rollback/README.md) for timing, generated checks,
failure evidence and limits.

### Interrupted rollback

Schema 6 adds rollback after a deliberately interrupted upgrade, again with both
policies and user/system profiles:

```sh
theseus validate --recipe examples/interrupted-rollback/binary-only.toml
theseus validate --recipe examples/interrupted-rollback/restore-data.toml
theseus run --headless --recipe examples/interrupted-rollback/binary-only.toml
theseus run --headless --recipe examples/interrupted-rollback/restore-data.toml
theseus run --recipe examples/interrupted-rollback/restore-data.toml  # preview, then r
```

The candidate changes the binary and data, starts a background writer and
reaches a cooperative barrier. Theseus verifies whole-container SIGKILL and
same-ID restart, observes actual Changed evidence, then authorizes explicit
rollback. `binary_only` proves the old tool can read retained partial data;
`restore_data` also proves mapped backup/restoration and cleanup. The
[interrupted rollback
walkthrough](../../examples/interrupted-rollback/README.md) explains original
versus immediate references, typed runtime/reader evidence, authorization and
limits.

## Related documentation

- [Evidence](evidence.md): outcome precedence, report compatibility and budgets.
- [Recovery](../operate/recovery.md): interruption and rollback authorization.
- [Plugin lifecycle](../guides/plugins.md) and [MCP lifecycle](../guides/mcp-lifecycle.md): schema 7/8 contracts.
- [Comparison suites](../guides/comparisons.md): compare declared variants.

[Documentation index](../README.md) · [Project README](../../README.md)

