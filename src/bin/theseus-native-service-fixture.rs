//! Compiled, explicitly supplied-context test fixture only.
#[cfg(target_os = "linux")]
#[path = "../../tests/support/native_desktop_installer.rs"]
mod desktop_installer;
#[cfg(target_os = "linux")]
#[path = "../../tests/support/native_desktop_provider.rs"]
mod desktop_provider;
#[path = "../../tests/support/native_service_installer.rs"]
mod installer;
#[path = "../../tests/support/native_service_provider.rs"]
mod provider;
#[cfg(target_os = "linux")]
#[derive(Debug)]
struct CombinedSutFailure {
    service: anyhow::Error,
    desktop: anyhow::Error,
}
#[cfg(target_os = "linux")]
impl std::fmt::Display for CombinedSutFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "original service SUT: {}; original desktop entry SUT: {}",
            self.service, self.desktop
        )
    }
}
#[cfg(target_os = "linux")]
impl std::error::Error for CombinedSutFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.service.as_ref())
    }
}
#[cfg(target_os = "linux")]
fn combined_effects(
    service: impl FnOnce() -> anyhow::Result<()>,
    desktop: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let service = service();
    let desktop = desktop();
    match (service, desktop) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(service), Err(desktop)) => {
            Err(anyhow::Error::new(CombinedSutFailure { service, desktop }))
        }
    }
}
fn execute(args: &[String]) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use anyhow::{Context, ensure};
        let provider_call = args.first().is_some_and(|s| s == "provider");
        let config_path = if provider_call {
            args.get(3)
        } else {
            args.get(1)
        }
        .context("explicit original fixture config required")?;
        let config: serde_json::Value = serde_json::from_slice(&std::fs::read(config_path)?)?;
        let desktop = config["integration_manifest"]["desktop"].is_object();
        let service = config["integration_manifest"]["service_authority"].is_object();
        ensure!(
            desktop || service,
            "source fixture has no declared integration"
        );
        if desktop && service {
            ensure!(
                config["integration_manifest"]["desktop_native"].is_null()
                    && config["integration_manifest"]["desktop"]["allowed_modes"]
                        == serde_json::json!(["owned_executable"]),
                "combined source fixture grants no native activation"
            );
            ensure!(
                config["integration_manifest"]["desktop"]["slot"]
                    != config["integration_manifest"]["service_authority"]["slot"],
                "combined source resource slots overlap"
            );
            if provider_call {
                return provider::run(&args[1..]);
            }
            ensure!(
                args.len() == 6 && args[5] == "--theseus-combined-entry-only",
                "combined SUT requires committed entry-only substep contract"
            );
            let mut service_args = args[..5].to_vec();
            let mut desktop_args = args.to_vec();
            match args[4].as_str() {
                "missing_entry" | "leaked_entry" => service_args[4] = "none".into(),
                "none" => {}
                _ => desktop_args[4] = "none".into(),
            }
            return combined_effects(
                || installer::run(&service_args),
                || desktop_installer::run(&desktop_args),
            );
        }
        if desktop {
            return if provider_call {
                desktop_provider::run(&args[1..])
            } else {
                desktop_installer::run(args)
            };
        }
    }
    if args.first().is_some_and(|arg| arg == "provider") {
        provider::run(&args[1..])
    } else {
        installer::run(args)
    }
}
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if let Err(error) = execute(&args) {
        eprintln!("service fixture: {error:#}");
        std::process::exit(2);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod combined_sut_tests {
    use std::cell::RefCell;
    #[test]
    fn combined_sut_one_run_executes_both_declared_effects_and_retains_partial_failures() {
        let calls = RefCell::new(Vec::new());
        let result = super::combined_effects(
            || {
                calls.borrow_mut().push("service");
                Err(anyhow::anyhow!("original service failure"))
            },
            || {
                calls.borrow_mut().push("desktop-entry-only");
                Err(anyhow::anyhow!("original entry failure"))
            },
        );
        assert_eq!(*calls.borrow(), ["service", "desktop-entry-only"]);
        let error = result.unwrap_err();
        let text = format!("{error:#}");
        assert!(
            text.contains("original service failure") && text.contains("original entry failure")
        );
    }
}
