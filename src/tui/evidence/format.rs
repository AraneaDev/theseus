use super::*;
use std::fmt::{self, Write};
pub(super) fn record(r: Record<'_>, out: &mut dyn Write) -> fmt::Result {
    match r {
        Record::TerminalError(s) | Record::Text(s) => out.write_str(s),
        Record::Skipped(s) => write!(out, "Skipped: {s}"),
        Record::Check(c) => check(c, out),
        Record::Stage(s) => write!(out, "{}: {}", s.name, s.outcome),
        Record::Restart(p) => write!(
            out,
            "Restart: {} running={}",
            p.container_id, p.running_verified
        ),
        Record::ExpectedFailure(p) => write!(
            out,
            "Expected failure: exit={} matched={} marker={:?}",
            p.expected_exit, p.stdout_matched, p.stdout_contains
        ),
        Record::DeclaredRefusal(c, p) => write!(
            out,
            "Declared refusal: actual={:?} expected={} matched={} marker={:?}",
            c.exit_status, p.expected_exit, p.stdout_matched, p.stdout_contains
        ),
        Record::Interruption(p) => write!(
            out,
            "Interruption {:?}: readiness={} stopped={}",
            p.mechanism, p.readiness_verified, p.stopped_verified
        ),
        Record::Barrier(c) => write!(
            out,
            "Barrier not reached: ordinary completion exit {:?}",
            c.exit_status
        ),
        Record::Finding(f) => write!(out, "Review {} {:?}: {}", f.kind, f.path, f.message),
        Record::Teardown(t) => write!(out, "Teardown: {t:?}"),
        Record::IncompleteObservation(s) => write!(out, "{} {:?}", s.root, s.completeness),
        Record::Observation(s, true) => write!(
            out,
            "Observation {}: {:?} ({} entries)",
            s.root,
            s.completeness,
            s.entries.len()
        ),
        Record::Observation(s, false) => write!(out, "Observation {} {:?}", s.root, s.completeness),
        Record::Entry(p, e) => write!(out, "{p}: {e:?}"),
        Record::Change(label, d) => write!(
            out,
            "{label} {:?} {}\nbefore: {:?}\nafter: {:?}",
            d.kind, d.path, d.before, d.after
        ),
        Record::Command(c) => write!(
            out,
            "{} {}: {}\nargv: {:?}\nUID:GID {}:{} account home={} cwd={}\nenv: {:?}\nforwarded env (values hidden): {:?}\nUTC {} | {}ms | exit {:?} | timeout={} cancelled={}\nstdout: {:?}\nstderr: {:?}\nerror: {:?} skipped: {:?}",
            c.id,
            c.phase,
            c.outcome,
            c.argv,
            c.identity.uid,
            c.identity.gid,
            c.identity.home,
            c.workdir,
            c.environment,
            c.forwarded_environment,
            c.started_at,
            c.duration_ms,
            c.exit_status,
            c.timed_out,
            c.cancelled,
            c.stdout,
            c.stderr,
            c.error,
            c.skip_reason
        ),
        Record::Log(c, s, l) => write!(
            out,
            "{} {s}: {} ({} bytes, truncated={})",
            c.id, l.path, l.bytes, l.truncated
        ),
        Record::Cell(c, m) => {
            let path = m
                .scopes
                .iter()
                .find(|s| s.id == c.scope_id)
                .map_or("<unknown scope>", |s| s.path.as_str());
            write!(
                out,
                "{} vs {} | {} | {} | {}\nScope {}: {} | {:?}",
                c.variant_id,
                m.baseline,
                c.profile,
                c.scenario,
                c.stage,
                c.scope_id,
                path,
                c.comparison.status
            )
        }
        Record::Diagnostic(s) => write!(out, "Unknown / diagnostic: {s}"),
        Record::Source(s) => write!(
            out,
            "{}: {} | run {}\nImage: {}\nImage ID: {}\nRecipe digest: {}",
            s.variant_id,
            s.outcome,
            s.run_id.as_deref().unwrap_or("<pending>"),
            s.image.as_deref().unwrap_or("<pending>"),
            s.image_id.as_deref().unwrap_or("<pending>"),
            s.recipe_digest.as_deref().unwrap_or("<pending>")
        ),
        Record::Artifact(a) => write!(
            out,
            "Artifact {}: {} ({} files)\nFile digests: {:?}",
            a.name,
            a.sha256,
            a.files.len(),
            a.files
        ),
        Record::SourcePath(root, path) => {
            out.write_str("Source logs/commands: theseus report ")?;
            shell_path(out, root, Some(path))
        }
        Record::Cleanup(root) => {
            out.write_str("Recovery: theseus cleanup ")?;
            shell_path(out, root, None)
        }
        Record::Json(root) => {
            out.write_str("Complete evidence: theseus comparison ")?;
            shell_path(out, root, None)?;
            out.write_str(" --json")
        }
    }
}
fn check(c: &CheckResult, out: &mut dyn Write) -> fmt::Result {
    write!(out, "{} {}: {}", c.id, c.outcome, c.message)?;
    if let Some(p) = &c.command_expectation {
        write!(
            out,
            "\nReader/runtime expectation: exit={} matched={} marker={:?}",
            p.expected_exit, p.stdout_matched, p.stdout_contains
        )?;
        if let Some(e) = &p.executable {
            write!(out, "; {} -> {} {:?}", e.name, e.resolved_path, e.args)?;
        }
    }
    if let Some(p) = &c.path {
        write!(out, "\nScope: {p}")?;
    }
    if let Some(b) = &c.backup {
        write!(out, " source {} stored {}", b.source_path, b.stored_path)?;
    }
    if let Some(c) = &c.comparison {
        write!(out, "\nComparison: {} -> {}", c.before_stage, c.after_stage)?;
    }
    Ok(())
}
// Stream POSIX quoting before projection; source paths can be large stored fields.
fn shell_path(out: &mut dyn Write, root: &Path, relative: Option<&str>) -> fmt::Result {
    let root = root.to_string_lossy();
    let chars = || {
        root.chars().chain(
            relative
                .into_iter()
                .flat_map(|r| std::iter::once('/').chain(r.chars())),
        )
    };
    let bare = (!root.is_empty() || relative.is_some())
        && chars().all(|c| c.is_ascii_alphanumeric() || "/_-.".contains(c));
    if !bare {
        out.write_char('\'')?;
    }
    for c in chars() {
        if c == '\'' {
            out.write_str("'\"'\"'")?;
        } else {
            out.write_char(c)?;
        }
    }
    if !bare {
        out.write_char('\'')?;
    }
    Ok(())
}
