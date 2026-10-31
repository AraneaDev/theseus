//! Fixed, bounded Docker command peers for application tests. These do not run
//! guest argv, contact Docker, invoke a shell, or accept arbitrary filesystem work.
use std::{
    ffi::OsString,
    io::{self, Write},
    path::Path,
    time::Duration,
};
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "unexpected simulated Docker invocation",
    )
}
fn append(root: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let path = root.join(name);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 1_048_576) {
        return Err(invalid());
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(bytes)
}
fn inspect(id: &str, run: &str, scenario: &str) -> io::Result<()> {
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    println!(
        "{{\"Id\":\"{id}\",\"Config\":{{\"Labels\":{{\"io.theseus.owner\":\"v1\",\"io.theseus.run\":\"{run}\",\"io.theseus.scenario\":\"{scenario}\"}}}},\"State\":{{\"Running\":true}}}}"
    );
    Ok(())
}
fn nonce(args: &[String]) -> io::Result<&str> {
    args.last()
        .and_then(|path| path.split('/').nth(1))
        .and_then(|part| part.strip_prefix(".theseus-"))
        .ok_or_else(invalid)
}
fn tar_sample() -> io::Result<()> {
    let mut header = [0u8; 512];
    header[..10].copy_from_slice(b"bin/sample");
    for (offset, len, value) in [
        (100, 8, 0o644u64),
        (108, 8, 0),
        (116, 8, 0),
        (124, 12, 6),
        (136, 12, 0),
    ] {
        let field = format!("{:0width$o}\0", value, width = len - 1);
        header[offset..offset + len].copy_from_slice(field.as_bytes());
    }
    header[148..156].fill(b' ');
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    let sum: u32 = header.iter().map(|b| u32::from(*b)).sum();
    header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    let mut directory = header;
    directory[..100].fill(0);
    directory[..4].copy_from_slice(b"bin/");
    directory[124..136].copy_from_slice(b"00000000000\0");
    directory[156] = b'5';
    directory[148..156].fill(b' ');
    let checksum: u32 = directory.iter().map(|b| u32::from(*b)).sum();
    directory[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
    let mut out = io::stdout().lock();
    out.write_all(&directory)?;
    out.write_all(&header)?;
    let mut body = [0; 512];
    body[..6].copy_from_slice(b"sample");
    out.write_all(&body)?;
    out.write_all(&[0; 1024])?;
    out.flush()
}
pub(super) fn run(mode: &str, root: &Path, args: &[OsString]) -> io::Result<i32> {
    if args.len() > 128 || args.iter().map(|a| a.len()).sum::<usize>() > 65536 {
        return Err(invalid());
    }
    let args: Vec<String> = args
        .iter()
        .map(|a| a.to_str().map(str::to_owned).ok_or_else(invalid))
        .collect::<io::Result<_>>()?;
    let first = args.first().ok_or_else(invalid)?.as_str();
    if mode == "failure" {
        eprintln!("control failure");
        return Ok(1);
    }
    if mode == "argv" {
        append(root, "argv", format!("{}\n", args.join("\n")).as_bytes())?;
        return Ok(0);
    }
    if mode == "mcp-control" {
        append(root, "calls", format!("{}\n", args.join(" ")).as_bytes())?;
        if first == "exec" && args.get(1).map(String::as_str) == Some("--user") {
            return Ok(0);
        }
        if first == "container" && args.get(1).map(String::as_str) == Some("inspect") {
            eprintln!("Error: No such object: {}", args.last().unwrap());
            return Ok(1);
        }
        return Err(invalid());
    }
    if mode == "metadata" {
        match (
            first,
            args.get(1).map(String::as_str),
            args.get(3).map(String::as_str),
        ) {
            ("version", Some("--format"), _) => println!("29.9.2"),
            ("image", Some("inspect"), Some("{{.Id}}")) => println!("sha256:{}", "a".repeat(64)),
            ("image", Some("inspect"), Some("{{json .Config.Volumes}}")) => {
                println!("{{\"/data\":{{}}}}")
            }
            _ => return Err(invalid()),
        }
        return Ok(0);
    }
    if mode == "archive" {
        match first {
            "exec" => print!("present"),
            "cp" => {
                tar_sample()?;
                // EOF on this sole write handle proves kernel exit of the leader.
                // Keep it owned across process::exit: ordinary Drop before return
                // would only prove a scope ended, not the leader's lifetime.
                let (exit_read, _exit_write) = std::io::pipe()?;
                let _holder = std::process::Command::new(
                    std::env::var_os("THESEUS_HOST_TEST_FIXTURE").ok_or_else(invalid)?,
                )
                .arg("archive-holder")
                .arg(root.join("archive-peer-ready"))
                .stdin(std::process::Stdio::from(exit_read))
                .spawn()?;
                std::process::exit(0);
            }
            _ => return Err(invalid()),
        }
        return Ok(0);
    }
    if first == "container" {
        if mode.starts_with("wait-") {
            eprintln!("No such container:");
            return Ok(1);
        }
        let (run, scenario) = if mode.starts_with("json-") {
            ("json", "capture")
        } else if mode == "interruption" {
            ("run", "boundary")
        } else {
            return Err(invalid());
        };
        inspect(args.last().ok_or_else(invalid)?, run, scenario)?;
        return Ok(0);
    }
    if first == "rm" && mode == "interruption" {
        return Ok(0);
    }
    if first != "exec" {
        return Err(invalid());
    }
    let script = args
        .iter()
        .position(|a| a == "-c")
        .and_then(|n| args.get(n + 1))
        .ok_or_else(invalid)?;
    if mode.starts_with("wait-") {
        let waiting = if mode == "wait-prepare" {
            script.starts_with("umask")
        } else if mode == "wait-receipt" {
            script.starts_with("test -f")
        } else {
            return Err(invalid());
        };
        if waiting {
            std::fs::write(root.join("control-started"), b"ready")?;
            std::thread::sleep(Duration::from_secs(30));
        }
        return Ok(0);
    }
    if mode == "interruption" {
        if script.starts_with("umask") {
            std::thread::sleep(Duration::from_secs(3));
        }
        return Ok(0);
    }
    if script.starts_with("umask") || script.contains("command -v dd") {
        return Ok(0);
    }
    if script.contains("printf waiting") {
        print!("waiting");
        return Ok(0);
    }
    if mode == "json-valid" || mode == "json-invalid" {
        if script.starts_with("receipt=") {
            io::stdout().write_all(if mode == "json-valid" {
                b"\"\xef\xbf\xbd\""
            } else {
                b"\"\xff\""
            })?;
            return Ok(0);
        }
        if script.starts_with("test -f") {
            println!("{}:0", nonce(&args)?);
            return Ok(0);
        }
    }
    if let Some(receipt) = mode.strip_prefix("receipt-") {
        if ![
            "absent",
            "stale",
            "invalid",
            "overflow",
            "transport",
            "read-error",
        ]
        .contains(&receipt)
        {
            return Err(invalid());
        }
        if script.starts_with("receipt=") {
            println!("application stdout");
            eprintln!("application stderr");
            return Ok(i32::from(receipt == "transport"));
        }
        if script.starts_with("test -f") {
            match receipt {
                "stale" => println!("old-nonce:1"),
                "invalid" => println!("{}:999", nonce(&args)?),
                "overflow" => println!("{}:1{}", nonce(&args)?, "x".repeat(1024)),
                "read-error" => {
                    println!("{}:1", nonce(&args)?);
                    return Ok(1);
                }
                _ => {}
            }
            return Ok(0);
        }
    }
    Err(invalid())
}
