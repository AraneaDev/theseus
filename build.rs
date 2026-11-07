fn main() {
    // The compiled fixture is also used for inert native shortcut parsing.
    // Embed explicit launch policy so absence of metadata cannot pass asInvoker
    // admission. These flags affect fixture binaries only, not the application.
    if std::env::var("TARGET").is_ok_and(|target| target.ends_with("-pc-windows-msvc")) {
        for binary in [
            "theseus-native-service-base",
            "theseus-native-service-target",
        ] {
            println!("cargo:rustc-link-arg-bin={binary}=/MANIFEST:EMBED");
            println!(
                "cargo:rustc-link-arg-bin={binary}=/MANIFESTUAC:level='asInvoker' uiAccess='false'"
            );
        }
    }
}
