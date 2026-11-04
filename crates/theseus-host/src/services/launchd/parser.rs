use super::model::*;
use std::{collections::BTreeMap, io};
/// Normalize only the supplied finite profile. Exit0 is never sufficient proof.
/// Unknown/unsupported output retains bounded exact bytes and its qualification.
pub fn normalize_print(
    authority: &LaunchdAuthority,
    mut evidence: CommandEvidence,
) -> PrintObservation {
    if evidence.stdout.len() > OUTPUT_LIMIT || evidence.stderr.len() > OUTPUT_LIMIT {
        evidence.truncated = true;
        evidence.stdout.truncate(OUTPUT_LIMIT);
        evidence.stderr.truncate(OUTPUT_LIMIT);
    }
    let result = (|| {
        authority.validate()?;
        let inspect_arguments: Vec<std::ffi::OsString> =
            vec!["print".into(), authority.service_target().into()];
        if evidence.qualification != authority.qualification
            || evidence.program != authority.launchctl
            || evidence.arguments != inspect_arguments
            || evidence.truncated
        {
            return Err(invalid("command/qualification incomplete or unsupported"));
        }
        if evidence.exit_code != Some(0) {
            if let Some(profile) = &authority.qualification.absence_profile
                && evidence.exit_code == Some(profile.exit_code)
                && evidence.stdout.is_empty()
                && evidence.stderr == profile.stderr
            {
                return Ok(None);
            }
            return Err(invalid(
                "unqualified launchd error; registration remains unknown",
            ));
        }
        if !evidence.stderr.is_empty() {
            return Err(invalid("successful print includes unexpected diagnostics"));
        }
        parse(authority, &evidence.stdout).map(Some)
    })();
    let (registration, loaded) = match result {
        Ok(Some(loaded)) => (Fact::Known(true), Fact::Known(loaded)),
        Ok(None) => (
            Fact::Known(false),
            Fact::Unavailable("qualified exact-domain registration absent".into()),
        ),
        Err(error) => {
            let reason = error.to_string();
            (Fact::Unknown(reason.clone()), Fact::Unknown(reason))
        }
    };
    PrintObservation {
        registration,
        loaded,
        evidence,
    }
}
fn decimal(s: &str) -> io::Result<u32> {
    if s.is_empty()
        || s.len() > 10
        || !s.bytes().all(|c| c.is_ascii_digit())
        || (s.len() > 1 && s.starts_with('0'))
    {
        return Err(invalid("invalid decimal field"));
    }
    s.parse().map_err(|_| invalid("decimal overflow"))
}
fn parse(authority: &LaunchdAuthority, bytes: &[u8]) -> io::Result<LoadedService> {
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("print is not UTF-8"))?;
    if text.contains('\0') || !text.ends_with("}\n") || text.lines().count() > 512 {
        return Err(invalid("truncated or unbounded print"));
    }
    let mut lines = text.lines();
    if lines.next() != Some(format!("{} = {{", authority.service_target()).as_str()) {
        return Err(invalid("wrong exact launchd target"));
    }
    let mut fields = BTreeMap::new();
    let mut arguments = None;
    let mut closed = false;
    while let Some(line) = lines.next() {
        if line == "}" {
            closed = true;
            break;
        }
        let (key, value) = line
            .strip_prefix('\t')
            .and_then(|s| s.split_once(" = "))
            .ok_or_else(|| invalid("unsupported print indentation/field"))?;
        if fields.insert(key, value).is_some() {
            return Err(invalid("duplicate print field"));
        }
        match key {
            "arguments" => {
                if value != "{" {
                    return Err(invalid("unsupported argument syntax"));
                }
                let mut values = Vec::new();
                let mut ended = false;
                for line in lines.by_ref() {
                    if line == "\t}" {
                        ended = true;
                        break;
                    }
                    let value = line
                        .strip_prefix("\t\t")
                        .ok_or_else(|| invalid("unsupported argument indentation"))?;
                    if values.len() == 64 || value.is_empty() || value.trim() != value {
                        return Err(invalid("unsupported argument projection"));
                    }
                    values.push(value.to_owned());
                }
                if !ended {
                    return Err(invalid("truncated argument list"));
                }
                arguments = Some(values);
            }
            "default environment" | "environment" => {
                if value != "{" {
                    return Err(invalid("unsupported environment syntax"));
                }
                let mut entries = BTreeMap::new();
                let mut ended = false;
                for line in lines.by_ref() {
                    if line == "\t}" {
                        ended = true;
                        break;
                    }
                    let (name, value) = line
                        .strip_prefix("\t\t")
                        .and_then(|s| s.split_once(" => "))
                        .ok_or_else(|| invalid("unsupported environment entry"))?;
                    if entries.insert(name, value).is_some()
                        || !match (key, name) {
                            ("environment", "XPC_SERVICE_NAME") => value == authority.label,
                            ("default environment", "PATH") => {
                                value == "/usr/bin:/bin:/usr/sbin:/sbin"
                            }
                            _ => false,
                        }
                    {
                        return Err(invalid("unadmitted environment projection"));
                    }
                }
                if !ended {
                    return Err(invalid("truncated environment"));
                }
            }
            "active count"
            | "runs"
            | "pid"
            | "asid"
            | "minimum runtime"
            | "exit timeout"
            | "spawn type"
            | "jetsam priority"
            | "jetsam memory limit (active)"
            | "jetsam memory limit (inactive)"
            | "jetsam thread limit"
            | "job state"
            | "probabilistic guard malloc policy"
            | "last exit code"
            | "last terminating signal"
            | "resource coalition"
            | "execution context"
            | "path"
            | "type"
            | "state"
            | "program"
            | "working directory"
            | "domain"
            | "properties" => {
                if value.is_empty()
                    || value.contains(['{', '}'])
                    || value.chars().any(char::is_control)
                {
                    return Err(invalid("unsupported scalar value"));
                }
            }
            _ => return Err(invalid("unknown print field in finite profile")),
        }
    }
    if !closed || lines.next().is_some() {
        return Err(invalid("truncated or trailing print"));
    }
    let required = |key| {
        fields
            .get(key)
            .copied()
            .ok_or_else(|| invalid("required print field unknown"))
    };
    decimal(required("active count")?)?;
    let origin = required("path")?;
    if Some(origin) != authority.plist_path().to_str() || required("type")? != "LaunchAgent" {
        return Err(invalid("wrong loaded definition origin/type"));
    }
    let domain = required("domain")?;
    let (target, asid) = domain
        .split_once(" [")
        .ok_or_else(|| invalid("unqualified domain projection"))?;
    if target != authority.domain.target() {
        return Err(invalid("wrong loaded domain"));
    }
    decimal(
        asid.strip_suffix(']')
            .ok_or_else(|| invalid("invalid domain audit session"))?,
    )?;
    if required("properties")? != "inferred program" {
        return Err(invalid("unsupported job properties/automatic activation"));
    }
    let definition = LaunchdDefinition {
        executable: required("program")?.into(),
        arguments: arguments.ok_or_else(|| invalid("arguments unknown"))?,
        working_directory: required("working directory")?.into(),
    };
    definition.validate()?;
    let state = match required("state")? {
        "running" => RuntimeState::Running,
        "not running" => RuntimeState::NotRunning,
        "waiting" => RuntimeState::Waiting,
        _ => return Err(invalid("unknown runtime state")),
    };
    let pid = match fields.get("pid") {
        Some(value) => {
            let pid = decimal(value)?;
            if pid == 0 || state != RuntimeState::Running {
                return Err(invalid("inconsistent runtime PID"));
            }
            Fact::Known(pid)
        }
        None if state != RuntimeState::Running => {
            Fact::Unavailable("no live process reported for loaded job".into())
        }
        None => return Err(invalid("running job has unknown PID")),
    };
    Ok(LoadedService {
        label: authority.label.clone(),
        domain: authority.domain,
        origin: origin.into(),
        definition,
        state,
        pid,
        registration_generation: Fact::Unavailable(
            "launchd print supplies no registration generation".into(),
        ),
        readiness: Fact::Unavailable("requires independent authenticated challenge".into()),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn authority() -> LaunchdAuthority {
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
                receipt: "model-only-not-native-acceptance".into(),
                launchctl_fingerprint: vec![1; 57],
            },
            ownership_id: "reserved-slot".into(),
            context_receipt: "exclusive-domain-namespace".into(),
            allowed_start: true,
            allowed_stop: true,
        }
    }
    const PRINT: &str = "user/501/org.theseus.fixture = {\n\tactive count = 1\n\tpath = /fixture/plists/org.theseus.fixture.plist\n\ttype = LaunchAgent\n\tstate = running\n\tprogram = /fixture/service\n\targuments = {\n\t\t/fixture/service\n\t\tservice\n\t}\n\tworking directory = /fixture\n\tdomain = user/501 [100004]\n\tpid = 42\n\tproperties = inferred program\n}\n";
    fn evidence(text: &str) -> CommandEvidence {
        CommandEvidence {
            program: "/bin/launchctl".into(),
            arguments: vec!["print".into(), "user/501/org.theseus.fixture".into()],
            stdout: text.as_bytes().to_vec(),
            stderr: vec![],
            exit_code: Some(0),
            truncated: false,
            qualification: authority().qualification,
        }
    }
    #[test]
    fn launchd_model_running_keeps_configuration_runtime_and_readiness_distinct() {
        let result = normalize_print(&authority(), evidence(PRINT));
        let Fact::Known(loaded) = result.loaded else {
            panic!("qualified model print rejected: {:?}", result.loaded)
        };
        assert_eq!(loaded.domain, LaunchdDomain::User(501));
        assert_eq!(loaded.origin, "/fixture/plists/org.theseus.fixture.plist");
        assert_eq!(loaded.definition.arguments, ["/fixture/service", "service"]);
        assert_eq!(loaded.state, RuntimeState::Running);
        assert_eq!(loaded.pid, Fact::Known(42));
        assert!(matches!(loaded.readiness, Fact::Unavailable(_)));
        assert!(matches!(
            loaded.registration_generation,
            Fact::Unavailable(_)
        ));
        assert_eq!(result.evidence.stdout, PRINT.as_bytes());
    }
    #[test]
    fn launchd_parser_rejects_truncation_duplicates_unknown_wrong_domain_and_exit0_missing_facts() {
        for text in [
            String::new(),
            PRINT.trim_end_matches("}\n").into(),
            PRINT.replace("\tstate = running", "\tstate = running\n\tstate = running"),
            PRINT.replace("\tstate = running", "\tstate = unknown"),
            PRINT.replace("user/501 [", "gui/501 ["),
            PRINT.replace(
                "user/501/org.theseus.fixture =",
                "user/502/org.theseus.fixture =",
            ),
            PRINT.replace("\tprogram = /fixture/service\n", ""),
            PRINT.replace("\tpid = 42", "\tpid = 0"),
            PRINT.replace("\tproperties = inferred program", "\tunknown fact = yes"),
        ] {
            assert!(
                matches!(
                    normalize_print(&authority(), evidence(&text)).loaded,
                    Fact::Unknown(_)
                ),
                "accepted {text}"
            );
        }
        let mut e = evidence(PRINT);
        e.truncated = true;
        assert!(matches!(
            normalize_print(&authority(), e).loaded,
            Fact::Unknown(_)
        ));
        let mut e = evidence(PRINT);
        e.qualification.os_build = "other-build".into();
        assert!(matches!(
            normalize_print(&authority(), e).loaded,
            Fact::Unknown(_)
        ));
        let mut e = evidence(PRINT);
        e.exit_code = Some(1);
        assert!(matches!(
            normalize_print(&authority(), e).loaded,
            Fact::Unknown(_)
        ));
    }
    #[test]
    fn launchd_parser_requires_original_inspect_command_provenance_and_bounds_diagnostics() {
        let mut e = evidence(PRINT);
        e.program = "/other/launchctl".into();
        assert!(matches!(
            normalize_print(&authority(), e).loaded,
            Fact::Unknown(_)
        ));
        let mut e = evidence(PRINT);
        e.arguments = vec!["print".into(), "gui/501/org.theseus.fixture".into()];
        assert!(matches!(
            normalize_print(&authority(), e).loaded,
            Fact::Unknown(_)
        ));
        let mut e = evidence(PRINT);
        e.stderr = vec![b'x'; OUTPUT_LIMIT + 1];
        let result = normalize_print(&authority(), e);
        assert!(matches!(result.loaded, Fact::Unknown(_)));
        assert_eq!(result.evidence.stderr.len(), OUTPUT_LIMIT);
        assert!(result.evidence.truncated);
    }
    #[test]
    fn launchd_dormant_loaded_job_does_not_become_running_or_ready() {
        let text = PRINT
            .replace("state = running", "state = not running")
            .replace("\tpid = 42\n", "");
        let Fact::Known(loaded) = normalize_print(&authority(), evidence(&text)).loaded else {
            panic!("dormant loaded print rejected")
        };
        assert_eq!(loaded.state, RuntimeState::NotRunning);
        assert!(matches!(loaded.pid, Fact::Unavailable(_)));
        assert!(matches!(loaded.readiness, Fact::Unavailable(_)));
    }
}

