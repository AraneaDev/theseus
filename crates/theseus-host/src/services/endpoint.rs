use std::io;
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
/// Closed filesystem socket endpoint, never a raw D-Bus address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnixPath(String);
impl UnixPath {
    pub fn new(path: String) -> io::Result<Self> {
        if path.len() > 100
            || !path.starts_with('/')
            || path.len() < 2
            || !path
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"/._-".contains(&c))
            || path[1..]
                .split('/')
                .any(|c| c.is_empty() || c == "." || c == "..")
        {
            return Err(invalid("invalid closed UnixPath endpoint"));
        }
        Ok(Self(path))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn address(&self) -> String {
        format!("unix:path={}", self.0)
    }
}
