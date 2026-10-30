#![allow(dead_code)]
use std::io::Write;

pub fn header(
    name: &str,
    kind: u8,
    data_size: u64,
    mode: u32,
    uid: u64,
    gid: u64,
    link: &str,
) -> [u8; 512] {
    let mut h = [0u8; 512];
    assert!(name.len() <= 100 && link.len() <= 100);
    h[..name.len()].copy_from_slice(name.as_bytes());
    octal(&mut h[100..108], mode as u64);
    octal(&mut h[108..116], uid);
    octal(&mut h[116..124], gid);
    octal(&mut h[124..136], data_size);
    octal(&mut h[136..148], 0);
    h[148..156].fill(b' ');
    h[156] = kind;
    h[157..157 + link.len()].copy_from_slice(link.as_bytes());
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    let sum: u64 = h.iter().map(|&b| b as u64).sum();
    let checksum = format!("{sum:06o}\0 ");
    h[148..156].copy_from_slice(checksum.as_bytes());
    h
}
fn octal(dst: &mut [u8], n: u64) {
    let text = format!("{:0width$o}\0", n, width = dst.len() - 1);
    dst.copy_from_slice(text.as_bytes());
}
pub struct Tar {
    bytes: Vec<u8>,
}
impl Tar {
    pub fn new() -> Self {
        Self { bytes: Vec::new() }
    }
    // Literal tar header fields keep malformed fixtures independently readable.
    #[allow(clippy::too_many_arguments)]
    pub fn entry(
        mut self,
        name: &str,
        kind: u8,
        data: &[u8],
        mode: u32,
        uid: u64,
        gid: u64,
        link: &str,
    ) -> Self {
        self.bytes
            .extend(header(name, kind, data.len() as u64, mode, uid, gid, link));
        self.bytes.write_all(data).unwrap();
        self.bytes.resize(self.bytes.len().next_multiple_of(512), 0);
        self
    }
    pub fn root(self) -> Self {
        self.entry("bin", b'5', b"", 0o755, 0, 0, "")
    }
    pub fn file(self, name: &str, data: &[u8]) -> Self {
        self.entry(name, b'0', data, 0o644, 10001, 10001, "")
    }
    pub fn finish(mut self) -> Vec<u8> {
        self.bytes.extend([0; 1024]);
        self.bytes
    }
}

pub mod plugin;
