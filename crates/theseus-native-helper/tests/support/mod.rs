use sha2::{Digest as _, Sha256};
use std::{
    ffi::OsString,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, ExitStatus, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    thread::JoinHandle,
    time::{Duration, Instant},
};
use theseus_host::{fs::HostDir, principal::current_principal};
use theseus_native_helper_store::{
    BootstrapRequest, IntentRecord, IntentStore, PreparedAcquire, bootstrap,
};
use theseus_native_protocol::{Role, SessionId, v2::*};

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
    #[cfg(windows)]
    pub probe: PathBuf,
    pub receipt: PathBuf,
    pub release: PathBuf,
}
impl Fixture {
    pub fn new(mode: &str) -> Self {
        let mut root = if let Some(evidence) = std::env::var_os("THESEUS_HOST_EVIDENCE") {
            let directory = PathBuf::from(evidence).join("helper-runtime");
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
            #[cfg(windows)]
            probe,
            receipt,
            release,
        }
    }
    pub fn start(&self) -> Peer {
        let peer = Peer::start(&self.config);
        let data: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&self.config).unwrap()).unwrap();
        assert_eq!(data["workspace_marker"], self.marker.to_hex());
        peer
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

pub struct Peer {
    child: Child,
    input: Option<ChildStdin>,
    read: Sender<()>,
    frames: Receiver<Result<Envelope, String>>,
    reader: Option<JoinHandle<()>>,
    stderr: Option<JoinHandle<Vec<u8>>>,
    guard: SessionGuard,
    sequence: u64,
    session: SessionId,
    status: Option<ExitStatus>,
    join_receipt: PathBuf,
    identity: serde_json::Value,
    pub output: Vec<(Channel, u64, Vec<u8>)>,
}
impl Peer {
    pub fn start(config: &Path) -> Self {
        Self::spawn(
            pinned("THESEUS_NATIVE_HELPER_BINARY"),
            vec![
                "serve".into(),
                "--config".into(),
                config.as_os_str().to_owned(),
            ],
            config,
            true,
        )
    }
    pub fn spawn(binary: PathBuf, args: Vec<OsString>, config: &Path, preflight: bool) -> Self {
        let pressure = args.first().is_some_and(|arg| arg == "fixture-serve")
            && args.get(2).is_some_and(|arg| arg == "pressure");
        let transcript_limit = if pressure { 9 * 1024 * 1024 } else { 262144 };
        let normal = binary;
        let config_value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(config).unwrap()).unwrap();
        let identity = serde_json::json!({"helper_path":normal,"helper_sha256":pinned_hash(if normal==pinned("THESEUS_NATIVE_HELPER_BINARY"){"THESEUS_NATIVE_HELPER_BINARY"}else{"THESEUS_NATIVE_HELPER_FIXTURE"}),"config_sha256":hex(&Sha256::digest(std::fs::read(config).unwrap())),"native_os":format!("{:?}",os()),"cross_compilation_is_native_acceptance":false,"output_pressure":pressure,"transcript_limit":transcript_limit,"nonce":config_value["programs"][0]["args"][2],"fixture_path":config_value["programs"][0]["program"]});
        std::fs::write(
            config.parent().unwrap().join("launch-authority.json"),
            serde_json::to_vec_pretty(&identity).unwrap(),
        )
        .unwrap();
        let mut child = Command::new(normal)
            .args(args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let mut stdout = child.stdout.take().unwrap();
        #[cfg(target_os = "linux")]
        let identity = {
            let mut identity = identity;
            let actual = rustix::pipe::fcntl_setpipe_size(&stdout, 4096)
                .expect("configure original ordinary pipe capacity");
            assert!(
                actual > 0 && actual <= 4096,
                "kernel accepted different capacity: {actual}"
            );
            identity["native_stdout_capacity_bytes"] = serde_json::json!(actual);
            std::fs::write(
                config.parent().unwrap().join("launch-authority.json"),
                serde_json::to_vec_pretty(&identity).unwrap(),
            )
            .unwrap();
            identity
        };

        let mut stderr = child.stderr.take().unwrap();
        let (read_tx, read_rx) = mpsc::channel();
        let (tx, frames) = mpsc::sync_channel(1);
        let log_root = config.parent().unwrap();
        let mut transcript =
            std::fs::File::create(log_root.join(format!("protocol-stdout-{}.bin", child.id())))
                .unwrap();
        let stderr_path = log_root.join(format!("helper-stderr-{}.bin", child.id()));
        let reader = std::thread::spawn(move || {
            let mut total = 0;
            let mut output_frames = 0;
            let mut output_bytes = 0;
            while read_rx.recv().is_ok() {
                let result = (|| {
                    let mut size = [0; 4];
                    stdout.read_exact(&mut size).map_err(|e| e.to_string())?;
                    let len = u32::from_be_bytes(size) as usize;
                    if len > MAX_BODY_BYTES {
                        return Err("oversize frame".into());
                    }
                    total += len + 4;
                    if total > transcript_limit {
                        return Err("transcript bound".into());
                    }
                    let mut raw = vec![0; len + 4];
                    raw[..4].copy_from_slice(&size);
                    stdout
                        .read_exact(&mut raw[4..])
                        .map_err(|e| e.to_string())?;
                    transcript.write_all(&raw).map_err(|e| e.to_string())?;
                    let mut decoder = Decoder::new();
                    let envelope = decoder
                        .feed(&raw)
                        .map_err(|e| format!("{e:?}"))?
                        .frame
                        .unwrap();
                    decoder.finish().map_err(|e| format!("{e:?}"))?;
                    if pressure && let Message::Output { bytes, .. } = envelope.message() {
                        output_frames += 1;
                        output_bytes += bytes.as_slice().len();
                        if raw.len() != 65536
                            || bytes.as_slice().len() > 512
                            || output_frames > 128
                            || output_bytes > MAX_STREAM_BYTES
                        {
                            return Err("pressure output bounds violated".into());
                        }
                    }
                    Ok(envelope)
                })();
                let failed = result.is_err();
                if tx.send(result).is_err() || failed {
                    break;
                }
            }
        });
        let stderr = std::thread::spawn(move || {
            let mut data = Vec::new();
            stderr.by_ref().take(4097).read_to_end(&mut data).unwrap();
            std::fs::write(stderr_path, &data).unwrap();
            data
        });
        let mut session_bytes = [0; 16];
        theseus_host::random_bytes(&mut session_bytes).unwrap();
        let session = SessionId::parse(&hex(&session_bytes)).unwrap();
        let join_receipt = log_root.join(format!("helper-join-{}.json", child.id()));
        let mut peer = Self {
            child,
            input,
            read: read_tx,
            frames,
            reader: Some(reader),
            stderr: Some(stderr),
            join_receipt,
            identity,
            guard: SessionGuard::new(session.clone(), Role::Controller),
            sequence: 0,
            session,
            status: None,
            output: vec![],
        };
        if !preflight {
            return peer;
        }
        peer.send_message(Message::Hello {
            role: Role::Controller,
        });
        assert!(matches!(peer.next(), Message::Hello { role: Role::Helper }));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let result = peer.request(Request::Describe);
            match result {
                Reply::Description {
                    os: observed,
                    binding,
                    manifest_digest,
                    workspace_marker,
                    ..
                } => {
                    assert_eq!(observed, os());
                    let data: serde_json::Value =
                        serde_json::from_slice(&std::fs::read(config).unwrap()).unwrap();
                    assert_eq!(data["binding"], binding.to_hex());
                    assert_eq!(data["manifest_digest"], manifest_digest.to_hex());
                    assert_eq!(data["workspace_marker"], workspace_marker.to_hex());
                    break;
                }
                Reply::Refused {
                    code: RefusalCode::OwnershipPending,
                } => {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(2));
                }
                other => panic!("Description preflight {other:?}"),
            }
        }

        peer
    }
    fn send_message(&mut self, message: Message) -> u64 {
        let seq = self.sequence;
        self.sequence += 1;
        let envelope = Envelope::new(self.session.clone(), seq, message).unwrap();
        self.guard.accept_outbound(&envelope).unwrap();
        self.input
            .as_mut()
            .unwrap()
            .write_all(&encode(&envelope).unwrap())
            .unwrap();
        seq
    }
    pub fn send(&mut self, request: Request) -> u64 {
        self.send_message(Message::Request { request })
    }
    pub fn next(&mut self) -> Message {
        self.read.send(()).unwrap();
        let frame = self
            .frames
            .recv_timeout(Duration::from_secs(8))
            .expect("bounded ordinary-pipe response")
            .expect("valid protocol bytes");
        self.guard.accept_inbound(&frame).unwrap();
        if let Message::Output {
            channel,
            offset,
            bytes,
            ..
        } = frame.message()
        {
            self.output
                .push((*channel, *offset, bytes.as_slice().to_vec()));
        }
        frame.message().clone()
    }
    pub fn response(&mut self, seq: u64) -> Reply {
        loop {
            if let Message::Response { reply_to, result } = self.next() {
                assert_eq!(reply_to, seq);
                return result;
            }
        }
    }
    pub fn request(&mut self, request: Request) -> Reply {
        let seq = self.send(request);
        self.response(seq)
    }
    pub fn acquire(&mut self, f: &Fixture) -> LeaseId {
        let result = self.request(f.prepared.request());
        match result {
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
                assert_eq!(intent, f.prepared.record().intent);
                assert_eq!(intent, nested);
                assert_eq!(binding, f.binding);
                assert_eq!(manifest_digest, f.digest);
                lease
            }
            other => panic!("acquire {other:?}"),
        }
    }
    pub fn run(&mut self, lease: LeaseId, operation: OperationId) {
        assert!(matches!(
            self.request(Request::Run {
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
            assert!(Instant::now() < deadline, "terminal deadline");
            match self.request(Request::QueryOperation { lease, operation }) {
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
                } => std::thread::sleep(Duration::from_millis(2)),
                other => panic!("terminal {other:?}"),
            }
        }
    }
    pub fn close(self) {
        self.close_expected(0, b"");
    }
    pub fn close_expected(mut self, code: i32, diagnostic: &[u8]) {
        self.input.take();
        let status = self.join();
        assert_eq!(status.code(), Some(code), "helper exit {status}");
        let stderr = self.stderr.take().unwrap().join().unwrap();
        assert_eq!(stderr, diagnostic, "bounded fixed stderr");
    }
    pub fn join(&mut self) -> ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                self.status = Some(status);
                std::fs::write(&self.join_receipt,serde_json::to_vec(&serde_json::json!({"joined":true,"status":format!("{status}"),"original_child_authority":true,"launch_authority":self.identity})).unwrap()).unwrap();
                return status;
            }
            if Instant::now() >= deadline {
                eprintln!(
                    "helper owner unresolved after cooperative cleanup; original authority retained"
                );
                std::process::exit(98);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.input.take();
        if self.status.is_none() {
            self.join();
        }
        let (tx, _) = mpsc::channel();
        self.read = tx;
        if let Some(reader) = self.reader.take() {
            reader.join().unwrap();
        }
        if let Some(stderr) = self.stderr.take() {
            let _ = stderr.join();
        }
    }
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
pub fn invalid_config(config: &Path) -> (ExitStatus, Vec<u8>, Vec<u8>) {
    let temp = tempfile::tempdir().unwrap();
    let out = temp.path().join("stdout");
    let err = temp.path().join("stderr");
    let mut child = Command::new(pinned("THESEUS_NATIVE_HELPER_BINARY"))
        .args(["serve", "--config"])
        .arg(config)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(&out).unwrap())
        .stderr(std::fs::File::create(&err).unwrap())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(6);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            eprintln!("invalid-config child original owner unresolved");
            std::process::exit(98);
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    let bounded = |path| {
        let mut data = vec![];
        std::fs::File::open(path)
            .unwrap()
            .take(4097)
            .read_to_end(&mut data)
            .unwrap();
        assert!(data.len() <= 4096);
        data
    };
    (status, bounded(out), bounded(err))
}
pub fn artifact_metadata(normal: &Path) -> serde_json::Value {
    let parent = normal.parent().unwrap();
    for path in [
        parent.join("artifacts.json"),
        parent.parent().unwrap().join("artifacts.json"),
    ] {
        if path.is_file() {
            return serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        }
    }
    panic!("source-bound pinned artifact graph metadata required")
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

#[cfg(windows)]
pub fn direct_probe_status(fixture: &Fixture) -> ExitStatus {
    let config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&fixture.config).unwrap()).unwrap();
    let args = config["programs"][0]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect::<Vec<_>>();
    let mut child = Command::new(&fixture.probe)
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            std::fs::write(fixture.root.path().join("direct-probe-join.json"),serde_json::to_vec(&serde_json::json!({"nonce":fixture.nonce,"native_windows_exit":status.code().unwrap() as u32,"original_child_joined":true})).unwrap()).unwrap();
            return status;
        }
        if Instant::now() >= deadline {
            eprintln!("direct probe unresolved with original process owner retained");
            std::process::exit(98);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}
impl Fixture {
    pub fn witness_path(&self) -> PathBuf {
        self.root.path().join("runtime-witness.json")
    }
    pub fn hook_release_path(&self) -> PathBuf {
        self.root.path().join("runtime-release")
    }
    pub fn start_feature(&self, mode: &str, preflight: bool) -> Peer {
        Peer::spawn(
            pinned("THESEUS_NATIVE_HELPER_FIXTURE"),
            vec![
                "fixture-serve".into(),
                self.config.as_os_str().to_owned(),
                mode.into(),
                self.nonce.clone().into(),
                self.witness_path().into_os_string(),
                self.hook_release_path().into_os_string(),
            ],
            &self.config,
            preflight,
        )
    }
    pub fn release_hook(&self) {
        std::fs::write(self.hook_release_path(), self.nonce.as_bytes()).unwrap();
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
