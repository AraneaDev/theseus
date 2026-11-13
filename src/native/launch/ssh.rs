//! OpenSSH subsystem argv construction and effective-option qualification.
use anyhow::{Result, ensure};
use theseus_host::process::{ChildEnvironment, NativeCommand};

/// Inert argv construction only. Acceptance still requires actual OpenSSH10.x
/// version and effective-option qualification using dedicated runtime material.
#[derive(Clone, Copy)]
pub struct SshMaterialPaths<'a> {
    pub identity: &'a std::path::Path,
    pub host_keys: &'a std::path::Path,
    pub empty_global: &'a std::path::Path,
}
pub fn openssh_subsystem_argv(
    program: &std::path::Path,
    host: &str,
    port: u16,
    user: &str,
    subsystem: &str,
    material: SshMaterialPaths<'_>,
) -> Result<NativeCommand> {
    ensure!(
        program.is_absolute() && port > 0,
        "invalid SSH program/port"
    );
    ensure!(valid_host(host), "invalid SSH host");
    ensure!(
        !user.is_empty()
            && user.len() <= 64
            && user
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "invalid SSH account"
    );
    ensure!(
        !subsystem.is_empty()
            && subsystem.len() <= 32
            && subsystem.as_bytes()[0].is_ascii_alphabetic()
            && subsystem
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
        "invalid SSH subsystem"
    );
    let path = |p: &std::path::Path| -> Result<String> {
        let s = p
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("SSH option path must be UTF-8"))?;
        // OpenSSH option values accept forward slashes on Windows. Normalize
        // native separators before checking the same closed literal alphabet.
        #[cfg(windows)]
        let s = {
            // canonicalize() can produce a verbatim drive prefix. Removing only
            // that prefix preserves the absolute drive path; other verbatim
            // namespaces remain outside the literal option profile.
            let ordinary = s
                .strip_prefix(r"\\?\")
                .filter(|tail| {
                    let bytes = tail.as_bytes();
                    bytes.len() >= 3
                        && bytes[0].is_ascii_alphabetic()
                        && bytes[1] == b':'
                        && matches!(bytes[2], b'\\' | b'/')
                })
                .unwrap_or(s);
            ordinary.replace('\\', "/")
        };
        #[cfg(not(windows))]
        let s = s.to_owned();
        ensure!(
            p.is_absolute()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/._-:".contains(&b)),
            "SSH option path permits expansion or separators"
        );
        Ok(s)
    };
    let identity = path(material.identity)?;
    let host_keys = path(material.host_keys)?;
    let empty_global = path(material.empty_global)?;
    let mut command = NativeCommand::new(program);
    command.environment = ChildEnvironment::Replace(vec![]);
    command.args([
        "-F",
        "none",
        "-T",
        "-s",
        "-S",
        "none",
        "-p",
        &port.to_string(),
        "-l",
        user,
        "-i",
        &identity,
    ]);
    for option in [
        "BatchMode=yes",
        "IdentitiesOnly=yes",
        "IdentityAgent=none",
        "PreferredAuthentications=publickey",
        "PasswordAuthentication=no",
        "KbdInteractiveAuthentication=no",
        "StrictHostKeyChecking=yes",
        "UpdateHostKeys=no",
        "VerifyHostKeyDNS=no",
        "ProxyCommand=none",
        "ProxyJump=none",
        "ControlMaster=no",
        "ControlPersist=no",
        "ClearAllForwardings=yes",
        "ForwardAgent=no",
        "ForwardX11=no",
        "Tunnel=no",
        "PermitLocalCommand=no",
        "CanonicalizeHostname=no",
        "PKCS11Provider=none",
    ] {
        command.args(["-o", option]);
    }
    command.args([
        "-o",
        &format!("UserKnownHostsFile={host_keys}"),
        "-o",
        &format!("GlobalKnownHostsFile={empty_global}"),
        host,
        subsystem,
    ]);
    Ok(command)
}

