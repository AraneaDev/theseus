//! Exact bounded fixture definitions shared by setup, installer and reader.
#![allow(dead_code)] // Each standalone fixture binary uses a different platform subset.
use theseus_native_protocol::v2;
fn quote(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
    )
}
pub fn definition_bytes(definition: &v2::ServiceDefinition) -> Vec<u8> {
    let command = definition
        .arguments
        .iter()
        .map(|arg| quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    let mut text = format!(
        "[Unit]\nDescription=Theseus fixture\nDefaultDependencies=no\n[Service]\nType=simple\nRestart=no\nExecStart={command}\nWorkingDirectory={}\n",
        quote(&definition.working_directory)
    );
    for env in &definition.environment {
        text.push_str(&format!("Environment={}\n", quote(env)));
    }
    text.into_bytes()
}

fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
/// No launchd defaults/environment or run-at-load behavior. The exact template
/// is hashed into each manifest; bootstrap leaves the foreground job dormant.
pub fn launchd_definition_bytes(label: &str, definition: &v2::ServiceDefinition) -> Vec<u8> {
    let args = definition
        .arguments
        .iter()
        .map(|arg| format!("<string>{}</string>", xml(arg)))
        .collect::<String>();
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array>{args}</array><key>WorkingDirectory</key><string>{}</string><key>RunAtLoad</key><false/><key>KeepAlive</key><false/></dict></plist>\n", xml(label), xml(&definition.working_directory)).into_bytes()
}

/// Immutable, provider-attested input to the SCM executable's no-argument entry.
/// The exact bytes are pinned by setup before the first accepted Start.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScmReaderConfig {
    pub kind: String,
    pub schema: u32,
    pub service_name: String,
    pub endpoint: String,
    pub data: String,
    pub cooperative_instance: String,
    pub fault: String,
    pub helper_sid: String,
    pub service_sid: String,
}
impl ScmReaderConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        use anyhow::ensure;
        let literal = |s: &str, max: usize| {
            !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
        };
        let name = |s: &str| {
            literal(s, 128)
                && !s.starts_with('-')
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        };
        let sid = |s: &str| {
            s.starts_with("S-1-")
                && literal(s, 256)
                && s[4..]
                    .split('-')
                    .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        };
        ensure!(
            self.kind == "theseus_native_scm_reader" && self.schema == 1,
            "wrong SCM reader schema"
        );
        ensure!(
            name(&self.service_name),
            "explicit SCM service name required"
        );
        ensure!(
            self.endpoint.starts_with(r"\\.\pipe\theseus.")
                && self.endpoint.len() <= 256
                && name(&self.endpoint[9..]),
            "explicit local readiness pipe required"
        );
        ensure!(
            v2::platform_absolute(&self.data, v2::ManagerPlatform::Scm),
            "explicit SCM data path required"
        );
        ensure!(
            literal(&self.cooperative_instance, 128),
            "immutable installation instance required"
        );
        ensure!(
            sid(&self.helper_sid) && self.service_sid == "S-1-5-18",
            "explicit helper and LocalSystem principals required"
        );
        ensure!(
            [
                "none",
                "missing_service",
                "leaked_service",
                "wrong_version",
                "wrong_data",
                "wrong_nonce",
                "wrong_peer"
            ]
            .contains(&self.fault.as_str()),
            "unknown fixture fault"
        );
        Ok(())
    }
}

