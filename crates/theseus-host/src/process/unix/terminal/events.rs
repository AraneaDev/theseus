//! Fixed native control for one resize and one queued key; no bridge operation.
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

fn publish(root: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let temporary = root.join(format!("{name}.pending"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.flush()?;
    drop(file);
    std::fs::rename(temporary, root.join(name))
}
fn complete(path: &Path, expected: &[u8]) -> io::Result<bool> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(65).read_to_end(&mut bytes)?;
    if bytes != expected {
        return Err(io::Error::other("invalid fixed terminal event gate"));
    }
    Ok(true)
}
pub(crate) fn run(root: &Path, mode: &str) -> io::Result<()> {
    let (expected_keys, expected_arrows) = match mode {
        "single" => (1, 0),
        "later" => (2, 0),
        "burst" => (2051, 0),
        "split" => (1023, 1),
        _ => return Err(io::Error::other("unknown fixed event control")),
    };
    use crossterm::{
        event::{self, Event, KeyCode},
        terminal::{disable_raw_mode, enable_raw_mode},
    };
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = disable_raw_mode();
        }
    }
    let _restore = Restore;
    enable_raw_mode()?;
    // Initialize the SAME locked event source before the controller queues both
    // readiness sources. The release file does not consume console input.
    let _ = event::poll(Duration::ZERO)?;
    publish(root, "ready", b"terminal-events-ready\n")?;
    let deadline = Instant::now() + Duration::from_secs(8);
    while !complete(&root.join("release"), b"terminal-events-release\n")? {
        if Instant::now() >= deadline {
            return Err(io::Error::other("terminal event release deadline"));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut resize = false;
    let mut keys = 0;
    let mut arrows = 0;
    let mut announced = false;
    while Instant::now() < deadline
        && !(resize && keys == expected_keys && arrows == expected_arrows)
    {
        if event::poll(deadline.saturating_duration_since(Instant::now()))? {
            match event::read()? {
                Event::Resize(82, 28) => resize = true,
                Event::Key(key) if key.code == KeyCode::Char('q') => keys += 1,
                Event::Key(key) if key.code == KeyCode::Up => arrows += 1,
                _ => return Err(io::Error::other("unexpected fixed terminal event")),
            }
        }
        if mode == "later" && resize && keys == 1 && !announced {
            if event::poll(Duration::ZERO)? {
                return Err(io::Error::other(
                    "unexpected pending input before later arrival",
                ));
            }
            publish(root, "drained", b"terminal-events-drained\n")?;
            announced = true;
        }
    }
    // Complete actual observations survive even when the expected event is lost.
    publish(
        root,
        "observed",
        format!("resize={resize} q={keys} up={arrows}\n").as_bytes(),
    )?;
    disable_raw_mode()?;
    if !resize || keys != expected_keys || arrows != expected_arrows {
        return Err(io::Error::other("resize or single q event missing"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{NativeCommand, terminal::Session};
    fn observe(mode: &str, input: &[u8], expected: &str) {
        let directory = tempfile::tempdir().unwrap();
        let fixture =
            std::env::var_os("THESEUS_HOST_TEST_FIXTURE").expect("required pinned fixture");
        let mut command = NativeCommand::new(fixture);
        command.args([
            std::ffi::OsStr::new("terminal-events"),
            directory.path().as_os_str(),
            std::ffi::OsStr::new(mode),
        ]);
        let mut session = Session::spawn(&command, 36, 120).unwrap();
        let observation = (|| -> Result<_, Box<dyn std::error::Error>> {
            let deadline = Instant::now() + Duration::from_secs(8);
            while !complete(&directory.path().join("ready"), b"terminal-events-ready\n")? {
                let value = session.poll()?;
                if !value["status"].is_null() || Instant::now() >= deadline {
                    return Err(io::Error::other(format!(
                        "native event readiness missing: {value}"
                    ))
                    .into());
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            session.resize(28, 82)?;
            session.write(input)?;
            publish(directory.path(), "release", b"terminal-events-release\n")?;
            if mode == "later" {
                while !complete(
                    &directory.path().join("drained"),
                    b"terminal-events-drained\n",
                )? {
                    let value = session.poll()?;
                    if !value["status"].is_null() || Instant::now() >= deadline {
                        return Err(io::Error::other("initial readiness was not drained").into());
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                session.write(b"q")?;
            }
            loop {
                let value = session.poll()?;
                if !value["status"].is_null() {
                    break Ok(value);
                }
                if Instant::now() >= deadline {
                    return Err(io::Error::other("native event exit deadline").into());
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        })();
        let cleanup = session.finish(true);
        let observed = std::fs::read_to_string(directory.path().join("observed"));
        println!("ACTUAL native resize+single-q observation={observed:?} result={observation:?}");
        cleanup.unwrap();
        let value = observation.unwrap();
        assert_eq!(observed.unwrap(), expected);
        assert_eq!(value["status"], 0);
        assert_eq!(value["eof"], true);
        assert_eq!(value["modes"]["before"], value["modes"]["current"]);
    }
    #[test]
    fn pty_resize_and_single_q_both_reach_native_event_reader() {
        observe("single", b"q", "resize=true q=1 up=0\n");
    }
    #[test]
    fn pty_resize_and_single_q_control_also_drains_multiple_read_chunks() {
        observe("burst", &vec![b'q'; 2051], "resize=true q=2051 up=0\n");
    }
    #[test]
    fn pty_resize_and_single_q_control_preserves_split_escape_parser() {
        let mut bytes = vec![b'q'; 1023];
        bytes.extend_from_slice(b"\x1b[A");
        observe("split", &bytes, "resize=true q=1023 up=1\n");
    }

    #[test]
    fn pty_resize_and_single_q_control_accepts_new_input_after_drain() {
        observe("later", b"q", "resize=true q=2 up=0\n");
    }
}
