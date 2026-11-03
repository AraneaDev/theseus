//! Actual local helper process and ordinary pipes. No provider or guest claims.
use sha2::{Digest as _, Sha256};
use std::{
    io::{Read, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};
use theseus_host::fs::HostDir;
use theseus_native_helper::config_manifest_digest;
use theseus_native_helper_store::{BootstrapRequest, bootstrap_native};
use theseus_native_protocol::v2::*;
struct Peer {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
    session: SessionId,
    next: u64,
}
impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Peer {
    fn send(&mut self, message: Message) {
        let e = Envelope::new(self.session.clone(), self.next, message).unwrap();
        self.next += 1;
        self.input.write_all(&encode(&e).unwrap()).unwrap();
        self.input.flush().unwrap();
    }
    fn recv(&mut self) -> Message {
        let mut len = [0; 4];
        self.output.read_exact(&mut len).unwrap();
        let n = u32::from_be_bytes(len) as usize;
        assert!(n <= MAX_BODY_BYTES);
        let mut raw = vec![0; 4 + n];
        raw[..4].copy_from_slice(&len);
        self.output.read_exact(&mut raw[4..]).unwrap();
        Decoder::new()
            .feed(&raw)
            .unwrap()
            .frame
            .unwrap()
            .message()
            .clone()
    }
    fn request(&mut self, request: Request) -> Reply {
        let seq = self.next;
        self.send(Message::Request { request });
        match self.recv() {
            Message::Response { reply_to, result } => {
                assert_eq!(reply_to, seq);
                result
            }
            m => panic!("unexpected helper message {m:?}"),
        }
    }
}
#[test]
fn actual_native_profile_stream_absence_and_sticky_prefix_revocation() {
    exercise_native_process(true);
}
#[test]
fn actual_unproved_prefix_mapping_retains_owner_until_incomplete_exit() {
    exercise_native_process(false);
}
fn exercise_native_process(replace: bool) {
    let temp = tempfile::tempdir().unwrap();
    let ledger_path = temp.path().join("ledger");
    let workspace = temp.path().join("workspace");
    let ledger = HostDir::create_private(&ledger_path).unwrap();
    let root = HostDir::create_private(&workspace).unwrap();
    let reader_leaf = if cfg!(windows) {
        "reader.exe"
    } else {
        "reader"
    };
    let bytes = vec![0x93; 16385];
    let sha = Digest::from_bytes(Sha256::digest(&bytes).into());
    let epoch = Digest::from_bytes([4; 32]);
    let os = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let native_os = if cfg!(windows) {
        Os::Windows
    } else if cfg!(target_os = "macos") {
        Os::Macos
    } else {
        Os::Linux
    };
    let programs=["check_base","check_data","check_target","configure","exercise","install_base","install_target","uninstall"].map(|id|{
        let program=if ["check_base","check_data","check_target","exercise"].contains(&id){workspace.join(if cfg!(windows){"reader.exe"}else{"reader"})}else{std::env::current_exe().unwrap()};
        serde_json::json!({"id":id,"program":program,"args":[id,workspace.join("data").to_str().unwrap()],"environment":{"kind":"replace","entries":[]}})
    });
    let mut config = serde_json::json!({"schema":2,"profile":"native_catalog_v1","capabilities":NativeCapabilities::NAMES,"provider_epoch":epoch.to_hex(),"artifacts":{"base":{"length":bytes.len(),"sha256":sha.to_hex()},"target":{"length":bytes.len(),"sha256":sha.to_hex()}},"ledger_root":ledger_path,"workspace_root":workspace,"binding":"11".repeat(16),"manifest_digest":"00".repeat(32),"workspace_marker":"00".repeat(16),"os":os,"programs":programs});
    let manifest = config_manifest_digest(&serde_json::to_vec(&config).unwrap()).unwrap();
    let receipt = bootstrap_native(
        &ledger,
        &root,
        BootstrapRequest {
            binding: BindingId::from_bytes([0x11; 16]),
            manifest_digest: manifest,
            os: native_os,
        },
        epoch,
    )
    .unwrap();
    config["manifest_digest"] = serde_json::json!(manifest.to_hex());
    config["workspace_marker"] = serde_json::json!(receipt.workspace_marker().to_hex());
    let config_path = temp.path().join("config.json");
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    std::fs::write(workspace.join(reader_leaf), &bytes).unwrap();
    let helper = std::env::var_os("THESEUS_NATIVE_HELPER_BINARY")
        .expect("required explicitly pinned normal native helper");
    let helper = std::path::PathBuf::from(helper);
    assert!(helper.is_absolute() && helper.is_file());
    let mut child = Command::new(helper)
        .args(["serve", "--config"])
        .arg(&config_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let mut peer = Peer {
        child,
        input,
        output,
        session: SessionId::from_bytes([0x22; 16]),
        next: 0,
    };
    peer.send(Message::Hello {
        role: Role::Controller,
    });
    assert!(matches!(peer.recv(), Message::Hello { role: Role::Helper }));
    loop {
        match peer.request(Request::Describe) {
            Reply::NativeDescription { description } => {
                assert_eq!(description.provider_epoch, epoch);
                assert_eq!(description.manifest_digest, manifest);
                assert_eq!(
                    description.artifact_commitment,
                    artifact_commitment(bytes.len() as u64, sha, bytes.len() as u64, sha).unwrap()
                );
                break;
            }
            Reply::Refused {
                code: RefusalCode::OwnershipPending,
            } => std::thread::yield_now(),
            r => panic!("native description required {r:?}"),
        }
    }
    let lease = match peer.request(Request::Acquire {
        intent: IntentId::from_bytes([3; 16]),
        binding: BindingId::from_bytes([0x11; 16]),
        manifest_digest: manifest,
    }) {
        Reply::Acquired {
            state: AcquireView::Lease { lease, .. },
            ..
        } => lease,
        r => panic!("acquire {r:?}"),
    };
    assert_eq!(
        peer.request(Request::InspectNativeFile {
            lease,
            path: LeafName::new(reader_leaf).unwrap()
        }),
        Reply::NativeFileInfo {
            lease,
            path: LeafName::new(reader_leaf).unwrap(),
            length: bytes.len() as u64,
            sha256: sha
        }
    );
    assert_eq!(
        peer.request(Request::InspectNativeFile {
            lease,
            path: LeafName::new("app").unwrap()
        }),
        Reply::FileAbsent {
            lease,
            path: LeafName::new("app").unwrap()
        }
    );
    let moved = temp.path().join("moved");
    std::fs::rename(&workspace, &moved).unwrap();
    let expected_reason = if replace {
        PrefixAuthorityFailure::Replaced
    } else {
        PrefixAuthorityFailure::MappingUnproved
    };
    if replace {
        std::fs::create_dir(&workspace).unwrap();
        assert!(matches!(
            peer.request(Request::InspectNativeFile {
                lease,
                path: LeafName::new(reader_leaf).unwrap()
            }),
            Reply::PrefixAuthorityRevoked {
                reason: PrefixAuthorityFailure::Replaced,
                ..
            }
        ));
        std::fs::remove_dir(&workspace).unwrap();
        std::fs::rename(&moved, &workspace).unwrap();
        for (n, id) in ["check_target", "uninstall"].iter().enumerate() {
            let reply = peer.request(Request::Run {
                lease,
                operation: OperationId::from_bytes([n as u8 + 5; 16]),
                program: ProgramId::new(id).unwrap(),
            });
            assert!(
                matches!(reply, Reply::PrefixAuthorityRevoked { .. }),
                "revoked {id} reply: {reply:?}"
            );
        }
        assert!(matches!(
            peer.request(Request::Release { lease }),
            Reply::Lease {
                state: LeaseView::AwaitingExternalReset { .. },
                ..
            }
        ));
        peer.send(Message::Closed {
            code: CloseCode::Normal,
        });
        assert!(peer.child.wait().unwrap().success());
    } else {
        assert!(matches!(
            peer.request(Request::InspectNativeFile {
                lease,
                path: LeafName::new(reader_leaf).unwrap()
            }),
            Reply::PrefixAuthorityRevoked {
                reason: PrefixAuthorityFailure::MappingUnproved,
                ..
            }
        ));
        peer.send(Message::Closed {
            code: CloseCode::Normal,
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        loop {
            if let Some(status) = peer.child.try_wait().unwrap() {
                assert_eq!(
                    status.code(),
                    Some(2),
                    "unproved native mapping must never claim Clean"
                );
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "retained owner must reach explicit incomplete standalone shutdown"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    let raw = std::fs::read(ledger_path.join("ledger.bin")).unwrap();
    let mut decoder = ledger::Decoder::new();
    let mut offset = 0;
    let mut records = Vec::new();
    while offset < raw.len() {
        let p = decoder.feed(&raw[offset..]).unwrap();
        offset += p.consumed;
        if let Some(r) = p.record {
            records.push(r)
        }
    }
    decoder.finish().unwrap();
    assert!(
        !records
            .iter()
            .any(|r| matches!(r.entry(), ledger::Entry::OperationAccepted { .. }))
    );
    assert_eq!(
        ledger::LedgerView::replay(&records)
            .unwrap()
            .native_authority_failure(),
        Some(expected_reason)
    );
}
