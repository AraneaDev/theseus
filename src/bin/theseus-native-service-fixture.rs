//! Compiled, explicitly supplied-context test fixture only.
#[path = "../../tests/support/native_service_installer.rs"]
mod installer;
#[path = "../../tests/support/native_service_provider.rs"]
mod provider;
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let result = if args.first().is_some_and(|arg| arg == "provider") {
        provider::run(&args[1..])
    } else {
        installer::run(&args)
    };
    if let Err(error) = result {
        eprintln!("service fixture: {error:#}");
        std::process::exit(2);
    }
}
