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
/// Root "This PC" ({20D04FE0-3AEA-1069-A2D8-08002B30309D}) shell item.
const MY_COMPUTER_ITEM: [u8; 20] = [
    0x14, 0, 0x1f, 0x50, 0xe0, 0x4f, 0xd0, 0x20, 0xea, 0x3a, 0x69, 0x10, 0xa2, 0xd8, 0x08, 0x00,
    0x2b, 0x30, 0x30, 0x9d,
];
/// One file-system shell item in the layout Shell writes (version 9
/// BEEF0004 extension carrying the long name). Volatile fields that only
/// describe the target at save time (size, DOS times, NTFS file reference)
/// are zero, which keeps the inert fixture bytes deterministic.
pub(super) fn file_system_item(name: &str, directory: bool) -> Vec<u8> {
    let mut item = vec![0u8; 2];
    item.push(if directory { 0x31 } else { 0x32 });
    item.push(0);
    item.extend([0; 8]); // file size, modified DOS date/time
    item.extend(if directory { 0x10u16 } else { 0x20 }.to_le_bytes());
    item.extend(name.as_bytes());
    item.push(0);
    if !item.len().is_multiple_of(2) {
        item.push(0);
    }
    let extension = item.len();
    let mut ext = vec![0u8; 2];
    ext.extend(9u16.to_le_bytes());
    ext.extend(0xbeef_0004u32.to_le_bytes());
    ext.extend([0; 8]); // created, accessed DOS date/time
    ext.extend(0x2eu16.to_le_bytes());
    ext.extend([0; 2 + 8 + 14 + 4]); // reserved, file reference, reserved
    for unit in name.encode_utf16().chain(Some(0)) {
        ext.extend(unit.to_le_bytes());
    }
    ext.extend((extension as u16).to_le_bytes());
    let size = ext.len() as u16;
    ext[..2].copy_from_slice(&size.to_le_bytes());
    item.extend(ext);
    let size = item.len() as u16;
    item[..2].copy_from_slice(&size.to_le_bytes());
    item
}
/// Deterministic inert bytes in the closed Shell-authored profile (see
/// `validate_link_bytes`): LinkTargetIDList (This PC, drive, file-system
/// items), local LinkInfo, relative path, working directory, arguments and no
/// ExtraData. Target must be `X:\component\...\leaf` with ASCII components.
pub(super) fn encode_link(target: &str, cwd: &str, args: &str) -> std::io::Result<Vec<u8>> {
    if target.len() > 2048
        || cwd.len() > 1024
        || args.encode_utf16().count() > 16384
        || [target, cwd, args].iter().any(|s| s.contains('\0'))
        || !target.is_ascii()
    {
        return Err(invalid("inert shortcut fixture input bound"));
    }
    let bytes = target.as_bytes();
    let components: Vec<&str> = target.get(3..).unwrap_or_default().split('\\').collect();
    if bytes.len() < 4
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || bytes[2] != b'\\'
        || components.iter().any(|c| c.is_empty() || c.len() > 255)
        || components.len() > 29
    {
        return Err(invalid(
            "inert shortcut fixture target must be a local drive path",
        ));
    }
    let mut flags = super::LINK_REQUIRED_FLAGS;
    if !args.is_empty() {
        flags |= super::LINK_ARGUMENTS_FLAG;
    }
    let mut data = vec![0u8; 76];
    put32(&mut data, 0, 76);
    data[4..20].copy_from_slice(&[1, 20, 2, 0, 0, 0, 0, 0, 192, 0, 0, 0, 0, 0, 0, 70]);
    put32(&mut data, 20, flags);
    put32(&mut data, 24, 0x20);
    put32(&mut data, 60, 1);
    let mut list = MY_COMPUTER_ITEM.to_vec();
    let mut drive = vec![0u8; 25];
    drive[..6].copy_from_slice(&[0x19, 0, 0x2f, bytes[0], b':', b'\\']);
    list.extend(drive);
    for (index, component) in components.iter().enumerate() {
        list.extend(file_system_item(component, index + 1 != components.len()));
    }
    list.extend([0, 0]);
    data.extend((u16::try_from(list.len()).map_err(|_| invalid("IDList bound"))?).to_le_bytes());
    data.extend(list);
    let mut info = vec![0u8; 28];
    put32(&mut info, 4, 28);
    put32(&mut info, 8, 1);
    put32(&mut info, 12, 28);
    let mut volume = vec![0u8; 17];
    put32(&mut volume, 0, 17);
    put32(&mut volume, 4, 3);
    put32(&mut volume, 12, 16);
    info.extend(volume);
    let offset = info.len() as u32;
    put32(&mut info, 16, offset);
    info.extend(bytes);
    info.push(0);
    let offset = info.len() as u32;
    put32(&mut info, 24, offset);
    info.push(0);
    let length = info.len() as u32;
    put32(&mut info, 0, length);
    data.extend(info);
    string_data(
        &mut data,
        &format!(".\\{}", components[components.len() - 1]),
    );
    string_data(&mut data, cwd);
    if !args.is_empty() {
        string_data(&mut data, args);
    }
    data.extend([0; 4]);
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shortcut_fixture_bytes_preserve_closed_local_profile_and_bounds() {
        let bytes = shortcut_bytes(r"C:\fixture", "reader.exe", &["literal value".into()]).unwrap();
        let layout = validate_link_bytes(&bytes).unwrap();
        assert_eq!(layout.flags, 0xbb);
        assert_eq!(layout.local_base_path, r"C:\fixture\reader.exe");
        assert_eq!(layout.relative_path, r".\reader.exe");
        // Inert bytes admit exactly what Shell COM saves, with save-time
        // volatile item fields zeroed (CI8 windows-2025 vector).
        let saved = super::super::tests::shell_saved_link();
        let zeroed = |item: &[u8]| {
            let mut item = item.to_vec();
            let ext = u16::from_le_bytes([item[item.len() - 2], item[item.len() - 1]]) as usize;
            for range in [
                4..12,
                ext + 8..ext + 16,
                ext + 20..ext + 28,
                ext + 42..ext + 46,
            ] {
                item[range].fill(0);
            }
            item
        };
        for (at, name, directory) in [(78 + 188, "Local", true), (78 + 268, "Temp", true)] {
            let size = u16::from_le_bytes([saved[at], saved[at + 1]]) as usize;
            assert_eq!(
                file_system_item(name, directory),
                zeroed(&saved[at..at + size])
            );
        }
        let at = 78 + 538 - 98;
        assert_eq!(
            file_system_item("fixture.exe", false),
            zeroed(&saved[at..at + 98])
        );
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
