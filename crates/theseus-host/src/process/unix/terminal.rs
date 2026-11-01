use super::OwnedChild;
use crate::process::NativeCommand;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::process::ExitStatus;
mod events;
pub(crate) use events::run as run_event_fixture;

pub(crate) struct Reader {
    file: File,
    slave_released: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl Read for Reader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        Self::classify_read(self.file.read(out), &self.slave_released)
    }
}
impl Reader {
    fn classify_read(
        result: io::Result<usize>,
        released: &std::sync::atomic::AtomicBool,
    ) -> io::Result<usize> {
        match result {
            Err(e)
                if e.raw_os_error() == Some(libc::EIO)
                    && released.load(std::sync::atomic::Ordering::SeqCst) =>
            {
                Ok(0)
            }
            result => result,
        }
    }
}
pub(crate) struct Terminal {
    child: Option<OwnedChild>,
    master: File,
    slave: Option<File>,
    before: Vec<u64>,
    after: Option<Vec<u64>>,
    slave_released: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
// Apple stores termios flags in u64; Linux uses u32. Preserve every bit
// without a same-width cast/conversion that native strict clippy rejects.
fn mode_bits(value: libc::tcflag_t) -> u64 {
    #[cfg(target_os = "macos")]
    {
        value
    }
    #[cfg(not(target_os = "macos"))]
    {
        u64::from(value)
    }
}
fn modes(file: &File, stage: &str) -> io::Result<Vec<u64>> {
    let mut value = std::mem::MaybeUninit::<libc::termios>::uninit();
    if unsafe { libc::tcgetattr(file.as_raw_fd(), value.as_mut_ptr()) } == -1 {
        let error = io::Error::last_os_error();
        return Err(io::Error::new(
            error.kind(),
            format!(
                "{stage} termios on owned PTY master fd {}: {error}",
                file.as_raw_fd()
            ),
        ));
    }
    let value = unsafe { value.assume_init() };
    let mut result = vec![
        mode_bits(value.c_iflag),
        mode_bits(value.c_oflag),
        mode_bits(value.c_cflag),
        mode_bits(value.c_lflag),
    ];
    result.extend(value.c_cc.iter().map(|v| u64::from(*v)));
    Ok(result)
}
impl Terminal {
    pub(crate) fn spawn(
        command: &NativeCommand,
        rows: u16,
        columns: u16,
        profile: crate::process::terminal::ConsoleProfile,
    ) -> Result<(Self, Reader), Box<dyn std::error::Error>> {
        if profile != crate::process::terminal::ConsoleProfile::Default {
            return Err(io::Error::other("console profiles require Windows").into());
        }
        let mut master = -1;
        let mut slave = -1;
        let mut size = libc::winsize {
            ws_row: rows,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::addr_of_mut!(size),
            )
        } == -1
        {
            return Err(io::Error::last_os_error().into());
        }
        let master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        for file in [&master, &slave] {
            rustix::io::fcntl_setfd(file, rustix::io::FdFlags::CLOEXEC)?;
        }
        let flags = rustix::fs::fcntl_getfl(&master)?;
        rustix::fs::fcntl_setfl(&master, flags | rustix::fs::OFlags::NONBLOCK)?;
        let before = modes(&master, "initial")?;
        let child = OwnedChild::spawn_files_with_session(
            command,
            Some(slave.try_clone()?),
            Some(slave.try_clone()?),
            Some(slave.try_clone()?),
            true,
        )?;
        let slave_released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = Reader {
            file: master.try_clone()?,
            slave_released: slave_released.clone(),
        };
        Ok((
            Self {
                child: Some(child),
                master,
                slave: Some(slave),
                before,
                after: None,
                slave_released,
            },
            reader,
        ))
    }
    pub(crate) fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.master.write(bytes)
    }
    pub(crate) fn resize(&mut self, rows: u16, columns: u16) -> io::Result<()> {
        let size = libc::winsize {
            ws_row: rows,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    pub(crate) fn modes(&self) -> io::Result<serde_json::Value> {
        let current = match &self.slave {
            Some(_) => modes(&self.master, "live")?,
            None => self
                .after
                .clone()
                .ok_or_else(|| io::Error::other("final PTY master modes were not captured"))?,
        };
        Ok(
            serde_json::json!({"kind":"termios", "before":self.before, "current":current,
            "raw": current.get(3).is_some_and(|v| v & mode_bits(libc::ICANON) == 0)}),
        )
    }
    pub(crate) fn exited(&mut self) -> io::Result<bool> {
        self.child
            .as_mut()
            .map_or(Ok(true), OwnedChild::exited_reserved)
    }
    pub(crate) fn signal(&mut self, event: &str) -> io::Result<()> {
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| io::Error::other("terminal child released"))?;
        if child.exited_reserved()? {
            return Err(io::Error::other("terminal child already exited"));
        }
        let signal = match event {
            "interrupt" => rustix::process::Signal::INT,
            "terminate" => rustix::process::Signal::TERM,
            _ => return Err(io::Error::other("invalid terminal event")),
        };
        rustix::process::kill_process_group(child.group.unwrap(), signal)?;
        Ok(())
    }
    pub(crate) fn close(&mut self, terminate: bool) -> io::Result<ExitStatus> {
        let observed = release_slave(
            self.slave.take(),
            &self.master,
            &self.slave_released,
            #[cfg(test)]
            || {},
        );
        let mut child = self
            .child
            .take()
            .ok_or_else(|| io::Error::other("terminal child released"))?;
        let termination = if terminate {
            child.terminate_owned()
        } else {
            Ok(())
        };
        let result = child.reap_and_release();
        self.after = observed?;
        termination?;
        result
    }
}

