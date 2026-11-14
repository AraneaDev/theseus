//! `y` copies the selected evidence record's most useful text to the system
//! clipboard with an OSC 52 sequence. The payload is base64, so stored text can
//! never reach the terminal as raw control bytes.
use super::{App, evidence::Record, paging::DETAIL_BYTES};

/// Largest copied text, before base64 encoding.
pub(super) const MAX_COPY: usize = 64 * 1024;

/// Standard base64 (RFC 4648, padded).
pub(super) fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// `ESC ] 52 ; c ; <base64> BEL`: set the clipboard selection.
pub(super) fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

/// What `y` copies for a record, by type: a file entry or change copies its
/// path, a command its shell-quoted command line, a log its stored file path;
/// anything else (stages, checks, findings, …) its full record text.
fn payload(app: &App, record: Record<'_>) -> Result<(&'static str, String), String> {
    let quote = |s: &str| crate::shell::path_argument(std::path::Path::new(s));
    let too_large =
        |what, bytes| format!("Not copied: {what} is {bytes} bytes; the copy limit is 64 KiB");
    let bounded = |what, text: String| {
        if text.len() > MAX_COPY {
            Err(too_large(what, text.len()))
        } else {
            Ok((what, text))
        }
    };
    match record {
        Record::Entry(path, _) => bounded("path", path.to_owned()),
        Record::Change(_, change) => bounded("path", change.path.clone()),
        Record::Observation(snapshot, _) | Record::IncompleteObservation(snapshot) => {
            bounded("path", snapshot.root.clone())
        }
        Record::Command(command) => {
            // Only bounded lengths are summed before anything is formatted.
            let length = command.argv.iter().map(|a| a.len() + 3).sum::<usize>();
            if length > MAX_COPY {
                return Err(too_large("command line", length));
            }
            bounded(
                "command line",
                command
                    .argv
                    .iter()
                    .map(|a| quote(a))
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        }
        Record::Log(_, _, log) => {
            bounded("log path", app.root.join(&log.path).display().to_string())
        }
        Record::SourcePath(root, path) => bounded("path", root.join(path).display().to_string()),
        other => {
            // Read at most the limit plus one page; never the whole record.
            let mut text = String::new();
            let mut offset = 0;
            loop {
                let page = other
                    .page(offset, DETAIL_BYTES)
                    .map_err(|e| format!("Not copied: {e:#}"))?;
                text.push_str(&page.text);
                match page.next_offset {
                    Some(next) if text.len() <= MAX_COPY => offset = next,
                    Some(_) => {
                        return Err(
                            "Not copied: record text is larger than the 64 KiB copy limit".into(),
                        );
                    }
                    None => break,
                }
            }
            bounded("record text", text)
        }
    }
}

/// Queue the selected record for the clipboard and say what was copied.
pub(super) fn copy(app: &mut App) {
    let result = {
        let mut view = app.viewport.borrow_mut();
        match view.selected_record(app) {
            None => Err("Nothing to copy: no evidence record is selected".to_string()),
            Some(selected) => payload(app, selected.record),
        }
    };
    match result {
        Ok((what, text)) => {
            app.status = format!(
                "Copied {what} ({} bytes) via OSC 52; the terminal must allow clipboard writes",
                text.len()
            );
            app.clipboard = Some(osc52(&text));
        }
        Err(message) => app.status = message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn base64_matches_rfc4648_vectors() {
        for (input, output) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), output);
        }
        assert_eq!(base64(&[0xff, 0xfe, 0x00]), "//4A");
    }
    #[test]
    fn osc52_never_carries_raw_control_bytes() {
        let sequence = osc52("evil\x1b]0;title\x07\x1b[2J");
        assert!(sequence.starts_with("\x1b]52;c;") && sequence.ends_with('\x07'));
        let body = &sequence["\x1b]52;c;".len()..sequence.len() - 1];
        assert!(
            body.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
        );
    }
}
