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
        match self.file.read(out) {
            Err(e)
                if e.raw_os_error() == Some(libc::EIO)
                    && self
                        .slave_released
                        .load(std::sync::atomic::Ordering::SeqCst) =>
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
fn modes(file: &File) -> io::Result<Vec<u64>> {
    let mut value = std::mem::MaybeUninit::<libc::termios>::uninit();
    if unsafe { libc::tcgetattr(file.as_raw_fd(), value.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let value = unsafe { value.assume_init() };
    let mut result = vec![
        value.c_iflag as u64,
        value.c_oflag as u64,
        value.c_cflag as u64,
        value.c_lflag as u64,
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
        let size = libc::winsize {
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
                std::ptr::null(),
                &size,
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
            "raw": current.get(3).is_some_and(|v| v & libc::ICANON as u64 == 0)}),
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
        let observed = self.slave.take().map(|slave| modes(&slave)).transpose();
        self.slave_released
            .store(true, std::sync::atomic::Ordering::SeqCst);
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
