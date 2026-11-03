#[path = "../../tests/support/native_catalog_fixture.rs"]
mod fixture;
const _: fn() -> i32 = fixture::installer_main;
fn main() {
    std::process::exit(fixture::reader_main("base"));
}
