# Theseus roadmap

Theseus rehearses a tool's lifecycle and makes the resulting changes inspectable.
The roadmap describes planned work, not features already available.

## V1: standalone Linux CLI tools

- Rust application with a Ratatui TUI and a shared headless execution engine.
- Explicit, versioned TOML project recipes.
- Disposable Docker environments for each scenario and installation profile.
- Unprivileged-user and root installation profiles. System installs must also
  satisfy declared runtime checks as an ordinary user.
- Clean install and base-to-target upgrade scenarios, including representative
  configuration, runtime verification, uninstall, and final-state checks.
- Executable identity and PATH checks, command assertions, configuration/data
  preservation, file metadata and content fingerprints, and declared removal
  obligations.
- Stage-by-stage TUI views for findings, files, commands, and captured output.
- Saved versioned JSON reports and logs; reports remain inspectable offline.
- Explicit incomplete outcomes, bounded execution, cancellation, and cleanup of
  resources owned by the run.
- Healthy and deliberately broken lifecycle fixtures, followed by a real Aranea
  tool rehearsal.

V1 is complete when both installation profiles and both scenarios work through
the TUI and headless CLI, the broken fixtures are detected, saved evidence can be
reviewed, and environment cleanup is exercised on failure and cancellation.

## Additional lifecycle scenarios

- Reinstallation and idempotency.
- Interrupted installation and upgrade.
- Downgrade and rollback, with explicit data and configuration compatibility
  contracts.
- Comparisons between installation methods and supported image versions.

## Claude Code plugins

- Install, update, disable, and remove plugins in a disposable host configuration.
- Verify plugin inventory, hooks, skills, commands, and configuration changes.
- Detect stale or duplicate registrations after updates and removal.
- Verify preservation of unrelated host settings and declared user data.
- Record the tested Claude Code version. Separate local registration checks from
  authenticated or model-backed checks that require explicit credentials.

## MCP server lifecycle and client registration

- Rehearse standalone and plugin-provided MCP server installations.
- Add adapters for explicitly supported client configurations.
- Check registration, server startup, handshake, and tool discovery.
- Inspect configuration preservation and stale or duplicate registrations during
  upgrades and removal.

## Theseus available through MCP

- Expose recipe inspection, run creation, status, cancellation, and evidence
  retrieval through the existing engine.
- Keep execution and result semantics identical across TUI, CLI, and MCP.

## Services and desktop applications

- Add a VM or machine environment adapter where container fidelity is insufficient.
- Observe systemd units, launcher entries, and application process lifecycle.
- Check service/session behavior inside the environment actually exercised.

## Additional operating systems

- Native macOS and Windows environment adapters.
- OS-specific installation, filesystem, permission, and registration contracts.

## Boundaries

Theseus will not infer a complete installation contract from a successful exit
code, label every retained file a defect, or claim that a container run proves
desktop or operating-system behavior. Future interfaces and environment adapters
must extend the same scenario engine rather than introduce separate runners.
