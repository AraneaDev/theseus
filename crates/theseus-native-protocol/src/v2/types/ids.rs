//! Hex helpers, fixed-width identities, bounded newtypes and identity digests.
use super::super::{Error, MAX_FILE_BYTES, MAX_PROGRAMS};
use super::MAX_NATIVE_FILE_BYTES;
fn parse_hex<const N: usize>(v: &str) -> Result<[u8; N], Error> {
    if v.len() != N * 2 {
        return Err(Error::InvalidValue);
    }
    fn nibble(v: u8) -> Result<u8, Error> {
        match v {
            b'0'..=b'9' => Ok(v - b'0'),
            b'a'..=b'f' => Ok(v - b'a' + 10),
            _ => Err(Error::InvalidValue),
        }
    }
    let mut out = [0; N];
    let (pairs, _) = v.as_bytes().as_chunks::<2>();
    for (target, pair) in out.iter_mut().zip(pairs) {
        *target = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Ok(out)
}
fn hex(v: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(v.len() * 2);
    for b in v {
        out.push(H[(b >> 4) as usize] as char);
        out.push(H[(b & 15) as usize] as char);
    }
    out
}
macro_rules! identity {
    ($name:ident,$len:literal) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name([u8; $len]);
        impl $name {
            pub fn from_bytes(v: [u8; $len]) -> Self {
                Self(v)
            }
            pub fn parse(v: &str) -> Result<Self, Error> {
                parse_hex(v).map(Self)
            }
            pub fn as_bytes(&self) -> &[u8; $len] {
                &self.0
            }
            pub fn to_hex(&self) -> String {
                hex(&self.0)
            }
        }
    };
}
identity!(BindingId, 16);
identity!(WorkspaceMarker, 16);
identity!(IntentId, 16);
identity!(LeaseId, 16);
identity!(OperationId, 16);
identity!(Digest, 32);
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProgramId(String);
impl ProgramId {
    pub fn new(v: &str) -> Result<Self, Error> {
        let b = v.as_bytes();
        if b.is_empty()
            || b.len() > 32
            || !b[0].is_ascii_lowercase()
            || !b
                .iter()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_-".contains(c))
        {
            return Err(Error::InvalidValue);
        }
        Ok(Self(v.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeafName(String);
impl LeafName {
    pub fn new(v: &str) -> Result<Self, Error> {
        let b = v.as_bytes();
        if b.is_empty()
            || b.len() > 64
            || !b[0].is_ascii_alphanumeric()
            || b.last() == Some(&b'.')
            || !b
                .iter()
                .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(c))
        {
            return Err(Error::InvalidValue);
        }
        let stem = v.split('.').next().ok_or(Error::InvalidValue)?;
        if ["CON", "PRN", "AUX", "NUL"]
            .iter()
            .any(|r| stem.eq_ignore_ascii_case(r))
            || (stem.len() == 4
                && (stem[..3].eq_ignore_ascii_case("COM") || stem[..3].eq_ignore_ascii_case("LPT"))
                && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        {
            return Err(Error::InvalidValue);
        }
        Ok(Self(v.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
/// Lexical metadata only; native SID validation remains a host obligation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidText(String);
impl SidText {
    pub fn new(v: &str) -> Result<Self, Error> {
        if !(5..=256).contains(&v.len())
            || !v.starts_with("S-1-")
            || !v.as_bytes()[4..]
                .iter()
                .all(|c| c.is_ascii_hexdigit() || b"x-".contains(c))
        {
            return Err(Error::InvalidValue);
        }
        Ok(Self(v.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileBytes(Vec<u8>);
impl FileBytes {
    pub fn new(v: &[u8]) -> Result<Self, Error> {
        if v.len() > MAX_FILE_BYTES {
            return Err(Error::LimitExceeded);
        }
        Ok(Self(v.to_vec()))
    }
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgramList(Vec<ProgramId>);
impl ProgramList {
    pub fn new(v: Vec<ProgramId>) -> Result<Self, Error> {
        if v.is_empty() || v.len() > MAX_PROGRAMS {
            return Err(Error::LimitExceeded);
        }
        if v.windows(2).any(|p| p[0] >= p[1]) {
            return Err(Error::InvalidValue);
        }
        Ok(Self(v))
    }
    pub fn as_slice(&self) -> &[ProgramId] {
        &self.0
    }
}
/// Domain-separated commitment to the catalog's ordered base and target artifacts.
pub fn artifact_commitment(
    base_length: u64,
    base: Digest,
    target_length: u64,
    target: Digest,
) -> Result<Digest, super::super::Error> {
    use sha2::{Digest as _, Sha256};
    if !(1..=MAX_NATIVE_FILE_BYTES).contains(&base_length)
        || !(1..=MAX_NATIVE_FILE_BYTES).contains(&target_length)
    {
        return Err(super::super::Error::InvalidValue);
    }
    let mut hash = Sha256::new();
    hash.update(b"theseus-native-artifacts-v1\0");
    hash.update(base_length.to_be_bytes());
    hash.update(base.0);
    hash.update(target_length.to_be_bytes());
    hash.update(target.0);
    Ok(Digest::from_bytes(hash.finalize().into()))
}

/// Immutable config3/store commitment. Component order and domain are protocol constants.
pub fn integration_manifest_digest(core: Digest, integration: Digest) -> Digest {
    use sha2::{Digest as _, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"theseus.native_integration_v1.manifest\0");
    hash.update(core.0);
    hash.update(integration.0);
    Digest::from_bytes(hash.finalize().into())
}