// The test callback observes the exact interval after the last slave closes.
fn release_slave(
    slave: Option<File>,
    observer: &File,
    released: &std::sync::atomic::AtomicBool,
    #[cfg(test)] after_close: impl FnOnce(),
) -> io::Result<Option<Vec<u64>>> {
    let observed = slave.as_ref().map(|_| modes(observer, "final")).transpose();
    // Publish while we still own the slave: the reader can only see EIO
    // from this final close after the release transition is visible.
    released.store(true, std::sync::atomic::Ordering::SeqCst);
    drop(slave);
    #[cfg(test)]
    after_close();
    observed
}

// Fixed mechanism fixture, separate from the Theseus-only bridge protocol.
// Only the child mutates its own attached terminal; the observer never does.
pub(crate) fn run_mode_fixture(restore: bool) -> io::Result<()> {
    let mut original = std::mem::MaybeUninit::<libc::termios>::uninit();
    if unsafe { libc::tcgetattr(0, original.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let original = unsafe { original.assume_init() };
    let mut changed = original;
    changed.c_lflag ^= libc::ECHO;
    if unsafe { libc::tcsetattr(0, libc::TCSANOW, &changed) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let mut output = io::stdout().lock();
    output.write_all(b"terminal-mode-ready\n")?;
    output.flush()?;
    let mut release = [0; 2];
    io::stdin().read_exact(&mut release)?;
    if release != *b"!\n" {
        return Err(io::Error::other("invalid fixed terminal mode release"));
    }
    if restore && unsafe { libc::tcsetattr(0, libc::TCSANOW, &original) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn observe_session_leader_exit(restore: bool) {
        use crate::process::terminal::Session;
        let fixture = std::env::var_os("THESEUS_HOST_TEST_FIXTURE")
            .expect("required Cargo-built native fixture");
        let mut command = NativeCommand::new(fixture);
        command.args([
            "terminal-mode-exit",
            if restore { "restore" } else { "leave" },
        ]);
        let mut session = Session::spawn(&command, 24, 80).unwrap();
        let observation = (|| -> Result<_, Box<dyn std::error::Error>> {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
            let mut bytes = Vec::new();
            let live = loop {
                let value = session.poll()?;
                bytes.extend(serde_json::from_value::<Vec<u8>>(value["bytes"].clone())?);
                if bytes == b"terminal-mode-ready\r\n" {
                    break value;
                }
                if bytes.len() > 256
                    || !value["status"].is_null()
                    || std::time::Instant::now() >= deadline
                {
                    return Err(io::Error::other(format!(
                        "complete fixed mode readiness missing: {bytes:?}; {value}"
                    ))
                    .into());
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            };
            let before: Vec<u64> = serde_json::from_value(live["modes"]["before"].clone())?;
            let changed: Vec<u64> = serde_json::from_value(live["modes"]["current"].clone())?;
            session.write(b"!\n")?;
            let final_value = loop {
                let value = session.poll()?;
                if !value["status"].is_null() {
                    break value;
                }
                if std::time::Instant::now() >= deadline {
                    return Err(io::Error::other("mode fixture did not exit").into());
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            };
            Ok((before, changed, final_value))
        })();
        // The common owner always closes its parent slave, drains EOF and joins
        // its reader, even when a readiness/deadline/observation assertion fails.
        let cleanup = session.finish(true);
        let (before, changed, final_value) = observation.unwrap();
        cleanup.unwrap();
        let mut expected_changed = before.clone();
        expected_changed[3] ^= mode_bits(libc::ECHO);
        assert_eq!(changed, expected_changed, "actual live termios mutation");
        let expected = if restore { &before } else { &changed };
        assert_eq!(
            &serde_json::from_value::<Vec<u64>>(final_value["modes"]["current"].clone()).unwrap(),
            expected
        );
        assert_eq!(final_value["status"], 0);
        assert_eq!(final_value["eof"], true);
        println!(
            "native PTY restore={restore} initial={before:?} live={changed:?} final={} reserved_leader_exit=true EOF=true reader_joined=true",
            final_value["modes"]["current"]
        );
    }

    #[test]
    fn pty_master_preserves_altered_modes_after_session_leader_exit() {
        observe_session_leader_exit(false);
    }
    #[test]
    fn pty_master_observes_restored_modes_after_session_leader_exit() {
        observe_session_leader_exit(true);
    }

    #[test]
    fn slave_release_publishes_before_reader_can_observe_last_close() {
        let mut master = -1;
        let mut slave = -1;
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        let master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        let released = std::sync::Arc::new(AtomicBool::new(false));
        let mut reader = Reader {
            file: master.try_clone().unwrap(),
            slave_released: released.clone(),
        };
        let observed = release_slave(Some(slave), &master, &released, || {
            // Controlled interleaving: execute the independent reader immediately
            // after the final close, before the closer's next statement.
            assert!(
                released.load(Ordering::SeqCst),
                "last slave closed before release publication"
            );
            assert_eq!(reader.read(&mut [0; 1]).unwrap(), 0);
        })
        .unwrap();
        assert!(observed.is_some());
    }

    #[test]
    fn slave_release_does_not_classify_pre_cleanup_eio_as_eof() {
        let released = AtomicBool::new(false);
        let error = Reader::classify_read(Err(io::Error::from_raw_os_error(libc::EIO)), &released)
            .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EIO));
        released.store(true, Ordering::SeqCst);
        assert_eq!(
            Reader::classify_read(Err(io::Error::from_raw_os_error(libc::EIO)), &released).unwrap(),
            0
        );
        assert_eq!(
            Reader::classify_read(Err(io::Error::from_raw_os_error(libc::EBADF)), &released)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EBADF)
        );
    }
}