pub(super) fn verify_effective_options(command: &NativeCommand, text: &str) -> Result<()> {
    let args = command
        .args
        .iter()
        .map(|s| {
            s.to_str()
                .ok_or_else(|| anyhow::anyhow!("nonUTF8 SSH option"))
        })
        .collect::<Result<Vec<_>>>()?;
    for pair in args.windows(2).filter(|p| p[0] == "-o") {
        let (key, value) = pair[1]
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("invalid SSH option"))?;
        let lines = text
            .lines()
            .filter_map(|line| line.split_once(' '))
            .filter(|(name, _)| name.eq_ignore_ascii_case(key))
            .collect::<Vec<_>>();
        ensure!(
            (lines.len() == 1 && equivalent_option(lines[0].1.trim(), value))
                || (lines.is_empty()
                    && value == "none"
                    && matches!(
                        key.to_ascii_lowercase().as_str(),
                        "proxycommand" | "proxyjump" | "pkcs11provider"
                    )),
            "OpenSSH effective option profile mismatch: {}",
            key
        );
    }
    Ok(())
}

fn valid_host(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    if host.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    if host.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return false;
    }
    let dns = host.strip_suffix('.').unwrap_or(host);
    dns.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label.as_bytes()[0].is_ascii_alphanumeric()
            && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

fn equivalent_option(actual: &str, expected: &str) -> bool {
    actual == expected || matches!((actual, expected), ("true", "yes") | ("false", "no"))
}
pub(super) fn verify_transport_selection(command: &NativeCommand, text: &str) -> Result<()> {
    let args = command
        .args
        .iter()
        .map(|s| {
            s.to_str()
                .ok_or_else(|| anyhow::anyhow!("nonUTF8 SSH selection"))
        })
        .collect::<Result<Vec<_>>>()?;
    let option = |flag: &str| {
        args.windows(2)
            .find(|p| p[0] == flag)
            .map(|p| p[1])
            .ok_or_else(|| anyhow::anyhow!("missing SSH selection"))
    };
    let one = |name: &str, value: &str| -> Result<()> {
        let lines = text
            .lines()
            .filter_map(|l| l.split_once(' '))
            .filter(|(k, _)| *k == name)
            .collect::<Vec<_>>();
        ensure!(
            lines.len() == 1 && equivalent_option(lines[0].1.trim(), value),
            "OpenSSH effective selection differs: {}",
            name
        );
        Ok(())
    };
    one("user", option("-l")?)?;
    one("port", option("-p")?)?;
    one("identityfile", option("-i")?)?;
    one("requesttty", "no")?;
    one("sessiontype", "subsystem")?;
    one("host", args[args.len() - 2])?;
    one("hostname", args[args.len() - 2])?;
    for name in [
        "certificatefile",
        "localforward",
        "remoteforward",
        "dynamicforward",
        "remotecommand",
        "controlpath",
    ] {
        ensure!(
            !text
                .lines()
                .filter_map(|l| l.split_once(' '))
                .any(|(k, v)| k == name && v.trim() != "none"),
            "OpenSSH unwanted configured effect: {}",
            name
        );
    }
    Ok(())
}

pub(super) fn unencrypted_private_key(key: &[u8]) -> bool {
    if key.windows(b"ENCRYPTED".len()).any(|s| s == b"ENCRYPTED") {
        return false;
    }
    if key.starts_with(b"-----BEGIN RSA PRIVATE KEY-----")
        || key.starts_with(b"-----BEGIN PRIVATE KEY-----")
    {
        return true;
    }
    if !key.starts_with(b"-----BEGIN OPENSSH PRIVATE KEY-----") {
        return false;
    }
    {
        // Decode only the bounded public container header, never format key bytes.
        let Some(newline) = key.iter().position(|b| *b == b'\n') else {
            return false;
        };
        let mut prefix = Vec::new();
        let mut acc = 0u32;
        let mut bits = 0;
        for c in key[newline + 1..]
            .iter()
            .copied()
            .filter(|b| !b.is_ascii_whitespace())
            .take(128)
        {
            let v = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => break,
            };
            acc = (acc << 6) | u32::from(v);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                prefix.push((acc >> bits) as u8)
            }
        }
        prefix.starts_with(b"openssh-key-v1\0\0\0\0\x04none\0\0\0\x04none")
            && !prefix.windows(3).any(|p| p == b"sk-")
            && !prefix.windows(9).any(|p| p == b"cert-v01@")
    }
}
