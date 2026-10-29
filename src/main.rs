use clap::Parser;
fn main() {
    let code = match theseus::cli::execute(theseus::cli::Cli::parse()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("theseus: {error:#}");
            2
        }
    };
    std::process::exit(code);
}
