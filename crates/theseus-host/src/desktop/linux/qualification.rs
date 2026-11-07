//! Native execution is limited to independently qualified, immutable compiled
//! host binaries. GIO/libc ENOEXEC shell fallback exists outside this subset.
use super::{ArtifactSlot, MAX_ARTIFACT, hash_file};
use crate::fs::{fingerprint, read_exact_at};
use std::{fs::File, io};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeElfAbi {
    LinuxX86_64,
    LinuxAarch64,
}
impl NativeElfAbi {
    pub fn current() -> io::Result<Self> {
        match std::env::consts::ARCH {
            "x86_64" => Ok(Self::LinuxX86_64),
            "aarch64" => Ok(Self::LinuxAarch64),
            _ => Err(unsupported("native GIO ELF ABI unsupported")),
        }
    }
    fn machine(self) -> u16 {
        match self {
            Self::LinuxX86_64 => 62,
            Self::LinuxAarch64 => 183,
        }
    }
    fn admits_interpreter(self, value: &[u8]) -> bool {
        match self {
            Self::LinuxX86_64 => matches!(
                value,
                b"/lib64/ld-linux-x86-64.so.2"
                    | b"/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2"
                    | b"/lib/ld-musl-x86_64.so.1"
            ),
            Self::LinuxAarch64 => matches!(
                value,
                b"/lib/ld-linux-aarch64.so.1"
                    | b"/lib/aarch64-linux-gnu/ld-linux-aarch64.so.1"
                    | b"/lib/ld-musl-aarch64.so.1"
            ),
        }
    }
}
/// Verified by the caller's authenticated provider/build or original Run
/// bridge, not by entry/application self-report or a caller-chosen digest.
/// Receipt authentication and truthful compiled-artifact qualification are
/// obligations of the unsafe dedicated broker admission contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QualificationProvenance {
    ProviderBuildAttested { verified_receipt_digest: [u8; 32] },
    OriginalNativeVersionRun { verified_receipt_digest: [u8; 32] },
}
impl QualificationProvenance {
    fn validate(&self) -> io::Result<()> {
        let digest = match self {
            Self::ProviderBuildAttested {
                verified_receipt_digest,
            }
            | Self::OriginalNativeVersionRun {
                verified_receipt_digest,
            } => verified_receipt_digest,
        };
        if digest == &[0; 32] {
            return Err(unsupported(
                "missing independently verified native artifact receipt",
            ));
        }
        Ok(())
    }
}
/// SDK qualification is independently authenticated, covers this exact helper
/// commitment and native ABI, and binds the supplied fixed-soname SDK/version.
/// It is not a claim read from .desktop bytes or reconstructed from report JSON.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QualifiedGioSdk {
    pub glib_version: [u32; 3],
    pub native_abi: NativeElfAbi,
    pub helper_length: u64,
    pub helper_sha256: [u8; 32],
    pub qualification_digest: [u8; 32],
}
impl QualifiedGioSdk {
    pub(super) fn validate(&self) -> io::Result<()> {
        if self.glib_version[0] != 2
            || self.glib_version[1] < 80
            || self.native_abi != NativeElfAbi::current()?
            || self.helper_length == 0
            || self.helper_length > MAX_ARTIFACT
            || self.helper_sha256 == [0; 32]
            || self.qualification_digest == [0; 32]
        {
            return Err(unsupported(
                "missing/mismatching independent GIO SDK qualification",
            ));
        }
        Ok(())
    }
}
/// Independently verified compiled-host-binary provenance. Its commitment must
/// match observed retained bytes and this original SDK qualification exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompiledArtifactQualification {
    pub length: u64,
    pub sha256: [u8; 32],
    pub native_abi: NativeElfAbi,
    pub sdk_qualification_digest: [u8; 32],
    pub provenance: QualificationProvenance,
}
/// Observed bytes/ELF projection are deliberately distinct from the independent
/// build/provider or original-native-version provenance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedNativeBinary {
    pub length: u64,
    pub sha256: [u8; 32],
    pub native_abi: NativeElfAbi,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeBinaryAdmission {
    pub observed: ObservedNativeBinary,
    pub qualification: CompiledArtifactQualification,
}
pub(super) fn validate_compiled_binary(
    file: &File,
    slot: &ArtifactSlot,
    qualification: Option<&CompiledArtifactQualification>,
    sdk: &QualifiedGioSdk,
) -> io::Result<NativeBinaryAdmission> {
    sdk.validate()?;
    let qualification = qualification.ok_or_else(|| unsupported("native activation requires independently qualified compiled host artifact; hash/ELF magic alone is insufficient"))?;
    qualification.provenance.validate()?;
    if slot.length < 64
        || slot.length > MAX_ARTIFACT
        || slot.sha256 == [0; 32]
        || qualification.length != slot.length
        || qualification.sha256 != slot.sha256
        || qualification.native_abi != sdk.native_abi
        || qualification.sdk_qualification_digest != sdk.qualification_digest
    {
        return Err(unsupported(
            "compiled artifact qualification differs from admitted tuple/SDK",
        ));
    }
    let before = fingerprint(file)?;
    if before.len != slot.length {
        return Err(unsupported("compiled artifact observed length mismatch"));
    }
    let abi = strict_elf(file, slot.length)?;
    let sha256 = hash_file(file, slot.length)?;
    if abi != qualification.native_abi
        || sha256 != qualification.sha256
        || fingerprint(file)? != before
    {
        return Err(unsupported(
            "compiled native artifact bytes/ABI changed or differ from qualification",
        ));
    }
    Ok(NativeBinaryAdmission {
        observed: ObservedNativeBinary {
            length: slot.length,
            sha256,
            native_abi: abi,
        },
        qualification: qualification.clone(),
    })
}
fn strict_elf(file: &File, length: u64) -> io::Result<NativeElfAbi> {
    let mut header = [0; 64];
    read_exact_at(file, &mut header, 0)?;
    let abi = NativeElfAbi::current()?;
    if header[..7] != *b"\x7fELF\x02\x01\x01"
        || !matches!(header[7], 0 | 3)
        || header[8..16] != [0; 8]
        || !matches!(u16_at(&header, 16), 2 | 3)
        || u16_at(&header, 18) != abi.machine()
        || u32_at(&header, 20) != 1
        || u32_at(&header, 48) != 0
        || u16_at(&header, 52) != 64
        || u16_at(&header, 54) != 56
    {
        return Err(unsupported(
            "artifact is not a supported native ELF64 compiled host binary",
        ));
    }
    let entry = u64_at(&header, 24);
    let phoff = u64_at(&header, 32);
    let count = u16_at(&header, 56) as u64;
    let table_end = phoff
        .checked_add(count * 56)
        .ok_or_else(|| unsupported("ELF program table overflow"))?;
    if count == 0 || count > 128 || phoff < 64 || !phoff.is_multiple_of(8) || table_end > length {
        return Err(unsupported("ELF program table exceeds bound"));
    }
    let shoff = u64_at(&header, 40);
    let shcount = u16_at(&header, 60) as u64;
    if (shcount == 0 && (shoff != 0 || u16_at(&header, 62) != 0))
        || (shcount != 0
            && (u16_at(&header, 58) != 64
                || shoff < 64
                || shoff
                    .checked_add(shcount * 64)
                    .is_none_or(|end| end > length)
                || u16_at(&header, 62) as u64 >= shcount))
    {
        return Err(unsupported("ELF section table malformed or unsupported"));
    }
    let mut executable_entry = false;
    let mut mapped_headers = false;
    let mut interpreter_seen = false;
    for index in 0..count {
        let mut ph = [0; 56];
        read_exact_at(file, &mut ph, phoff + index * 56)?;
        let kind = u32_at(&ph, 0);
        let flags = u32_at(&ph, 4);
        let offset = u64_at(&ph, 8);
        let address = u64_at(&ph, 16);
        let filesz = u64_at(&ph, 32);
        let memsz = u64_at(&ph, 40);
        let align = u64_at(&ph, 48);
        if flags & !7 != 0
            || offset.checked_add(filesz).is_none_or(|end| end > length)
            || (align > 1 && !align.is_power_of_two())
        {
            return Err(unsupported("ELF program segment malformed or out of file"));
        }
        if kind == 1 {
            let end = address
                .checked_add(memsz)
                .ok_or_else(|| unsupported("ELF virtual segment overflow"))?;
            if memsz < filesz
                || end > i64::MAX as u64
                || (align > 1 && address % align != offset % align)
            {
                return Err(unsupported("ELF load segment malformed"));
            }
            executable_entry |= flags & 1 != 0 && entry >= address && entry < end;
            mapped_headers |= offset == 0 && filesz >= table_end;
        } else if kind == 3 {
            if interpreter_seen || !(2..=1024).contains(&filesz) {
                return Err(unsupported("ELF interpreter outside bound"));
            }
            let mut interpreter = [0; 1024];
            read_exact_at(file, &mut interpreter[..filesz as usize], offset)?;
            let text = &interpreter[..filesz as usize - 1];
            if interpreter[filesz as usize - 1] != 0
                || text.contains(&0)
                || !abi.admits_interpreter(text)
            {
                return Err(unsupported(
                    "ELF interpreter differs from supported host SDK ABI",
                ));
            }
            interpreter_seen = true;
        }
    }
    if !executable_entry || !mapped_headers {
        return Err(unsupported(
            "ELF lacks mapped headers and native executable entry",
        ));
    }
    Ok(abi)
}
fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(
        bytes[offset..offset + 2]
            .try_into()
            .expect("bounded ELF field"),
    )
}
fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("bounded ELF field"),
    )
}
fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("bounded ELF field"),
    )
}
fn unsupported(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}

