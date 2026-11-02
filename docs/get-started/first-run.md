# Your first run

Install [Theseus and Docker](installation.md) first. Commands below start from
the repository root; the first example explicitly changes into its recipe
project. Runs execute only after a headless command or a fresh `r` in preview.

The included fixture installs real v1/v2 shell executables, configures them,
checks their identity and retained data, and removes the declared executable
paths:

```sh
docker pull ubuntu:24.04
cd examples/standalone
theseus validate
theseus                       # preview first; press r explicitly to run
theseus run --headless         # all four user/system × clean/upgrade entries
theseus run --headless --profile user --scenario clean
```

You can also use `cargo run -- run --headless --recipe
examples/standalone/theseus.toml` from the repository root. Keep that command on
one line. An absent image fails preflight unless `run --pull` is explicitly
supplied. Theseus resolves and records the immutable image ID used by the run.



## Inspect the result

The run prints its report location. From the recipe project, use the saved ID to
inspect evidence and recover only journaled resources:

```sh
theseus report .theseus-runs/RUN_ID
theseus report .theseus-runs/RUN_ID --json
theseus cleanup .theseus-runs/RUN_ID
```

Inspect the outcome and review findings separately. A report read successfully
with `--json` returns 0 even when the saved run failed. See [outcomes and exit
codes](../reference/evidence.md#outcomes-and-exit-codes) and
[recovery](../operate/recovery.md).

## Create a recipe

Create your own starter with `theseus init`. It refuses to overwrite an existing
recipe. Replace the failing placeholder commands **and** the `.` artifact paths
before running it. `theseus validate --recipe PATH` validates without Docker.

## Try another lifecycle

The [recipe reference](../reference/recipes.md#lifecycle-examples) walks through
reinstall, interruption, downgrade and rollback recipes. The [examples
index](../README.md#examples) links the existing walkthroughs and manifests.

## Related documentation

- [Workbench](../guides/workbench.md): preview, edit a copy, run and browse.
- [Recipe reference](../reference/recipes.md): choose scenarios and define checks.
- [Comparison suites](../guides/comparisons.md): compare methods or image IDs.

[Documentation index](../README.md) · [Project README](../../README.md)

