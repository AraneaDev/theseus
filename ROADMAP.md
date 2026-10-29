# Theseus roadmap

Theseus V1 ships a shared Docker lifecycle engine, Ratatui workbench, headless
CLI, versioned recipes/reports, private bounded logs, user/system profiles,
clean/upgrade scenarios, explicit assertions, cancellation and verified recovery.
The standalone fixture and a real Phrourion binary-copy rehearsal establish those
boundaries. The following capabilities are planned, not available features.

## Additional lifecycle scenarios

- Reinstallation and idempotency.
- Interrupted installation/upgrade, downgrade and rollback with explicit data
  compatibility contracts.
- Comparisons between installation methods and image versions.
- More scalable metadata/log paging for large reports and preview editing.

## Claude Code plugins

- Install, update, disable and remove plugins in a disposable host configuration.
- Verify plugin inventory, hooks, skills, commands and configuration changes.
- Detect duplicate/stale registrations while preserving unrelated settings.
- Record tested client versions and distinguish local checks from authenticated
  model-backed checks that need explicitly supplied credentials.

## MCP lifecycle and client registration

- Rehearse standalone and plugin-provided MCP server installations.
- Support explicit client configuration adapters, startup/handshake/discovery
  checks, registration preservation and duplicate detection.
- Expose recipe inspection, run creation, status, cancellation and evidence through
  an MCP interface using the same engine and outcome semantics.

## Services, desktop applications and other operating systems

- Machine/VM adapters where containers cannot establish service/session fidelity.
- Observe systemd units, launcher entries and application process lifecycles.
- Native macOS and Windows environment adapters with OS-specific contracts.

Theseus will continue to require explicit lifecycle contracts and distinguish
observations from assumptions. New interfaces must share the scenario engine.
