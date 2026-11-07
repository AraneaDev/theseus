//! Error formatting is deliberately independent of retained native sources.
pub enum InvocationError {
    Io(std::io::Error),
    InvalidRoots,
    InvalidCatalog,
    WrongOs,
}
pub enum RuntimeError {
    InvalidGrant,
    Protocol(theseus_native_protocol::v2::Error),
    Transport(std::io::Error),
    Worker,
    Capacity,
    InvalidState,
}
macro_rules! bounded_error {
    ($t:ty, $s:literal) => {
        impl std::fmt::Display for $t {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str($s)
            }
        }
        impl std::fmt::Debug for $t {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Display::fmt(self, f)
            }
        }
    };
}
bounded_error!(InvocationError, "invalid helper invocation");
bounded_error!(RuntimeError, "helper runtime failure");

impl std::error::Error for InvocationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}
impl std::error::Error for RuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Protocol(error) => Some(error),
            Self::Transport(error) => Some(error),
            _ => None,
        }
    }
}
