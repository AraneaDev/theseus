use super::{HostDir, invalid, platform, validate_name};
use std::{
    ffi::OsStr,
    fs::{File, TryLockError},
    io::{self, Read, Seek, SeekFrom, Write},
};

/// An exclusively locked existing regular file. The lock covers this file
/// object, not its pathname. Drop releases the file and lock together.
///
/// Trusted fixture bootstrap must create a private root/file and retain exclusive
/// namespace management. This API does not check owner, ACL or hardlink count,
/// or prevent same-principal writers or replacement with a different object.
#[derive(Debug)]
pub struct LockedLedgerFile {
    file: File,
    poisoned: bool,
    #[cfg(test)]
    fault: Option<Fault>,
}

impl HostDir {
    /// Open one existing regular file read/write without following links, and
    /// acquire a nonblocking exclusive lock through retained directory authority.
    /// Never creates or truncates. Contention returns `WouldBlock`.
    pub fn open_locked_ledger(&self, name: &OsStr) -> io::Result<LockedLedgerFile> {
        validate_name(name)?;
        let file = platform::open_ledger(&self.directory, name)?;
        match file.try_lock() {
            Ok(()) => (),
            Err(TryLockError::WouldBlock) => return Err(io::ErrorKind::WouldBlock.into()),
            Err(TryLockError::Error(error)) => return Err(error),
        }
        Ok(LockedLedgerFile {
            file,
            poisoned: false,
            #[cfg(test)]
            fault: None,
        })
    }
}
impl LockedLedgerFile {
    /// Read from the start, inspecting at most limit+1 bytes. After an append
    /// failure these bytes are diagnostic only, not evidence of a valid commit.
    pub fn read_bounded(&mut self, limit: usize) -> io::Result<Vec<u8>> {
        let bound = limit
            .checked_add(1)
            .ok_or_else(|| invalid("read bound overflow"))?;
        let bound = u64::try_from(bound).map_err(|_| invalid("read bound overflow"))?;
        self.file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut self.file)
            .take(bound)
            .read_to_end(&mut bytes)?;
        if bytes.len() > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "ledger exceeds read bound",
            ));
        }
        Ok(bytes)
    }
    /// Append within the cap and sync the file before success. Any write/sync
    /// error permanently refuses further appends on this owner, including empty
    /// appends. Reopen and validate the full ledger before interpreting it.
    /// Successful flush does not claim Windows directory power-loss persistence.
    pub fn append_and_sync(&mut self, bytes: &[u8], max_len: u64) -> io::Result<()> {
        if self.poisoned {
            return Err(io::Error::other("ledger append owner is poisoned"));
        }
        let amount = u64::try_from(bytes.len()).map_err(|_| invalid("append length overflow"))?;
        let len = self.file.metadata()?.len();
        append_length(len, amount, max_len)?;
        self.file.seek(SeekFrom::End(0))?;
        // Poison before potentially partial I/O; only a successful sync clears it.
        self.poisoned = true;
        #[cfg(test)]
        if self
            .fault
            .take_if(|fault| matches!(fault, Fault::PartialWrite))
            .is_some()
        {
            self.file.write_all(&bytes[..bytes.len().min(1)])?;
            return Err(io::Error::from_raw_os_error(5));
        }
        self.file.write_all(bytes)?;
        #[cfg(test)]
        if self
            .fault
            .take_if(|fault| matches!(fault, Fault::Sync))
            .is_some()
        {
            return Err(io::Error::from_raw_os_error(5));
        }
        self.file.sync_all()?;
        self.poisoned = false;
        Ok(())
    }
}
fn append_length(len: u64, amount: u64, max_len: u64) -> io::Result<u64> {
    len.checked_add(amount)
        .filter(|total| *total <= max_len)
        .ok_or_else(|| invalid("ledger append exceeds length bound"))
}
#[cfg(test)]
#[derive(Debug)]
enum Fault {
    PartialWrite,
    Sync,
}
#[cfg(test)]
mod tests;
