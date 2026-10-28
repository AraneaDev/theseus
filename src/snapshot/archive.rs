use super::*;
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::io::Read;

// Metadata has an independent ceiling; a malicious PAX name cannot allocate gigabytes.
const MAX_EXTENSION_BYTES: u64 = 1_048_576;

/// Read Docker's basename-root tar stream without ever extracting a host file.
/// GNU long names and local PAX headers are bounded and applied before path checks.
/// Unsupported entries (including hardlinks and devices) fail the entire observation.
pub fn read_archive(mut reader: impl Read, root: &str, limits: Limits) -> Result<Snapshot> {
    let root = crate::recipe::normalize_container_path(root)?;
    ensure!(root != "/", "cannot observe the whole filesystem");
    ensure!(
        limits.max_entries > 0 && limits.max_content_bytes > 0,
        "limits must be positive"
    );
    let basename = root.rsplit('/').next().context("missing root basename")?;
    let mut snapshot = Snapshot {
        root: root.clone(),
        completeness: ObservationStatus::Complete,
        entries: BTreeMap::new(),
    };
    let mut headers = 0u64;
    let mut total_content = 0u64;
    let mut extensions = BTreeMap::new();
    loop {
        let mut header = [0u8; 512];
        reader
            .read_exact(&mut header)
            .context("truncated tar header")?;
        if header.iter().all(|&b| b == 0) {
            reader
                .read_exact(&mut header)
                .context("truncated tar trailer")?;
            ensure!(header.iter().all(|&b| b == 0), "invalid tar trailer");
            ensure!(extensions.is_empty(), "orphan tar extension");
            break;
        }
        headers = headers.checked_add(1).context("entry counter overflow")?;
        ensure!(
            headers <= limits.max_entries,
            "snapshot entry limit exceeded"
        );
        verify_checksum(&header)?;
        let mut name = field(&header[..100])?;
        let prefix = field(&header[345..500])?;
        if !prefix.is_empty() {
            // Prefix is defined by POSIX ustar, not GNU's reused bytes.
            ensure!(
                &header[257..263] == b"ustar\0",
                "unsupported tar prefix format"
            );
            name = format!("{prefix}/{name}");
        }
        let kind = header[156];
        let mut size = number(&header[124..136])?;
        if matches!(kind, b'x' | b'L' | b'K') {
            ensure!(size <= MAX_EXTENSION_BYTES, "tar extension too large");
            let data = read_small(&mut reader, size)?;
            skip_padding(&mut reader, size)?;
            if kind == b'x' {
                for (key, value) in parse_pax(&data)? {
                    ensure!(
                        !key.starts_with("GNU.sparse"),
                        "sparse tar entries are unsupported"
                    );
                    ensure!(
                        extensions.insert(key.clone(), value).is_none(),
                        "duplicate tar extension key {key}"
                    );
                }
            } else {
                let key = if kind == b'L' { "path" } else { "linkpath" };
                let value = std::str::from_utf8(&data)?
                    .trim_end_matches('\0')
                    .to_owned();
                ensure!(
                    extensions.insert(key.into(), value).is_none(),
                    "duplicate GNU extension"
                );
            }
            // Bound aggregate extension allocations, including many individual PAX headers.
            ensure!(
                extensions
                    .iter()
                    .map(|(k, v)| k.len() + v.len())
                    .sum::<usize>()
                    <= MAX_EXTENSION_BYTES as usize,
                "combined tar extensions too large"
            );
            continue;
        }
        if let Some(path) = extensions.remove("path") {
            name = path;
        }
        let link = extensions
            .remove("linkpath")
            .unwrap_or(field(&header[157..257])?);
        if let Some(value) = extensions.remove("size") {
            size = value.parse().context("invalid PAX size")?;
        }
        let uid = extension_number(&mut extensions, "uid", &header[108..116])?;
        let gid = extension_number(&mut extensions, "gid", &header[116..124])?;
        extensions.clear();
        let path = map_path(&name, basename, &root)?;
        ensure!(
            !snapshot.entries.contains_key(&path),
            "duplicate archive path {path}"
        );
        let file_kind = match kind {
            0 | b'0' => FileKind::File,
            b'5' => FileKind::Directory,
            b'2' => FileKind::Symlink,
            _ => bail!("unsupported tar entry type {kind} at {path}"),
        };
        ensure!(
            file_kind == FileKind::File || size == 0,
            "non-file tar entry contains data"
        );
        // Query ancestors and a single ordered descendant rather than scanning all
        // earlier entries: a snapshot at the default 100k limit must stay practical.
        for (index, _) in path.match_indices('/').skip(1) {
            if let Some(entry) = snapshot.entries.get(&path[..index]) {
                ensure!(
                    entry.kind == FileKind::Directory,
                    "archive entry descends through a non-directory"
                );
            }
        }
        if file_kind != FileKind::Directory {
            let prefix = format!("{path}/");
            ensure!(
                !snapshot
                    .entries
                    .range(prefix.clone()..)
                    .next()
                    .is_some_and(|(existing, _)| existing.starts_with(&prefix)),
                "archive parent is not a directory"
            );
        }
        let sha256 = if file_kind == FileKind::File {
            total_content = total_content
                .checked_add(size)
                .context("content size overflow")?;
            ensure!(
                total_content <= limits.max_content_bytes,
                "snapshot content limit exceeded"
            );
            let mut hash = Sha256::new();
            let mut remaining = size;
            let mut buffer = [0u8; 32 * 1024];
            while remaining > 0 {
                let count = remaining.min(buffer.len() as u64) as usize;
                reader
                    .read_exact(&mut buffer[..count])
                    .context("truncated file content")?;
                hash.update(&buffer[..count]);
                remaining -= count as u64;
            }
            Some(format!("{:x}", hash.finalize()))
        } else {
            None
        };
        skip_padding(&mut reader, size)?;
        snapshot.entries.insert(
            path,
            FileEntry {
                kind: file_kind,
                mode: u32::try_from(number(&header[100..108])?)? & 0o7777,
                uid,
                gid,
                size,
                sha256,
                link_target: (file_kind == FileKind::Symlink).then_some(link),
            },
        );
    }
    ensure!(
        snapshot.entries.contains_key(&root),
        "archive does not include requested root"
    );
    Ok(snapshot)
}
fn map_path(name: &str, basename: &str, root: &str) -> Result<String> {
    ensure!(
        !name.starts_with('/') && !name.contains('\0'),
        "absolute or NUL archive path"
    );
    let mut pieces = Vec::new();
    for piece in name.split('/') {
        match piece {
            "" | "." => {}
            ".." => bail!("archive path traversal"),
            value => pieces.push(value),
        }
    }
    ensure!(
        pieces.first().is_some_and(|p| *p == basename),
        "archive entry is outside requested root: {name}"
    );
    Ok(if pieces.len() == 1 {
        root.to_owned()
    } else {
        format!("{root}/{}", pieces[1..].join("/"))
    })
}
fn field(bytes: &[u8]) -> Result<String> {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    Ok(std::str::from_utf8(&bytes[..end])
        .context("non-UTF8 tar names are unsupported")?
        .to_owned())
}
fn number(bytes: &[u8]) -> Result<u64> {
    if bytes[0] & 0x80 != 0 {
        ensure!(bytes[0] & 0x40 == 0, "negative tar number");
        let mut value = u64::from(bytes[0] & 0x7f);
        for byte in &bytes[1..] {
            value = value
                .checked_mul(256)
                .and_then(|v| v.checked_add(u64::from(*byte)))
                .context("tar number overflow")?;
        }
        return Ok(value);
    }
    let text = std::str::from_utf8(bytes)?.trim_matches(['\0', ' ']);
    if text.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(text, 8).context("invalid octal tar number")
}
fn verify_checksum(header: &[u8; 512]) -> Result<()> {
    let expected = number(&header[148..156])?;
    let actual: u64 = header
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            if (148..156).contains(&i) {
                32
            } else {
                u64::from(b)
            }
        })
        .sum();
    ensure!(expected == actual, "invalid tar header checksum");
    Ok(())
}
fn skip_padding(reader: &mut impl Read, size: u64) -> Result<()> {
    let padding = (512 - size % 512) % 512;
    reader
        .read_exact(&mut [0u8; 512][..padding as usize])
        .context("truncated tar padding")?;
    Ok(())
}
fn read_small(reader: &mut impl Read, size: u64) -> Result<Vec<u8>> {
    let mut data = vec![0; usize::try_from(size)?];
    reader
        .read_exact(&mut data)
        .context("truncated tar extension")?;
    Ok(data)
}
fn extension_number(
    extensions: &mut BTreeMap<String, String>,
    key: &str,
    header: &[u8],
) -> Result<u64> {
    match extensions.remove(key) {
        Some(value) => value.parse().with_context(|| format!("invalid PAX {key}")),
        None => number(header),
    }
}
fn parse_pax(mut data: &[u8]) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    while !data.is_empty() {
        let space = data
            .iter()
            .position(|&b| b == b' ')
            .context("malformed PAX record")?;
        let length: usize = std::str::from_utf8(&data[..space])?
            .parse()
            .context("invalid PAX length")?;
        ensure!(
            length > space + 2 && length <= data.len() && data[length - 1] == b'\n',
            "invalid PAX record length"
        );
        let record = std::str::from_utf8(&data[space + 1..length - 1])?;
        let (key, value) = record.split_once('=').context("invalid PAX key/value")?;
        ensure!(
            result.insert(key.to_owned(), value.to_owned()).is_none(),
            "duplicate PAX key"
        );
        data = &data[length..];
    }
    Ok(result)
}
