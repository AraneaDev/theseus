//! Inert catalog fixture only: writes exactly reader/app/data in a supplied prefix.
use std::{
    io,
    path::{Path, PathBuf},
};
fn leaf(prefix: &Path, name: &str) -> PathBuf {
    prefix.join(name)
}
pub fn installer_main() -> i32 {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|a| a == "provider") {
        return match provider(&args[1..]) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        };
    }
    if args.first().is_some_and(|a| a == "-V") {
        eprintln!("OpenSSH_10.0 fixture-only");
        return 0;
    }
    if args.first().is_some_and(|a| a == "-G") {
        for pair in args.windows(2).filter(|p| p[0] == "-o") {
            let Some((key, value)) = pair[1].split_once('=') else {
                return 2;
            };
            println!("{} {}", key.to_ascii_lowercase(), value);
        }
        for (flag, key) in [("-l", "user"), ("-p", "port"), ("-i", "identityfile")] {
            if let Some(pair) = args.windows(2).find(|p| p[0] == flag) {
                println!("{} {}", key, pair[1]);
            }
        }
        if args.len() >= 2 {
            println!("host {}", args[args.len() - 2]);
            println!("hostname {}", args[args.len() - 2]);
        }
        println!("requesttty false\nsessiontype subsystem");
        return 0;
    }

    match install() {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}
fn install() -> io::Result<()> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 5 {
        return Err(io::Error::other(
            "expected catalog ID, prefix, base, target, reader leaf",
        ));
    }
    let id = args[0]
        .to_str()
        .ok_or_else(|| io::Error::other("non-UTF8 ID"))?;
    let prefix = Path::new(&args[1]);
    let reader = args[4]
        .to_str()
        .ok_or_else(|| io::Error::other("reader leaf"))?;
    if !prefix.is_absolute() || !matches!(reader, "reader" | "reader.exe") {
        return Err(io::Error::other("explicit private prefix/reader required"));
    }
    match id {
        "install_base" | "install_target" => {
            std::fs::copy(
                if id == "install_base" {
                    &args[2]
                } else {
                    &args[3]
                },
                leaf(prefix, reader),
            )?;
            std::fs::write(leaf(prefix, "app"), b"theseus fixture app\n")?;
        }
        "configure" => std::fs::write(leaf(prefix, "data"), b"theseus fixture data\n")?,
        "uninstall" => {
            for name in [reader, "app", "data"] {
                match std::fs::remove_file(leaf(prefix, name)) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
        }
        // Reader checks/exercise must configure the *installed reader itself* as
        // their catalog executable; routing them through installer is refused.
        _ => return Err(io::Error::other("unsupported fixture installer ID")),
    }
    Ok(())
}
pub fn reader_main(version: &str) -> i32 {
    match read(version) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}
fn read(version: &str) -> io::Result<()> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        return Err(io::Error::other("expected reader action and data leaf"));
    }
    let action = args[0]
        .to_str()
        .ok_or_else(|| io::Error::other("reader action"))?;
    match action {
        "check_base" if version == "base" => println!("base"),
        "check_target" | "exercise" if version == "target" => println!("target"),
        "check_data" => {
            if std::fs::read(&args[1])? != b"theseus fixture data\n" {
                return Err(io::Error::other("fixture data mismatch"));
            }
            println!("data");
        }
        _ => return Err(io::Error::other("reader version/action mismatch")),
    }
    Ok(())
}

// Fake provider contract model only. It reserves durable exclusive ownership and
// returns preprovisioned inert metadata; it never resets a VM or real target.
fn provider(args: &[String]) -> io::Result<()> {
    use std::io::Write;
    if args.len() != 4 {
        return Err(io::Error::other(
            "expected fake provider root/template/mode/identity",
        ));
    }
    let root = Path::new(&args[0]);
    if !root.is_absolute() || !root.is_dir() {
        return Err(io::Error::other(
            "explicit private fake provider root required",
        ));
    }
    let invocation: serde_json::Value = serde_json::from_str(&args[3]).map_err(io::Error::other)?;
    let field = |key: &str| -> io::Result<&str> {
        invocation[key]
            .as_str()
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 256
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            })
            .ok_or_else(|| io::Error::other("invalid fake provider identity"))
    };
    let owner = field("ownership_id")?;
    let id = field("invocation_id")?;
    let name = field("provider")?;
    let action = field("action")?;
    let fault = std::env::var("THESEUS_INERT_PROVIDER_FAULT").unwrap_or_default();
    let response = root.join(format!("{id}.json"));
    if args[2] == "query" {
        let stored = match std::fs::read(&response) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                print!("null");
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let stored: serde_json::Value =
            serde_json::from_slice(&stored).map_err(io::Error::other)?;
        if stored["invocation"] != invocation {
            return Err(io::Error::other("query must use exact original identity"));
        }
        if action == "prepare" && fault == "lost_prepare_query_none" {
            print!("null");
            return Ok(());
        }
        print!(
            "{}",
            serde_json::to_string(&stored["receipt"]).map_err(io::Error::other)?
        );
        return Ok(());
    }
    if args[2] != "invoke" {
        return Err(io::Error::other("unsupported fake provider mode"));
    }
    let reservation = root.join("owner");
    match action {
        "prepare" => {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&reservation)?;
            file.write_all(owner.as_bytes())?;
            file.sync_all()?
        }
        "reset" | "dispose" => {
            if std::fs::read(&reservation)? != owner.as_bytes() {
                return Err(io::Error::other("unknown/conflicting original owner"));
            }
        }
        _ => return Err(io::Error::other("unsupported fake provider action")),
    }
    let mut receipt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&args[1])?).map_err(io::Error::other)?;
    let typed: theseus::native::model::ProviderInvocation =
        serde_json::from_value(invocation.clone()).map_err(io::Error::other)?;
    receipt["invocation_digest"] = typed.digest().map_err(io::Error::other)?.into();
    if matches!(action, "reset" | "dispose") {
        let mut epoch = [0u8; 32];
        theseus_host::random_bytes(&mut epoch)?;
        receipt["reset_epoch"] = epoch
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
            .into();
    }
    receipt["provider"] = name.into();
    receipt["invocation_id"] = id.into();
    receipt["receipt"] = format!("inert-model-{action}-{id}").into();
    let stored = serde_json::json!({"invocation":invocation,"receipt":receipt});
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&response)?;
    file.write_all(&serde_json::to_vec(&stored).map_err(io::Error::other)?)?;
    file.sync_all()?;
    if matches!(action, "reset" | "dispose") {
        std::fs::remove_file(reservation)?
    }
    if action == "prepare"
        && matches!(
            fault.as_str(),
            "lost_prepare_query_none" | "lost_prepare_query_receipt"
        )
    {
        print!("null");
        return Ok(());
    }
    print!(
        "{}",
        serde_json::to_string(&stored["receipt"]).map_err(io::Error::other)?
    );
    Ok(())
}
