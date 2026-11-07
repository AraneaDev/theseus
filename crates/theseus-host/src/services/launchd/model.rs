use std::{io, path::PathBuf};
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fact<T> {
    Known(T),
    Unknown(String),
    Unavailable(String),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchdDomain {
    User(u32),
    System,
    Gui(u32),
}
impl LaunchdDomain {
    pub fn target(self) -> String {
        match self {
            Self::User(uid) => format!("user/{uid}"),
            Self::System => "system".into(),
            Self::Gui(uid) => format!("gui/{uid}"),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchdDefinition {
    pub executable: String,
    pub arguments: Vec<String>,
    pub working_directory: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrintQualification {
    pub absence_profile: Option<AbsenceQualification>,
    pub os_version: String,
    pub os_build: String,
    pub api_profile: String,
    pub receipt: String,
    pub launchctl_fingerprint: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AbsenceQualification {
    pub profile: String,
    pub exit_code: i32,
    pub stderr: Vec<u8>,
    pub receipt: String,
}
#[derive(Clone, Debug)]
pub struct LaunchdAuthority {
    pub domain: LaunchdDomain,
    pub label: String,
    pub definition_root: PathBuf,
    pub exclusive_load_roots: Vec<PathBuf>,
    pub launchctl: PathBuf,
    pub qualification: PrintQualification,
    pub ownership_id: String,
    pub context_receipt: String,
    pub allowed_start: bool,
    pub allowed_stop: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeState {
    Running,
    NotRunning,
    Waiting,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedService {
    pub label: String,
    pub domain: LaunchdDomain,
    pub origin: String,
    pub definition: LaunchdDefinition,
    pub state: RuntimeState,
    pub pid: Fact<u32>,
    pub registration_generation: Fact<String>,
    pub readiness: Fact<bool>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandEvidence {
    pub program: PathBuf,
    pub arguments: Vec<std::ffi::OsString>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: Option<i32>,
    pub truncated: bool,
    pub qualification: PrintQualification,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrintObservation {
    pub registration: Fact<bool>,
    pub loaded: Fact<LoadedService>,
    pub evidence: CommandEvidence,
}
pub(super) fn invalid(s: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, s)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceAction {
    Start,
    Stop,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchctlCommand {
    Inspect,
    Runtime(ServiceAction),
}
pub const OUTPUT_LIMIT: usize = 65536;
pub(super) fn plain_absolute(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 4096
        && !path.chars().any(char::is_control)
        && (path == "/"
            || !path[1..]
                .split('/')
                .any(|c| c.is_empty() || c == "." || c == ".."))
}
fn literal(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}
impl LaunchdAuthority {
    /// The caller authenticates the provider receipt and retains exclusive writers
    /// for this exact label, domain, plist root and executable for the lease.
    /// Only explicitly supplied user domains are admitted in this initial port.
    pub fn validate(&self) -> io::Result<()> {
        if !matches!(self.domain, LaunchdDomain::User(_))
            || !literal(&self.label, 128)
            || !self
                .label
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
            || self.label.starts_with('-')
            || self.launchctl != std::path::Path::new("/bin/launchctl")
            || !self.definition_root.to_str().is_some_and(plain_absolute)
            || !literal(&self.ownership_id, 256)
            || !literal(&self.context_receipt, 4096)
            || !literal(&self.qualification.os_version, 128)
            || !literal(&self.qualification.os_build, 128)
            || self.qualification.api_profile != "launchctl-qualified-v1"
            || !literal(&self.qualification.receipt, 4096)
            || self.qualification.launchctl_fingerprint.len() != 57
            || self.qualification.launchctl_fingerprint.first() != Some(&1)
            || self.exclusive_load_roots.is_empty()
            || self.exclusive_load_roots.len() > 16
            || !self.exclusive_load_roots.contains(&self.definition_root)
            || self
                .exclusive_load_roots
                .iter()
                .any(|p| !p.to_str().is_some_and(plain_absolute))
            || self
                .exclusive_load_roots
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.exclusive_load_roots.len()
        {
            return Err(invalid(
                "unsupported or incomplete explicit launchd authority",
            ));
        }
        if let Some(profile) = &self.qualification.absence_profile {
            let LaunchdDomain::User(uid) = self.domain else {
                return Err(invalid("absence profile requires exact User domain"));
            };
            // Exact launchctl forms only; `uid:` is the form observed on a real
            // macOS 15.8.2 runner, `user:` the previously modelled form.
            let exact = ["user", "uid"].iter().any(|kind| {
                let diagnostic = format!(
                    "Could not find service \"{}\" in domain for {kind}: {uid}\n",
                    self.label
                );
                profile.stderr == diagnostic.as_bytes()
                    || profile.stderr == format!("Bad request.\n{diagnostic}").as_bytes()
            });
            if profile.profile != "launchctl-user-no-service-v1"
                || !(1..=255).contains(&profile.exit_code)
                || !literal(&profile.receipt, 4096)
                || !exact
            {
                return Err(invalid(
                    "unsupported or unqualified exact launchd absence profile",
                ));
            }
        }
        Ok(())
    }
    pub fn service_target(&self) -> String {
        format!("{}/{}", self.domain.target(), self.label)
    }
    pub fn plist_path(&self) -> PathBuf {
        self.definition_root.join(format!("{}.plist", self.label))
    }
    /// The definition origin literal launchd itself reports. launchd paths are
    /// always POSIX, so this is rendered with `/` independent of the build
    /// host's separator (on macOS it equals `plist_path()` for every validated
    /// root). Only a plain absolute POSIX root yields an origin.
    pub fn plist_origin(&self) -> Option<String> {
        let root = self
            .definition_root
            .to_str()
            .filter(|r| plain_absolute(r))?;
        Some(format!(
            "{}/{}.plist",
            root.strip_suffix('/').unwrap_or(root),
            self.label
        ))
    }
    pub fn command(&self, command: LaunchctlCommand) -> io::Result<crate::process::NativeCommand> {
        self.validate()?;
        let mut native = crate::process::NativeCommand::new(&self.launchctl);
        native.environment = crate::process::ChildEnvironment::Replace(vec![]);
        native.current_dir("/");
        match command {
            LaunchctlCommand::Inspect => {
                native.arg("print");
            }
            LaunchctlCommand::Runtime(ServiceAction::Start) if self.allowed_start => {
                native.arg("kickstart");
            }
            LaunchctlCommand::Runtime(ServiceAction::Stop) if self.allowed_stop => {
                native.args(["kill", "SIGTERM"]);
            }
            _ => return Err(invalid("runtime action not granted")),
        }
        native.arg(self.service_target());
        Ok(native)
    }
}
impl LaunchdDefinition {
    pub fn validate(&self) -> io::Result<()> {
        if !plain_absolute(&self.executable)
            || !plain_absolute(&self.working_directory)
            || self.arguments.first() != Some(&self.executable)
            || self.arguments.len() > 64
            || self.arguments.iter().any(|s| {
                s.len() > 4096 || s.chars().any(char::is_control) || s.trim() != s || s.is_empty()
            })
            || self.arguments.iter().map(String::len).sum::<usize>() > OUTPUT_LIMIT
        {
            return Err(invalid("unsupported launchd foreground command projection"));
        }
        Ok(())
    }
}
/// A deliberately closed XML plist profile. No binary/OpenStep plist, XML
/// extension/entities, environment, login/boot activation, dependencies or restart.
/// The original bytes are retained separately by the registration owner.
pub fn validate_plist(
    bytes: &[u8],
    authority: &LaunchdAuthority,
    expected: &LaunchdDefinition,
) -> io::Result<()> {
    authority.validate()?;
    expected.validate()?;
    if bytes.len() > OUTPUT_LIMIT || bytes.contains(&0) {
        return Err(invalid("unbounded or NUL plist"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("plist is not UTF-8 XML"))?;
    let mut xml = Xml(text.trim());
    if xml.0.starts_with("<?xml") {
        xml.take("<?xml version=\"1.0\" encoding=\"UTF-8\"?>")?;
    }
    if xml.0.starts_with("<!DOCTYPE") {
        xml.take("<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">")?;
    }
    xml.take("<plist version=\"1.0\">")?;
    xml.take("<dict>")?;
    let mut seen = std::collections::BTreeSet::new();
    let mut label = None;
    let mut args = None;
    let mut cwd = None;
    let mut session = None;
    while !xml.0.starts_with("</dict>") {
        let key = xml.string("key")?;
        if !seen.insert(key.clone()) {
            return Err(invalid("duplicate plist key"));
        }
        match key.as_str() {
            "Label" => label = Some(xml.string("string")?),
            "WorkingDirectory" => cwd = Some(xml.string("string")?),
            "ProgramArguments" => {
                xml.take("<array>")?;
                let mut values = Vec::new();
                while !xml.0.starts_with("</array>") {
                    if values.len() == 64 {
                        return Err(invalid("plist argument bound"));
                    }
                    values.push(xml.string("string")?);
                }
                xml.take("</array>")?;
                args = Some(values);
            }
            "RunAtLoad" | "KeepAlive" => xml.take("<false/>")?,
            // Only the scalar string form; the array form and any other session
            // type stay outside the closed profile.
            "LimitLoadToSessionType" => session = Some(xml.string("string")?),
            _ => return Err(invalid("unsupported plist key")),
        }
    }
    xml.take("</dict>")?;
    xml.take("</plist>")?;
    // The explicit `user/<uid>` domain is a Background session: a User-domain
    // job must declare exactly that session type; no other domain admits the key.
    let required_session = match authority.domain {
        LaunchdDomain::User(_) => Some("Background"),
        LaunchdDomain::System | LaunchdDomain::Gui(_) => None,
    };
    if session.as_deref() != required_session {
        return Err(invalid(
            "plist session type does not match the exact domain",
        ));
    }
    if !xml.0.is_empty()
        || label.as_ref() != Some(&authority.label)
        || args.as_ref() != Some(&expected.arguments)
        || cwd.as_ref() != Some(&expected.working_directory)
    {
        return Err(invalid("plist does not match exact admitted configuration"));
    }
    Ok(())
}
struct Xml<'a>(&'a str);
impl Xml<'_> {
    fn take(&mut self, token: &str) -> io::Result<()> {
        self.0 = self
            .0
            .strip_prefix(token)
            .ok_or_else(|| invalid("unsupported XML plist syntax"))?
            .trim_start();
        Ok(())
    }
    fn string(&mut self, tag: &str) -> io::Result<String> {
        self.0 = self
            .0
            .strip_prefix(&format!("<{tag}>"))
            .ok_or_else(|| invalid("unsupported XML string tag"))?;
        let (raw, rest) = self
            .0
            .split_once(&format!("</{tag}>"))
            .ok_or_else(|| invalid("unterminated plist string"))?;
        if raw.contains(['<', '>']) {
            return Err(invalid("unescaped XML content"));
        }
        let mut value = String::new();
        let mut remaining = raw;
        while let Some((before, after)) = remaining.split_once('&') {
            value.push_str(before);
            let (entity, after) = after
                .split_once(';')
                .ok_or_else(|| invalid("unterminated XML entity"))?;
            value.push(match entity {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '\"',
                "apos" => '\'',
                _ => return Err(invalid("unsupported XML entity")),
            });
            remaining = after;
        }
        value.push_str(remaining);
        self.0 = rest.trim_start();
        Ok(value)
    }
}
#[cfg(test)]
pub(super) mod tests {
    use super::*;
    pub(in crate::services::launchd) fn authority() -> LaunchdAuthority {
        LaunchdAuthority {
            domain: LaunchdDomain::User(501),
            label: "org.theseus.fixture".into(),
            definition_root: "/fixture/plists".into(),
            exclusive_load_roots: vec!["/fixture/plists".into()],
            launchctl: "/bin/launchctl".into(),
            qualification: PrintQualification {
                absence_profile: None,
                os_version: "model-version".into(),
                os_build: "model-build".into(),
                api_profile: "launchctl-qualified-v1".into(),
                receipt: "model-only-not-native".into(),
                launchctl_fingerprint: vec![1; 57],
            },
            ownership_id: "original-reservation".into(),
            context_receipt: "exclusive-namespace-proof".into(),
            allowed_start: true,
            allowed_stop: true,
        }
    }
    fn definition() -> LaunchdDefinition {
        LaunchdDefinition {
            executable: "/fixture/service".into(),
            arguments: vec!["/fixture/service".into(), "literal $x; *".into()],
            working_directory: "/fixture".into(),
        }
    }
    #[test]
    fn launchd_origin_literal_is_posix_on_every_build_host() {
        let mut a = authority();
        assert_eq!(
            a.plist_origin().as_deref(),
            Some("/fixture/plists/org.theseus.fixture.plist")
        );
        a.definition_root = "/".into();
        assert_eq!(
            a.plist_origin().as_deref(),
            Some("/org.theseus.fixture.plist")
        );
        for root in ["relative/plists", "/fixture/../plists", "/fixture//plists"] {
            a.definition_root = root.into();
            assert_eq!(a.plist_origin(), None, "{root}");
        }
    }
    const PLIST: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict><key>Label</key><string>org.theseus.fixture</string><key>ProgramArguments</key><array><string>/fixture/service</string><string>literal $x; *</string></array><key>WorkingDirectory</key><string>/fixture</string><key>LimitLoadToSessionType</key><string>Background</string><key>RunAtLoad</key><false/><key>KeepAlive</key><false/></dict></plist>";
    #[test]
    fn launchd_commands_use_exact_explicit_user_domain_and_literal_argv() {
        let a = authority();
        assert!(a.validate().is_ok());
        for (kind, want) in [
            (
                LaunchctlCommand::Inspect,
                vec!["print", "user/501/org.theseus.fixture"],
            ),
            (
                LaunchctlCommand::Runtime(ServiceAction::Start),
                vec!["kickstart", "user/501/org.theseus.fixture"],
            ),
            (
                LaunchctlCommand::Runtime(ServiceAction::Stop),
                vec!["kill", "SIGTERM", "user/501/org.theseus.fixture"],
            ),
        ] {
            let c = a.command(kind).unwrap();
            assert_eq!(c.program, "/bin/launchctl");
            assert_eq!(
                c.args,
                want.into_iter()
                    .map(std::ffi::OsString::from)
                    .collect::<Vec<_>>()
            );
            assert!(
                matches!(c.environment,crate::process::ChildEnvironment::Replace(ref e) if e.is_empty())
            );
        }
        for domain in [LaunchdDomain::System, LaunchdDomain::Gui(501)] {
            let mut a = authority();
            a.domain = domain;
            assert!(a.validate().is_err());
        }
        let mut a = authority();
        a.label = "org.*".into();
        assert!(a.validate().is_err());
        let mut a = authority();
        a.context_receipt.clear();
        assert!(a.validate().is_err());
        let mut a = authority();
        a.qualification.api_profile = "unknown".into();
        assert!(a.validate().is_err());
    }
    #[test]
    fn launchd_user_plist_requires_exact_background_session_type() {
        const KEY: &str = "<key>LimitLoadToSessionType</key><string>Background</string>";
        assert!(PLIST.contains(KEY));
        assert!(validate_plist(PLIST.as_bytes(), &authority(), &definition()).is_ok());
        for text in [
            // Missing where the User domain now requires it.
            PLIST.replace(KEY, ""),
            // Any other session type.
            PLIST.replace(">Background<", ">Aqua<"),
            PLIST.replace(">Background<", ">LoginWindow<"),
            PLIST.replace(">Background<", ">background<"),
            PLIST.replace(">Background<", "> Background<"),
            // Array form, even of exactly Background.
            PLIST.replace(
                "<string>Background</string>",
                "<array><string>Background</string></array>",
            ),
            // Duplicate declaration.
            PLIST.replace(KEY, &format!("{KEY}{KEY}")),
        ] {
            assert!(
                validate_plist(text.as_bytes(), &authority(), &definition()).is_err(),
                "{text}"
            );
        }
        // No non-User domain admits the key (or any plist): authority is User-only.
        for domain in [LaunchdDomain::System, LaunchdDomain::Gui(501)] {
            let mut a = authority();
            a.domain = domain;
            assert!(validate_plist(PLIST.as_bytes(), &a, &definition()).is_err());
            assert!(validate_plist(PLIST.replace(KEY, "").as_bytes(), &a, &definition()).is_err());
        }
    }
    #[test]
    fn launchd_plist_closed_profile_rejects_automatic_activation_and_hidden_configuration() {
        assert!(validate_plist(PLIST.as_bytes(), &authority(), &definition()).is_ok());
        for text in [
            PLIST.replace("<false/>", "<true/>"),
            PLIST.replace("</dict>", "<key>MachServices</key><dict></dict></dict>"),
            PLIST.replace(
                "</dict>",
                "<key>Label</key><string>org.theseus.fixture</string></dict>",
            ),
            PLIST.replace("org.theseus.fixture", "org.other"),
            PLIST.replace(
                "<string>/fixture/service</string>",
                "<string>/other/service</string>",
            ),
            PLIST.replace("<false/>", "<dict></dict>"),
        ] {
            assert!(validate_plist(text.as_bytes(), &authority(), &definition()).is_err());
        }
    }
}