/// Exact independently qualified SDK image closure. The unsafe original broker
/// caller supplies provenance; this DTO only validates the closed path/role ceiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SdkImageRole {
    Gio,
    Glib,
    Gobject,
    Support,
}
#[derive(Clone, Debug)]
pub struct QualifiedSdkImage {
    pub role: SdkImageRole,
    pub artifact: ArtifactSlot,
}
pub struct QualifiedSdkImages {
    pub(super) images: Vec<QualifiedSdkImage>,
}
impl QualifiedSdkImages {
    pub fn new(images: Vec<QualifiedSdkImage>) -> io::Result<Self> {
        if !(3..=35).contains(&images.len()) {
            return Err(unsupported("SDK closure image bound"));
        }
        let mut paths = std::collections::BTreeSet::new();
        for image in &images {
            let a = &image.artifact;
            if !a.root.is_absolute()
                || a.length < 64
                || a.length > MAX_ARTIFACT
                || a.sha256 == [0; 32]
                || crate::fs::RelativePath::parse(&a.relative).is_err()
                || !paths.insert((a.root.clone(), a.relative.clone()))
            {
                return Err(unsupported(
                    "SDK closure requires distinct exact original artifacts",
                ));
            }
        }
        for role in [SdkImageRole::Gio, SdkImageRole::Glib, SdkImageRole::Gobject] {
            if images.iter().filter(|i| i.role == role).count() != 1 {
                return Err(unsupported(
                    "SDK closure mandatory role missing or duplicated",
                ));
            }
        }
        Ok(Self { images })
    }
}

