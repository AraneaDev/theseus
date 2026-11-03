# Theseus documentation

Each page answers one task or contract question. Commands say which working
directory they assume, and each contract keeps its limits beside its claims. The
[project README](../README.md) is the short introduction.

## Get started

- [Installation and controller scope](get-started/installation.md): what do I
  need, where does the controller run, and which native gates remain required?
- [Your first run](get-started/first-run.md): how do I validate a recipe,
  explicitly run it, inspect the result and start my own recipe?

## Guides

- [The workbench](guides/workbench.md): how do I preview, edit a copy, run,
  filter evidence and read a complete log?
- [Comparison suites](guides/comparisons.md): how do I compare installation
  methods or image IDs, and what do the results establish?
- [Local Claude Code plugin lifecycles](guides/plugins.md): how do I prepare
  an explicit native client and check installation, update and removal?
- [MCP lifecycle and client registration](guides/mcp-lifecycle.md): how do I
  rehearse another server's registration, health and typed stdio discovery?
- [Supplied native targets](guides/native-targets.md): how do I preview native
  catalog recipes, supply launch/provider authority and preserve recovery evidence?

## Agents

- [Use Theseus as an MCP server](agents/mcp-server.md): how do I launch the
  six-tool local server, inspect recipes, start a run and page its evidence?

The server interface runs Theseus itself. The lifecycle guide above instead
tests client/server registrations inside disposable Linux guests.

## Reference

- [Recipe reference](reference/recipes.md): which schemas, scenarios, inputs,
  environment declarations, checkpoints and checks can a recipe use?
- [Evidence, outcomes and limits](reference/evidence.md): what do exit codes,
  report schemas, review findings, capture limits and saved logs mean?

## Operate

- [Recovery and owned cleanup](operate/recovery.md): when may interruption or
  rollback continue, and how do I recover only recorded owned resources?

## Contribute

- [Validation and acceptance scope](contribute/validation.md): how do I run
  native and Docker checks locally, and what counts as actual acceptance?

## Examples

Start with [your first run](get-started/first-run.md). Paths below point to the
checked-in recipes and existing walkthroughs; their commands assume the
repository root unless they explicitly change directory.

| Task | Recipe or walkthrough |
| --- | --- |
| Clean installation and upgrade | [Standalone recipe](../examples/standalone/theseus.toml) |
| Reinstall the same artifact | [Reinstall recipe](../examples/reinstall/theseus.toml) |
| Interrupt and explicitly recover | [Interruption recipe](../examples/interruption/theseus.toml) |
| Downgrade compatibility or rejection | [Compatible recipe](../examples/downgrade/compatible.toml), [rejection recipe](../examples/downgrade/rejection.toml) |
| Roll back a declared failed upgrade | [Rollback walkthrough](../examples/rollback/README.md) |
| Roll back an interrupted upgrade | [Interrupted rollback walkthrough](../examples/interrupted-rollback/README.md) |
| Compare methods and image IDs | [Comparison walkthrough](../examples/comparisons/README.md) |
| Rehearse Claude Code plugins | [Plugin walkthrough](../examples/plugins/README.md) |
| Rehearse MCP registration and discovery | [MCP fixture walkthrough](../examples/mcp/README.md) |
| Preview a supplied native target recipe | [Native catalog template](../examples/native/README.md) |

Completed milestones, current progress and remaining work are tracked in the [roadmap](../ROADMAP.md).
