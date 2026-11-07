use sha2::{Digest as _, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use theseus_host::{
    fs::HostDir,
    principal::current_principal,
    process::{ChildEnvironment, NativeCommand},
};
use theseus_native_client::{Action, Client, Event, ExpectedHelper, Poll, RequestId};
use theseus_native_helper_store::{
    BootstrapRequest, IntentRecord, IntentStore, PreparedAcquire, StoreBinding, bootstrap,
};
use theseus_native_protocol::v2::*;
pub fn pinned(name: &str) -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os(name).expect("source-bound pinned native artifact required"),
    );
    assert!(path.is_absolute() && path.is_file());
    path
}
pub fn os() -> Os {
    if cfg!(windows) {
        Os::Windows
    } else if cfg!(target_os = "macos") {
        Os::Macos
    } else {
        Os::Linux
    }
}
pub struct Fixture {
    pub root: tempfile::TempDir,
    pub config: PathBuf,
    pub binding: BindingId,
    pub digest: Digest,
    pub marker: WorkspaceMarker,
    pub prepared: PreparedAcquire,
    pub nonce: String,
    pub receipt: PathBuf,
    pub release: PathBuf,
}
impl Fixture {
    pub fn new(mode: &str) -> Self {
        let mut root = if let Some(evidence) = std::env::var_os("THESEUS_HOST_EVIDENCE") {
            let directory = PathBuf::from(evidence).join("native-client");
            std::fs::create_dir_all(&directory).unwrap();
            tempfile::Builder::new()
                .prefix("case-")
                .tempdir_in(directory)
                .unwrap()
        } else {
            tempfile::tempdir().unwrap()
        };
        // Preserve original roots and bounded raw evidence even on assertion failure.
        root.disable_cleanup(true);
        let ledger = HostDir::create_private(&root.path().join("ledger")).unwrap();
        let workspace = HostDir::create_private(&root.path().join("workspace")).unwrap();
        let intents = HostDir::create_private(&root.path().join("intents")).unwrap();
        let probe = pinned("THESEUS_NATIVE_HELPER_FIXTURE");
        let mut bytes = [0; 16];
        theseus_host::random_bytes(&mut bytes).unwrap();
        let nonce = IntentId::from_bytes(bytes).to_hex();
        let receipt = root.path().join("probe-receipt.json");
        let release = root.path().join("release");
        let args = vec![
            "probe".to_owned(),
            mode.to_owned(),
            nonce.clone(),
            receipt.to_str().unwrap().to_owned(),
            release.to_str().unwrap().to_owned(),
        ];
        let binding = BindingId::from_bytes([1; 16]);
        // Independent exact profile encoder: never call the loader's digest implementation.
        let mut authority = b"theseus-native-helper-config-v1\0".to_vec();
        authority.extend_from_slice(binding.to_hex().as_bytes());
        authority.push(match os() {
            Os::Linux => 1,
            Os::Macos => 2,
            Os::Windows => 3,
        });
        authority.push(1);
        authority.push(5);
        authority.extend_from_slice(b"probe");
        let program = probe.to_str().unwrap().as_bytes();
        authority.extend_from_slice(&(program.len() as u32).to_be_bytes());
        authority.extend_from_slice(program);
        authority.push(args.len() as u8);
        for arg in &args {
            authority.extend_from_slice(&(arg.len() as u16).to_be_bytes());
            authority.extend_from_slice(arg.as_bytes());
        }
        authority.push(0);
        let digest = Digest::from_bytes(Sha256::digest(&authority).into());
        let marker = bootstrap(
            &ledger,
            &workspace,
            BootstrapRequest {
                binding,
                manifest_digest: digest,
                os: os(),
            },
        )
        .unwrap()
        .workspace_marker();
        let mut intent_store = IntentStore::new(intents);
        let prepared = intent_store
            .prepare(IntentRecord {
                intent: IntentId::from_bytes(bytes),
                binding,
                manifest_digest: digest,
                workspace_marker: marker,
            })
            .unwrap();
        let config = root.path().join("config.json");
        let data = serde_json::json!({"schema":1,"ledger_root":root.path().join("ledger"),"workspace_root":root.path().join("workspace"),"binding":binding.to_hex(),"manifest_digest":digest.to_hex(),"workspace_marker":marker.to_hex(),"os":match os(){Os::Linux=>"linux",Os::Macos=>"macos",Os::Windows=>"windows"},"programs":[{"id":"probe","program":probe,"args":args,"environment":{"kind":"replace","entries":[]}}]});
        std::fs::write(&config, serde_json::to_vec(&data).unwrap()).unwrap();
        Self {
            root,
            config,
            binding,
            digest,
            marker,
            prepared,
            nonce,
            receipt,
            release,
        }
    }
    pub fn assert_probe_receipt(&self) {
        let data: serde_json::Value =
            serde_json::from_slice(&bounded_file(&self.receipt, 2048)).unwrap();
        assert_eq!(data["nonce"], self.nonce);
        assert_eq!(
            data["executable_sha256"],
            pinned_hash("THESEUS_NATIVE_HELPER_FIXTURE")
        );
        let expected = match current_principal().unwrap() {
            theseus_host::principal::CurrentPrincipal::Posix {
                effective_uid,
                effective_gid,
            } => serde_json::json!({"effective_uid":effective_uid,"effective_gid":effective_gid}),
            theseus_host::principal::CurrentPrincipal::Windows { token_user_sid } => {
                serde_json::json!({"token_user_sid":token_user_sid})
            }
        };
        assert_eq!(
            data["principal"], expected,
            "principal must be observed inside actual probe"
        );
    }
}
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Fixture {
    pub fn ledger_bytes(&self) -> Vec<u8> {
        std::fs::read(self.root.path().join("ledger/ledger.bin")).unwrap()
    }
    pub fn records(&self) -> Vec<ledger::Entry> {
        self.checked_records()
            .expect("whole postjoin ledger and binding validation")
    }
    pub fn checked_records(&self) -> Result<Vec<ledger::Entry>, ledger::LedgerError> {
        let bytes = bounded_file(
            &self.root.path().join("ledger/ledger.bin"),
            ledger::MAX_LEDGER_BYTES as u64,
        );
        let mut decoder = ledger::Decoder::new();
        let mut offset = 0;
        let mut records = vec![];
        while offset < bytes.len() {
            let progress = decoder.feed(&bytes[offset..])?;
            if progress.consumed == 0 {
                return Err(ledger::LedgerError::InvalidLength);
            }
            offset += progress.consumed;
            if let Some(record) = progress.record {
                records.push(record);
            }
        }
        decoder.finish()?;
        ledger::LedgerView::replay(&records)?;
        match records.first().map(ledger::Record::entry) {
            Some(ledger::Entry::Header {
                binding,
                manifest_digest,
                workspace_marker,
                os: actual_os,
            }) if *binding == self.binding
                && *manifest_digest == self.digest
                && *workspace_marker == self.marker
                && *actual_os == os() => {}
            _ => return Err(ledger::LedgerError::InvalidRecord),
        }
        if bounded_file(
            &self.root.path().join("workspace/.theseus-workspace-marker"),
            32,
        ) != self.marker.to_hex().as_bytes()
        {
            return Err(ledger::LedgerError::InvalidRecord);
        }
        Ok(records
            .into_iter()
            .map(|record| record.entry().clone())
            .collect())
    }
    pub fn wait_receipt(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if self.receipt.exists() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "probe readiness receipt deadline"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
impl Fixture {
    pub fn witness_path(&self) -> PathBuf {
        self.root.path().join("runtime-witness.json")
    }
    pub fn hook_release_path(&self) -> PathBuf {
        self.root.path().join("runtime-release")
    }
    pub fn witness(&self) -> serde_json::Value {
        let data: serde_json::Value =
            serde_json::from_slice(&bounded_file(&self.witness_path(), 4096)).unwrap();
        assert_eq!(data["nonce"], self.nonce);
        assert_eq!(
            data["config_sha256"],
            hex(&Sha256::digest(std::fs::read(&self.config).unwrap()))
        );
        assert_eq!(
            data["executable_sha256"],
            pinned_hash("THESEUS_NATIVE_HELPER_FIXTURE")
        );
        data
    }
    pub fn wait_witness(&self, fields: &[&str]) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if self.witness_path().is_file() {
                let data: serde_json::Value =
                    serde_json::from_slice(&bounded_file(&self.witness_path(), 4096)).unwrap();
                if fields.iter().all(|field| data[*field] == true) {
                    return self.witness();
                }
            }
            assert!(
                Instant::now() < deadline,
                "feature witness deadline: {fields:?}"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

pub fn bounded_file(path: &Path, limit: u64) -> Vec<u8> {
    let mut data = vec![];
    std::fs::File::open(path)
        .unwrap()
        .take(limit + 1)
        .read_to_end(&mut data)
        .unwrap();
    assert!(data.len() <= limit as usize, "bounded artifact exceeded");
    data
}

pub fn pinned_hash(name: &str) -> String {
    use std::sync::OnceLock;
    static NORMAL: OnceLock<(PathBuf, String)> = OnceLock::new();
    static FIXTURE: OnceLock<(PathBuf, String)> = OnceLock::new();
    let cell = match name {
        "THESEUS_NATIVE_HELPER_BINARY" => &NORMAL,
        "THESEUS_NATIVE_HELPER_FIXTURE" => &FIXTURE,
        _ => panic!("unknown pinned artifact role"),
    };
    let path = pinned(name);
    let (original, digest) = cell.get_or_init(|| {
        (
            path.clone(),
            hex(&Sha256::digest(std::fs::read(&path).unwrap())),
        )
    });
    assert_eq!(
        *original, path,
        "pinned executable path changed during fixture suite"
    );
    digest.clone()
}

impl Fixture {
    pub fn expected(&self) -> ExpectedHelper {
        ExpectedHelper {
            profile: theseus_native_protocol::v2::HelperProfile::Fixture,
            binding: StoreBinding {
                binding: self.binding,
                manifest_digest: self.digest,
                workspace_marker: self.marker,
                os: os(),
            },
            programs: ProgramList::new(vec![ProgramId::new("probe").unwrap()]).unwrap(),
        }
    }
    pub fn command(&self, feature: Option<&str>) -> NativeCommand {
        let mut command = NativeCommand::new(pinned(if feature.is_some() {
            "THESEUS_NATIVE_HELPER_FIXTURE"
        } else {
            "THESEUS_NATIVE_HELPER_BINARY"
        }));
        command.environment = ChildEnvironment::Replace(vec![]);
        if let Some(mode) = feature {
            command
                .arg("fixture-serve")
                .arg(&self.config)
                .arg(mode)
                .arg(&self.nonce)
                .arg(self.witness_path())
                .arg(self.hook_release_path());
        } else {
            command.arg("serve").arg("--config").arg(&self.config);
        }
        let metadata_path = pinned("THESEUS_NATIVE_HELPER_BINARY")
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("artifacts.json");
        let metadata_bytes = bounded_file(&metadata_path, 4 * 1024 * 1024);
        let metadata: serde_json::Value = serde_json::from_slice(&metadata_bytes).unwrap();
        for (role, name) in [
            ("helper_production", "THESEUS_NATIVE_HELPER_BINARY"),
            ("helper_fixture", "THESEUS_NATIVE_HELPER_FIXTURE"),
        ] {
            assert_eq!(metadata[role]["path"], pinned(name).to_str().unwrap());
            assert_eq!(metadata[role]["sha256"], pinned_hash(name));
        }
        let authority = serde_json::json!({
            "artifact_metadata_sha256":hex(&Sha256::digest(&metadata_bytes)),
            "artifact_source_sha":metadata["source_sha"],
            "artifact_source_tree_sha256":metadata["source_tree_sha256"],
            "artifact_git_diff_sha256":metadata["git_diff_sha256"],
            "artifact_source_dirty":metadata["dirty"],
            "helper_path": command.program, "helper_sha256": pinned_hash(if feature.is_some() {
                "THESEUS_NATIVE_HELPER_FIXTURE"
            } else { "THESEUS_NATIVE_HELPER_BINARY" }),
            "config_sha256": hex(&Sha256::digest(std::fs::read(&self.config).unwrap())),
            "nonce": self.nonce, "native_os": format!("{:?}",os()),
            "ordinary_native_pipes": true, "cross_compilation_is_native_acceptance": false,
            "scope": "single fixed local probe; no descendants/guest/SSH/reset/service/desktop proof"
        });
        // Preserve each supplied command's authority separately, including
        // reconnect attempts. Constructing a command is not a spawn receipt.
        let mut index = 0;
        let path = loop {
            let path = self
                .root
                .path()
                .join(format!("prepared-launch-authority-{index}.json"));
            if !path.exists() {
                break path;
            }
            index += 1;
        };
        std::fs::write(path, serde_json::to_vec_pretty(&authority).unwrap()).unwrap();
        command
    }
    pub fn start(&self, feature: Option<&str>) -> OwnedClient {
        let client = match Client::start(self.command(feature), self.expected()) {
            Ok(client) => client,
            Err(mut failure) => {
                failure.request_stop();
                let deadline = Instant::now() + Duration::from_secs(12);
                while !failure.poll_settle().unwrap_or(false) {
                    tick(deadline, "initial failed owner settlement");
                }
                panic!("actual helper client start failed: {:?}", failure.error());
            }
        };
        let mut owned = OwnedClient {
            client,
            output: Vec::new(),
            root: self.root.path().to_owned(),
            joined: false,
        };
        owned.ready();
        owned
    }
}

/// Owns the actual Client until its settlement worker has joined. No root is ever
/// deleted (including on panic); expiration exits nonzero with original owners live.
pub struct OwnedClient {
    pub client: Client,
    pub output: Vec<(Channel, u64, Vec<u8>)>,
    root: PathBuf,
    joined: bool,
}
impl OwnedClient {
    pub fn ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(7);
        loop {
            if matches!(self.next(deadline), Some(Event::Ready)) {
                return;
            }
        }
    }
    pub fn next(&mut self, deadline: Instant) -> Option<Event> {
        tick(deadline, "native client pump");
        match self.client.poll().expect("valid actual helper protocol") {
            Poll::Event(event) => {
                if let Event::Output {
                    channel,
                    offset,
                    bytes,
                    ..
                } = &event
                {
                    assert!(
                        self.output.iter().map(|(_, _, b)| b.len()).sum::<usize>()
                            + bytes.as_slice().len()
                            <= MAX_STREAM_BYTES
                    );
                    self.output
                        .push((*channel, *offset, bytes.as_slice().to_vec()));
                    use std::io::Write as _;
                    let mut raw = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(self.root.join("delivered-output.bin"))
                        .unwrap();
                    raw.write_all(bytes.as_slice()).unwrap();
                }
                Some(event)
            }
            Poll::Progress | Poll::Pending | Poll::TransportSettled => None,
        }
    }
    pub fn response(&mut self, request: RequestId) -> Reply {
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            if let Some(Event::Reply {
                request: actual,
                reply,
            }) = self.next(deadline)
            {
                assert_eq!(actual, request, "public response correlation");
                return reply;
            }
        }
    }
    pub fn request(&mut self, action: Action<'_>) -> Reply {
        let id = self.client.submit(action).expect("native action admission");
        self.response(id)
    }
    pub fn acquire(&mut self, fixture: &Fixture) -> LeaseId {
        match self.request(Action::Acquire(&fixture.prepared)) {
            Reply::Acquired {
                intent,
                binding,
                manifest_digest,
                state:
                    AcquireView::Lease {
                        lease,
                        state: LeaseView::Held { intent: nested },
                    },
            } => {
                assert_eq!(intent, fixture.prepared.record().intent);
                assert_eq!(nested, intent);
                assert_eq!(binding, fixture.binding);
                assert_eq!(manifest_digest, fixture.digest);
                lease
            }
            other => panic!("actual acquisition: {other:?}"),
        }
    }
    pub fn run(&mut self, lease: LeaseId, operation: OperationId) {
        assert!(matches!(
            self.request(Action::Run {
                lease,
                operation,
                program: ProgramId::new("probe").unwrap()
            }),
            Reply::Operation {
                state: OperationView::Accepted { .. },
                ..
            }
        ));
    }
    pub fn terminal(&mut self, lease: LeaseId, operation: OperationId) -> Outcome {
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            tick(deadline, "actual native outcome");
            match self.request(Action::Query { lease, operation }) {
                Reply::Operation {
                    state: OperationView::Finished { outcome },
                    ..
                } => return outcome,
                Reply::Operation {
                    state: OperationView::Accepted { .. } | OperationView::Running,
                    ..
                }
                | Reply::Refused {
                    code: RefusalCode::OwnershipPending,
                    ..
                } => {}
                other => panic!("native terminal: {other:?}"),
            }
        }
    }
    pub fn settle(&mut self) {
        if self.joined {
            return;
        }
        self.client.request_stop();
        let deadline = Instant::now() + Duration::from_secs(12);
        while !self.client.poll_settle().unwrap_or(false) {
            tick(deadline, "original client/helper join");
        }
        assert!(self.client.is_settled());
        self.joined = true;
        std::fs::write(self.root.join("client-transport-joined.json"),serde_json::to_vec(&serde_json::json!({
            "local_transport_settled":true,"original_client_owner":true,"actual_settlement_worker_join":true,
            "remote_cleanup_receipt":false
        })).unwrap()).unwrap();
    }
    pub fn reconnect(&mut self, command: NativeCommand) {
        assert!(self.joined && self.client.is_settled());
        self.client
            .reconnect(command)
            .expect("same owned client reconnect");
        self.joined = false;
        self.ready();
    }
}
impl Drop for OwnedClient {
    fn drop(&mut self) {
        self.settle();
    }
}

pub fn tick(deadline: Instant, context: &str) {
    if Instant::now() >= deadline {
        eprintln!(
            "native fixture deadline expired ({context}); original owners and evidence roots retained"
        );
        std::process::exit(98);
    }
    // Predicate-driven polling only; no timing assumption establishes readiness.
    std::thread::sleep(Duration::from_millis(1));
}
impl OwnedClient {
    pub fn joined_again_after_failed_attempt(&mut self) {
        // Reconnect failed after the previous joined connection. Mark the new
        // attempt as owned even when it failed before acquiring native pipes.
        self.joined = false;
        self.settle();
    }
}
