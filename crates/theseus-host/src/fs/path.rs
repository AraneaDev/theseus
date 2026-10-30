use std::io;

/// A lexical report reference made of nonempty native components separated by `/`.
///
/// Validation does not open a file or authorize traversal through links.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelativePath {
    components: Vec<String>,
}

impl RelativePath {
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
    if component.ends_with(['.', ' '])
        || component
            .chars()
            .any(|c| c <= '\u{1f}' || matches!(c, ':' | '\\' | '<' | '>' | '"' | '|' | '?' | '*'))
    {
        return false;
    }
    let stem = component
        .split('.')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return false;
    }
    !["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    })
}
