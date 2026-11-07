//! Exact original SDK image/dependency closure; never discovers authority.

use super::super::{MAX_ARTIFACT, QualifiedSdkImages, RetainedArtifact, SdkImageRole};
use crate::fs::read_exact_at;
use std::{
    collections::BTreeSet,
    ffi::{CStr, CString},
    fs::File,
    io,
    os::unix::ffi::OsStrExt,
};
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}
struct DynamicImage {
    soname: String,
    needed: Vec<String>,
}
fn u16_at(b: &[u8], n: usize) -> u16 {
    u16::from_le_bytes(b[n..n + 2].try_into().expect("fixed ELF field"))
}
fn u32_at(b: &[u8], n: usize) -> u32 {
    u32::from_le_bytes(b[n..n + 4].try_into().expect("fixed ELF field"))
}
fn u64_at(b: &[u8], n: usize) -> u64 {
    u64::from_le_bytes(b[n..n + 8].try_into().expect("fixed ELF field"))
}
fn parse_dynamic_file(file: &File, length: u64) -> io::Result<DynamicImage> {
    if length < 64 {
        return Err(invalid("SDK ELF header truncated"));
    }
    let mut h = [0; 64];
    read_exact_at(file, &mut h, 0)?;
    let machine = match std::env::consts::ARCH {
        "x86_64" => 62,
        "aarch64" => 183,
        _ => return Err(invalid("SDK native ABI unavailable")),
    };
    if h[..7] != *b"\x7fELF\x02\x01\x01"
        || u16_at(&h, 16) != 3
        || u16_at(&h, 18) != machine
        || u32_at(&h, 20) != 1
        || u16_at(&h, 52) != 64
        || u16_at(&h, 54) != 56
    {
        return Err(invalid("SDK image is not admitted ELF64 shared object"));
    }
    let offset = u64_at(&h, 32);
    let count = u16_at(&h, 56) as u64;
    if count == 0
        || count > 128
        || offset < 64
        || offset.checked_add(count * 56).is_none_or(|n| n > length)
    {
        return Err(invalid("SDK program table outside bound"));
    }
    let mut loads = Vec::new();
    let mut dynamic = None;
    for n in 0..count {
        let mut p = [0; 56];
        read_exact_at(file, &mut p, offset + n * 56)?;
        let start = u64_at(&p, 8);
        let size = u64_at(&p, 32);
        if start.checked_add(size).is_none_or(|end| end > length) {
            return Err(invalid("SDK segment outside image"));
        }
        match u32_at(&p, 0) {
            1 => loads.push((start, u64_at(&p, 16), size)),
            2 if dynamic.replace((start, size)).is_some() => {
                return Err(invalid("duplicate SDK dynamic table"));
            }
            _ => {}
        }
    }
    let (start, size) = dynamic.ok_or_else(|| invalid("SDK dynamic table absent"))?;
    if size == 0 || size > 65536 || size % 16 != 0 {
        return Err(invalid("SDK dynamic table outside bound"));
    }
    let mut strings = None;
    let mut string_size = None;
    let mut soname = None;
    let mut needed = Vec::new();
    let mut ended = false;
    for n in 0..size / 16 {
        let mut e = [0; 16];
        read_exact_at(file, &mut e, start + n * 16)?;
        let v = u64_at(&e, 8);
        match u64_at(&e, 0) {
            0 => {
                ended = true;
                break;
            }
            1 => {
                if needed.len() >= 64 {
                    return Err(invalid("SDK dependency count bound"));
                }
                needed.push(v);
            }
            5 => {
                if strings.replace(v).is_some() {
                    return Err(invalid("duplicate SDK string table"));
                }
            }
            10 => {
                if string_size.replace(v).is_some() {
                    return Err(invalid("duplicate SDK string size"));
                }
            }
            14 => {
                if soname.replace(v).is_some() {
                    return Err(invalid("duplicate SDK SONAME"));
                }
            }
            15 | 29 | 0x7ffffffd | 0x7fffffff | 0x6ffffefa | 0x6ffffefb | 0x6ffffefc => {
                return Err(invalid(
                    "SDK loader search/filter/audit indirection unsupported",
                ));
            }
            _ => {}
        }
    }
    if !ended {
        return Err(invalid("SDK dynamic terminator absent"));
    }
    let strings = strings.ok_or_else(|| invalid("SDK string table absent"))?;
    let string_size = string_size.ok_or_else(|| invalid("SDK string size absent"))?;
    let string_file = loads
        .iter()
        .find_map(|(file, va, size)| {
            strings
                .checked_sub(*va)
                .filter(|delta| delta.checked_add(string_size).is_some_and(|n| n <= *size))
                .and_then(|delta| file.checked_add(delta))
        })
        .ok_or_else(|| invalid("SDK string table not file backed"))?;
    let name = |index: u64| -> io::Result<String> {
        if index >= string_size {
            return Err(invalid("SDK string index out of bound"));
        }
        let mut bytes = Vec::new();
        for n in 0..=(string_size - index).min(256) {
            if n == 256 || index + n >= string_size {
                return Err(invalid("SDK name outside bound"));
            }
            let mut b = [0];
            read_exact_at(file, &mut b, string_file + index + n)?;
            if b[0] == 0 {
                let name = String::from_utf8(bytes).map_err(|_| invalid("SDK name not UTF8"))?;
                if name.is_empty() || name.contains('/') || name == "." || name == ".." {
                    return Err(invalid("SDK dependency must be exact SONAME"));
                }
                return Ok(name);
            }
            bytes.push(b[0]);
        }
        Err(invalid("SDK name unterminated"))
    };
    Ok(DynamicImage {
        soname: name(soname.ok_or_else(|| invalid("SDK SONAME absent"))?)?,
        needed: needed.into_iter().map(name).collect::<io::Result<_>>()?,
    })
}
fn dependency_order(
    images: &[DynamicImage],
    baseline: &BTreeSet<String>,
) -> io::Result<Vec<usize>> {
    let mut available = baseline.clone();
    let mut names = BTreeSet::new();
    for image in images {
        if !names.insert(&image.soname) || available.contains(&image.soname) {
            return Err(invalid("SDK image duplicates original/baseline SONAME"));
        }
    }
    let mut order = Vec::new();
    while order.len() != images.len() {
        let before = order.len();
        for (n, image) in images.iter().enumerate() {
            if !order.contains(&n) && image.needed.iter().all(|v| available.contains(v)) {
                available.insert(image.soname.clone());
                order.push(n);
            }
        }
        if before == order.len() {
            return Err(invalid(
                "SDK dependency missing from exact original graph or cyclic",
            ));
        }
    }
    Ok(order)
}
#[derive(Clone, PartialEq, Eq)]
struct Mapping {
    address: usize,
    name: Vec<u8>,
}
fn platform_baseline_name(path: &[u8]) -> bool {
    if path.is_empty() {
        return true;
    }
    let name = path.rsplit(|b| *b == b'/').next().unwrap_or_default();
    matches!(
        name,
        b"linux-vdso.so.1"
            | b"libc.so.6"
            | b"libm.so.6"
            | b"libgcc_s.so.1"
            | b"libdl.so.2"
            | b"libpthread.so.0"
            | b"librt.so.1"
            | b"libutil.so.1"
            | b"libresolv.so.2"
            | b"ld-linux-x86-64.so.2"
            | b"ld-linux-aarch64.so.1"
            | b"ld-musl-x86_64.so.1"
            | b"ld-musl-aarch64.so.1"
    )
}
fn mappings() -> io::Result<Vec<Mapping>> {
    struct Snapshot {
        entries: Vec<Mapping>,
        invalid: bool,
    }
    unsafe extern "C" fn observe(
        info: *mut libc::dl_phdr_info,
        _size: usize,
        data: *mut libc::c_void,
    ) -> libc::c_int {
        let out = unsafe { &mut *data.cast::<Snapshot>() };
        let info = unsafe { &*info };
        if out.entries.len() >= 256 || info.dlpi_name.is_null() {
            out.invalid = true;
            return 1;
        }
        let name = unsafe { CStr::from_ptr(info.dlpi_name) }.to_bytes();
        if name.len() > 4096 {
            out.invalid = true;
            return 1;
        }
        out.entries.push(Mapping {
            address: info.dlpi_addr as usize,
            name: name.to_vec(),
        });
        0
    }
    let mut out = Snapshot {
        entries: Vec::with_capacity(32),
        invalid: false,
    };
    unsafe {
        libc::dl_iterate_phdr(Some(observe), (&mut out as *mut Snapshot).cast());
    }
    if out.invalid {
        return Err(invalid("original loader mapping inventory outside bound"));
    }
    Ok(out.entries)
}
fn check_mapping_closure(
    baseline: &[Mapping],
    admitted: &[Vec<u8>],
    observed: &[Mapping],
) -> io::Result<()> {
    if baseline.iter().any(|original| !observed.contains(original)) {
        return Err(invalid("original platform loader mapping lost or replaced"));
    }
    for image in observed {
        if !baseline.contains(image) && !admitted.contains(&image.name) {
            return Err(invalid(
                "loader mapped an image outside the admitted original SDK closure",
            ));
        }
    }
    Ok(())
}
pub(crate) struct PinnedImages {
    pins: Vec<(SdkImageRole, RetainedArtifact)>,
    baseline: Vec<Mapping>,
    order: Vec<usize>,
    handles: std::cell::RefCell<Vec<*mut libc::c_void>>,
    revoked: std::cell::Cell<bool>,
}
impl PinnedImages {
    pub(crate) fn admit(images: QualifiedSdkImages) -> io::Result<Self> {
        let baseline = mappings()?;
        // Names are considered ONLY for mappings already in this original
        // dedicated binary. They never authorize a future newly loaded library.
        if baseline
            .iter()
            .any(|image| !platform_baseline_name(&image.name))
        {
            return Err(invalid(
                "nonplatform image already mapped before original SDK admission",
            ));
        }
        // The stock dedicated binary links no GIO. Existing process-global SDK
        // symbols/mappings refuse; caller's unsafe no-other-SDK contract still applies.
        for symbol in [
            c"g_main_context_new",
            c"g_desktop_app_info_new_from_keyfile",
            c"g_object_ref",
        ] {
            if !unsafe { libc::dlsym(libc::RTLD_DEFAULT, symbol.as_ptr()) }.is_null() {
                return Err(invalid(
                    "GIO/GLib already present before original SDK admission",
                ));
            }
        }
        let mut pins = Vec::new();
        let mut projection = Vec::new();
        for image in images.images {
            let pin = RetainedArtifact::admit(image.artifact, MAX_ARTIFACT, false)?;
            projection.push(parse_dynamic_file(&pin.file, pin.slot.length)?);
            pins.push((image.role, pin));
        }
        let names = baseline
            .iter()
            .filter_map(|m| {
                std::path::Path::new(std::ffi::OsStr::from_bytes(&m.name))
                    .file_name()?
                    .to_str()
                    .map(str::to_owned)
            })
            .collect();
        let order = dependency_order(&projection, &names)?;
        Ok(Self {
            pins,
            baseline,
            order,
            handles: std::cell::RefCell::new(Vec::new()),
            revoked: std::cell::Cell::new(false),
        })
    }
    fn validate_mappings(&self) -> io::Result<()> {
        let paths = self
            .pins
            .iter()
            .map(|(_, p)| p.path().as_os_str().as_bytes().to_vec())
            .collect::<Vec<_>>();
        check_mapping_closure(&self.baseline, &paths, &mappings()?)
    }
    pub(crate) fn matches(&self, images: &QualifiedSdkImages) -> bool {
        self.pins.len() == images.images.len()
            && images.images.iter().all(|image| {
                self.pins.iter().any(|(role, pin)| {
                    *role == image.role
                        && pin.slot.root == image.artifact.root
                        && pin.slot.relative == image.artifact.relative
                        && pin.slot.length == image.artifact.length
                        && pin.slot.sha256 == image.artifact.sha256
                })
            })
    }
    pub(crate) fn revalidate(&self) -> io::Result<()> {
        let result = (|| {
            if self.revoked.get() {
                return Err(invalid("original SDK graph revoked"));
            }
            for (_, p) in &self.pins {
                p.revalidate()?;
            }
            self.validate_mappings()
        })();
        if result.is_err() {
            self.revoked.set(true);
        }
        result
    }
    pub(crate) fn load(&self) -> io::Result<*mut libc::c_void> {
        self.revalidate()?;
        let result = (|| {
            let mut handles = self.handles.borrow_mut();
            if handles.is_empty() {
                for &index in &self.order {
                    let path = CString::new(self.pins[index].1.path().as_os_str().as_bytes())
                        .map_err(|_| invalid("SDK path contains NUL"))?;
                    let handle =
                        unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
                    if handle.is_null() {
                        return Err(super::loader_failure(
                            "load exact independently admitted SDK image",
                        ));
                    }
                    handles.push(handle);
                    self.validate_mappings()?;
                }
            }
            let index = self
                .pins
                .iter()
                .position(|(role, _)| *role == SdkImageRole::Gio)
                .ok_or_else(|| invalid("original GIO image missing"))?;
            let position = self
                .order
                .iter()
                .position(|n| *n == index)
                .ok_or_else(|| invalid("original GIO dependency order missing"))?;
            Ok(handles[position])
        })();
        match result {
            Err(error) => {
                self.revoked.set(true);
                Err(error)
            }
            Ok(handle) => {
                self.revalidate()?;
                Ok(handle)
            }
        }
    }
    pub(crate) fn symbol(
        &self,
        handle: *mut libc::c_void,
        name: &CStr,
    ) -> io::Result<*mut libc::c_void> {
        let address = unsafe { libc::dlsym(handle, name.as_ptr()) };
        if address.is_null() {
            return Err(super::loader_failure(
                "resolve original qualified SDK symbol",
            ));
        }
        let mut info = std::mem::MaybeUninit::<libc::Dl_info>::uninit();
        if unsafe { libc::dladdr(address, info.as_mut_ptr()) } == 0 {
            return Err(invalid("SDK symbol origin unavailable"));
        }
        let info = unsafe { info.assume_init() };
        if info.dli_fname.is_null() {
            return Err(invalid("SDK symbol original image unavailable"));
        }
        let origin = unsafe { CStr::from_ptr(info.dli_fname) }.to_bytes();
        if !self
            .pins
            .iter()
            .any(|(_, p)| p.path().as_os_str().as_bytes() == origin)
        {
            return Err(invalid(
                "SDK symbol resolved outside exact admitted image closure",
            ));
        }
        self.validate_mappings()?;
        Ok(address)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn elf() -> Vec<u8> {
        let mut b = vec![0; 512];
        b[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        b[16..18].copy_from_slice(&3u16.to_le_bytes());
        let machine = if cfg!(target_arch = "aarch64") {
            183u16
        } else {
            62
        };
        b[18..20].copy_from_slice(&machine.to_le_bytes());
        b[20..24].copy_from_slice(&1u32.to_le_bytes());
        b[32..40].copy_from_slice(&64u64.to_le_bytes());
        b[52..54].copy_from_slice(&64u16.to_le_bytes());
        b[54..56].copy_from_slice(&56u16.to_le_bytes());
        b[56..58].copy_from_slice(&2u16.to_le_bytes());
        b[64..68].copy_from_slice(&1u32.to_le_bytes());
        b[96..104].copy_from_slice(&512u64.to_le_bytes());
        b[104..112].copy_from_slice(&512u64.to_le_bytes());
        b[120..124].copy_from_slice(&2u32.to_le_bytes());
        b[128..136].copy_from_slice(&192u64.to_le_bytes());
        b[136..144].copy_from_slice(&192u64.to_le_bytes());
        b[152..160].copy_from_slice(&80u64.to_le_bytes());
        for (n, (tag, value)) in [(5u64, 320u64), (10, 64), (14, 1), (1, 13), (0, 0)]
            .into_iter()
            .enumerate()
        {
            let p = 192 + n * 16;
            b[p..p + 8].copy_from_slice(&tag.to_le_bytes());
            b[p + 8..p + 16].copy_from_slice(&value.to_le_bytes());
        }
        let strings = b"\0libgio.so.0\0libglib.so.0\0";
        b[320..320 + strings.len()].copy_from_slice(strings);
        b
    }
    #[test]
    fn only_original_fixed_platform_abi_images_can_form_the_pre_sdk_baseline() {
        assert!(platform_baseline_name(b""));
        assert!(platform_baseline_name(b"linux-vdso.so.1"));
        assert!(platform_baseline_name(b"/lib/libc.so.6"));
        assert!(!platform_baseline_name(b"/ambient/libgio-2.0.so.0"));
        assert!(!platform_baseline_name(b"/ambient/plugin.so"));
    }
    #[test]
    fn loader_mapping_closure_only_accepts_original_baseline_or_exact_admitted_paths() {
        let base = Mapping {
            address: 1,
            name: b"/platform/libc.so.6".to_vec(),
        };
        let sdk = Mapping {
            address: 2,
            name: b"/owned-sdk/libgio.so.0".to_vec(),
        };
        let admitted = vec![b"/owned-sdk/libgio.so.0".to_vec()];
        assert!(
            check_mapping_closure(
                std::slice::from_ref(&base),
                &admitted,
                &[base.clone(), sdk.clone()]
            )
            .is_ok()
        );
        let wrong = Mapping {
            address: 2,
            name: b"/ambient/libgio.so.0".to_vec(),
        };
        assert!(
            check_mapping_closure(
                std::slice::from_ref(&base),
                &admitted,
                &[base.clone(), wrong]
            )
            .is_err()
        );
        assert!(
            check_mapping_closure(
                std::slice::from_ref(&base),
                &admitted,
                std::slice::from_ref(&sdk)
            )
            .is_err()
        );
        let changed = Mapping {
            address: 99,
            name: base.name.clone(),
        };
        assert!(check_mapping_closure(&[base], &admitted, &[changed, sdk]).is_err());
    }
    #[test]
    fn exact_sdk_dependency_parser_rejects_loader_search_and_truncated_tables() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("sdk");
        let bytes = elf();
        std::fs::write(&path, &bytes).unwrap();
        let parsed =
            parse_dynamic_file(&std::fs::File::open(&path).unwrap(), bytes.len() as u64).unwrap();
        assert_eq!(parsed.soname, "libgio.so.0");
        assert_eq!(parsed.needed, ["libglib.so.0"]);
        // Preserve the valid string table and add one independent loader tag
        // before a new DT_NULL. Rejection must come from loader policy itself.
        for tag in [
            15u64, 29, 0x6ffffefa, 0x6ffffefb, 0x6ffffefc, 0x7ffffffd, 0x7fffffff,
        ] {
            let mut bad = bytes.clone();
            bad[152..160].copy_from_slice(&96u64.to_le_bytes());
            bad[256..264].copy_from_slice(&tag.to_le_bytes());
            std::fs::write(&path, &bad).unwrap();
            assert!(
                parse_dynamic_file(&std::fs::File::open(&path).unwrap(), bad.len() as u64).is_err(),
                "accepted loader indirection tag {tag:x}"
            );
        }
        assert!(parse_dynamic_file(&std::fs::File::open(&path).unwrap(), 127).is_err());
    }
    #[test]
    fn exact_dependency_order_cannot_fill_missing_sdk_from_ambient_loader() {
        let entries = vec![
            DynamicImage {
                soname: "gio".into(),
                needed: vec!["gobject".into()],
            },
            DynamicImage {
                soname: "glib".into(),
                needed: vec!["libc.so.6".into()],
            },
            DynamicImage {
                soname: "gobject".into(),
                needed: vec!["glib".into()],
            },
        ];
        let baseline = ["libc.so.6".into()].into_iter().collect();
        assert_eq!(dependency_order(&entries, &baseline).unwrap(), [1, 2, 0]);
        assert!(dependency_order(&entries, &Default::default()).is_err());
        let mut cycle = entries;
        cycle[1].needed = vec!["gio".into()];
        assert!(dependency_order(&cycle, &baseline).is_err());
    }
}