#[cfg(test)]
mod image_set_tests {
    use super::*;
    fn image(role: SdkImageRole, name: &str) -> QualifiedSdkImage {
        QualifiedSdkImage {
            role,
            artifact: ArtifactSlot {
                root: "/owned-sdk".into(),
                relative: name.into(),
                length: 128,
                sha256: [7; 32],
            },
        }
    }
    #[test]
    fn native_generation_handoff_requires_both_original_retirements_and_no_uncertainty() {
        use super::super::native_generation_handoff_allowed as allowed;
        assert!(allowed(true, true, false, false, true));
        for flags in [
            (false, true, false, false, true),
            (true, false, false, false, true),
            (true, true, true, false, true),
            (true, true, false, true, true),
            (true, true, false, false, false),
        ] {
            assert!(!allowed(flags.0, flags.1, flags.2, flags.3, flags.4));
        }
    }
    #[test]
    fn qualified_image_set_requires_three_distinct_original_roles_and_paths() {
        assert!(QualifiedSdkImages::new(vec![image(SdkImageRole::Gio, "gio")]).is_err());
        let exact = vec![
            image(SdkImageRole::Gio, "gio"),
            image(SdkImageRole::Glib, "glib"),
            image(SdkImageRole::Gobject, "gobject"),
        ];
        assert!(QualifiedSdkImages::new(exact.clone()).is_ok());
        let mut bad = exact.clone();
        bad.push(image(SdkImageRole::Gio, "other-gio"));
        assert!(QualifiedSdkImages::new(bad).is_err());
        let mut bad = exact;
        bad[2].artifact.relative = "gio".into();
        assert!(QualifiedSdkImages::new(bad).is_err());
    }
}
