use std::{env, path::PathBuf, process::Command};

fn run(command: &mut Command) {
    let status = command.status().expect("start macOS bridge tool");
    assert!(status.success(), "macOS bridge tool failed: {command:?}");
}

fn main() {
    println!("cargo:rerun-if-changed=src/desktop/macos/bridge.m");
    println!("cargo:rerun-if-changed=src/desktop/macos/bridge.h");
    println!("cargo:rerun-if-env-changed=MACOSX_DEPLOYMENT_TARGET");
    let target = env::var("TARGET").expect("Cargo TARGET");
    if !target.ends_with("-apple-darwin") {
        return;
    }
    if !env::var("HOST")
        .expect("Cargo HOST")
        .ends_with("-apple-darwin")
    {
        // Cross-target cargo check validates Rust only. Native macOS builds must
        // compile the bridge with the actual Apple SDK before they can link.
        println!(
            "cargo:warning=macOS cross-check does not qualify the AppKit bridge; native Apple SDK build required"
        );
        return;
    }
    let arch = match target.split('-').next().expect("target architecture") {
        "aarch64" => "arm64",
        "x86_64" => "x86_64",
        other => panic!("unsupported macOS bridge architecture: {other}"),
    };
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo OUT_DIR"));
    let object = output.join("theseus_macos_desktop.o");
    let archive = output.join("libtheseus_macos_desktop.a");
    let deployment = env::var("MACOSX_DEPLOYMENT_TARGET").unwrap_or_else(|_| "11.0".into());
    run(Command::new("xcrun")
        .args([
            "--sdk",
            "macosx",
            "clang",
            "-arch",
            arch,
            "-fobjc-arc",
            "-fblocks",
            "-fexceptions",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-c",
            "src/desktop/macos/bridge.m",
            "-o",
        ])
        .arg(&object)
        .arg(format!("-mmacosx-version-min={deployment}")));
    run(Command::new("xcrun")
        .args(["--sdk", "macosx", "ar", "crs"])
        .arg(&archive)
        .arg(&object));
    println!("cargo:rustc-link-search=native={}", output.display());
    println!("cargo:rustc-link-lib=static=theseus_macos_desktop");
    for framework in ["Foundation", "AppKit", "Security"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
