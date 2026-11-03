//! Fixed finite single-process probe, only in the explicit test-support graph.
#![forbid(unsafe_code)]
use sha2::{Digest as _, Sha256};
use std::{
    io::Write,
    path::Path,
    time::{Duration, Instant},
};
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn publish(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension("pending");
    let mut file = std::fs::File::create_new(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(temporary, path)
}
fn probe(args: &[String]) -> i32 {
    assert_eq!(args.len(), 5);
    let mode = args[1].as_str();
    assert!(matches!(
        mode,
        "success" | "wait" | "flood" | "flood_wait" | "high_bit"
    ));
    let nonce = &args[2];
    assert_eq!(nonce.len(), 32);
    assert!(
        nonce
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    let receipt = Path::new(&args[3]);
    let release = Path::new(&args[4]);
    assert!(receipt.is_absolute() && release.is_absolute());
    let executable_sha256 = hex(&Sha256::digest(
        std::fs::read(std::env::current_exe().unwrap()).unwrap(),
    ));
    let principal = match theseus_host::principal::current_principal().unwrap() {
        theseus_host::principal::CurrentPrincipal::Posix {
            effective_uid,
            effective_gid,
        } => serde_json::json!({"effective_uid":effective_uid,"effective_gid":effective_gid}),
        theseus_host::principal::CurrentPrincipal::Windows { token_user_sid } => {
            serde_json::json!({"token_user_sid":token_user_sid})
        }
    };
    if matches!(mode, "flood" | "flood_wait") {
        let block = [b'X'; 4096];
        for _ in 0..1024 {
            std::io::stdout().lock().write_all(&block).unwrap();
            std::io::stderr().lock().write_all(&block).unwrap();
        }
        std::io::stdout().flush().unwrap();
        std::io::stderr().flush().unwrap();
    }
    publish(receipt,&serde_json::to_vec(&serde_json::json!({"nonce":nonce,"executable_sha256":executable_sha256,"principal":principal,"mode":mode,"single_process":true,"fixture_marker":"theseus-native-helper-fixture-only-v1"})).unwrap()).unwrap();
    if matches!(mode, "wait" | "flood_wait") {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if std::fs::read(release).is_ok_and(|v| v == nonce.as_bytes()) {
                break;
            }
            if Instant::now() >= deadline {
                return 92;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    if mode == "high_bit" {
        #[cfg(windows)]
        {
            0x80000005_u32 as i32
        }
        #[cfg(not(windows))]
        {
            2
        }
    } else {
        23
    }
}
fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let code = if args.first().is_some_and(|s| s == "probe") {
        probe(&args)
    } else if args.first().is_some_and(|s| s == "fixture-serve") {
        fixture_serve(&args)
    } else {
        2
    };
    std::process::exit(code);
}

fn fixture_serve(args: &[String]) -> i32 {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use theseus_native_helper::{
        Poll, Runtime, load_config,
        test_support::{FixtureControl, FixtureMode},
    };
    assert_eq!(args.len(), 6);
    let config = Path::new(&args[1]);
    let witness = Path::new(&args[4]);
    let release = Path::new(&args[5]);
    assert!(config.is_absolute() && witness.is_absolute() && release.is_absolute());
    let nonce = &args[3];
    assert_eq!(nonce.len(), 32);
    assert!(
        nonce
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    let mode = match args[2].as_str() {
        "observe" => FixtureMode::Observe,
        "pressure" => FixtureMode::OutputPressure,
        "hold-init" => FixtureMode::HoldInit,
        "hold-before-spawn" => FixtureMode::HoldBeforeSpawn,
        "panic-after-owned-spawn" => FixtureMode::PanicAfterOwnedSpawn,
        "hold-before-file-effect" => FixtureMode::HoldBeforeFileEffect,
        "hold-after-file-finished" => FixtureMode::HoldAfterFileFinished,
        "fail-file-write" => FixtureMode::FailFileWrite,
        _ => return 2,
    };
    let invocation = load_config(config).unwrap();
    let config_bytes = std::fs::read(config).unwrap();
    let data: serde_json::Value = serde_json::from_slice(&config_bytes).unwrap();
    assert_eq!(data["programs"][0]["args"][2], *nonce);
    let config_sha256 = hex(&Sha256::digest(&config_bytes));
    let executable_sha256 = hex(&Sha256::digest(
        std::fs::read(std::env::current_exe().unwrap()).unwrap(),
    ));
    let profile = if mode == FixtureMode::OutputPressure {
        serde_json::json!({"name":"output-pressure","raw_chunk_cap":512,"publication_cap":128,"output_body_bytes":65532,"output_frame_bytes":65536,"raw_lifetime_cap":16384,"data_permits":4})
    } else {
        serde_json::json!({"name":"compact"})
    };
    let control = FixtureControl::new(mode);
    let stdio = match theseus_host::stdio::Stdio::acquire() {
        Ok(stdio) => stdio,
        Err(mut owner) => {
            owner.request_stop();
            let start = Instant::now();
            loop {
                if owner.poll_stop_or_restore().unwrap_or(false) {
                    return 2;
                }
                if start.elapsed() >= Duration::from_secs(5) {
                    let _ = std::io::stderr().write_all(b"helper: cleanup incomplete\n");
                    std::process::exit(2);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    };
    let runtime = match Runtime::start_with_fixture(invocation, stdio, control.clone()) {
        Ok(runtime) => runtime,
        Err(mut owner) => {
            owner.request_stop();
            let start = Instant::now();
            loop {
                if owner.poll_settle().unwrap_or(false) {
                    return 2;
                }
                if start.elapsed() >= Duration::from_secs(5) {
                    let _ = std::io::stderr().write_all(b"helper: cleanup incomplete\n");
                    std::process::exit(2);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    };
    let mut runtime = RetainedRuntime {
        runtime,
        drop_receipt: witness.with_extension("owner-dropped"),
        nonce: nonce.clone(),
    };
    let mut last = None;
    let mut stopping = None;
    let mut primary = None;
    let mut panic_payload = None;
    let mut witness_error = None;
    loop {
        if std::fs::read(release).is_ok_and(|bytes| bytes == nonce.as_bytes()) {
            control.release();
        }
        let result = catch_unwind(AssertUnwindSafe(|| runtime.poll()));
        let settled = matches!(result, Ok(Ok(Poll::Settled)));
        match result {
            Ok(Err(error)) => {
                if primary.is_none() {
                    primary = Some(error);
                }
                runtime.request_stop();
            }
            Err(payload) => {
                if panic_payload.is_none() {
                    panic_payload = Some(payload);
                }
                runtime.request_stop();
            }
            _ => {}
        }
        let snapshot = control.snapshot();
        if last != Some(snapshot) {
            let observation = serde_json::json!({"nonce":nonce,"config_sha256":config_sha256,"executable_sha256":executable_sha256,"fixture_mode":args[2],"profile":profile,"file_before_effect":snapshot.file_before_effect,"file_effect_attempted":snapshot.file_effect_attempted,"file_finished_committed":snapshot.file_finished_committed,"file_write_failed":snapshot.file_write_failed,"file_quarantine_committed":snapshot.file_quarantine_committed,"init_reached":snapshot.init_reached,"before_spawn_reached":snapshot.before_spawn_reached,"acquire_committed":snapshot.acquire_committed,"accepted_committed":snapshot.accepted_committed,"terminal_committed":snapshot.terminal_committed,"spawn_attempted":snapshot.spawn_attempted,"spawn_owned":snapshot.spawn_owned,"write_pending":snapshot.write_pending,"cancel_requested":snapshot.cancel_requested,"panic_injected":snapshot.panic_injected,"capture_saturated":snapshot.capture_saturated,"control_write_acks":snapshot.control_write_acks,"saturation_wire_bytes":snapshot.saturation_wire_bytes,"saturation_published_frames":snapshot.saturation_published_frames});
            if let Err(error) = publish(witness, &serde_json::to_vec(&observation).unwrap()) {
                if witness_error.is_none() {
                    witness_error = Some(error);
                }
                runtime.request_stop();
            }
            last = Some(snapshot);
        }
        if settled {
            if primary.is_some() || panic_payload.is_some() || witness_error.is_some() {
                let _ = std::io::stderr().write_all(b"helper: runtime failed\n");
                return 2;
            }
            return 0;
        }
        if runtime.is_stopping() {
            let began = stopping.get_or_insert_with(Instant::now);
            if began.elapsed() >= Duration::from_secs(5) {
                let _ = std::io::stderr().write_all(b"helper: cleanup incomplete\n");
                std::process::exit(2);
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

struct RetainedRuntime {
    runtime: theseus_native_helper::Runtime,
    drop_receipt: std::path::PathBuf,
    nonce: String,
}
impl std::ops::Deref for RetainedRuntime {
    type Target = theseus_native_helper::Runtime;
    fn deref(&self) -> &Self::Target {
        &self.runtime
    }
}
impl std::ops::DerefMut for RetainedRuntime {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.runtime
    }
}
impl Drop for RetainedRuntime {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.drop_receipt, self.nonce.as_bytes());
    }
}
