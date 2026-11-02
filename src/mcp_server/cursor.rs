//! Fixed stateless cursor codec and typed selector binding.
use super::types::*;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use std::io::{self, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Family {
    RecipeCatalog = 1,
    RecipeRecord = 2,
    RunList = 3,
    EvidenceCatalog = 4,
    EvidenceRecord = 5,
    Log = 6,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CursorRecord {
    pub family: Family,
    pub instance: [u8; 16],
    pub admission: u8,
    pub revision: [u8; 32],
    pub selector_digest: [u8; 16],
    pub next_position: u64,
    pub log_change_digest: [u8; 16],
    pub list_high_water: u8,
    pub requested_limit: u16,
}
impl CursorRecord {
    pub fn recipe(instance: &Instance, args: &InspectArgs) -> Result<Self, ErrorCode> {
        ToolCall::RecipeInspect(args.clone())
            .validate()
            .map_err(|_| ErrorCode::InvalidArguments)?;
        let family = match args.selector {
            InspectSelector::Catalog { .. } => Family::RecipeCatalog,
            InspectSelector::Record { .. } => Family::RecipeRecord,
            _ => return Err(ErrorCode::InvalidArguments),
        };
        let digest = args
            .recipe_digest
            .as_ref()
            .ok_or(ErrorCode::InvalidArguments)?;
        Ok(Self::base(
            family,
            unhex(instance.as_str())?,
            0,
            unhex(digest.as_str())?,
            hash(&(family as u8, &args.recipe_path, digest, &args.selector))?,
            args.limit.map_or(family.maximum(), |v| v.get() as u16),
        ))
    }
    pub fn evidence(args: &EvidenceArgs, revision: &Revision) -> Result<Self, ErrorCode> {
        ToolCall::RunEvidence(args.clone())
            .validate()
            .map_err(|_| ErrorCode::InvalidArguments)?;
        let family = match args.selector {
            EvidenceSelector::Record(_) => Family::EvidenceRecord,
            EvidenceSelector::Log(_) => Family::Log,
            _ => Family::EvidenceCatalog,
        };
        let admission = args
            .handle
            .as_str()
            .split_once(':')
            .and_then(|(_, v)| v.parse().ok())
            .ok_or(ErrorCode::InvalidArguments)?;
        Ok(Self::base(
            family,
            unhex(args.server_instance.as_str())?,
            admission,
            unhex(revision.as_str())?,
            hash(&(
                family as u8,
                &args.server_instance,
                &args.handle,
                revision,
                &args.selector,
            ))?,
            args.limit.map_or(family.maximum(), |v| v.get() as u16),
        ))
    }
    pub fn list(args: &ListArgs, high_water: u8) -> Result<Self, ErrorCode> {
        ToolCall::RunList(args.clone())
            .validate()
            .map_err(|_| ErrorCode::InvalidArguments)?;
        if high_water > 128 {
            return Err(ErrorCode::InvalidArguments);
        }
        let mut result = Self::base(
            Family::RunList,
            unhex(args.server_instance.as_str())?,
            0,
            [0; 32],
            hash(&(Family::RunList as u8, &args.server_instance))?,
            args.limit.map_or(32, |v| v.get() as u16),
        );
        result.list_high_water = high_water;
        Ok(result)
    }
    fn base(
        family: Family,
        instance: [u8; 16],
        admission: u8,
        revision: [u8; 32],
        selector_digest: [u8; 16],
        requested_limit: u16,
    ) -> Self {
        Self {
            family,
            instance,
            admission,
            revision,
            selector_digest,
            next_position: 0,
            log_change_digest: [0; 16],
            list_high_water: 0,
            requested_limit,
        }
    }
    pub fn with_log_fingerprint(mut self, canonical_bytes: &[u8]) -> Result<Self, ErrorCode> {
        if self.family != Family::Log {
            return Err(ErrorCode::InvalidCursor);
        }
        self.log_change_digest = Sha256::digest(canonical_bytes)[..16]
            .try_into()
            .expect("digest width");
        Ok(self)
    }
    pub fn change_token(&self) -> String {
        to_hex(&self.log_change_digest)
    }
    fn check(&self) -> Result<(), ErrorCode> {
        let run = matches!(
            self.family,
            Family::EvidenceCatalog | Family::EvidenceRecord | Family::Log
        );
        if (run && !(1..=128).contains(&self.admission))
            || (!run && self.admission != 0)
            || !(1..=self.family.maximum()).contains(&self.requested_limit)
            || self.next_position > UINT_MAX
            || (self.family != Family::Log && self.log_change_digest != [0; 16])
            || (self.family != Family::RunList && self.list_high_water != 0)
            || (self.family == Family::RunList
                && (self.revision != [0; 32]
                    || self.list_high_water > 128
                    || self.next_position > u64::from(self.list_high_water)))
        {
            return Err(ErrorCode::InvalidCursor);
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<Cursor, ErrorCode> {
        self.check()?;
        let mut b = [0u8; 94];
        b[0] = 1;
        b[1] = self.family as u8;
        b[2..18].copy_from_slice(&self.instance);
        b[18] = self.admission;
        b[19..51].copy_from_slice(&self.revision);
        b[51..67].copy_from_slice(&self.selector_digest);
        b[67..75].copy_from_slice(&self.next_position.to_be_bytes());
        b[75..91].copy_from_slice(&self.log_change_digest);
        b[91] = self.list_high_water;
        b[92..94].copy_from_slice(&self.requested_limit.to_be_bytes());
        Cursor::parse(&to_hex(&b)).map_err(|_| ErrorCode::InvalidCursor)
    }
    pub fn decode(cursor: &Cursor, instance: &Instance) -> Result<Self, ErrorCode> {
        let b: [u8; 94] = unhex(cursor.as_str())?;
        if b[0] != 1 {
            return Err(ErrorCode::InvalidCursor);
        }
        let family = match b[1] {
            1 => Family::RecipeCatalog,
            2 => Family::RecipeRecord,
            3 => Family::RunList,
            4 => Family::EvidenceCatalog,
            5 => Family::EvidenceRecord,
            6 => Family::Log,
            _ => return Err(ErrorCode::InvalidCursor),
        };
        let result = Self {
            family,
            instance: b[2..18].try_into().expect("fixed width"),
            admission: b[18],
            revision: b[19..51].try_into().expect("fixed width"),
            selector_digest: b[51..67].try_into().expect("fixed width"),
            next_position: u64::from_be_bytes(b[67..75].try_into().expect("fixed width")),
            log_change_digest: b[75..91].try_into().expect("fixed width"),
            list_high_water: b[91],
            requested_limit: u16::from_be_bytes(b[92..94].try_into().expect("fixed width")),
        };
        if result.instance != unhex::<16>(instance.as_str())? {
            return Err(ErrorCode::StaleInstance);
        }
        result.check()?;
        Ok(result)
    }
    /// `expected` binds freshly loaded arguments/snapshot/fingerprint. For lists,
    /// retain the decoded high-water after checking it against current membership.
    /// A continuation never points at an empty collection or its final boundary.
    pub fn validate(&self, expected: &Self, total: u64) -> Result<(), ErrorCode> {
        self.check()?;
        expected.check()?;
        if self.instance != expected.instance {
            return Err(ErrorCode::StaleInstance);
        }
        if self.family != expected.family || self.admission != expected.admission {
            return Err(ErrorCode::InvalidCursor);
        }
        if self.revision != expected.revision {
            return Err(
                if matches!(self.family, Family::RecipeCatalog | Family::RecipeRecord) {
                    ErrorCode::RecipeChanged
                } else {
                    ErrorCode::RevisionChanged
                },
            );
        }
        if self.selector_digest != expected.selector_digest
            || self.requested_limit != expected.requested_limit
            || self.list_high_water != expected.list_high_water
        {
            return Err(ErrorCode::InvalidCursor);
        }
        if self.log_change_digest != expected.log_change_digest {
            return Err(ErrorCode::LogChanged);
        }
        if total > UINT_MAX
            || self.next_position >= total
            || (self.family == Family::RunList && total != u64::from(self.list_high_water))
        {
            return Err(ErrorCode::InvalidCursor);
        }
        Ok(())
    }
}
impl Family {
    pub fn maximum(self) -> u16 {
        if matches!(
            self,
            Self::RecipeCatalog | Self::RunList | Self::EvidenceCatalog
        ) {
            32
        } else {
            4096
        }
    }
}
fn unhex<const N: usize>(text: &str) -> Result<[u8; N], ErrorCode> {
    if text.len() != N * 2 {
        return Err(ErrorCode::InvalidCursor);
    }
    let digit = |b| match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        _ => Err(ErrorCode::InvalidCursor),
    };
    let mut bytes = [0; N];
    for (i, pair) in text.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        bytes[i] = (digit(pair[0])? << 4) | digit(pair[1])?;
    }
    Ok(bytes)
}
fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        text.push(HEX[(b >> 4) as usize] as char);
        text.push(HEX[(b & 15) as usize] as char);
    }
    text
}
struct HashWriter(Sha256);
impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn hash(value: &impl Serialize) -> Result<[u8; 16], ErrorCode> {
    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer(&mut writer, value).map_err(|_| ErrorCode::InvalidArguments)?;
    Ok(writer.0.finalize()[..16].try_into().expect("digest width"))
}
#[cfg(test)]
#[path = "cursor_tests.rs"]
mod tests;
