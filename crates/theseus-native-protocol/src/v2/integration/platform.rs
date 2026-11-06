//! Closed platform authority and raw next-start commitments. No host effects.
use super::*;
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagerPlatform {
    #[default]
    Systemd,
    Launchd,
    Scm,
}
impl ManagerPlatform {
    pub fn is_systemd(&self) -> bool {
        *self == Self::Systemd
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LaunchdDomain {
    User { uid: u32 },
    Gui { uid: u32 },
    System,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchdQualification {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absence_profile: Option<LaunchdAbsenceQualification>,
    pub os_version: String,
    pub os_build: String,
    pub api_profile: String,
    pub receipt_sha256: String,
    pub launchctl_fingerprint: Vec<u8>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchdAbsenceQualification {
    pub profile: String,
    pub exit_code: i32,
    pub stderr: String,
    pub receipt_sha256: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScmContext {
    LocalMachine,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScmPreflight {
    pub helper_sid: String,
    pub configured_account_sid: String,
    pub executable: String,
    pub definition: String,
    pub data: String,
    pub pipe: String,
    pub executable_and_definition_read_execute: bool,
    pub data_declared_modify_only: bool,
    pub pipe_system_and_helper_only: bool,
    pub pipe_rejects_remote_clients: bool,
    pub no_other_acl_grants: bool,
    pub exact_access_receipt: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScmFailureAction {
    pub kind: u32,
    pub delay_ms: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScmDefinition {
    pub command_line: Vec<u16>,
    pub display_name: Vec<u16>,
    pub account: Vec<u16>,
    pub service_type: u32,
    pub start_type: u32,
    pub error_control: u32,
    pub load_order_group: Vec<u16>,
    pub dependencies: Vec<Vec<u16>>,
    pub tag_id: u32,
    pub delayed_auto_start: bool,
    pub failure_actions: Vec<ScmFailureAction>,
    pub failure_reset_period: u32,
    pub failure_reboot_message: Vec<u16>,
    pub failure_command: Vec<u16>,
    pub failure_actions_on_non_crash: bool,
    pub service_sid_type: u32,
    pub required_privileges: Vec<Vec<u16>>,
    pub triggers_present: bool,
    pub launch_protected: u32,
}
fn literal(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}
fn name(s: &str) -> bool {
    literal(s, 128)
        && !s.starts_with('-')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
pub fn platform_absolute(s: &str, platform: ManagerPlatform) -> bool {
    if !literal(s, 4096) {
        return false;
    }
    if platform == ManagerPlatform::Scm {
        let b = s.as_bytes();
        b.len() > 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && b[2] == b'\\'
            && !s[3..]
                .split('\\')
                .any(|p| p.is_empty() || p == "." || p == "..")
            && !s.contains(['/', '"'])
    } else {
        s.starts_with('/')
            && (s == "/"
                || !s[1..]
                    .split('/')
                    .any(|p| p.is_empty() || p == "." || p == ".."))
    }
}
fn sid(s: &str) -> bool {
    s.starts_with("S-1-")
        && s.len() <= 256
        && s[4..]
            .split('-')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}
fn named_pipe(s: &str) -> bool {
    s.starts_with("\\\\.\\pipe\\theseus.") && s.len() <= 256 && name(&s[9..])
}
impl ServiceAuthority {
    pub fn platform(&self) -> ManagerPlatform {
        match self {
            Self::Systemd { .. } => ManagerPlatform::Systemd,
            Self::Launchd { .. } => ManagerPlatform::Launchd,
            Self::Scm { .. } => ManagerPlatform::Scm,
        }
    }
    pub fn api_profile(&self) -> &str {
        match self {
            Self::Systemd { qualification, .. } | Self::Scm { qualification, .. } => {
                &qualification.api_profile
            }
            Self::Launchd { qualification, .. } => &qualification.api_profile,
        }
    }
    pub fn validate_context(
        &self,
        c: &IntegrationRuntimeContext,
        os: super::super::Os,
    ) -> Result<(), Error> {
        self.validate()?;
        c.validate_os(os)?;
        if c.manager_platform != Some(self.platform())
            || !matches!(
                (os, self.platform()),
                (super::super::Os::Linux, ManagerPlatform::Systemd)
                    | (super::super::Os::Macos, ManagerPlatform::Launchd)
                    | (super::super::Os::Windows, ManagerPlatform::Scm)
            )
        {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
    pub(super) fn validate_launchd(&self) -> Result<(), Error> {
        let Self::Launchd {
            slot,
            domain,
            label,
            definition_root,
            exclusive_load_roots,
            launchctl,
            qualification: q,
            managed_by,
            sut_effects,
            allowed_actions,
        } = self
        else {
            return Err(Error::InvalidValue);
        };
        ResourceSlot::new(slot.as_str())?;
        Digest::parse(&q.receipt_sha256)?;
        if !matches!(domain, LaunchdDomain::User { .. })
            || !name(label)
            || launchctl != "/bin/launchctl"
            || !platform_absolute(definition_root, ManagerPlatform::Launchd)
            || exclusive_load_roots.is_empty()
            || exclusive_load_roots.len() > 16
            || !exclusive_load_roots.contains(definition_root)
            || exclusive_load_roots
                .iter()
                .any(|p| !platform_absolute(p, ManagerPlatform::Launchd))
            || exclusive_load_roots
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != exclusive_load_roots.len()
            || !literal(&q.os_version, 128)
            || !literal(&q.os_build, 128)
            || q.api_profile != "launchctl-qualified-v1"
            || q.launchctl_fingerprint.len() != 57
            || q.launchctl_fingerprint.first() != Some(&1)
            || managed_by != "installer"
            || *sut_effects != ["registration", "configuration", "removal"]
            || *allowed_actions != ["inspect", "start", "stop"]
        {
            return Err(Error::InvalidValue);
        }
        if let Some(absence) = &q.absence_profile {
            Digest::parse(&absence.receipt_sha256)?;
            let LaunchdDomain::User { uid } = domain else {
                return Err(Error::InvalidValue);
            };
            // Exact launchctl forms only; `uid:` is the form observed on a real
            // macOS 15.8.2 runner, `user:` the previously modelled form.
            let exact = ["user", "uid"].iter().any(|kind| {
                let diagnostic =
                    format!("Could not find service \"{label}\" in domain for {kind}: {uid}\n");
                absence.stderr == diagnostic
                    || absence.stderr == format!("Bad request.\n{diagnostic}")
            });
            if absence.profile != "launchctl-user-no-service-v1"
                || !(1..=255).contains(&absence.exit_code)
                || !exact
            {
                return Err(Error::InvalidValue);
            }
        }
        Ok(())
    }
    pub(super) fn validate_scm(&self) -> Result<(), Error> {
        let Self::Scm {
            slot,
            service_name,
            qualification: q,
            allowed_access,
            allow_local_system,
            preflight: p,
            managed_by,
            sut_effects,
            allowed_actions,
            ..
        } = self
        else {
            return Err(Error::InvalidValue);
        };
        ResourceSlot::new(slot.as_str())?;
        Digest::parse(&q.receipt_sha256)?;
        Digest::parse(&p.exact_access_receipt)?;
        if !name(service_name)
            || q.manager != "windows_scm"
            || !canonical_windows_version(&q.version)
            || q.api_profile != "scm_own_process_v1"
            || q.features
                != [
                    "query_config",
                    "query_status",
                    "start_stop",
                    "original_handle",
                ]
            || *allowed_access != 0x35
            || !allow_local_system
            || p.configured_account_sid != "S-1-5-18"
            || !sid(&p.helper_sid)
            || [&p.executable, &p.definition, &p.data]
                .iter()
                .any(|v| !platform_absolute(v, ManagerPlatform::Scm))
            || !named_pipe(&p.pipe)
            || !p.executable_and_definition_read_execute
            || !p.data_declared_modify_only
            || !p.pipe_system_and_helper_only
            || !p.pipe_rejects_remote_clients
            || !p.no_other_acl_grants
            || managed_by != "installer"
            || *sut_effects != ["registration", "configuration", "removal"]
            || *allowed_actions != ["inspect", "start", "stop"]
        {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
}
impl ScmDefinition {
    pub fn digest(&self) -> Result<Digest, Error> {
        use sha2::{Digest as _, Sha256};
        let mut h = Sha256::new();
        h.update(b"theseus.native_integration_v1.scm_next_start\0");
        h.update(serde_json::to_vec(self).map_err(|_| Error::InvalidValue)?);
        Ok(Digest::from_bytes(h.finalize().into()))
    }
    pub fn validate(&self, d: &ServiceDefinition, a: &ServiceAuthority) -> Result<(), Error> {
        let ServiceAuthority::Scm { preflight, .. } = a else {
            return Err(Error::InvalidValue);
        };
        let quoted = format!("\"{}\"", preflight.executable);
        if d.executable != preflight.executable
            || d.arguments != [d.executable.clone()]
            || !d.environment.is_empty()
            || self.command_line != quoted.encode_utf16().collect::<Vec<_>>()
            || self.display_name.is_empty()
            || self.display_name.len() > 256
            || self.display_name.contains(&0)
            || self.account != "LocalSystem".encode_utf16().collect::<Vec<_>>()
            || self.service_type != 0x10
            || self.start_type != 3
            || self.error_control != 1
            || !self.load_order_group.is_empty()
            || !self.dependencies.is_empty()
            || self.tag_id != 0
            || self.delayed_auto_start
            || !self.failure_actions.is_empty()
            || self.failure_reset_period != 0
            || !self.failure_reboot_message.is_empty()
            || !self.failure_command.is_empty()
            || self.failure_actions_on_non_crash
            || self.service_sid_type != 0
            || !self.required_privileges.is_empty()
            || self.triggers_present
            || self.launch_protected != 0
        {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
}
impl IntegrationReadiness {
    pub(super) fn valid_endpoint(&self, p: ManagerPlatform) -> bool {
        if p == ManagerPlatform::Systemd {
            if self.cooperative_instance.is_some() {
                return false;
            }
        } else if !self
            .cooperative_instance
            .as_deref()
            .is_some_and(|s| literal(s, 128))
        {
            return false;
        }
        match (&self.endpoint, p) {
            (
                LocalEndpoint::UnixPath { path },
                ManagerPlatform::Systemd | ManagerPlatform::Launchd,
            ) => {
                self.peer_sid.is_none()
                    && self.peer_uid.is_some()
                    && self.peer_gid.is_some()
                    && platform_absolute(&self.root, p)
                    && path.len() <= 100
                    && std::path::Path::new(path).parent() == Some(std::path::Path::new(&self.root))
            }
            (LocalEndpoint::NamedPipe { path }, ManagerPlatform::Scm) => {
                self.peer_sid.as_deref() == Some("S-1-5-18")
                    && self.peer_uid.is_none()
                    && self.peer_gid.is_none()
                    && named_pipe(path)
                    && platform_absolute(&self.root, p)
            }
            _ => false,
        }
    }
}

/// Additional independent manager facts; none prove application readiness or a token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServicePlatformFacts {
    Launchd {
        definition_present: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        loaded_registration_present: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        runtime_state: Option<LaunchdRuntimeState>,
        registration_generation_available: bool,
        command_evidence_sha256: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command_exit_code: Option<i32>,
        output_truncated: bool,
    },
    Scm {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_start: Option<Box<ScmDefinition>>,
        registration: ScmRegistrationState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<ScmStatus>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        configured_account_sid: Option<String>,
        child_token_measured: bool,
        original_handle_retained: bool,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchdRuntimeState {
    Running,
    NotRunning,
    Waiting,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScmRegistrationState {
    Absent,
    Present,
    DeletePending,
    SameNamePresent,
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScmStatus {
    pub service_type: u32,
    pub state: u32,
    pub controls_accepted: u32,
    pub win32_exit_code: u32,
    pub service_specific_exit_code: u32,
    pub checkpoint: u32,
    pub wait_hint_ms: u32,
    pub process_id: u32,
    pub service_flags: u32,
}
impl ServicePlatformFacts {
    pub fn validate(&self) -> Result<(), Error> {
        match self {
            Self::Launchd {
                registration_generation_available,
                command_evidence_sha256,
                ..
            } => {
                Digest::parse(command_evidence_sha256)?;
                if *registration_generation_available {
                    return Err(Error::InvalidValue);
                }
            }
            Self::Scm {
                next_start,
                configured_account_sid,
                child_token_measured,
                ..
            } => {
                if let Some(config) = next_start {
                    config.validate_observation()?;
                }
                if *child_token_measured
                    || configured_account_sid
                        .as_ref()
                        .is_some_and(|s| s != "S-1-5-18")
                {
                    return Err(Error::InvalidValue);
                }
            }
        }
        Ok(())
    }
}

impl ServiceObservation {
    pub fn validate(&self) -> Result<(), Error> {
        ResourceSlot::new(self.slot.as_str())?;
        Digest::parse(&self.binding_digest)?;
        if let Some(d) = &self.definition_digest {
            Digest::parse(d)?;
        }
        if self.manager_generation.is_empty()
            || self.manager_generation.len() > 128
            || self.manager_generation.contains('\0')
            || self
                .origin
                .as_ref()
                .is_some_and(|s| s.len() > 4096 || s.contains('\0'))
            || (self.configured && self.definition_digest.is_none())
            || (!self.registration_present && (self.running || self.configured))
        {
            return Err(Error::InvalidValue);
        }
        if let Some(p) = &self.platform {
            p.validate()?;
            match p {
                ServicePlatformFacts::Launchd {
                    definition_present,
                    loaded_registration_present,
                    runtime_state,
                    output_truncated,
                    ..
                } => {
                    if self.complete
                        && (*output_truncated
                            || loaded_registration_present.is_none()
                            || (*loaded_registration_present == Some(true)
                                && runtime_state.is_none())
                            || (*loaded_registration_present != Some(true)
                                && runtime_state.is_some())
                            || self.registration_present
                                != (*definition_present
                                    || *loaded_registration_present == Some(true))
                            || self.running
                                != (*runtime_state == Some(LaunchdRuntimeState::Running)))
                    {
                        return Err(Error::InvalidValue);
                    }
                }
                ServicePlatformFacts::Scm {
                    registration,
                    next_start,
                    status,
                    original_handle_retained,
                    ..
                } => {
                    if self.complete
                        && matches!(
                            registration,
                            ScmRegistrationState::DeletePending | ScmRegistrationState::Unknown
                        )
                    {
                        return Err(Error::InvalidValue);
                    }
                    if self.registration_present == (*registration == ScmRegistrationState::Absent)
                        || (*registration == ScmRegistrationState::Absent
                            && *original_handle_retained)
                    {
                        return Err(Error::InvalidValue);
                    }
                    if self.configured
                        && next_start
                            .as_ref()
                            .map(|raw| raw.digest().map(|d| d.to_hex()))
                            .transpose()?
                            .as_ref()
                            != self.definition_digest.as_ref()
                    {
                        return Err(Error::InvalidValue);
                    }
                    if let Some(s) = status
                        && (self.running != (s.state == 4) || s.service_type != 0x10)
                    {
                        return Err(Error::InvalidValue);
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scm() -> ServiceAuthority {
        serde_json::from_value(serde_json::json!({"kind":"scm","slot":"fixture_service","context":"local_machine","service_name":"TheseusFixture","qualification":{"manager":"windows_scm","version":"10.0.26100","api_profile":"scm_own_process_v1","features":["query_config","query_status","start_stop","original_handle"],"receipt_sha256":"11".repeat(32)},"allowed_access":53,"allow_local_system":true,"preflight":{"helper_sid":"S-1-5-21-123-456-789-1001","configured_account_sid":"S-1-5-18","executable":"C:\\fixture\\service.exe","definition":"C:\\fixture\\service.json","data":"C:\\fixture\\data","pipe":"\\\\.\\pipe\\theseus.fixture","executable_and_definition_read_execute":true,"data_declared_modify_only":true,"pipe_system_and_helper_only":true,"pipe_rejects_remote_clients":true,"no_other_acl_grants":true,"exact_access_receipt":"22".repeat(32)},"managed_by":"installer","sut_effects":["registration","configuration","removal"],"allowed_actions":["inspect","start","stop"]})).unwrap()
    }
    #[test]
    fn scm_requires_local_system_exact_local_rights_and_closed_preflight() {
        let good = scm();
        good.validate().unwrap();
        let json = serde_json::to_value(good).unwrap();
        for (pointer, value) in [
            (
                "/qualification/version",
                serde_json::json!("10.0.26100.extra"),
            ),
            ("/allow_local_system", serde_json::json!(false)),
            ("/allowed_access", serde_json::json!(0xF01FF)),
            (
                "/preflight/configured_account_sid",
                serde_json::json!("S-1-5-32-544"),
            ),
            (
                "/preflight/pipe",
                serde_json::json!("\\\\remote\\pipe\\fixture"),
            ),
            ("/preflight/no_other_acl_grants", serde_json::json!(false)),
        ] {
            let mut bad = json.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(
                serde_json::from_value::<ServiceAuthority>(bad)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
        let mut unknown = json;
        unknown["context"] = serde_json::json!("remote_machine");
        assert!(serde_json::from_value::<ServiceAuthority>(unknown).is_err());
    }
    #[test]
    fn readiness_platform_peers_preserve_unix_zero_and_do_not_fabricate_windows_uid() {
        let mut json = serde_json::json!({"endpoint":{"kind":"unix_path","path":"/private/ready.sock"},"root":"/private","peer_uid":0,"peer_gid":0,"expectations":[]});
        let unix: IntegrationReadiness = serde_json::from_value(json.clone()).unwrap();
        assert!(unix.valid_endpoint(ManagerPlatform::Systemd));
        assert_eq!(serde_json::to_value(unix).unwrap(), json);
        json["endpoint"] =
            serde_json::json!({"kind":"named_pipe","path":"\\\\.\\pipe\\theseus.fixture"});
        json["root"] = serde_json::json!("C:\\fixture");
        json["peer_sid"] = serde_json::json!("S-1-5-18");
        json["cooperative_instance"] = serde_json::json!("provider-original-install-instance");
        json.as_object_mut().unwrap().remove("peer_uid");
        json.as_object_mut().unwrap().remove("peer_gid");
        let windows: IntegrationReadiness = serde_json::from_value(json.clone()).unwrap();
        assert!(windows.valid_endpoint(ManagerPlatform::Scm));
        assert!(!windows.valid_endpoint(ManagerPlatform::Launchd));
        assert_eq!(serde_json::to_value(windows).unwrap(), json);
    }
    #[test]
    fn raw_scm_status_does_not_infer_readiness_or_allow_pending_absence() {
        let mut observation = ServiceObservation {
            platform: Some(ServicePlatformFacts::Scm {
                next_start: None,
                registration: ScmRegistrationState::DeletePending,
                status: None,
                configured_account_sid: None,
                child_token_measured: false,
                original_handle_retained: true,
            }),
            instance: None,
            slot: ResourceSlot::new("fixture_service").unwrap(),
            registration_present: true,
            definition_digest: None,
            configured: false,
            running: false,
            origin: None,
            manager_generation: "provider-context-original".into(),
            complete: false,
            binding_digest: "33".repeat(32),
        };
        observation.validate().unwrap();
        use crate::v2::{Decoder, Envelope, LeaseId, Message, Reply, SessionId, encode};
        let frame = Envelope::new(
            SessionId::from_bytes([1; 16]),
            2,
            Message::Response {
                reply_to: 1,
                result: Reply::ServiceObservation {
                    lease: LeaseId::from_bytes([2; 16]),
                    observation: Box::new(observation.clone()),
                },
            },
        )
        .unwrap();
        assert_eq!(
            Decoder::new()
                .feed(&encode(&frame).unwrap())
                .unwrap()
                .frame
                .unwrap(),
            frame
        );
        observation.complete = true;
        assert!(observation.validate().is_err());
        observation.complete = false;
        observation.registration_present = false;
        assert!(observation.validate().is_err());
        let json = serde_json::to_value(&observation).unwrap();
        assert!(json["platform"].get("status").is_none());
    }
}

impl IntegrationManifest {
    pub fn validate_context(
        &self,
        context: &IntegrationRuntimeContext,
        os: super::super::Os,
    ) -> Result<(), Error> {
        use sha2::{Digest as _, Sha256};
        self.validate()?;
        match (&self.service_authority, context.manager_platform) {
            (Some(authority), Some(_)) => authority.validate_context(context, os)?,
            (None, None) if self.desktop.is_some() => context.validate_os(os)?,
            _ => return Err(Error::InvalidValue),
        }
        match (&self.desktop, &context.desktop_broker_header) {
            (Some(desktop), Some(header))
                if header
                    .allowed_modes
                    .iter()
                    .all(|mode| desktop.allowed_modes.contains(mode)) => {}
            (None, None) => {}
            _ => return Err(Error::InvalidValue),
        }
        if let Some(owned) = self.desktop_owned_lifetime {
            if os != super::super::Os::Macos {
                return Err(Error::InvalidValue);
            }
            owned.validate_platform(
                &context
                    .desktop_broker_header
                    .as_ref()
                    .ok_or(Error::InvalidValue)?
                    .os,
            )?;
        }
        if let Some(entry) = &self.desktop_owned_entry {
            let expected = match os {
                super::super::Os::Linux => "linux",
                super::super::Os::Macos => "macos",
                super::super::Os::Windows => "windows",
            };
            if entry.os() != expected
                || context
                    .desktop_broker_header
                    .as_ref()
                    .is_none_or(|h| h.os != entry.os())
            {
                return Err(Error::InvalidValue);
            }
        }
        if let Some(native) = &self.desktop_native {
            let expected = match os {
                super::super::Os::Linux => "linux",
                super::super::Os::Macos => "macos",
                super::super::Os::Windows => "windows",
            };
            if native.os() != expected
                || context
                    .desktop_broker_header
                    .as_ref()
                    .is_none_or(|header| header.os != native.os())
            {
                return Err(Error::InvalidValue);
            }
        }
        if context.integration_manifest_digest != self.digest()?.to_hex() {
            return Err(Error::InvalidValue);
        }
        if self
            .service_authority
            .as_ref()
            .is_some_and(|a| a.platform() != ManagerPlatform::Systemd)
        {
            let instance = self
                .readiness
                .as_ref()
                .and_then(|r| r.cooperative_instance.as_ref())
                .ok_or(Error::InvalidValue)?;
            if context.installation_instance_digest.as_deref()
                != Some(format!("{:x}", Sha256::digest(instance.as_bytes())).as_str())
            {
                return Err(Error::InvalidValue);
            }
        }
        Ok(())
    }
}

impl ScmDefinition {
    fn validate_observation(&self) -> Result<(), Error> {
        let mut total = 0usize;
        for value in [
            &self.command_line,
            &self.display_name,
            &self.account,
            &self.load_order_group,
            &self.failure_reboot_message,
            &self.failure_command,
        ]
        .into_iter()
        .chain(self.dependencies.iter())
        .chain(self.required_privileges.iter())
        {
            if value.len() > 4096 || value.contains(&0) {
                return Err(Error::InvalidValue);
            }
            total = total.checked_add(value.len()).ok_or(Error::InvalidValue)?;
        }
        if total > 32768
            || self.dependencies.len() > 128
            || self.required_privileges.len() > 128
            || self.failure_actions.len() > 128
        {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
}

fn canonical_windows_version(version: &str) -> bool {
    let parts: Vec<_> = version.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && (p.len() == 1 || !p.starts_with('0'))
                && p.bytes().all(|b| b.is_ascii_digit())
                && p.parse::<u32>().is_ok()
        })
}

impl IntegrationRuntimeContext {
    pub fn validate_os(&self, os: super::super::Os) -> Result<(), Error> {
        self.validate()?;
        if !matches!(
            (os, self.manager_platform),
            (super::super::Os::Linux, Some(ManagerPlatform::Systemd))
                | (super::super::Os::Macos, Some(ManagerPlatform::Launchd))
                | (super::super::Os::Windows, Some(ManagerPlatform::Scm))
        ) && !self.desktop_broker_header.as_ref().is_some_and(|h| {
            self.manager_platform.is_none()
                && matches!(
                    (os, h.os.as_str()),
                    (super::super::Os::Linux, "linux")
                        | (super::super::Os::Macos, "macos")
                        | (super::super::Os::Windows, "windows")
                )
        }) {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
}

#[cfg(test)]
mod launchd_absence_wire_tests {
    use super::*;
    #[test]
    fn qualified_absence_has_no_loaded_service_and_unknown_is_never_complete_absence() {
        let mut observed = ServiceObservation {
            platform: Some(ServicePlatformFacts::Launchd {
                definition_present: false,
                loaded_registration_present: Some(false),
                runtime_state: None,
                registration_generation_available: false,
                command_evidence_sha256: "11".repeat(32),
                command_exit_code: Some(113),
                output_truncated: false,
            }),
            instance: None,
            slot: ResourceSlot::new("fixture_service").unwrap(),
            registration_present: false,
            definition_digest: None,
            configured: false,
            running: false,
            origin: None,
            manager_generation: "provider-original-domain".into(),
            complete: true,
            binding_digest: "22".repeat(32),
        };
        observed.validate().unwrap();
        use crate::v2::{Decoder, Envelope, LeaseId, Message, Reply, SessionId, encode};
        let frame = Envelope::new(
            SessionId::from_bytes([1; 16]),
            2,
            Message::Response {
                reply_to: 1,
                result: Reply::ServiceObservation {
                    lease: LeaseId::from_bytes([2; 16]),
                    observation: Box::new(observed.clone()),
                },
            },
        )
        .unwrap();
        assert_eq!(
            Decoder::new()
                .feed(&encode(&frame).unwrap())
                .unwrap()
                .frame
                .unwrap(),
            frame
        );
        if let Some(ServicePlatformFacts::Launchd {
            loaded_registration_present,
            ..
        }) = &mut observed.platform
        {
            *loaded_registration_present = None;
        }
        assert!(observed.validate().is_err());
        observed.complete = false;
        observed.validate().unwrap();
    }
}
