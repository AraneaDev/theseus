use std::{error::Error, fmt, io};
use theseus_native_protocol::v2::ledger::LedgerError;
pub enum StoreError {
    Io(io::Error),
    Protocol(LedgerError),
    BindingMismatch,
    MarkerMismatch,
    AliasedRoots,
    BootstrapConflict,
    RecoveredUnfinished,
    WritePoisoned,
}
impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Io(_) => "store I/O failure",
            Self::Protocol(_) => "store protocol failure",
            Self::BindingMismatch => "store binding mismatch",
            Self::MarkerMismatch => "workspace marker mismatch",
            Self::AliasedRoots => "aliased store roots",
            Self::BootstrapConflict => "bootstrap conflict",
            Self::RecoveredUnfinished => "recovered unfinished history",
            Self::WritePoisoned => "store write poisoned",
        })
    }
}
impl Error for StoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Protocol(e) => Some(e),
            _ => None,
        }
    }
}
impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<LedgerError> for StoreError {
    fn from(e: LedgerError) -> Self {
        Self::Protocol(e)
    }
}
pub enum IntentError {
    Io(io::Error),
    Conflict,
    InvalidExisting,
    Publication(io::Error),
}
impl fmt::Display for IntentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Io(_) => "intent I/O failure",
            Self::Conflict => "intent conflict",
            Self::InvalidExisting => "invalid existing intent",
            Self::Publication(_) => "intent publication failure",
        })
    }
}
impl Error for IntentError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(e) | Self::Publication(e) => Some(e),
            _ => None,
        }
    }
}
impl From<io::Error> for IntentError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl fmt::Debug for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
impl fmt::Debug for IntentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