#[cfg(test)]
mod absence_tests {
    use super::*;
    fn authority() -> LaunchdAuthority {
        LaunchdAuthority{domain:LaunchdDomain::User(501),label:"org.theseus.fixture".into(),definition_root:"/fixture/plists".into(),exclusive_load_roots:vec!["/fixture/plists".into()],launchctl:"/bin/launchctl".into(),qualification:PrintQualification{os_version:"model-version".into(),os_build:"model-build".into(),api_profile:"launchctl-qualified-v1".into(),receipt:"model-only-not-native-acceptance".into(),launchctl_fingerprint:vec![1;57],absence_profile:Some(AbsenceQualification{profile:"launchctl-user-no-service-v1".into(),exit_code:113,stderr:b"Bad request.\nCould not find service \"org.theseus.fixture\" in domain for user: 501\n".to_vec(),receipt:"model-only-absence-receipt".into()})},ownership_id:"original".into(),context_receipt:"exclusive".into(),allowed_start:true,allowed_stop:true}
    }
    fn evidence(a: &LaunchdAuthority) -> CommandEvidence {
        CommandEvidence {
            program: a.launchctl.clone(),
            arguments: vec!["print".into(), a.service_target().into()],
            stdout: vec![],
            stderr: a
                .qualification
                .absence_profile
                .as_ref()
                .unwrap()
                .stderr
                .clone(),
            exit_code: Some(113),
            truncated: false,
            qualification: a.qualification.clone(),
        }
    }
    #[test]
    fn qualified_absence_is_separate_from_unavailable_loaded_definition() {
        let a = authority();
        let e = evidence(&a);
        let normalized = normalize_print(&a, e);
        assert_eq!(normalized.registration, Fact::Known(false));
        assert!(matches!(normalized.loaded, Fact::Unavailable(_)));
    }
    #[test]
    fn absence_never_comes_from_unqualified_nonzero_wrong_target_or_permission_errors() {
        let a = authority();
        let original = evidence(&a);
        for diagnostic in [
            "Operation not permitted\n",
            "Permission denied\n",
            "No such process\n",
            "Could not find domain for user: 501\n",
        ] {
            let mut e = original.clone();
            e.stderr = diagnostic.as_bytes().to_vec();
            assert!(matches!(
                normalize_print(&a, e).registration,
                Fact::Unknown(_)
            ));
        }
        let mut wrong = original.clone();
        wrong.exit_code = Some(1);
        assert!(matches!(
            normalize_print(&a, wrong).registration,
            Fact::Unknown(_)
        ));
        let mut wrong = original.clone();
        wrong.stderr = String::from_utf8(wrong.stderr)
            .unwrap()
            .replace("501", "502")
            .into_bytes();
        assert!(matches!(
            normalize_print(&a, wrong).registration,
            Fact::Unknown(_)
        ));
        let mut wrong = original.clone();
        wrong.stdout = b"unexpected".to_vec();
        assert!(matches!(
            normalize_print(&a, wrong).registration,
            Fact::Unknown(_)
        ));
        let mut wrong = original.clone();
        wrong.truncated = true;
        assert!(matches!(
            normalize_print(&a, wrong).registration,
            Fact::Unknown(_)
        ));
        let mut absent_profile = a.clone();
        absent_profile.qualification.absence_profile = None;
        let mut unqualified = original;
        unqualified.qualification = absent_profile.qualification.clone();
        assert!(matches!(
            normalize_print(&absent_profile, unqualified).registration,
            Fact::Unknown(_)
        ));
    }
}
