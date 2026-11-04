//! Explicit UTF-8-only schema1 configuration. No bootstrap or path discovery.
use crate::{CatalogEntry, Invocation, InvocationError};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::{
    fmt,
    fs::File,
    io::{self, Read},
    path::Path,
};
use theseus_host::process::{ChildEnvironment, NativeCommand};
use theseus_native_helper_store::StoreBinding;
use theseus_native_protocol::v2;
const MAX_CONFIG: u64 = 1_048_576;
/// Sources remain owned, but ordinary formatting never invokes their formatters.
pub struct ConfigError {
    kind: ConfigErrorKind,
}
enum ConfigErrorKind {
    Invalid,
    Io(io::Error),
    Json(serde_json::Error),
    Invocation(InvocationError),
}
impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid helper configuration")
    }
}
impl fmt::Debug for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            ConfigErrorKind::Invalid => None,
            ConfigErrorKind::Io(e) => Some(e),
            ConfigErrorKind::Json(e) => Some(e),
            ConfigErrorKind::Invocation(e) => Some(e),
        }
    }
}
fn invalid() -> ConfigError {
    ConfigError {
        kind: ConfigErrorKind::Invalid,
    }
}
fn io_error(e: io::Error) -> ConfigError {
    ConfigError {
        kind: ConfigErrorKind::Io(e),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    schema: u8,
    core_manifest_digest: Option<String>,
    integration_manifest_digest: Option<String>,
    integration_manifest: Option<v2::IntegrationManifest>,
    runtime_context: Option<v2::IntegrationRuntimeContext>,
    profile: Option<String>,
    capabilities: Option<Vec<String>>,
    provider_epoch: Option<String>,
    artifacts: Option<Artifacts>,
    ledger_root: String,
    workspace_root: String,
    binding: String,
    manifest_digest: String,
    workspace_marker: String,
    os: String,
    programs: Vec<Program>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifacts {
    base: Artifact,
    target: Artifact,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    length: u64,
    sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Program {
    id: String,
    program: String,
    args: Vec<String>,
    environment: Environment,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Environment {
    kind: String,
    entries: Vec<Entry>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    name: String,
    value: String,
}
fn bounded(s: &str, max: usize) -> bool {
    s.len() <= max && !s.contains('\0')
}
fn absolute(s: &str) -> bool {
    bounded(s, 16384) && Path::new(s).is_absolute()
}
fn os(s: &str) -> Result<v2::Os, ConfigError> {
    match s {
        "linux" => Ok(v2::Os::Linux),
        "macos" => Ok(v2::Os::Macos),
        "windows" => Ok(v2::Os::Windows),
        _ => Err(invalid()),
    }
}
fn parse_bytes(b: &[u8]) -> Result<Config, ConfigError> {
    if b.len() > MAX_CONFIG as usize || b.starts_with(&[0xef, 0xbb, 0xbf]) {
        return Err(invalid());
    }
    let c: Config = serde_json::from_slice(b).map_err(|e| ConfigError {
        kind: ConfigErrorKind::Json(e),
    })?;
    if !matches!(c.schema, 1..=3)
        || !absolute(&c.ledger_root)
        || !absolute(&c.workspace_root)
        || c.programs.is_empty()
        || c.programs.len() > 8
    {
        return Err(invalid());
    }
    if c.schema == 1 {
        let value: serde_json::Value = serde_json::from_slice(b).map_err(|_| invalid())?;
        if ["profile", "capabilities", "provider_epoch", "artifacts"]
            .iter()
            .any(|k| value.get(k).is_some())
        {
            return Err(invalid());
        }
    } else {
        if c.profile.as_deref()
            != Some(if c.schema == 3 {
                "native_integration_v1"
            } else {
                "native_catalog_v1"
            })
            || c.capabilities.as_ref().is_none_or(|v| {
                !v.iter()
                    .map(String::as_str)
                    .eq(v2::NativeCapabilities::NAMES)
            })
        {
            return Err(invalid());
        }
        v2::Digest::parse(c.provider_epoch.as_deref().ok_or_else(invalid)?)
            .map_err(|_| invalid())?;
        let artifacts = c.artifacts.as_ref().ok_or_else(invalid)?;
        for a in [&artifacts.base, &artifacts.target] {
            if a.length == 0 || a.length > v2::MAX_NATIVE_FILE_BYTES {
                return Err(invalid());
            }
            v2::Digest::parse(&a.sha256).map_err(|_| invalid())?;
        }
        const IDS: [&str; 8] = [
            "check_base",
            "check_data",
            "check_target",
            "configure",
            "exercise",
            "install_base",
            "install_target",
            "uninstall",
        ];
        if !c.programs.iter().map(|p| p.id.as_str()).eq(IDS) {
            return Err(invalid());
        }
        let reader = Path::new(&c.workspace_root).join(if c.os == "windows" {
            "reader.exe"
        } else {
            "reader"
        });
        for p in &c.programs {
            if ["check_base", "check_data", "check_target", "exercise"].contains(&p.id.as_str())
                && Path::new(&p.program) != reader
            {
                return Err(invalid());
            }
        }
    }
    if c.schema != 3 {
        let value: serde_json::Value = serde_json::from_slice(b).map_err(|_| invalid())?;
        if [
            "core_manifest_digest",
            "integration_manifest_digest",
            "integration_manifest",
            "runtime_context",
        ]
        .iter()
        .any(|k| value.get(k).is_some())
        {
            return Err(invalid());
        }
    }
    if c.schema == 3 {
        let core = v2::Digest::parse(c.core_manifest_digest.as_deref().ok_or_else(invalid)?)
            .map_err(|_| invalid())?;
        let integration = v2::Digest::parse(
            c.integration_manifest_digest
                .as_deref()
                .ok_or_else(invalid)?,
        )
        .map_err(|_| invalid())?;
        let manifest = c.integration_manifest.as_ref().ok_or_else(invalid)?;
        manifest.validate().map_err(|_| invalid())?;
        if manifest.core_manifest_digest != core.to_hex()
            || manifest.digest().map_err(|_| invalid())? != integration
            || v2::integration_manifest_digest(core, integration).to_hex() != c.manifest_digest
        {
            return Err(invalid());
        }
        let context = c.runtime_context.as_ref().ok_or_else(invalid)?;
        context.validate().map_err(|_| invalid())?;
        manifest
            .validate_context(context, os(&c.os)?)
            .map_err(|_| invalid())?;
        if context.binding != c.binding
            || context.manifest_digest != c.manifest_digest
            || context.workspace_marker != c.workspace_marker
            || Some(&context.provider_epoch) != c.provider_epoch.as_ref()
            || context.integration_manifest_digest != integration.to_hex()
        {
            return Err(invalid());
        }
    } else if c.core_manifest_digest.is_some()
        || c.integration_manifest_digest.is_some()
        || c.integration_manifest.is_some()
        || c.runtime_context.is_some()
    {
        return Err(invalid());
    }
    v2::BindingId::parse(&c.binding).map_err(|_| invalid())?;
    v2::Digest::parse(&c.manifest_digest).map_err(|_| invalid())?;
    v2::WorkspaceMarker::parse(&c.workspace_marker).map_err(|_| invalid())?;
    os(&c.os)?;
    let mut previous: Option<&str> = None;
    for p in &c.programs {
        v2::ProgramId::new(&p.id).map_err(|_| invalid())?;
        if previous.is_some_and(|id| id >= p.id.as_str())
            || !absolute(&p.program)
            || Path::new(&p.program)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
            || p.args.len() > 16
            || p.args.iter().any(|a| !bounded(a, 512))
            || p.environment.kind != "replace"
            || p.environment.entries.len() > 32
        {
            return Err(invalid());
        }
        previous = Some(&p.id);
        let mut names: Vec<&str> = Vec::with_capacity(32);
        let mut total = p.program.len() + p.args.iter().map(String::len).sum::<usize>();
        for e in &p.environment.entries {
            let key = e.name.as_bytes();
            if key.is_empty()
                || key.len() > 128
                || !(key[0].is_ascii_alphabetic() || key[0] == b'_')
                || !key.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_')
                || !bounded(&e.value, 1024)
                || names.last().is_some_and(|k| *k >= e.name.as_str())
                || names.iter().any(|k| k.eq_ignore_ascii_case(&e.name))
            {
                return Err(invalid());
            }
            total += e.name.len() + e.value.len();
            names.push(&e.name);
        }
        if total > 16384 {
            return Err(invalid());
        }
    }
    Ok(c)
}
// Only validated schema values reach the encoder; counts/lengths fit their widths.
fn authority_digest(c: &Config) -> String {
    let mut h = Sha256::new();
    if c.schema == 1 {
        h.update(b"theseus-native-helper-config-v1\0");
    } else {
        h.update(b"theseus-native-helper-config-v2\0");
        let profile = "native_catalog_v1";
        h.update([profile.len() as u8]);
        h.update(profile.as_bytes());
        for root in [&c.ledger_root, &c.workspace_root] {
            h.update((root.len() as u32).to_be_bytes());
            h.update(root.as_bytes());
        }
        for name in v2::NativeCapabilities::NAMES {
            h.update([name.len() as u8]);
            h.update(name.as_bytes());
        }
        for a in [
            &c.artifacts.as_ref().expect("native artifacts").base,
            &c.artifacts.as_ref().unwrap().target,
        ] {
            h.update(a.length.to_be_bytes());
            h.update(a.sha256.as_bytes());
        }
    }
    if c.schema == 1 {
        h.update(c.binding.as_bytes());
    }
    h.update([
        match c.os.as_str() {
            "linux" => 1,
            "macos" => 2,
            _ => 3,
        },
        c.programs.len() as u8,
    ]);
    for p in &c.programs {
        h.update([p.id.len() as u8]);
        h.update(p.id.as_bytes());
        h.update((p.program.len() as u32).to_be_bytes());
        h.update(p.program.as_bytes());
        h.update([p.args.len() as u8]);
        for a in &p.args {
            h.update((a.len() as u16).to_be_bytes());
            h.update(a.as_bytes());
        }
        h.update([p.environment.entries.len() as u8]);
        for e in &p.environment.entries {
            for s in [&e.name, &e.value] {
                h.update((s.len() as u16).to_be_bytes());
                h.update(s.as_bytes());
            }
        }
    }
    format!("{:x}", h.finalize())
}
/// Compute the manifest over explicit, strictly validated configuration authority.
/// The supplied manifest_digest is syntax checked but not compared by this helper.
pub fn config_manifest_digest(bytes: &[u8]) -> Result<v2::Digest, ConfigError> {
    let config = parse_bytes(bytes)?;
    let core = v2::Digest::parse(&authority_digest(&config)).map_err(|_| invalid())?;
    if config.schema == 3 {
        Ok(v2::integration_manifest_digest(
            core,
            config
                .integration_manifest
                .as_ref()
                .ok_or_else(invalid)?
                .digest()
                .map_err(|_| invalid())?,
        ))
    } else {
        Ok(core)
    }
}
/// Load a bounded, explicit regular config; verify authority before opening roots.
/// UTF-8 paths/argv are this CLI profile's restriction, not a native API restriction.
pub fn load_config(path: &Path) -> Result<Invocation, ConfigError> {
    if !path.is_absolute() || path.to_str().is_none_or(|s| !bounded(s, 16384)) {
        return Err(invalid());
    }
    if !std::fs::symlink_metadata(path)
        .map_err(io_error)?
        .file_type()
        .is_file()
    {
        return Err(invalid());
    }
    let file = File::open(path).map_err(io_error)?;
    if !file.metadata().map_err(io_error)?.is_file() {
        return Err(invalid());
    }
    let mut b = Vec::new();
    file.take(MAX_CONFIG + 1)
        .read_to_end(&mut b)
        .map_err(io_error)?;
    let c = parse_bytes(&b)?;
    if (if c.schema == 3 {
        Some(authority_digest(&c)) != c.core_manifest_digest
    } else {
        authority_digest(&c) != c.manifest_digest
    }) || os(&c.os)? != crate::invocation::host_os()
    {
        return Err(invalid());
    }
    let expected = StoreBinding {
        binding: v2::BindingId::parse(&c.binding).map_err(|_| invalid())?,
        manifest_digest: v2::Digest::parse(&c.manifest_digest).map_err(|_| invalid())?,
        workspace_marker: v2::WorkspaceMarker::parse(&c.workspace_marker).map_err(|_| invalid())?,
        os: os(&c.os)?,
    };
    let provider_epoch = if c.schema >= 2 {
        Some(
            v2::Digest::parse(c.provider_epoch.as_deref().ok_or_else(invalid)?)
                .map_err(|_| invalid())?,
        )
    } else {
        None
    };
    let catalog = c
        .programs
        .into_iter()
        .map(|p| {
            let mut command = NativeCommand::new(&p.program);
            command.args(p.args);
            command.environment = ChildEnvironment::Replace(
                p.environment
                    .entries
                    .into_iter()
                    .map(|e| (e.name.into(), e.value.into()))
                    .collect(),
            );
            Ok(CatalogEntry {
                id: v2::ProgramId::new(&p.id).map_err(|_| invalid())?,
                command,
            })
        })
        .collect::<Result<Vec<_>, ConfigError>>()?;
    let result = if let Some(provider_epoch) = provider_epoch {
        Invocation::new_native(
            c.ledger_root.into(),
            c.workspace_root.into(),
            expected,
            catalog,
            provider_epoch,
            {
                let artifacts = c.artifacts.as_ref().ok_or_else(invalid)?;
                v2::artifact_commitment(
                    artifacts.base.length,
                    v2::Digest::parse(&artifacts.base.sha256).map_err(|_| invalid())?,
                    artifacts.target.length,
                    v2::Digest::parse(&artifacts.target.sha256).map_err(|_| invalid())?,
                )
                .map_err(|_| invalid())?
            },
        )
    } else {
        Invocation::new(
            c.ledger_root.into(),
            c.workspace_root.into(),
            expected,
            catalog,
        )
    };
    let mut result = result.map_err(|e| ConfigError {
        kind: ConfigErrorKind::Invocation(e),
    })?;
    if c.schema == 3 {
        result.profile = v2::HelperProfile::Integration {
            provider_epoch: provider_epoch.ok_or_else(invalid)?,
            core_manifest_digest: v2::Digest::parse(
                c.core_manifest_digest.as_deref().ok_or_else(invalid)?,
            )
            .map_err(|_| invalid())?,
            integration_manifest_digest: v2::Digest::parse(
                c.integration_manifest_digest
                    .as_deref()
                    .ok_or_else(invalid)?,
            )
            .map_err(|_| invalid())?,
        };
        result.integration = Some(Box::new(crate::integration::service::ServiceConfig {
            manifest: c.integration_manifest.ok_or_else(invalid)?,
            context: c.runtime_context.ok_or_else(invalid)?,
        }));
    }
    Ok(result)
    /*result.map_err(|e| ConfigError {
        kind: ConfigErrorKind::Invocation(e),
    })*/
}
#[cfg(test)]
mod tests {
    use super::*;
    fn vector() -> serde_json::Value {
        let mut v = serde_json::json!({"schema":1,"ledger_root":"/missing/ledger","workspace_root":"/missing/workspace","binding":"11111111111111111111111111111111","manifest_digest":"21fbf07b874e4de2f81be82dbd28b74ad99d53e955cdddb02015d707988a1b49","workspace_marker":"99999999999999999999999999999999","os":"linux","programs":[{"id":"probe_success","program":"/bin/probe","args":["","é!"],"environment":{"kind":"replace","entries":[{"name":"A","value":"x"},{"name":"Z","value":""}]}}]});
        if cfg!(windows) {
            v["ledger_root"] = serde_json::json!("C:\\missing\\ledger");
            v["workspace_root"] = serde_json::json!("C:\\missing\\workspace");
            v["programs"][0]["program"] = serde_json::json!("C:\\bin\\probe");
        }
        v
    }
    fn parse(v: &serde_json::Value) -> Result<Config, ConfigError> {
        parse_bytes(&serde_json::to_vec(v).unwrap())
    }
    fn native_vector() -> serde_json::Value {
        let mut v = vector();
        v["schema"] = serde_json::json!(2);
        v["profile"] = serde_json::json!("native_catalog_v1");
        v["capabilities"] = serde_json::json!(v2::NativeCapabilities::NAMES);
        v["provider_epoch"] = serde_json::json!("44".repeat(32));
        v["artifacts"] = serde_json::json!({"base":{"length":10000,"sha256":"55".repeat(32)},"target":{"length":20000,"sha256":"66".repeat(32)}});
        let program = v["programs"][0].clone();
        v["programs"] = serde_json::json!(
            [
                "check_base",
                "check_data",
                "check_target",
                "configure",
                "exercise",
                "install_base",
                "install_target",
                "uninstall"
            ]
            .map(|id| {
                let mut p = program.clone();
                p["id"] = serde_json::json!(id);
                if ["check_base", "check_data", "check_target", "exercise"].contains(&id) {
                    let prefix = Path::new(v["workspace_root"].as_str().unwrap());
                    p["program"] = serde_json::json!(prefix.join(if v["os"] == "windows" {
                        "reader.exe"
                    } else {
                        "reader"
                    }));
                    p["args"] = serde_json::json!([id, prefix.join("data").to_str().unwrap()]);
                }
                p
            })
        );
        v
    }
    #[test]
    fn native_manifest_binds_catalog_and_reader_selection_not_instance_identity() {
        let v = native_vector();
        let original = authority_digest(&parse(&v).unwrap());
        for (pointer, value) in [
            (
                "/programs/5/program",
                serde_json::json!(if cfg!(windows) {
                    "C:\\bin\\other"
                } else {
                    "/bin/other"
                }),
            ),
            ("/programs/5/args", serde_json::json!(["different"])),
            (
                "/programs/5/environment/entries/0/value",
                serde_json::json!("different"),
            ),
            (
                "/artifacts/target/sha256",
                serde_json::json!("77".repeat(32)),
            ),
        ] {
            let mut changed = v.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert_ne!(original, authority_digest(&parse(&changed).unwrap()));
        }
        for id in [0, 1, 2, 4] {
            let mut bad = v.clone();
            bad["programs"][id]["program"] = serde_json::json!(std::env::current_exe().unwrap());
            assert!(
                parse(&bad).is_err(),
                "native check must select installed reader"
            );
            let mut changed = v.clone();
            changed["programs"][id]["args"] = serde_json::json!(["--version"]);
            assert_ne!(original, authority_digest(&parse(&changed).unwrap()));
        }
        let mut bad = v;
        bad["profile"] = serde_json::json!("another_profile");
        assert!(parse(&bad).is_err());
    }
    #[test]
    fn native_config_digest_matches_independent_binary_vector() {
        let mut v = native_vector();
        v["workspace_root"] = serde_json::json!("/missing/workspace");
        v["ledger_root"] = serde_json::json!("/missing/ledger");
        v["os"] = serde_json::json!("linux");
        for p in v["programs"].as_array_mut().unwrap() {
            let id = p["id"].as_str().unwrap().to_owned();
            if ["check_base", "check_data", "check_target", "exercise"].contains(&id.as_str()) {
                p["program"] = serde_json::json!("/missing/workspace/reader");
                p["args"] = serde_json::json!([id, "/missing/workspace/data".to_owned()]);
            } else {
                p["program"] = serde_json::json!("/bin/probe");
            }
        }
        let c: Config = serde_json::from_value(v).unwrap();
        assert_eq!(
            authority_digest(&c),
            "0fabb114943b8e79b452d6b5241ab7b0ef75e4934075a3189af0a7950a630228"
        );
    }
    #[test]
    fn native_config_requires_exact_profile_epoch_artifacts_and_catalog() {
        let v = native_vector();
        let original = authority_digest(&parse(&v).unwrap());
        for key in ["profile", "capabilities", "provider_epoch", "artifacts"] {
            let mut bad = v.clone();
            bad.as_object_mut().unwrap().remove(key);
            assert!(parse(&bad).is_err());
            let mut fixture = vector();
            fixture[key] = serde_json::Value::Null;
            assert!(parse(&fixture).is_err());
        }
        for (key, value) in [
            ("profile", serde_json::json!("trusted_fixture_v1")),
            ("provider_epoch", serde_json::json!("not an epoch")),
            ("capabilities", serde_json::json!(["catalog_run"])),
        ] {
            let mut bad = v.clone();
            bad[key] = value;
            assert!(parse(&bad).is_err());
        }
        for (key, value) in [
            ("binding", "88".repeat(16)),
            ("provider_epoch", "77".repeat(32)),
            ("workspace_marker", "99".repeat(16)),
        ] {
            let mut changed = v.clone();
            changed[key] = serde_json::json!(value);
            assert_eq!(original, authority_digest(&parse(&changed).unwrap()));
        }
        let mut changed = v.clone();
        changed["artifacts"]["target"]["sha256"] = serde_json::json!("77".repeat(32));
        assert_ne!(original, authority_digest(&parse(&changed).unwrap()));
        let mut bad = v;
        bad["programs"].as_array_mut().unwrap().pop();
        assert!(parse(&bad).is_err());
    }
    #[test]
    fn config_digest_matches_independent_literal_binary_vector() {
        let mut v = vector();
        v["programs"][0]["program"] = serde_json::json!("/bin/probe");
        let c: Config = serde_json::from_value(v).unwrap();
        assert_eq!(
            authority_digest(&c),
            "21fbf07b874e4de2f81be82dbd28b74ad99d53e955cdddb02015d707988a1b49"
        );
    }
    #[test]
    fn config_schema_rejects_duplicates_unknown_missing_null_and_wrong_types() {
        let raw = serde_json::to_string(&vector()).unwrap();
        for s in [
            raw.replacen("\"schema\":1", "\"schema\":1,\"schema\":1", 1),
            raw.replacen(
                "\"kind\":\"replace\"",
                "\"kind\":\"replace\",\"kind\":\"replace\"",
                1,
            ),
            raw.replacen("\"value\":\"x\"", "\"value\":\"x\",\"value\":\"x\"", 1),
            raw.replacen(
                "\"id\":\"probe_success\"",
                "\"id\":\"probe_success\",\"id\":\"probe_success\"",
                1,
            ),
        ] {
            assert!(parse_bytes(s.as_bytes()).is_err());
        }
        for key in [
            "schema",
            "ledger_root",
            "workspace_root",
            "binding",
            "manifest_digest",
            "workspace_marker",
            "os",
            "programs",
        ] {
            let mut v = vector();
            v.as_object_mut().unwrap().remove(key);
            assert!(parse(&v).is_err());
            let mut v = vector();
            v[key] = serde_json::Value::Null;
            assert!(parse(&v).is_err());
        }
        for (key, value) in [
            ("extra", serde_json::json!(true)),
            ("schema", serde_json::json!(1.0)),
            ("schema", serde_json::json!("1")),
            ("schema", serde_json::json!(2)),
        ] {
            let mut v = vector();
            v[key] = value;
            assert!(parse(&v).is_err());
        }
        assert!(parse_bytes(b"\xef\xbb\xbf{}").is_err());
        assert!(parse_bytes(b"\xff").is_err());
        assert!(parse_bytes(br#"{"schema":1,"bad":"\ud800"}"#).is_err());
    }
    #[test]
    fn config_authority_refuses_bounds_ordering_and_implicit_environment() {
        for (key, value) in [
            ("program", serde_json::json!("relative")),
            ("program", serde_json::json!("/bin/a.BAT")),
            ("program", serde_json::json!("/bin/a\u{0}")),
            ("args", serde_json::json!(["x".repeat(513)])),
            ("args", serde_json::json!(vec![""; 17])),
        ] {
            let mut v = vector();
            v["programs"][0][key] = value;
            assert!(parse(&v).is_err());
        }
        for entries in [
            serde_json::json!([{"name":"a","value":""},{"name":"A","value":""}]),
            serde_json::json!([{"name":"A","value":""},{"name":"a","value":""}]),
            serde_json::json!([{"name":"1BAD","value":""}]),
            serde_json::json!([{"name":"A","value":"x".repeat(1025)}]),
        ] {
            let mut v = vector();
            v["programs"][0]["environment"]["entries"] = entries;
            assert!(parse(&v).is_err());
        }
        let mut v = vector();
        v["programs"][0]["environment"]["kind"] = serde_json::json!("inherit");
        assert!(parse(&v).is_err());
        let mut v = vector();
        let program = v["programs"][0].clone();
        v["programs"].as_array_mut().unwrap().push(program);
        assert!(parse(&v).is_err());
    }
    #[test]
    fn config_mismatch_precedes_root_acquisition_and_errors_are_private() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.json");
        let mut v = vector();
        v["manifest_digest"] = serde_json::json!("a".repeat(64));
        std::fs::write(&p, serde_json::to_vec(&v).unwrap()).unwrap();
        let e = load_config(&p).err().unwrap();
        assert!(matches!(e.kind, ConfigErrorKind::Invalid));
        assert_eq!(format!("{e}"), "invalid helper configuration");
        assert_eq!(format!("{e:?}"), "invalid helper configuration");
    }
    #[test]
    fn config_closed_nested_objects_and_aggregate_bounds_are_enforced() {
        for pointer in [
            "/programs/0",
            "/programs/0/environment",
            "/programs/0/environment/entries/0",
        ] {
            let mut v = vector();
            v.pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("secret_extra".into(), serde_json::json!(1));
            assert!(parse(&v).is_err());
            let original = vector();
            for key in original
                .pointer(pointer)
                .unwrap()
                .as_object()
                .unwrap()
                .keys()
            {
                let mut v = vector();
                v.pointer_mut(pointer)
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .remove(key);
                assert!(parse(&v).is_err());
            }
        }
        let mut v = vector();
        v["programs"][0]["environment"]["entries"] = serde_json::json!(
            (0..32)
                .map(|i| serde_json::json!({"name":format!("K{i:02}"),"value":"x".repeat(1024)}))
                .collect::<Vec<_>>()
        );
        assert!(parse(&v).is_err());
    }
    #[test]
    fn config_loader_constructs_only_explicit_authority_and_retains_sources() {
        use std::error::Error;
        let root = tempfile::tempdir().unwrap();
        let ledger = root.path().join("ledger");
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&ledger).unwrap();
        std::fs::create_dir(&workspace).unwrap();
        let path = root.path().join("config");
        let mut v = vector();
        v["ledger_root"] = serde_json::json!(ledger);
        v["workspace_root"] = serde_json::json!(workspace);
        v["os"] = serde_json::json!(match crate::invocation::host_os() {
            v2::Os::Linux => "linux",
            v2::Os::Macos => "macos",
            v2::Os::Windows => "windows",
        });
        let c = parse(&v).unwrap();
        v["manifest_digest"] = serde_json::json!(authority_digest(&c));
        std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        let invocation = load_config(&path).unwrap();
        assert_eq!(invocation.catalog.len(), 1);
        assert!(invocation.catalog[0].command.cwd.is_none());
        assert!(matches!(
            invocation.catalog[0].command.environment,
            ChildEnvironment::Replace(_)
        ));
        assert_eq!(std::fs::read_dir(&ledger).unwrap().count(), 0);
        std::fs::write(&path, b"{SECRET_INVALID_JSON").unwrap();
        let e = load_config(&path).err().unwrap();
        assert!(e.source().is_some());
        assert!(!format!("{e:?}").contains("SECRET"));
        let e = load_config(&root.path().join("SECRET_MISSING"))
            .err()
            .unwrap();
        assert!(e.source().is_some());
        assert!(!format!("{e}").contains("SECRET"));
    }
    #[test]
    fn config_source_formatters_are_never_invoked_by_public_formatting() {
        struct Explosive;
        impl fmt::Debug for Explosive {
            fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
                panic!("private debug invoked")
            }
        }
        impl fmt::Display for Explosive {
            fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
                panic!("private display invoked")
            }
        }
        impl std::error::Error for Explosive {}
        let e = io_error(io::Error::other(Explosive));
        assert_eq!(format!("{e}"), "invalid helper configuration");
        assert_eq!(format!("{e:?}"), "invalid helper configuration");
    }
    #[test]
    fn config_file_is_explicit_regular_and_size_bounded() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_config(dir.path()).is_err());
        assert!(load_config(Path::new("relative.json")).is_err());
        let p = dir.path().join("config");
        std::fs::write(&p, vec![b' '; 1048577]).unwrap();
        assert!(matches!(
            load_config(&p).err().unwrap().kind,
            ConfigErrorKind::Invalid
        ));
    }
}
