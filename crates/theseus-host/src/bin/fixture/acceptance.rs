//! Fixed native peers replacing host shell/Python executables in mixed suites.
use std::{
    ffi::OsString,
    io::{self, Write},
    path::Path,
    time::Duration,
};
fn invalid() -> io::Error {
    io::Error::other("unexpected acceptance Docker invocation")
}
pub(super) fn run(mode: &str, root: &Path, args: &[OsString]) -> io::Result<i32> {
    let args = args
        .iter()
        .map(|a| a.to_str().map(str::to_owned).ok_or_else(invalid))
        .collect::<io::Result<Vec<_>>>()?;
    if args.is_empty() || args.len() > 128 || args.iter().map(String::len).sum::<usize>() > 65536 {
        return Err(invalid());
    }
    if mode.starts_with("mcp-") {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("calls"))?
            .write_all(format!("{}\n", args.join(" ")).as_bytes())?;
        if args[0] == "exec" && args.get(1).map(String::as_str) == Some("--user") {
            match mode {
                "mcp-expired" => {
                    std::thread::sleep(Duration::from_millis(2200));
                    return Ok(0);
                }
                "mcp-emergency" => {
                    std::thread::sleep(Duration::from_millis(800));
                    return Ok(1);
                }
                "mcp-remove" => {
                    let current = std::env::current_exe()?;
                    std::fs::rename(&current, current.with_extension("used"))?;
                    return Ok(0);
                }
                _ => return Err(invalid()),
            }
        }
        if args[0] == "container" && args.get(1).map(String::as_str) == Some("inspect") {
            if mode == "mcp-emergency" {
                // Alone this emergency inspection exceeds both the 3 s exchange
                // window and the 4 s execution budget, so counting it against
                // either manufactures a timeout; exec plus startup stays within.
                std::thread::sleep(Duration::from_millis(3500));
            }
            eprintln!("Error: No such object: {}", args.last().unwrap());
            return Ok(1);
        }
        return Err(invalid());
    }
    let mode = mode.strip_prefix("comparison-").ok_or_else(invalid)?;
    if !matches!(
        mode,
        "probe-transport"
            | "archive-transport"
            | "archive-io"
            | "command-transport"
            | "nofollow"
            | "limit"
    ) {
        return Err(invalid());
    }
    let state_path = root.join("docker-state.json");
    let mut state: serde_json::Value = match std::fs::read(&state_path) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            serde_json::json!({"observations":0,"creates":0})
        }
        Err(e) => return Err(e),
    };
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("docker-calls.jsonl"))?;
    serde_json::to_writer(&mut log, &args)?;
    log.write_all(b"\n")?;
    match args[0].as_str() {
        "version" => println!("29.0"),
        "image" if args.get(1).map(String::as_str) == Some("inspect") => println!(
            "{}",
            if args.get(3).map(String::as_str) == Some("{{.Id}}") {
                format!("sha256:{}", "a".repeat(64))
            } else {
                "null".into()
            }
        ),
        "create" => {
            let count = state["creates"].as_u64().ok_or_else(invalid)? + 1;
            state["creates"] = count.into();
            state["id"] = format!("{count:064x}").into();
            let mut labels = serde_json::Map::new();
            for pair in args.windows(2) {
                if pair[0] == "--label" {
                    let (k, v) = pair[1].split_once('=').ok_or_else(invalid)?;
                    labels.insert(k.into(), v.into());
                }
            }
            state["labels"] = labels.into();
            std::fs::write(&state_path, serde_json::to_vec(&state)?)?;
            println!("{}", state["id"].as_str().ok_or_else(invalid)?);
        }
        "container" if args.get(1).map(String::as_str) == Some("inspect") => println!(
            "{}",
            serde_json::json!({"Id":state["id"],"Config":{"Labels":state["labels"]},"State":{"Running":true}})
        ),
        "start" | "rm" => (),
        "cp" => {
            if args.last().map(String::as_str) != Some("-") {
                return Ok(0);
            }
            if mode == "archive-transport" {
                eprintln!("daemon disconnected");
                return Ok(1);
            }
            if mode == "archive-io" {
                print!("partial");
                return Ok(0);
            }
            let mut output = io::stdout().lock();
            for (name, kind) in [("theseus/", b'5'), ("theseus/child", b'0')] {
                let mut h = [0u8; 512];
                h[..name.len()].copy_from_slice(name.as_bytes());
                for (start, len, value) in [
                    (100, 8, 0o644),
                    (108, 8, 0),
                    (116, 8, 0),
                    (124, 12, 0),
                    (136, 12, 0),
                ] {
                    h[start..start + len]
                        .copy_from_slice(format!("{value:0width$o}\0", width = len - 1).as_bytes());
                }
                h[148..156].fill(b' ');
                h[156] = kind;
                h[257..263].copy_from_slice(b"ustar\0");
                h[263..265].copy_from_slice(b"00");
                let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
                h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
                output.write_all(&h)?;
            }
            output.write_all(&[0; 1024])?;
        }
        "exec" => {
            let i = args.iter().position(|a| a == "-c").ok_or_else(invalid)?;
            let script = args.get(i + 1).ok_or_else(invalid)?;
            if script.starts_with("p=$1;") {
                let count = state["observations"].as_u64().ok_or_else(invalid)? + 1;
                state["observations"] = count.into();
                std::fs::write(&state_path, serde_json::to_vec(&state)?)?;
                if mode == "probe-transport" && count == 1 {
                    eprintln!("daemon disconnected");
                    return Ok(1);
                }
                print!(
                    "{}",
                    if mode == "nofollow" {
                        "nofollow"
                    } else if matches!(mode, "limit" | "archive-transport" | "archive-io") {
                        "present"
                    } else {
                        "missing"
                    }
                );
            } else if script.starts_with("receipt=") && mode == "command-transport" {
                eprintln!("daemon disconnected");
                return Ok(1);
            } else if script.starts_with("test -f") {
                println!(
                    "{}:0",
                    args.last()
                        .and_then(|p| p.split('/').nth(1))
                        .and_then(|p| p.strip_prefix(".theseus-"))
                        .ok_or_else(invalid)?
                );
            }
        }
        _ => return Err(invalid()),
    }
    Ok(0)
}
