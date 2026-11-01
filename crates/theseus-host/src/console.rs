//! Exact Windows console-mode ownership; application code chooses restoration order.
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::io::AsRawHandle;
use windows_sys::Win32::System::Console::{GetConsoleMode, SetConsoleMode};

pub struct ConsoleModeSnapshot {
    input: File,
    output: File,
    input_mode: u32,
    output_mode: u32,
}
fn mode(file: &File, stage: &str) -> io::Result<u32> {
    let mut value = 0;
    // SAFETY: File owns a live console handle and value is valid writable storage.
    if unsafe { GetConsoleMode(file.as_raw_handle(), &mut value) } == 0 {
        return Err(io::Error::other(format!(
            "{stage}: {}",
            io::Error::last_os_error()
        )));
    }
    Ok(value)
}
fn set(file: &File, value: u32, stage: &str) -> io::Result<()> {
    // SAFETY: File retains the buffer captured before any terminal mutation.
    if unsafe { SetConsoleMode(file.as_raw_handle(), value) } == 0 {
        return Err(io::Error::other(format!(
            "{stage}: {}",
            io::Error::last_os_error()
        )));
    }
    Ok(())
}
impl ConsoleModeSnapshot {
    pub fn capture() -> io::Result<Self> {
        let input = OpenOptions::new().read(true).write(true).open("CONIN$")?;
        let output = OpenOptions::new().read(true).write(true).open("CONOUT$")?;
        let input_mode = mode(&input, "capture console input mode")?;
        let output_mode = mode(&output, "capture active console output mode")?;
        Ok(Self {
            input,
            output,
            input_mode,
            output_mode,
        })
    }
    /// Call after leaving the alternate screen and all other terminal writes.
    /// Both buffers are attempted even when the first restoration fails.
    pub fn restore(&self) -> io::Result<()> {
        let output = set(
            &self.output,
            self.output_mode,
            "restore console output mode",
        );
        let input = set(&self.input, self.input_mode, "restore console input mode");
        match (output, input) {
            (Err(output), Err(input)) => Err(io::Error::other(format!("{output}; {input}"))),
            (Err(error), _) | (_, Err(error)) => Err(error),
            _ => Ok(()),
        }
    }
}
