#[path = "../../tests/support/native_service_reader.rs"]
mod reader;
fn main() {
    if let Err(error) = reader::run("base") {
        eprintln!("fixture: {error:#}");
        std::process::exit(2);
    }
}
