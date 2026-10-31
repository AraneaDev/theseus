//! Host recovery-command presentation only; execution uses native argument arrays.
pub(crate) fn path_argument(path: &std::path::Path) -> String {
    let text = path.to_string_lossy();
    if !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/_-.".contains(&b))
    {
        text.into_owned()
    } else {
        #[cfg(unix)]
        {
            format!("'{}'", text.replace('\'', "'\"'\"'"))
        }
        #[cfg(windows)]
        {
            format!("'{}'", text.replace('\'', "''"))
        }
    }
}
