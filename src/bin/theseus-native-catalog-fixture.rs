#[path = "../../tests/support/native_catalog_fixture.rs"]
mod fixture;
const _: fn(&str) -> i32 = fixture::reader_main;
fn main() {
    std::process::exit(fixture::installer_main());
}
