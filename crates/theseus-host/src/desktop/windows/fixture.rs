//! Test-only inert Shell Link bytes. No file publication, COM mutation or activation.
use super::*;
/// Encode the closed local shortcut profile for publication by an original test SUT.
pub fn shortcut_bytes(root: &str, executable: &str, arguments: &[String]) -> io::Result<Vec<u8>> {
    if !absolute_root(root)
        || !leaf(executable)
        || !executable.ends_with(".exe")
        || executable.len() <= 4
        || arguments.len() > 64
        || arguments.iter().any(|a| a.len() > 1024 || a.contains('\0'))
    {
        return Err(invalid("unsupported inert shortcut fixture profile"));
    }
    encode_link(
        &format!("{root}\\{executable}"),
        root,
        &command_line(arguments),
    )
}
fn put32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn string_data(data: &mut Vec<u8>, value: &str) {
    let units: Vec<u16> = value.encode_utf16().collect();
    data.extend((units.len() as u16).to_le_bytes());
    for unit in units {
        data.extend(unit.to_le_bytes());
    }
}
pub(super) fn encode_link(target: &str, cwd: &str, args: &str) -> std::io::Result<Vec<u8>> {
    if target.len() > 2048
        || cwd.len() > 1024
        || args.encode_utf16().count() > 16384
        || [target, cwd, args].iter().any(|s| s.contains('\0'))
    {
        return Err(invalid("inert shortcut fixture input bound"));
    }
    let mut data = vec![0u8; 76];
    put32(&mut data, 0, 76);
    data[4..20].copy_from_slice(&[1, 20, 2, 0, 0, 0, 0, 0, 192, 0, 0, 0, 0, 0, 0, 70]);
    put32(&mut data, 20, 0xb2);
    put32(&mut data, 60, 1);
    let mut info = vec![0u8; 36];
    put32(&mut info, 4, 36);
    put32(&mut info, 8, 1);
    put32(&mut info, 12, 36);
    let mut volume = vec![0u8; 17];
    put32(&mut volume, 0, 17);
    put32(&mut volume, 4, 3);
    put32(&mut volume, 12, 16);
    info.extend(volume);
    let offset = info.len() as u32;
    put32(&mut info, 16, offset);
    info.extend(target.as_bytes());
    info.push(0);
    let offset = info.len() as u32;
    put32(&mut info, 24, offset);
    info.push(0);
    let offset = info.len() as u32;
    put32(&mut info, 28, offset);
    for unit in target.encode_utf16().chain(Some(0)) {
        info.extend(unit.to_le_bytes());
    }
    let offset = info.len() as u32;
    put32(&mut info, 32, offset);
    info.extend([0, 0]);
    let length = info.len() as u32;
    put32(&mut info, 0, length);
    data.extend(info);
    string_data(&mut data, cwd);
    string_data(&mut data, args);
    data.extend([0; 4]);
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shortcut_fixture_bytes_preserve_closed_local_profile_and_bounds() {
        let bytes = shortcut_bytes(r"C:\fixture", "reader.exe", &["literal value".into()]).unwrap();
        assert_eq!(validate_link_bytes(&bytes).unwrap(), 0xb2);
        for (root, exe, args) in [
            (r"C:\fixture", "reader.exe", vec!["bad\0value".into()]),
            (r"C:\fixture", "reader.exe", vec!["x".repeat(1025)]),
            (r"\\server\share", "reader.exe", vec![]),
            (r"C:\fixture", "../reader.exe", vec![]),
        ] {
            assert!(
                shortcut_bytes(root, exe, &args).is_err(),
                "invalid inert fixture profile"
            );
        }
    }
}
