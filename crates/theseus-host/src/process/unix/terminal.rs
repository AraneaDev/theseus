use super::OwnedChild;
use crate::process::NativeCommand;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::process::ExitStatus;

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
fn modes(file: &File) -> io::Result<Vec<u64>> {
    let mut value = std::mem::MaybeUninit::<libc::termios>::uninit();
    if unsafe { libc::tcgetattr(file.as_raw_fd(), value.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
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
    ) -> Result<(Self, Reader), Box<dyn std::error::Error>> {
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
        let before = modes(&slave)?;
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
            Some(slave) => modes(slave)?,
            None => self.after.clone().unwrap_or_default(),
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
    released: &std::sync::atomic::AtomicBool,
    #[cfg(test)] after_close: impl FnOnce(),
) -> io::Result<Option<Vec<u64>>> {
    let observed = slave.as_ref().map(modes).transpose();
    // Publish while we still own the slave: the reader can only see EIO
    // from this final close after the release transition is visible.
    released.store(true, std::sync::atomic::Ordering::SeqCst);
    drop(slave);
    #[cfg(test)]
    after_close();
    observed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

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
            file: master,
            slave_released: released.clone(),
        };
        let observed = release_slave(Some(slave), &released, || {
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
