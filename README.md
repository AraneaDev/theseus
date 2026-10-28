# Theseus

Know what your tool leaves behind.

Theseus is a planned installation lifecycle workbench for standalone CLI tools.
It will rehearse installation, configuration, upgrade, verification, and removal
in disposable Linux environments, then present the evidence in a terminal UI or
through a headless command for CI.

## Status

Design approved; implementation has not started. There is no runnable binary yet.

The first version targets Rust, Ratatui, and Docker, with both unprivileged-user
and system-wide installation profiles. System installations will also be checked
as an ordinary user.

The scenario engine will be shared by the TUI and CLI. Reports will distinguish
failed expectations from incomplete observations and command failures.

See [ROADMAP.md](ROADMAP.md) for the scope and planned extensions.

Design specifications and implementation plans are local working documents under
`docs/superpowers/`. They are intentionally ignored by Git and must not be committed.
