//! Fixed ConPTY VT-input controls; never reads an ambient user's console.
use crate::console::ConsoleModeSnapshot;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use std::fs::{File, OpenOptions};
#[cfg(test)]
use std::io::Read;
use std::io::{self, Write};
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::time::{Duration, Instant};
use windows_sys::Win32::System::Console::*;

const PASTE: &str = "r\nq ñ😀";
#[cfg(test)]
const INPUT: &str = "\x1b[200~r\nq ñ😀\x1b[201~";
fn publish(root: &Path, name: &str, value: &serde_json::Value) -> io::Result<()> {
    let temporary = root.join(format!("{name}.pending"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    serde_json::to_writer(&mut file, value)?;
    file.flush()?;
    drop(file);
    std::fs::rename(temporary, root.join(name))
}
#[cfg(test)]
fn load(root: &Path, name: &str) -> io::Result<Option<serde_json::Value>> {
    let file = match File::open(root.join(name)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(131073).read_to_end(&mut bytes)?;
    if bytes.len() > 131072 {
        return Err(io::Error::other("VT observation bound"));
    }
    Ok(Some(serde_json::from_slice(&bytes)?))
}
fn input_mode(file: &File) -> io::Result<u32> {
    let mut mode = 0;
    // SAFETY: File retains the live console handle and mode is writable.
    if unsafe { GetConsoleMode(file.as_raw_handle(), &mut mode) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(mode)
}
struct Restore(ConsoleModeSnapshot);
impl Drop for Restore {
    fn drop(&mut self) {
        let _ = self.0.restore();
    }
}
pub(crate) fn run(root: &Path, mode: &str) -> io::Result<()> {
    if !matches!(mode, "raw-legacy" | "raw-vt" | "events" | "records") {
        return Err(io::Error::other("unknown fixed VT mode"));
    }
    let restore = Restore(ConsoleModeSnapshot::capture()?);
    let input = OpenOptions::new().read(true).write(true).open("CONIN$")?;
    let before = input_mode(&input)?;
    crossterm::terminal::enable_raw_mode()?;
    if matches!(mode, "raw-vt" | "events") {
        restore.0.enable_vt_input()?;
    }
    let active = input_mode(&input)?;
    publish(
        root,
        "ready",
        &serde_json::json!({"before":before,"active":active}),
    )?;
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut records = Vec::new();
    let mut events = Vec::new();
    let result = (|| -> io::Result<()> {
        if matches!(mode, "events" | "records") {
            let expected = if mode == "records" {
                // A queued opener ESC release must be inert before escape
                // resolution: require one literal Paste before any other keys.
                let mut queued_records = vec![(27, 0, 1, true), (27, 0, 1, false)];
                queued_records.extend(
                    "[200~r\nq\x1b[201~"
                        .encode_utf16()
                        .map(|unit| (unit, 0, 1, true)),
                );
                // Preserve the existing repeat/surrogate/native fallback controls.
                queued_records.extend([
                    (120, 0, 3, true),
                    (0xd83d, 0, 1, true),
                    (0xd83d, 0, 1, false),
                    (0xde00, 0, 1, true),
                    (0xde00, 0, 1, false),
                    (0, 38, 1, true),
                    (0, 38, 1, false),
                    (113, 0, 1, true),
                ]);
                let mut records: Vec<INPUT_RECORD> = Vec::new();
                for (unit, vkey, repeat, down) in queued_records {
                    let mut record: INPUT_RECORD = unsafe { std::mem::zeroed() };
                    record.EventType = KEY_EVENT as u16;
                    record.Event.KeyEvent = KEY_EVENT_RECORD {
                        bKeyDown: i32::from(down),
                        wRepeatCount: repeat,
                        wVirtualKeyCode: vkey,
                        wVirtualScanCode: 0,
                        uChar: KEY_EVENT_RECORD_0 { UnicodeChar: unit },
                        dwControlKeyState: 0,
                    };
                    records.push(record);
                }
                let mut written = 0;
                // SAFETY: fixed initialized array and owned private console.
                if unsafe {
                    WriteConsoleInputW(
                        input.as_raw_handle(),
                        records.as_ptr(),
                        records.len() as u32,
                        &mut written,
                    )
                } == 0
                    || written != records.len() as u32
                {
                    return Err(io::Error::last_os_error());
                }
                vec![
                    Event::Paste("r\nq".into()),
                    Event::Key(KeyCode::Char('x').into()),
                    Event::Key(KeyCode::Char('x').into()),
                    Event::Key(KeyCode::Char('x').into()),
                    Event::Key(KeyCode::Char('😀').into()),
                    Event::Key(KeyCode::Up.into()),
                    Event::Key(KeyCode::Char('q').into()),
                ]
            } else {
                vec![
                    Event::Paste(PASTE.into()),
                    Event::Key(KeyCode::Up.into()),
                    Event::Key(crossterm::event::KeyEvent::new(
                        KeyCode::Char('c'),
                        KeyModifiers::CONTROL,
                    )),
                    Event::Key(KeyCode::Esc.into()),
                ]
            };
            for (index, wanted) in expected.into_iter().enumerate() {
                loop {
                    if Instant::now() >= deadline {
                        return Err(io::Error::other("VT event deadline"));
                    }
                    if event::poll(Duration::from_millis(20))? {
                        let observed = event::read()?;
                        if matches!(
                            observed,
                            Event::Resize(..) | Event::FocusGained | Event::FocusLost
                        ) {
                            continue;
                        }
                        if matches!(&observed, Event::Key(key) if key.kind == crossterm::event::KeyEventKind::Release)
                        {
                            continue;
                        }
                        events.push(format!("{observed:?}"));
                        if observed != wanted {
                            return Err(io::Error::other(format!("wrong VT event: {observed:?}")));
                        }
                        publish(
                            root,
                            &format!("event-{index}"),
                            &serde_json::json!({"index":index}),
                        )?;
                        break;
                    }
                }
            }
        } else {
            loop {
                if Instant::now() >= deadline {
                    return Err(io::Error::other("raw VT record deadline"));
                }
                let mut pending = 0;
                // SAFETY: retained console handle and valid writable count.
                if unsafe { GetNumberOfConsoleInputEvents(input.as_raw_handle(), &mut pending) }
                    == 0
                {
                    return Err(io::Error::last_os_error());
                }
                if pending == 0 {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                let mut record: INPUT_RECORD = unsafe { std::mem::zeroed() };
                let mut count = 0;
                // SAFETY: exactly one valid record slot, this fixture is sole reader.
                if unsafe { ReadConsoleInputW(input.as_raw_handle(), &mut record, 1, &mut count) }
                    == 0
                {
                    return Err(io::Error::last_os_error());
                }
                if count != 1 {
                    return Err(io::Error::other("missing fixed input record"));
                }
                let mut done = false;
                let value = if record.EventType == KEY_EVENT as u16 {
                    // SAFETY: EventType identifies the initialized union member.
                    let key = unsafe { record.Event.KeyEvent };
                    let unit = unsafe { key.uChar.UnicodeChar };
                    done = key.bKeyDown != 0 && unit == u16::from(b'!');
                    serde_json::json!({"type":record.EventType,"down":key.bKeyDown != 0,"repeat":key.wRepeatCount,"vkey":key.wVirtualKeyCode,"scan":key.wVirtualScanCode,"unit":unit,"control":key.dwControlKeyState,"pending":pending})
                } else {
                    serde_json::json!({"type":record.EventType,"pending":pending})
                };
                records.push(value);
                if records.len() > 512 {
                    return Err(io::Error::other("fixed trace exceeded 512 records"));
                }
                if done {
                    break;
                }
            }
        }
        Ok(())
    })();
    let restored = restore.0.restore();
    let after = input_mode(&input)?;
    publish(
        root,
        "observed",
        &serde_json::json!({"mode":mode,"before":before,"active":active,"after":after,"records":records,"events":events,"error":result.as_ref().err().map(ToString::to_string)}),
    )?;
    result?;
    restored
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{NativeCommand, terminal::Session};
    fn pinned_fixture_copy(root: &Path) -> std::path::PathBuf {
        let original =
            std::env::var_os("THESEUS_HOST_TEST_FIXTURE").expect("required pinned fixture");
        // run_client retains its Theseus-only child guard. The trusted fixture
        // dispatches terminal-vt-events before its basename-dependent commands.
        let fixture = root.join("theseus.exe");
        std::fs::copy(&original, &fixture).unwrap();
        let mut original = File::open(original).unwrap();
        let mut copied = File::open(&fixture).unwrap();
        let mut remaining = original.metadata().unwrap().len();
        assert_eq!(remaining, copied.metadata().unwrap().len());
        let (mut expected, mut actual) = ([0; 8192], [0; 8192]);
        while remaining != 0 {
            let count = remaining.min(expected.len() as u64) as usize;
            original.read_exact(&mut expected[..count]).unwrap();
            copied.read_exact(&mut actual[..count]).unwrap();
            assert_eq!(&expected[..count], &actual[..count]);
            remaining -= count as u64;
        }
        fixture
    }
    fn await_file(
        session: &mut Session,
        root: &Path,
        name: &str,
        deadline: Instant,
    ) -> io::Result<serde_json::Value> {
        loop {
            if let Some(value) = load(root, name)? {
                return Ok(value);
            }
            let value = session.poll()?;
            if !value["status"].is_null() || Instant::now() >= deadline {
                return Err(io::Error::other(format!("VT phase {name}: {value}")));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn observe(mode: &str, split: Option<usize>) -> serde_json::Value {
        let root = tempfile::tempdir().unwrap();
        let fixture = pinned_fixture_copy(root.path());
        let mut command = NativeCommand::new(fixture);
        command.args([
            std::ffi::OsStr::new("terminal-vt-events"),
            root.path().as_os_str(),
            std::ffi::OsStr::new(mode),
        ]);
        let mut session = Session::fixture_control(&command, 24, 80).unwrap();
        let result = (|| -> io::Result<serde_json::Value> {
            let deadline = Instant::now() + Duration::from_secs(10);
            await_file(&mut session, root.path(), "ready", deadline)?;
            if mode == "records" {
                // The private child injects its own fixed records.
            } else if let Some(split) = split {
                session.write(&INPUT.as_bytes()[..split])?;
                // This is a real separate ConPTY pipe write, not decoder mocking.
                session.poll()?;
                session.write(&INPUT.as_bytes()[split..])?;
            } else {
                session.write(INPUT.as_bytes())?;
            }
            if mode == "events" {
                await_file(&mut session, root.path(), "event-0", deadline)?;
                session.write(b"\x1b[A")?;
                await_file(&mut session, root.path(), "event-1", deadline)?;
                session.write(b"\x03")?;
                await_file(&mut session, root.path(), "event-2", deadline)?;
                session.write(b"\x1b")?;
                await_file(&mut session, root.path(), "event-3", deadline)?;
            } else if mode != "records" {
                session.write(b"!")?;
            }
            let observed = await_file(&mut session, root.path(), "observed", deadline)?;
            loop {
                let value = session.poll()?;
                if !value["status"].is_null() {
                    if value["status"] != 0
                        || value["eof"] != true
                        || value["modes"]["before"] != value["modes"]["current"]
                    {
                        return Err(io::Error::other(format!("VT exit/restoration: {value}")));
                    }
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(io::Error::other("VT exit deadline"));
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(observed)
        })();
        println!(
            "ACTUAL ConPTY VT mode={mode} split={split:?} result={result:?} partial={:?}",
            load(root.path(), "observed")
        );
        session.finish(true).unwrap();
        let observed = result.unwrap();
        assert_eq!(observed["before"], observed["after"]);
        assert!(observed["error"].is_null());
        observed
    }
    #[test]
    fn conpty_vt_mode_preserves_fixed_paste_records_and_restores_original_mode() {
        let legacy = observe("raw-legacy", None);
        let vt = observe("raw-vt", None);
        let units = |value: &serde_json::Value| -> Vec<u16> {
            value["records"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|r| r["down"] == true)
                .filter_map(|r| r["unit"].as_u64().map(|u| u as u16))
                .collect()
        };
        assert_eq!(
            legacy["active"].as_u64().unwrap() & u64::from(ENABLE_VIRTUAL_TERMINAL_INPUT),
            0
        );
        assert_eq!(
            vt["active"].as_u64().unwrap() & u64::from(ENABLE_VIRTUAL_TERMINAL_INPUT),
            u64::from(ENABLE_VIRTUAL_TERMINAL_INPUT)
        );
        assert_eq!(
            units(&vt),
            format!("{INPUT}!").encode_utf16().collect::<Vec<_>>()
        );
        assert!(
            !units(&legacy).contains(&27),
            "legacy reader unexpectedly retained paste delimiters"
        );
        for record in vt["records"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["down"] == true)
        {
            assert_eq!(record["vkey"], 0);
            assert_eq!(record["scan"], 0);
            assert_eq!(record["control"], 0);
            assert_eq!(record["repeat"], 1);
        }
    }
    #[test]
    fn conpty_native_raw_repeats_surrogates_and_virtual_key_fallback_keep_order() {
        let observed = observe("records", None);
        assert_eq!(observed["events"].as_array().unwrap().len(), 7);
    }
    #[test]
    fn conpty_fragmented_paste_controls_and_standalone_escape_reach_actual_reader() {
        for split in (1..6).chain((INPUT.len() - 5)..INPUT.len()) {
            let observed = observe("events", Some(split));
            assert_eq!(observed["events"].as_array().unwrap().len(), 4);
        }
    }
}
