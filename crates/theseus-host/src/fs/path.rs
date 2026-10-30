use std::io;

/// A lexical report reference made of nonempty native components separated by `/`.
///
/// Validation does not open a file or authorize traversal through links.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelativePath {
    components: Vec<String>,
}

impl RelativePath {
    pub(super) fn components(&self) -> &[String] {
        &self.components
    }

    pub fn parse(text: &str) -> io::Result<Self> {
        let mut components = Vec::new();
        for component in text.split('/') {
            if component.is_empty()
                || matches!(component, "." | "..")
                || component.contains('\0')
                || !native_component(component)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid report path component",
                ));
            }
            components.push(component.to_owned());
        }
        Ok(Self { components })
    }
}

#[cfg(not(windows))]
fn native_component(_: &str) -> bool {
    true
}

#[cfg(windows)]
fn native_component(component: &str) -> bool {
    super::windows::validate_native_name(std::ffi::OsStr::new(component)).is_ok()
}
