//! Fixed peers for application duplex policy tests; no host command execution.
use std::io::{self, BufRead, Read, Write};
use std::time::Duration;

fn eof() -> io::Result<()> {
    let mut byte = [0];
    if io::stdin().read(&mut byte)? != 0 {
        return Err(io::Error::other("unexpected input after protocol frame"));
    }
    Ok(())
}
fn line(expected: &str) -> io::Result<()> {
    let mut line = String::new();
    io::stdin().lock().take(4097).read_line(&mut line)?;
    if line != expected {
        return Err(io::Error::other("unexpected protocol frame"));
    }
    Ok(())
}
fn out(bytes: &[u8]) -> io::Result<()> {
    io::stdout().write_all(bytes)?;
    io::stdout().flush()
}
pub(super) fn run(case: &str) -> io::Result<()> {
    match case {
        "mcp-modern" => {
            let mut request = String::new();
            io::stdin().lock().take(4097).read_line(&mut request)?;
            if !request.ends_with('\n')
                || request.len() > 4096
                || !request.contains("\"method\":\"server/discover\"")
                || !request.contains("\"id\":\"theseus-discover-1\"")
            {
                return Err(io::Error::other("unexpected fixed MCP discovery"));
            }
            out(br#"{"jsonrpc":"2.0","id":"theseus-discover-1","result":{"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"public","_meta":{"io.modelcontextprotocol/serverInfo":{"name":"fixture","version":"2"}}}}
"#)?;
            request.clear();
            io::stdin().lock().take(4097).read_line(&mut request)?;
            if !request.ends_with('\n')
                || request.len() > 4096
                || !request.contains("\"method\":\"tools/list\"")
                || !request.contains("\"id\":1")
            {
                return Err(io::Error::other("unexpected fixed MCP tools request"));
            }
            out(br#"{"jsonrpc":"2.0","id":1,"result":{"resultType":"complete","tools":[{"name":"hello","inputSchema":{"type":"object"}}],"ttlMs":0,"cacheScope":"public"}}
"#)?;
            eof()?;
        }
        "ping" => {
            line("PING\n")?;
            out(b"PONG\n")?;
            eof()?;
            io::stderr().write_all(b"diagnostic\n")?;
            std::process::exit(17);
        }
        "reply-hold" => {
            out(b"PONG\n")?;
            std::thread::sleep(Duration::from_secs(30));
        }
        "reply-delay-eof" => {
            out(b"PONG\n")?;
            std::thread::sleep(Duration::from_millis(400));
            io::copy(&mut io::stdin().take(1_048_577), &mut io::sink())?;
            std::process::exit(17);
        }
        "blocked-read" => {
            std::thread::sleep(Duration::from_millis(400));
            io::copy(&mut io::stdin().take(2_097_153), &mut io::sink())?;
            out(b"done")?;
        }
        "notify" => {
            line("stop\n")?;
            eof()?;
            std::process::exit(7);
        }
        "output" | "output-hold" => {
            io::stderr().write_all(b"err")?;
            io::stderr().flush()?;
            out(b"out")?;
            if case == "output-hold" {
                std::thread::sleep(Duration::from_secs(30));
            }
        }
        "stdout-eof-hold" => {
            // This isolated fixture owns its standard output and never uses it
            // after closing. Stderr deliberately remains open until termination.
            #[cfg(unix)]
            {
                use std::os::fd::FromRawFd;
                unsafe {
                    drop(std::os::fd::OwnedFd::from_raw_fd(1));
                }
            }
            #[cfg(windows)]
            {
                use std::os::windows::io::AsRawHandle;
                if unsafe {
                    windows_sys::Win32::Foundation::CloseHandle(io::stdout().as_raw_handle())
                } == 0
                {
                    return Err(io::Error::last_os_error());
                }
            }
            std::thread::sleep(Duration::from_secs(30));
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unknown duplex fixture",
            ));
        }
    }
    Ok(())
}
