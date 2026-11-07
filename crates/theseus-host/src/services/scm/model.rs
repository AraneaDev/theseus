use std::{
    io,
    time::{Duration, Instant},
};

pub const QUERY_CONFIG: u32 = 0x0001;
pub const QUERY_STATUS: u32 = 0x0004;
pub const START: u32 = 0x0010;
pub const STOP: u32 = 0x0020;
pub const QUERY_ACCESS: u32 = QUERY_CONFIG | QUERY_STATUS;
pub const NATIVE_BUFFER_LIMIT: usize = 65536;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScmContext {
    LocalMachine,
}
/// Provider-authenticated exact access checks, performed before SUT registration.
/// These are not ACL observations by this adapter: the caller MUST authenticate
/// the receipt and check the exact executable/definition/data/pipe object rights.
/// No controller account, elevated helper role, or matching string replaces that
/// preflight. Only SYSTEM and this exact helper SID may be granted access.
#[derive(Clone, Debug)]
pub struct ScmPreflight {
    pub helper_sid: String,
    pub configured_account_sid: String,
    pub executable: Vec<u16>,
    pub definition: Vec<u16>,
    pub data: Vec<u16>,
    pub pipe: Vec<u16>,
    pub executable_and_definition_read_execute: bool,
    pub data_declared_modify_only: bool,
    pub pipe_system_and_helper_only: bool,
    pub pipe_rejects_remote_clients: bool,
    pub no_other_acl_grants: bool,
    pub exact_access_receipt: String,
}
#[derive(Clone, Debug)]
pub struct ScmAuthority {
    pub context: ScmContext,
    pub service_name: String,
    pub ownership_id: String,
    pub context_receipt: String,
    pub qualification_receipt: String,
    /// Exact helper service mask: QUERY_CONFIG|QUERY_STATUS plus admitted Start/Stop.
    /// SCM manager rights are always CONNECT only, never inferred from this mask.
    pub allowed_access: u32,
    pub allowed_start: bool,
    pub allowed_stop: bool,
    pub allow_local_system: bool,
    pub preflight: ScmPreflight,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailureAction {
    pub kind: u32,
    pub delay_ms: u32,
}
/// Raw UTF-16 code units exclude terminating NULs. No lossy Unicode conversion or
/// command-line reserialization occurs when comparing native configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    pub failure_actions: Vec<FailureAction>,
    pub failure_reset_period: u32,
    pub failure_reboot_message: Vec<u16>,
    pub failure_command: Vec<u16>,
    pub failure_actions_on_non_crash: bool,
    pub service_sid_type: u32,
    pub required_privileges: Vec<Vec<u16>>,
    pub triggers_present: bool,
    pub launch_protected: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceStatus {
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeState {
    Stopped,
    StartPending,
    StopPending,
    Running,
    ContinuePending,
    PausePending,
    Paused,
    Unknown(u32),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readiness {
    NotMeasured,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildToken {
    NotMeasured,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfiguredAccount {
    LocalSystem,
    Other(Vec<u16>),
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistrationState {
    Absent,
    Present,
    DeletePending,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceObservation {
    pub registration: RegistrationState,
    /// Configured definition for the next Start; never measured current image/argv.
    pub next_start: Option<ScmDefinition>,
    pub status: Option<ServiceStatus>,
    pub configured_account: ConfiguredAccount,
    pub application_readiness: Readiness,
    pub child_token: ChildToken,
}
impl ServiceObservation {
    pub fn present(config: ScmDefinition, status: ServiceStatus) -> Self {
        let account = if is_local_system(&config.account) {
            ConfiguredAccount::LocalSystem
        } else {
            ConfiguredAccount::Other(config.account.clone())
        };
        Self {
            registration: RegistrationState::Present,
            next_start: Some(config),
            status: Some(status),
            configured_account: account,
            application_readiness: Readiness::NotMeasured,
            child_token: ChildToken::NotMeasured,
        }
    }
    #[cfg(windows)]
    pub(super) fn absent(registration: RegistrationState) -> Self {
        Self {
            registration,
            next_start: None,
            status: None,
            configured_account: ConfiguredAccount::Unknown,
            application_readiness: Readiness::NotMeasured,
            child_token: ChildToken::NotMeasured,
        }
    }
}
impl ServiceStatus {
    pub fn state(&self) -> RuntimeState {
        match self.state {
            1 => RuntimeState::Stopped,
            2 => RuntimeState::StartPending,
            3 => RuntimeState::StopPending,
            4 => RuntimeState::Running,
            5 => RuntimeState::ContinuePending,
            6 => RuntimeState::PausePending,
            7 => RuntimeState::Paused,
            n => RuntimeState::Unknown(n),
        }
    }
    /// Conservatively expose a PID only in stable states. It is metadata only;
    /// no OpenProcess, kill, token inference, readiness or reattachment uses it.
    pub fn observed_process_id(&self) -> Option<u32> {
        (matches!(self.state(), RuntimeState::Running | RuntimeState::Paused)
            && self.process_id != 0)
            .then_some(self.process_id)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceAction {
    Start,
    Stop,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeError {
    pub api: &'static str,
    pub code: u32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnedActionProgress {
    Pending {
        action: ServiceAction,
        status: Option<ServiceStatus>,
    },
    /// A native stable status; does not certify readiness or child-tree settlement.
    Terminal {
        action: ServiceAction,
        status: ServiceStatus,
        succeeded: bool,
    },
    Unknown {
        action: ServiceAction,
        status: Option<ServiceStatus>,
        reason: String,
        native_error: Option<NativeError>,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameObservation {
    Absent,
    MarkedForDeletion,
    Present,
    Unknown(u32),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeletionProgress {
    Pending,
    Complete,
    SameNamePresent,
    Unknown(u32),
}
pub fn normalize_deletion(
    stopped_original: bool,
    original_closed: bool,
    name: NameObservation,
) -> DeletionProgress {
    if !stopped_original || !original_closed {
        return DeletionProgress::Pending;
    }
    match name {
        NameObservation::Absent => DeletionProgress::Complete,
        NameObservation::MarkedForDeletion => DeletionProgress::Pending,
        NameObservation::Present => DeletionProgress::SameNamePresent,
        NameObservation::Unknown(code) => DeletionProgress::Unknown(code),
    }
}
pub(super) fn invalid(s: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, s)
}
fn literal(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}
fn valid_wide(s: &[u16], max: usize) -> bool {
    s.len() <= max && !s.contains(&0)
}
fn absolute_drive_path(s: &[u16]) -> bool {
    s.len() >= 4
        && s.len() <= 4096
        && valid_wide(s, 4096)
        && matches!(s[0],65..=90|97..=122)
        && s[1] == 58
        && s[2] == 92
        && !s
            .iter()
            .any(|c| matches!(*c, 0..=31 | 34 | 42 | 47 | 60 | 62 | 63 | 124))
        && !s[3..].split(|c| *c == 92).any(|c| {
            c.is_empty()
                || c == [46]
                || c == [46, 46]
                || c.last() == Some(&32)
                || c.last() == Some(&46)
                || c.contains(&58)
        })
}
fn is_local_system(account: &[u16]) -> bool {
    account == "LocalSystem".encode_utf16().collect::<Vec<_>>()
}
impl ScmAuthority {
    pub fn service_access(&self) -> io::Result<u32> {
        self.validate()?;
        Ok(self.allowed_access)
    }
    pub fn validate(&self) -> io::Result<()> {
        let p = &self.preflight;
        let mask = QUERY_ACCESS
            | if self.allowed_start { START } else { 0 }
            | if self.allowed_stop { STOP } else { 0 };
        if self.service_name.is_empty()
            || self.service_name.len() > 128
            || self.service_name.starts_with('-')
            || !self
                .service_name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
            || self.allowed_access != mask
            || !self.allow_local_system
            || p.configured_account_sid != "S-1-5-18"
            || !literal(&self.ownership_id, 256)
            || !literal(&self.context_receipt, 4096)
            || !literal(&self.qualification_receipt, 4096)
            || !literal(&p.exact_access_receipt, 4096)
            || crate::services::readiness::Sid::new(p.helper_sid.clone()).is_err()
            || !absolute_drive_path(&p.executable)
            || !absolute_drive_path(&p.definition)
            || !absolute_drive_path(&p.data)
            || !valid_wide(&p.pipe, 256)
            || !String::from_utf16(&p.pipe)
                .is_ok_and(|pipe| crate::services::readiness::NamedPipe::new(pipe).is_ok())
            || !p.executable_and_definition_read_execute
            || !p.data_declared_modify_only
            || !p.pipe_system_and_helper_only
            || !p.pipe_rejects_remote_clients
            || !p.no_other_acl_grants
        {
            return Err(invalid(
                "incomplete explicit local SCM authority or exact LocalSystem access preflight",
            ));
        }
        Ok(())
    }
}
impl ScmDefinition {
    pub fn validate(&self, authority: &ScmAuthority) -> io::Result<()> {
        authority.validate()?;
        let exe = &authority.preflight.executable;
        // Requiring an exact quoted executable avoids Windows ambiguous unquoted
        // executable search. The remaining command line is compared as raw UTF-16.
        let prefix = [vec![34], exe.clone(), vec![34]].concat();
        if !self.command_line.starts_with(&prefix)
            || !matches!(self.command_line.get(prefix.len()), None | Some(32))
            || !valid_wide(&self.command_line, 32767)
            || self.command_line.iter().any(|c| *c < 32)
            || self.display_name.is_empty()
            || !valid_wide(&self.display_name, 256)
            || !is_local_system(&self.account)
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
            return Err(invalid(
                "unsupported SCM own-process demand-start LocalSystem definition",
            ));
        }
        Ok(())
    }
}
/// Stable-state normalization retains native status lag: a Start reply followed
/// by the pre-call Stopped snapshot is not evidence of a failed new invocation.
pub(super) struct ActionTracker {
    action: ServiceAction,
    initial: ServiceStatus,
    progress_observed: bool,
}
impl ActionTracker {
    pub fn new(action: ServiceAction, initial: ServiceStatus) -> Self {
        Self {
            action,
            initial,
            progress_observed: false,
        }
    }
    pub fn observe(&mut self, status: ServiceStatus) -> Option<bool> {
        match (self.action, status.state()) {
            (ServiceAction::Start, RuntimeState::Running) => Some(true),
            (ServiceAction::Stop, RuntimeState::Stopped) => Some(true),
            (ServiceAction::Start, RuntimeState::StartPending) => {
                self.progress_observed = true;
                None
            }
            (ServiceAction::Start, RuntimeState::Stopped)
                if self.progress_observed
                    || (status.win32_exit_code != 0
                        && (status.win32_exit_code, status.service_specific_exit_code)
                            != (
                                self.initial.win32_exit_code,
                                self.initial.service_specific_exit_code,
                            )) =>
            {
                Some(false)
            }
            _ => None,
        }
    }
}
/// Monotonic deadline retained from the accepted operation. A later Query cannot
/// extend it. Native wait-hints tune the next read only, never the overall budget.
pub(super) struct PollBudget {
    pub deadline: Instant,
    next: Instant,
    checkpoint: u32,
    advanced: Instant,
    wait_hint: Duration,
}
impl PollBudget {
    pub fn new(now: Instant, deadline: Instant) -> io::Result<Self> {
        if deadline <= now || deadline.duration_since(now) > Duration::from_secs(30) {
            return Err(invalid("SCM operation deadline must be within 30 seconds"));
        }
        Ok(Self {
            deadline,
            next: now,
            checkpoint: 0,
            advanced: now,
            wait_hint: Duration::from_secs(30),
        })
    }
    pub fn may_query(&self, now: Instant) -> bool {
        now >= self.next
    }
    pub fn expired(&self, now: Instant) -> bool {
        now >= self.deadline
    }
    pub fn stalled(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.advanced) >= self.wait_hint
    }
    pub fn observed(&mut self, now: Instant, checkpoint: u32, wait_hint_ms: u32) {
        if checkpoint > self.checkpoint {
            self.advanced = now;
            self.checkpoint = checkpoint;
        }
        self.wait_hint = Duration::from_millis(u64::from(wait_hint_ms).clamp(1000, 30000));
        self.next = now + Duration::from_millis(u64::from(wait_hint_ms / 10).clamp(25, 1000));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }
    fn authority() -> ScmAuthority {
        ScmAuthority {
            context: ScmContext::LocalMachine,
            service_name: "TheseusFixture_01".into(),
            ownership_id: "owned".into(),
            context_receipt: "provider-context".into(),
            qualification_receipt: "pinned-scm-build".into(),
            allowed_access: 0x35,
            allowed_start: true,
            allowed_stop: true,
            allow_local_system: true,
            preflight: ScmPreflight {
                helper_sid: "S-1-5-21-1-2-3-1001".into(),
                configured_account_sid: "S-1-5-18".into(),
                executable: wide(r"C:\fixture\service.exe"),
                definition: wide(r"C:\fixture\definition.json"),
                data: wide(r"C:\fixture\data"),
                pipe: wide(r"\\.\pipe\theseus.fixture"),
                executable_and_definition_read_execute: true,
                data_declared_modify_only: true,
                pipe_system_and_helper_only: true,
                pipe_rejects_remote_clients: true,
                no_other_acl_grants: true,
                exact_access_receipt: "source-fixture-preflight".into(),
            },
        }
    }
    fn definition() -> ScmDefinition {
        ScmDefinition {
            command_line: wide(r#""C:\fixture\service.exe" --service"#),
            display_name: wide("Theseus Fixture"),
            account: wide("LocalSystem"),
            service_type: 0x10,
            start_type: 3,
            error_control: 1,
            load_order_group: vec![],
            dependencies: vec![],
            tag_id: 0,
            delayed_auto_start: false,
            failure_actions: vec![],
            failure_reset_period: 0,
            failure_reboot_message: vec![],
            failure_command: vec![],
            failure_actions_on_non_crash: false,
            service_sid_type: 0,
            required_privileges: vec![],
            triggers_present: false,
            launch_protected: 0,
        }
    }
    #[test]
    fn scm_protocol_pipe_and_token_user_principals_are_admitted_before_native_work() {
        let mut a = authority();
        a.service_name = "Theseus.Fixture_01".into();
        a.preflight.helper_sid = "S-1-5-18".into();
        a.preflight.pipe = wide(r"\\.\pipe\theseus.fixture_01");
        assert!(a.validate().is_ok());
        for sid in ["S-1-5-18", "S-1-5-19", "S-1-5-20", "S-1-5-21-1-2-3-1001"] {
            a.preflight.helper_sid = sid.into();
            assert!(a.validate().is_ok(), "refused token user {sid}");
        }
        for pipe in [
            r"\\remote\pipe\theseus.fixture",
            r"\\.\pipe\unrelated.fixture",
            r"\\.\pipe\theseus.",
            r"\\.\pipe\theseus.a\b",
            r"\\.\pipe\theseus.a b",
        ] {
            a.preflight.pipe = wide(pipe);
            assert!(a.validate().is_err(), "accepted {pipe}");
        }
    }
    #[test]
    fn authority_refuses_broad_rights_remote_context_and_implicit_system_authorization() {
        let a = authority();
        assert!(a.validate().is_ok());
        assert_eq!(a.service_access().unwrap(), 0x35);
        for mask in [0, 0x01, 0xf003f, 0x37, 0x10035] {
            let mut a = authority();
            a.allowed_access = mask;
            assert!(a.validate().is_err(), "accepted {mask:x}");
        }
        let mut a = authority();
        a.allow_local_system = false;
        assert!(a.validate().is_err());
        let mut a = authority();
        a.preflight.configured_account_sid = "S-1-5-19".into();
        assert!(a.validate().is_err());
        let mut a = authority();
        a.preflight.helper_sid = "S-1-1-0".into();
        assert!(a.validate().is_err());
        let mut a = authority();
        a.preflight.no_other_acl_grants = false;
        assert!(a.validate().is_err());
        let mut a = authority();
        a.preflight.pipe_rejects_remote_clients = false;
        assert!(a.validate().is_err());
        for name in ["", "*", "a/b", "a\\b", "a\0b", "service@other", "-service"] {
            let mut a = authority();
            a.service_name = name.into();
            assert!(a.validate().is_err());
        }
        let mut a = authority();
        a.allowed_start = false;
        a.allowed_access = 0x25;
        assert!(a.validate().is_ok());
    }
    #[test]
    fn configuration_qualification_rejects_shared_process_automatic_restart_and_non_system_accounts()
     {
        let a = authority();
        let d = definition();
        assert!(d.validate(&a).is_ok());
        for kind in [1, 2, 0x20, 0x110] {
            let mut d = definition();
            d.service_type = kind;
            assert!(d.validate(&a).is_err());
        }
        for start in [0, 1, 2, 4] {
            let mut d = definition();
            d.start_type = start;
            assert!(d.validate(&a).is_err());
        }
        let mut d = definition();
        d.account = wide("NT AUTHORITY\\LocalService");
        assert!(d.validate(&a).is_err());
        let mut d = definition();
        d.failure_actions.push(FailureAction {
            kind: 1,
            delay_ms: 0,
        });
        assert!(d.validate(&a).is_err());
        let mut d = definition();
        d.dependencies.push(wide("OtherService"));
        assert!(d.validate(&a).is_err());
        let mut d = definition();
        d.triggers_present = true;
        assert!(d.validate(&a).is_err());
        let mut d = definition();
        d.command_line = wide(r#""C:\other\service.exe" --service"#);
        assert!(d.validate(&a).is_err());
    }
    #[test]
    fn raw_wide_configuration_preserves_surrogates_without_lossy_identity() {
        let mut d = definition();
        d.display_name = vec![0xd800, 0x61];
        assert!(d.validate(&authority()).is_ok());
        assert_ne!(d.display_name, vec![0xfffd, 0x61]);
        d.command_line.push(0);
        assert!(d.validate(&authority()).is_err());
    }
    #[test]
    fn status_never_turns_stale_pid_or_running_into_readiness() {
        let mut s = ServiceStatus {
            service_type: 0x10,
            state: 1,
            controls_accepted: 0,
            win32_exit_code: 0,
            service_specific_exit_code: 0,
            checkpoint: 0,
            wait_hint_ms: 0,
            process_id: 123,
            service_flags: 0,
        };
        assert_eq!(s.state(), RuntimeState::Stopped);
        assert_eq!(s.observed_process_id(), None);
        s.state = 2;
        assert_eq!(s.state(), RuntimeState::StartPending);
        assert_eq!(s.observed_process_id(), None);
        s.state = 4;
        assert_eq!(s.observed_process_id(), Some(123));
        let o = ServiceObservation::present(definition(), s);
        assert_eq!(o.application_readiness, Readiness::NotMeasured);
        assert_eq!(o.child_token, ChildToken::NotMeasured);
        assert_eq!(o.configured_account, ConfiguredAccount::LocalSystem);
    }
    #[test]
    fn pending_action_uses_original_deadline_checkpoint_and_minimum_poll_interval() {
        let t = Instant::now();
        let mut p = PollBudget::new(t, t + Duration::from_secs(2)).unwrap();
        assert!(p.may_query(t));
        p.observed(t, 1, 1);
        assert!(!p.may_query(t + Duration::from_millis(24)));
        assert!(p.may_query(t + Duration::from_millis(25)));
        assert!(p.stalled(t + Duration::from_secs(1)));
        p.observed(t + Duration::from_secs(1), 2, u32::MAX);
        assert!(!p.expired(t + Duration::from_millis(1999)));
        assert!(p.expired(t + Duration::from_secs(2)));
        assert!(PollBudget::new(t, t + Duration::from_secs(31)).is_err());
    }
    #[test]
    fn start_status_lag_is_pending_until_original_progress_or_a_new_failure_is_observed() {
        let stopped = ServiceStatus {
            service_type: 0x10,
            state: 1,
            controls_accepted: 0,
            win32_exit_code: 0,
            service_specific_exit_code: 0,
            checkpoint: 0,
            wait_hint_ms: 0,
            process_id: 999,
            service_flags: 0,
        };
        let mut tracker = ActionTracker::new(ServiceAction::Start, stopped);
        assert_eq!(tracker.observe(stopped), None);
        let mut s = stopped;
        s.state = 2;
        s.checkpoint = 1;
        assert_eq!(tracker.observe(s), None);
        assert_eq!(tracker.observe(stopped), Some(false));
        let mut tracker = ActionTracker::new(ServiceAction::Start, stopped);
        let mut failed = stopped;
        failed.win32_exit_code = 1067;
        assert_eq!(tracker.observe(failed), Some(false));
        let mut tracker = ActionTracker::new(ServiceAction::Start, stopped);
        let mut running = stopped;
        running.state = 4;
        running.process_id = 42;
        assert_eq!(tracker.observe(running), Some(true));
    }
    #[test]
    fn stop_status_lag_needs_stopped_and_preserves_native_failure_domains() {
        let running = ServiceStatus {
            service_type: 0x10,
            state: 4,
            controls_accepted: 1,
            win32_exit_code: 0,
            service_specific_exit_code: 0,
            checkpoint: 0,
            wait_hint_ms: 0,
            process_id: 42,
            service_flags: 0,
        };
        let mut tracker = ActionTracker::new(ServiceAction::Stop, running);
        assert_eq!(tracker.observe(running), None);
        let mut pending = running;
        pending.state = 3;
        pending.checkpoint = 1;
        assert_eq!(tracker.observe(pending), None);
        let mut stopped = running;
        stopped.state = 1;
        stopped.win32_exit_code = 1066;
        stopped.service_specific_exit_code = 77;
        assert_eq!(tracker.observe(stopped), Some(true));
        assert_eq!(stopped.win32_exit_code, 1066);
        assert_eq!(stopped.service_specific_exit_code, 77);
    }
    #[test]
    fn deletion_needs_stopped_original_handle_closure_and_exact_absence() {
        assert_eq!(
            normalize_deletion(false, false, NameObservation::Absent),
            DeletionProgress::Pending
        );
        assert_eq!(
            normalize_deletion(true, false, NameObservation::Absent),
            DeletionProgress::Pending
        );
        assert_eq!(
            normalize_deletion(true, true, NameObservation::MarkedForDeletion),
            DeletionProgress::Pending
        );
        assert_eq!(
            normalize_deletion(true, true, NameObservation::Present),
            DeletionProgress::SameNamePresent
        );
        assert_eq!(
            normalize_deletion(true, true, NameObservation::Unknown(5)),
            DeletionProgress::Unknown(5)
        );
        assert_eq!(
            normalize_deletion(true, true, NameObservation::Absent),
            DeletionProgress::Complete
        );
    }
}
