# Theseus roadmap

Theseus 0.3 ships a shared Docker lifecycle engine, Ratatui workbench, headless
CLI, versioned recipes/reports, private bounded logs, user/system profiles,
clean/upgrade/reinstall and interrupted-install/upgrade scenarios, explicit
assertions, cancellation and verified recovery.
The standalone fixture and a real Phrourion binary-copy rehearsal establish those
boundaries. Reinstall uses explicit schema-2 declarations and stable path/subtree
contracts across two installer invocations, with report-2 evidence and version-1
recovery journals. Both profiles pass real healthy/broken Docker acceptance and
live/offline PTY comparison browsing. This observes declared stability rather
than proving universal idempotency.

Schema 3 adds explicit interruption and forward recovery: a nonce-bound
cooperative barrier, verified whole-container SIGKILL, same-ID restart without
setup/input recopy, partial-state assertions, explicit recovery and runtime/data
contracts. Both profiles pass real Docker acceptance and live/offline PTY proof
and comparison browsing. Reports write schema 3 and read schemas 1/2/3; recovery
journals remain version 1. This establishes the declared container observations,
not host power-loss durability or arbitrary fault timing.

The following capabilities are planned, not available features.

## Additional lifecycle scenarios

- **Required next: downgrade and compatibility contracts.** Start from the newer
  artifact, configure/exercise actual data, explicitly install the older artifact,
  and check old-version runtime/data compatibility. Support declared compatible
  success and safe rejection that leaves the newer version and data usable.
- **Required after downgrade: rollback to a declared known-good state.** Verify the
  failure point, invoke an explicit rollback command, and verify restored runtime
  and data. Distinguish binary reversal from data restoration; restoration claims
  require scoped backup/restore evidence, including failed and later interrupted
  upgrades. Forward recovery does not implement either of these milestones.
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
