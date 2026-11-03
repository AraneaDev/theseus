//! CLI parsing is offline; handlers select explicitly authorized runtime backends.
use crate::{
    comparison,
    engine::{self, CancellationToken, RunOptions},
    model::{ProfileName, ScenarioKind},
    report,
    runner::Docker,
};
use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};
use theseus_host::fs::{HostDir, RelativePath, RootPolicy};
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
    /// Serve the local six-tool MCP facade over ordinary stdio pipes.
    Serve {
        #[arg(long)]
        project_root: PathBuf,
        #[arg(long)]
        output_root: PathBuf,
        /// Server-selected native authority; tool arguments cannot change the target.
        #[arg(long)]
        native_target: Option<PathBuf>,
        #[arg(long, default_value_t = 3600, value_parser = clap::value_parser!(u64).range(1..=86400))]
        max_run_seconds: u64,
    },
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
        #[arg(long, value_enum)]
        profile: Option<ProfileSelection>,
        /// Explicit server/helper target; used only with a native catalog recipe.
        #[arg(long)]
        native_target: Option<PathBuf>,
        /// Display accepted selection without runtime authentication or process creation.
        #[arg(long)]
        preview: bool,
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
        /// Explicit matching launch authority for native recovery only.
        #[arg(long)]
        native_target: Option<PathBuf>,
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
    PluginLifecycle,
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
        Some(Action::Serve {
            project_root,
            output_root,
            native_target,
            max_run_seconds,
        }) => Ok(serve(
            &project_root,
            &output_root,
            max_run_seconds,
            native_target.as_deref(),
        )),
        None => {
            crate::tui::terminal::require_tty()?;
            match crate::native::storage::load_recipe(Path::new("theseus.toml"))? {
                engine::domain::SelectedRecipe::Docker(recipe) => {
                    let options = RunOptions::all(&recipe);
                    crate::tui::run_with_options(
                        crate::tui::App::preview(recipe, options),
                        animation,
                    )
                }
                engine::domain::SelectedRecipe::Native(recipe) => {
                    run_native(recipe, None, false, true, ScenarioSelection::All, animation)
                }
            }
        }
        Some(Action::Init) => {
            init()?;
            println!(
                "Created theseus.toml; configure its failing placeholder commands before running."
            );
            Ok(0)
        }
        Some(Action::Validate { recipe: path }) => {
            match crate::native::storage::load_recipe(&path)? {
                engine::domain::SelectedRecipe::Docker(recipe) => println!(
                    "Valid recipe: {} ({} profiles)",
                    recipe.recipe.name,
                    recipe.recipe.profiles.len()
                ),
                engine::domain::SelectedRecipe::Native(recipe) => println!(
                    "Valid native recipe: {} ({} scenarios)",
                    recipe.digest,
                    recipe.recipe.scenarios.len()
                ),
            }
            Ok(0)
        }
        Some(Action::Report {
            run_directory,
            json,
        }) => {
            match crate::native::storage::load_run(&run_directory)? {
                engine::domain::StoredRun::Docker(report) => {
                    if !json {
                        return crate::tui::run_with_options(
                            crate::tui::App::offline(report, run_directory),
                            animation,
                        );
                    }
                    println!("{}", serde_json::to_string_pretty(&report)?);
                }
                engine::domain::StoredRun::Native(report) => {
                    if !json {
                        return crate::tui::run_with_options(
                            crate::tui::App::native_offline(report, run_directory),
                            animation,
                        );
                    }
                    println!("{}", serde_json::to_string_pretty(&report)?);
                }
            }
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
        Some(Action::Cleanup {
            run_directory,
            native_target,
        }) => {
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
                anyhow::ensure!(
                    native_target.is_none(),
                    "--native-target requires a native run"
                );
                comparison::cleanup(&run_directory)?;
            } else {
                let store = report::open_existing(&run_directory)?;
                match store.load_selected_journal()? {
                    engine::domain::StoredJournal::Docker(_) => {
                        anyhow::ensure!(
                            native_target.is_none(),
                            "--native-target requires a native run"
                        );
                        Docker::default().cleanup(&run_directory)?
                    }
                    engine::domain::StoredJournal::Native(_) => {
                        let path = native_target.as_deref().ok_or_else(|| anyhow::anyhow!(
                            "native cleanup requires --native-target with explicit matching launch authority; saved report endpoints do not authorize recovery"
                        ))?;
                        let launch = read_native_launch(path)?;
                        let cancel = CancellationToken::default();
                        let _signals = crate::tui::Signals::register(&cancel)?;
                        let result = engine::recover_native(store, &launch, cancel, |_| {})?;
                        println!(
                            "{}",
                            crate::tui::safe_text(&format!(
                                "Recovery report: {}",
                                result.report_path.display()
                            ))
                        );
                        if !result.complete() {
                            eprintln!(
                                "Native recovery remains incomplete; retained evidence records unresolved cleanup dispositions."
                            );
                            return Ok(2);
                        }
                    }
                }
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
            native_target,
            preview,
        }) => {
            if !headless {
                crate::tui::terminal::require_tty()?;
            }
            let selected = crate::native::storage::load_recipe(&recipe)?;
            let recipe = match selected {
                engine::domain::SelectedRecipe::Native(recipe) => {
                    anyhow::ensure!(
                        profile.is_none() && !pull,
                        "Docker profile/pull options do not apply to native recipes"
                    );
                    return run_native(
                        recipe,
                        native_target.as_deref(),
                        headless,
                        preview,
                        scenario,
                        animation,
                    );
                }
                engine::domain::SelectedRecipe::Docker(recipe) => recipe,
            };
            anyhow::ensure!(
                native_target.is_none(),
                "--native-target requires a native recipe"
            );
            let mut options = RunOptions::all(&recipe);
            options.pull = pull;
            options.profiles = match profile.unwrap_or(ProfileSelection::All) {
                ProfileSelection::User => vec![ProfileName::User],
                ProfileSelection::System => vec![ProfileName::System],
                ProfileSelection::All => options.profiles,
            };
            options.scenarios = match scenario {
                ScenarioSelection::PluginLifecycle => vec![ScenarioKind::PluginLifecycle],
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
            if !headless || preview {
                if headless {
                    println!(
                        "{}",
                        crate::tui::App::preview(recipe, options).preview_text()
                    );
                    return Ok(0);
                }
                return crate::tui::run_with_options(
                    crate::tui::App::preview(recipe, options),
                    animation,
                );
            }
            let cancel = CancellationToken::default();
            let registration =
                theseus_host::cancel::CancellationRegistration::install(cancel.flag())?;
            let result = engine::run(&recipe, options, cancel, |_| {});
            drop(registration);
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
fn run_native(
    recipe: crate::native::recipe::ValidatedNativeRecipe,
    launch_path: Option<&Path>,
    headless: bool,
    preview: bool,
    scenario: ScenarioSelection,
    animation: crate::tui::AnimationOptions,
) -> Result<i32> {
    let launch = launch_path.map(read_native_launch).transpose()?;
    let scenarios = match scenario {
        ScenarioSelection::All => recipe.recipe.scenarios.clone(),
        ScenarioSelection::Clean => vec![ScenarioKind::Clean],
        ScenarioSelection::Upgrade => vec![ScenarioKind::Upgrade],
        ScenarioSelection::Reinstall => vec![ScenarioKind::Reinstall],
        _ => anyhow::bail!("unsupported native scenario"),
    };
    let options = engine::domain::NativeRunOptions {
        output_root: recipe
            .path
            .parent()
            .unwrap_or(Path::new("."))
            .join(".theseus-runs"),
        scenarios,
        limits: recipe.recipe.limits.clone(),
    };
    recipe.validate_selection(&options.scenarios, &options.limits)?;
    if !headless {
        let mut app = crate::tui::App::native_preview(crate::native::view::preview(
            &recipe,
            launch.as_ref(),
        )?);
        if !preview {
            app.native
                .as_mut()
                .unwrap()
                .set_execution(recipe, options, launch);
        }
        return crate::tui::run_with_options(app, animation);
    }
    if preview {
        println!(
            "{}",
            crate::tui::safe_text(&crate::native::view::preview(&recipe, launch.as_ref())?)
        );
        return Ok(0);
    }
    let launch = launch.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "native execution requires --native-target; use --preview for offline inspection"
        )
    })?;
    let cancel = CancellationToken::default();
    let _registration = theseus_host::cancel::CancellationRegistration::install(cancel.flag())?;
    let result = engine::run_selected(
        &engine::domain::SelectedRecipe::Native(recipe),
        engine::domain::SelectedRunOptions::Native(options),
        Some(launch),
        cancel,
        |_| {},
    )?;
    let engine::domain::StoredRun::Native(report) = result.report else {
        anyhow::bail!("native execution returned another backend");
    };
    println!(
        "Report: {}",
        crate::tui::safe_text(&result.report_path.display().to_string())
    );
    println!("{}", report.outcome);
    for failure in &report.failures {
        eprintln!("{}", crate::tui::safe_text(failure));
    }
    Ok(report.outcome.exit_code())
}
fn read_native_launch(path: &Path) -> Result<crate::native::launch::LaunchAuthority> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let directory = HostDir::open(parent, RootPolicy::AllComponentsNoFollow)?;
    let leaf = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("invalid launch filename"))?;
    let mut bytes = Vec::new();
    directory
        .open_file(&RelativePath::parse(leaf)?)?
        .take(65537)
        .read_to_end(&mut bytes)?;
    crate::native::launch::LaunchAuthority::decode(&bytes)
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
    let directory = HostDir::open(Path::new("."), RootPolicy::LeafNoFollow)?;
    let name = RelativePath::parse("theseus.toml")?;
    let mut file = directory.create_file(&name)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    let mut saved = Vec::new();
    directory
        .open_file(&name)?
        .take(text.len() as u64 + 1)
        .read_to_end(&mut saved)?;
    anyhow::ensure!(
        saved == text.as_bytes(),
        "init bytes changed during persistence"
    );
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
/// Serve has its own quiet binary boundary: errors never enter the CLI printer.
fn serve(
    project_root: &Path,
    output_root: &Path,
    max_run_seconds: u64,
    native_target: Option<&Path>,
) -> i32 {
    use crate::mcp_server::{
        coordinator::{IDLE_WAIT, ServerRuntime},
        diagnostics,
        roots::Roots,
        shutdown::RuntimeAction,
        types::Instance,
    };
    use std::{sync::Arc, time::Instant};
    diagnostics::install_quiet_panic_hook();
    let mut random = [0u8; 16];
    if theseus_host::random_bytes(&mut random).is_err() {
        return 2;
    }
    let instance = Instance::parse(&format!("{:032x}", u128::from_be_bytes(random)))
        .expect("random instance encoding");
    let stop = CancellationToken::default();
    let Ok(_signals) = theseus_host::cancel::CancellationRegistration::install(stop.flag()) else {
        return 2;
    };
    let roots = Roots::open(project_root, output_root).and_then(|mut roots| {
        roots.native_target = native_target
            .map(read_native_launch)
            .transpose()
            .map_err(|e| {
                crate::mcp_server::types::QueryError::new(
                    crate::mcp_server::types::ErrorCode::InvalidArguments,
                    e,
                )
            })?
            .map(Arc::new);
        Ok(Arc::new(roots))
    });
    let transport = theseus_host::stdio::Stdio::acquire();
    let mut runtime = ServerRuntime::new(
        transport,
        roots,
        instance,
        max_run_seconds,
        stop,
        Instant::now(),
    );
    loop {
        match runtime.poll(Instant::now()) {
            RuntimeAction::Continue => std::thread::sleep(IDLE_WAIT),
            RuntimeAction::OrderlyExit(code) => return code,
            // Runtime and all unresolved native owners stay live until process death.
            RuntimeAction::ForceProcessExit(code) => std::process::exit(code),
        }
    }
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
