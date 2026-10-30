//! CLI parsing is offline; handlers only construct Docker for execution/cleanup.
use crate::{
    comparison,
    engine::{self, CancellationToken, RunOptions},
    model::{ProfileName, ScenarioKind},
    recipe, report,
    runner::Docker,
};
use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use std::{fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt, path::PathBuf};
#[derive(Debug, Parser)]
#[command(name = "theseus", version, about = "Installation lifecycle workbench")]
pub struct Cli {
    /// Skip decorative startup and exit scenes.
    #[arg(long, global = true)]
    pub no_animation: bool,
    #[command(subcommand)]
    pub command: Option<Action>,
}
#[derive(Debug, Subcommand)]
pub enum Action {
    Init,
    Validate {
        #[arg(long, default_value = "theseus.toml")]
        recipe: PathBuf,
    },
    Run {
        #[arg(long)]
        headless: bool,
        #[arg(long, default_value = "theseus.toml")]
        recipe: PathBuf,
        #[arg(long, value_enum, default_value = "all")]
        profile: ProfileSelection,
        #[arg(long, value_enum, default_value = "all")]
        scenario: ScenarioSelection,
        #[arg(long)]
        pull: bool,
    },
    Compare {
        #[arg(long)]
        suite: PathBuf,
        #[arg(long)]
        headless: bool,
        #[arg(long)]
        pull: bool,
    },
    Comparison {
        suite_directory: PathBuf,
        #[arg(long)]
        json: bool,
    },
    Report {
        run_directory: PathBuf,
        #[arg(long)]
        json: bool,
    },
    Cleanup {
        run_directory: PathBuf,
    },
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ProfileSelection {
    User,
    System,
    All,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ScenarioSelection {
    Rollback,
    InterruptedRollback,
    Downgrade,
    Clean,
    Upgrade,
    Reinstall,
    InterruptedInstall,
    InterruptedUpgrade,
    All,
}
/// Dispatch interactive and headless interfaces through the shared engine.
pub fn execute(cli: Cli) -> Result<i32> {
    let animation = crate::tui::AnimationOptions {
        enabled: !cli.no_animation,
    };
    match cli.command {
        None => {
            crate::tui::terminal::require_tty()?;
            let recipe = recipe::load(std::path::Path::new("theseus.toml"))?;
            let options = RunOptions::all(&recipe);
            crate::tui::run_with_options(crate::tui::App::preview(recipe, options), animation)
        }
        Some(Action::Init) => {
            init()?;
            println!(
                "Created theseus.toml; configure its failing placeholder commands before running."
            );
            Ok(0)
        }
        Some(Action::Validate { recipe: path }) => {
            let recipe = recipe::load(&path)?;
            println!(
                "Valid recipe: {} ({} profiles)",
                recipe.recipe.name,
                recipe.recipe.profiles.len()
            );
            Ok(0)
        }
        Some(Action::Report {
            run_directory,
            json,
        }) => {
            let report = report::load(&run_directory)?;
            if !json {
                return crate::tui::run_with_options(
                    crate::tui::App::offline(report, run_directory),
                    animation,
                );
            }
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(0)
        }
        Some(Action::Comparison {
            suite_directory,
            json,
        }) => {
            let document = comparison::load_document(&suite_directory)?;
            if !json {
                return crate::tui::run_with_options(
                    crate::tui::App::comparison_offline(document, suite_directory),
                    animation,
                );
            }
            write_comparison_json(
                &document.report,
                &mut std::io::stdout().lock(),
                comparison::MAX_DERIVED_BYTES,
            )?;
            Ok(0)
        }
        Some(Action::Compare {
            suite,
            headless,
            pull,
        }) => {
            if !headless {
                crate::tui::terminal::require_tty()?;
            }
            let suite = comparison::load(&suite)?;
            let options = comparison::SuiteRunOptions {
                output_root: suite.project_root.join(".theseus-comparisons"),
                pull,
            };
            if !headless {
                return crate::tui::run_with_options(
                    crate::tui::App::comparison_preview(suite, options),
                    animation,
                );
            }
            let cancel = CancellationToken::default();
            let _signals = crate::tui::Signals::register(&cancel)?;
            let result = comparison::run(&suite, options, cancel, |_| {})?;
            println!(
                "{}",
                crate::tui::safe_text(&format!("Comparison: {}", result.report_path.display()))
            );
            println!("{}", result.report.outcome);
            for source in &result.report.sources {
                println!("{}: {}", source.variant_id, source.outcome);
                if let Some(error) = &source.error {
                    let root = result.report_path.parent().unwrap();
                    eprintln!(
                        "{}",
                        crate::tui::safe_text(&format!(
                            "Source {}: {error}\nSource report: {}\nRecovery: theseus cleanup {}",
                            source.variant_id,
                            source.report_path.as_ref().map_or_else(
                                || "<not started>".into(),
                                |p| root.join(p).display().to_string()
                            ),
                            crate::shell::path_argument(root)
                        ))
                    );
                }
            }
            for diagnostic in &result.report.diagnostics {
                eprintln!("{}", crate::tui::safe_text(diagnostic));
            }
            Ok(result.report.outcome.exit_code())
        }
        Some(Action::Cleanup { run_directory }) => {
            // Shape is only a routing hint; comparison cleanup validates the entire
            // envelope and pinned journals before any external removal.
            if run_directory
                .file_name()
                .is_some_and(|n| n == "comparison.json")
                || run_directory
                    .join("comparison.json")
                    .symlink_metadata()
                    .is_ok()
            {
                comparison::cleanup(&run_directory)?;
            } else {
                Docker::default().cleanup(&run_directory)?;
            }
            println!("Journaled resources cleaned up.");
            Ok(0)
        }
        Some(Action::Run {
            headless,
            recipe,
            profile,
            scenario,
            pull,
        }) => {
            if !headless {
                crate::tui::terminal::require_tty()?;
            }
            let recipe = recipe::load(&recipe)?;
            let mut options = RunOptions::all(&recipe);
            options.pull = pull;
            options.profiles = match profile {
                ProfileSelection::User => vec![ProfileName::User],
                ProfileSelection::System => vec![ProfileName::System],
                ProfileSelection::All => options.profiles,
            };
            options.scenarios = match scenario {
                ScenarioSelection::Downgrade => vec![ScenarioKind::Downgrade],
                ScenarioSelection::Rollback => vec![ScenarioKind::Rollback],
                ScenarioSelection::InterruptedRollback => vec![ScenarioKind::InterruptedRollback],
                ScenarioSelection::Clean => vec![ScenarioKind::Clean],
                ScenarioSelection::Upgrade => vec![ScenarioKind::Upgrade],
                ScenarioSelection::Reinstall => vec![ScenarioKind::Reinstall],
                ScenarioSelection::InterruptedInstall => vec![ScenarioKind::InterruptedInstall],
                ScenarioSelection::InterruptedUpgrade => vec![ScenarioKind::InterruptedUpgrade],
                ScenarioSelection::All => options.scenarios,
            };
            if !headless {
                return crate::tui::run_with_options(
                    crate::tui::App::preview(recipe, options),
                    animation,
                );
            }
            let cancel = CancellationToken::default();
            let sigint = signal_hook::flag::register(signal_hook::consts::SIGINT, cancel.flag())?;
            let sigterm =
                match signal_hook::flag::register(signal_hook::consts::SIGTERM, cancel.flag()) {
                    Ok(id) => id,
                    Err(error) => {
                        signal_hook::low_level::unregister(sigint);
                        return Err(error.into());
                    }
                };
            let result = engine::run(&recipe, options, cancel, |_| {});
            signal_hook::low_level::unregister(sigint);
            signal_hook::low_level::unregister(sigterm);
            let result = result?;
            let counts = result.report.counts();
            println!("Report: {}", result.report_path.display());
            println!(
                "{}: passed={} failed={} review={} incomplete={} skipped={}",
                result.report.outcome,
                counts.passed_checks,
                counts.failed_checks,
                counts.review_findings,
                counts.incomplete_checks,
                counts.skipped_checks
            );
            for error in &result.report.errors {
                eprintln!("{error}");
            }
            Ok(result.report.outcome.exit_code())
        }
    }
}
fn init() -> Result<()> {
    let mut text = String::from(
        "# Replace each command with your installation lifecycle. Every placeholder fails.\n# Artifact '.' is the recipe project; choose actual input directories before running.\nschema_version = 1\nname = \"my-cli\"\nimage = \"ubuntu:24.04\"\nobserve = [\"/home/theseus\", \"/usr/local/bin\"]\n[artifacts]\nbase = \".\"\ntarget = \".\"\n",
    );
    for profile in ["user", "system"] {
        for step in [
            "install_base",
            "install_target",
            "configure",
            "upgrade",
            "exercise",
            "uninstall",
        ] {
            text += &format!(
                "\n[profiles.{profile}.steps.{step}]\nscript = \"echo 'Configure {profile}/{step} in theseus.toml' >&2; exit 1\"\n"
            );
        }
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open("theseus.toml")?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

fn write_comparison_json(
    report: &comparison::ComparisonReport,
    output: &mut impl std::io::Write,
    limit: usize,
) -> Result<()> {
    let json_limit = limit
        .checked_sub(1)
        .ok_or_else(|| anyhow::anyhow!("JSON exceeds size limit"))?;
    let bytes = report::json_bytes(report, json_limit)?;
    output.write_all(&bytes)?;
    output.write_all(b"\n")?;
    Ok(())
}
#[cfg(test)]
mod bounded_comparison_json_tests {
    use super::*;
    #[test]
    fn oversized_comparison_json_emits_nothing_and_success_is_complete() {
        let report = comparison::ComparisonReport {
            sources: vec![],
            cells: vec![],
            outcome: crate::model::RunOutcome::Incomplete,
            diagnostics: vec!["\u{0001}".repeat(256)],
        };
        let mut stdout = vec![];
        assert!(write_comparison_json(&report, &mut stdout, 512).is_err());
        assert!(stdout.is_empty(), "partial JSON must never reach stdout");
        write_comparison_json(&report, &mut stdout, 4096).unwrap();
        let decoded: comparison::ComparisonReport = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(decoded.diagnostics, report.diagnostics);
        assert!(stdout.len() <= 4096);
        let exact_size = stdout.len();
        stdout.clear();
        assert!(write_comparison_json(&report, &mut stdout, exact_size - 1).is_err());
        assert!(stdout.is_empty());
    }
}
