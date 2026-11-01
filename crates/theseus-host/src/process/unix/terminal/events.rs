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
fn wait_phase(root: &Path, name: &str, expected: &[u8], deadline: Instant) -> io::Result<()> {
    while !complete(&root.join(name), expected)? {
        if Instant::now() >= deadline {
            return Err(io::Error::other(format!(
                "terminal event phase deadline: {name}"
            )));
        }
        // Poll a complete owned record, never use elapsed sleep as readiness.
        std::thread::sleep(Duration::from_millis(2));
    }
    Ok(())
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
    let mut resize = false;
    let mut keys = 0;
    let mut arrows = 0;
    let mut partial_consumed = false;
    let mut phase = "initialize";
    let result = (|| -> io::Result<()> {
        enable_raw_mode()?;
        // Initialize the SAME locked source before queuing resize plus one key.
        let _ = event::poll(Duration::ZERO)?;
        publish(root, "ready", b"terminal-events-ready\n")?;
        phase = "release";
        wait_phase(
            root,
            "release",
            b"terminal-events-release\n",
            Instant::now() + Duration::from_secs(8),
        )?;
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut announced = false;
        let mut streaming = false;
        phase = "read";
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
                if keys > expected_keys || arrows > expected_arrows {
                    return Err(io::Error::other("excess fixed terminal events"));
                }
            }
            if matches!(mode, "burst" | "split") && resize && keys == 1 && !streaming {
                // The reader is now running. The producer can stream the unchanged
                // remainder without requiring it to fit in the native input queue.
                publish(root, "stream", b"terminal-events-stream\n")?;
                streaming = true;
                phase = "stream";
            }
            if mode == "split" && keys == 1023 && !partial_consumed {
                if event::poll(Duration::ZERO)? {
                    return Err(io::Error::other("unexpected input before partial CSI"));
                }
                publish(root, "keys-drained", b"terminal-events-keys-drained\n")?;
                phase = "partial-sent";
                wait_phase(
                    root,
                    "partial-sent",
                    b"terminal-events-partial-sent\n",
                    deadline,
                )?;
                // The producer has queued ONLY ESC[, and cannot send A until the
                // acknowledgement below. Observe actual consumption across reads;
                // payload position alone says nothing about PTY read boundaries.
                let queued = rustix::io::ioctl_fionread(std::io::stdin().lock())?;
                if queued != 2 {
                    return Err(io::Error::other("partial CSI was not the two queued bytes"));
                }
                if event::poll(Duration::ZERO)? {
                    return Err(io::Error::other("incomplete CSI produced an event"));
                }
                if rustix::io::ioctl_fionread(std::io::stdin().lock())? != 0 {
                    return Err(io::Error::other("partial CSI was not consumed"));
                }
                partial_consumed = true;
                publish(
                    root,
                    "partial-consumed",
                    b"terminal-events-partial-consumed\n",
                )?;
                phase = "completion";
            }
            if mode == "later" && resize && keys == 1 && !announced {
                if event::poll(Duration::ZERO)? {
                    return Err(io::Error::other(
                        "unexpected pending input before later arrival",
                    ));
                }
                publish(root, "drained", b"terminal-events-drained\n")?;
                announced = true;
                phase = "later";
            }
        }
        if !resize
            || keys != expected_keys
            || arrows != expected_arrows
            || (mode == "split" && !partial_consumed)
        {
            return Err(io::Error::other("required fixed terminal event missing"));
        }
        phase = "complete";
        Ok(())
    })();
    // Preserve partial/error observations BEFORE raw-mode fallback or forced
    // parent cleanup. Complete records never turn a failure into success.
    let record = serde_json::json!({"resize":resize,"q":keys,"up":arrows,"partial_consumed":partial_consumed,"phase":phase,
        "error":result.as_ref().err().map(|error| error.to_string().chars().take(256).collect::<String>())});
    let published = publish(root, "observed", &serde_json::to_vec(&record)?);
    let restored = disable_raw_mode();
    result?;
    published?;
    restored
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{NativeCommand, terminal::Session};
    fn await_phase(
        session: &mut Session,
        root: &Path,
        name: &str,
        expected: &[u8],
        deadline: Instant,
    ) -> io::Result<()> {
        while !complete(&root.join(name), expected)? {
            let value = session.poll()?;
            if !value["status"].is_null() || Instant::now() >= deadline {
                return Err(io::Error::other(format!(
                    "native event phase {name} missing: {value}"
                )));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    }
    fn observation(root: &Path) -> io::Result<serde_json::Value> {
        let mut bytes = Vec::new();
        std::fs::File::open(root.join("observed"))?
            .take(4097)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 4096 {
            return Err(io::Error::other("oversized event observation"));
        }
        Ok(serde_json::from_slice(&bytes)?)
    }
    fn observe(mode: &str, input: &[u8], keys: usize, arrows: usize) {
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
        let result = (|| -> io::Result<serde_json::Value> {
            let deadline = Instant::now() + Duration::from_secs(8);
            await_phase(
                &mut session,
                directory.path(),
                "ready",
                b"terminal-events-ready\n",
                deadline,
            )?;
            session.resize(28, 82)?;
            // Keep the original resize+single-q readiness race; stream only after
            // the child has consumed this bounded prefix and reported progress.
            session.write(&input[..1])?;
            publish(directory.path(), "release", b"terminal-events-release\n")?;
            if matches!(mode, "burst" | "split") {
                await_phase(
                    &mut session,
                    directory.path(),
                    "stream",
                    b"terminal-events-stream\n",
                    deadline,
                )?;
                if mode == "burst" {
                    session.write(&input[1..])?;
                } else {
                    session.write(&input[1..1023])?;
                    await_phase(
                        &mut session,
                        directory.path(),
                        "keys-drained",
                        b"terminal-events-keys-drained\n",
                        deadline,
                    )?;
                    session.write(&input[1023..1025])?;
                    publish(
                        directory.path(),
                        "partial-sent",
                        b"terminal-events-partial-sent\n",
                    )?;
                    await_phase(
                        &mut session,
                        directory.path(),
                        "partial-consumed",
                        b"terminal-events-partial-consumed\n",
                        deadline,
                    )?;
                    session.write(&input[1025..])?;
                }
            }
            if mode == "later" {
                await_phase(
                    &mut session,
                    directory.path(),
                    "drained",
                    b"terminal-events-drained\n",
                    deadline,
                )?;
                session.write(b"q")?;
            }
            loop {
                let value = session.poll()?;
                if !value["status"].is_null() {
                    break Ok(value);
                }
                if Instant::now() >= deadline {
                    return Err(io::Error::other("native event exit deadline"));
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        })();
        let observed = observation(directory.path());
        let before_cleanup = session.poll();
        println!(
            "ACTUAL native resize+single-q mode={mode} observation={observed:?} result={result:?} before_forced_cleanup={before_cleanup:?}"
        );
        let cleanup = session.finish(true);
        cleanup.unwrap();
        let value = result.unwrap();
        let observed = observed.unwrap();
        assert_eq!(observed["resize"], true);
        assert_eq!(observed["q"], keys);
        assert_eq!(observed["up"], arrows);
        assert_eq!(observed["phase"], "complete");
        assert!(observed["error"].is_null());
        assert_eq!(observed["partial_consumed"], mode == "split");
        assert_eq!(value["status"], 0);
        assert_eq!(value["eof"], true);
        assert_eq!(value["modes"]["before"], value["modes"]["current"]);
    }
    #[test]
    fn pty_resize_and_single_q_both_reach_native_event_reader() {
        observe("single", b"q", 1, 0);
    }
    #[test]
    fn pty_resize_and_single_q_control_also_drains_multiple_read_chunks() {
        observe("burst", &vec![b'q'; 2051], 2051, 0);
    }
    #[test]
    fn pty_resize_and_single_q_control_preserves_split_escape_parser() {
        let mut bytes = vec![b'q'; 1023];
        bytes.extend_from_slice(b"\x1b[A");
        observe("split", &bytes, 1023, 1);
    }
    #[test]
    fn pty_resize_and_single_q_control_accepts_new_input_after_drain() {
        observe("later", b"q", 2, 0);
    }
    #[test]
    fn event_phase_gate_rejects_partial_wrong_and_oversized_records() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("phase");
        assert!(!complete(&path, b"complete\n").unwrap());
        for bytes in [b"".as_slice(), b"comp", b"wrong\n", &[b'x'; 66]] {
            std::fs::write(&path, bytes).unwrap();
            assert!(complete(&path, b"complete\n").is_err());
        }
        std::fs::write(&path, b"complete\n").unwrap();
        assert!(complete(&path, b"complete\n").unwrap());
    }
}