/// Measures the current token user only to compare with an already supplied SID.
/// It never selects a principal, logs on, impersonates, or creates another token.
#[cfg(windows)]
pub fn verify_current_sid(expected: &str) -> anyhow::Result<()> {
    use std::{ffi::c_void, io, ptr};
    type Handle = *mut c_void;
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
        fn GetTokenInformation(
            token: Handle,
            class: u32,
            buffer: *mut c_void,
            size: u32,
            required: *mut u32,
        ) -> i32;
        fn ConvertSidToStringSidW(sid: *const c_void, text: *mut *mut u16) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> Handle;
        fn CloseHandle(handle: Handle) -> i32;
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }
    struct Token(Handle);
    impl Drop for Token {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }
    struct Text(*mut u16);
    impl Drop for Text {
        fn drop(&mut self) {
            unsafe { LocalFree(self.0.cast()) };
        }
    }
    let mut token = ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), 8, &mut token) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    let token = Token(token);
    let mut needed = 0;
    // Fixed aligned buffer bounds the TOKEN_USER and native SID metadata.
    let mut buffer = [0usize; 128];
    if unsafe {
        GetTokenInformation(
            token.0,
            1,
            buffer.as_mut_ptr().cast(),
            std::mem::size_of_val(&buffer) as u32,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    anyhow::ensure!(
        needed as usize <= std::mem::size_of_val(&buffer)
            && needed as usize >= std::mem::size_of::<usize>(),
        "bounded TOKEN_USER required"
    );
    let sid = buffer[0] as *const c_void;
    let start = buffer.as_ptr() as usize;
    anyhow::ensure!(
        (sid as usize) >= start + std::mem::size_of::<usize>()
            && (sid as usize) + 8 <= start + needed as usize,
        "TOKEN_USER SID outside native buffer"
    );
    let mut text = ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    let text = Text(text);
    anyhow::ensure!(!text.0.is_null(), "native SID text missing");
    let mut length = 0;
    while length <= 256 && unsafe { *text.0.add(length) } != 0 {
        length += 1;
    }
    anyhow::ensure!(length <= 256, "native SID text bound");
    let actual = String::from_utf16(unsafe { std::slice::from_raw_parts(text.0, length) })?;
    anyhow::ensure!(
        actual == expected,
        "actual process token user differs from explicit fixture SID"
    );
    Ok(())
}

#[cfg(test)]
mod platform_tests {
    use super::*;
    fn config() -> ScmReaderConfig {
        ScmReaderConfig {
            kind: "theseus_native_scm_reader".into(),
            schema: 1,
            service_name: "theseus.fixture".into(),
            endpoint: r"\\.\pipe\theseus.fixture".into(),
            data: r"C:\fixture\data".into(),
            cooperative_instance: "provider-attested-installation-instance".into(),
            fault: "none".into(),
            helper_sid: "S-1-5-21-123-456-789-1001".into(),
            service_sid: "S-1-5-18".into(),
        }
    }
    #[test]
    fn scm_reader_config_requires_closed_explicit_installation_input() {
        let original = config();
        original.validate().unwrap();
        assert_eq!(
            serde_json::from_slice::<ScmReaderConfig>(&serde_json::to_vec(&original).unwrap())
                .unwrap(),
            original
        );
        for field in [
            "kind",
            "service_name",
            "endpoint",
            "data",
            "cooperative_instance",
            "helper_sid",
            "service_sid",
            "fault",
        ] {
            let mut json = serde_json::to_value(&original).unwrap();
            json[field] = "".into();
            assert!(
                serde_json::from_value::<ScmReaderConfig>(json)
                    .unwrap()
                    .validate()
                    .is_err(),
                "{field}"
            );
        }
        let mut unknown = serde_json::to_value(&original).unwrap();
        unknown["discover_domain"] = true.into();
        assert!(serde_json::from_value::<ScmReaderConfig>(unknown).is_err());
    }
    #[test]
    fn scm_reader_config_refuses_remote_pipe_wrong_principal_and_unbound_fault() {
        let original = config();
        for (field, value) in [
            ("endpoint", r"\\server\pipe\theseus.fixture"),
            ("data", "relative/data"),
            ("service_sid", "S-1-5-32-544"),
            ("fault", "discover"),
            ("cooperative_instance", "bad\ninstance"),
            ("service_name", "other/service"),
        ] {
            let mut json = serde_json::to_value(&original).unwrap();
            json[field] = value.into();
            assert!(
                serde_json::from_value::<ScmReaderConfig>(json)
                    .unwrap()
                    .validate()
                    .is_err(),
                "{field}"
            );
        }
    }
}
