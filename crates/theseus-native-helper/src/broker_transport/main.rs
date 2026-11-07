//! Explicit standalone broker. Never discovers a broker/session or creates a
//! bootstrap. Qualified Linux startup retains the original provider-created
//! private bootstrap; no fixture argument or source environment override exists.
use std::{
    ffi::OsString,
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};
use theseus_host::stdio::{ReadStep, Stdio};
use theseus_native_helper::broker_transport::{BrokerServer, BrokerServerConfig, ServerPoll};
fn config_path(args: impl IntoIterator<Item = OsString>) -> Option<PathBuf> {
    let mut args = args.into_iter();
    args.next()?;
    if args.next()?.to_str()? != "serve" || args.next()?.to_str()? != "--config" {
        return None;
    }
    let path = PathBuf::from(args.next()?);
    if args.next().is_some() || !path.is_absolute() || path.as_os_str().len() > 16384 {
        return None;
    }
    Some(path)
}
#[cfg(target_os = "linux")]
fn initialize_linux_factory(
    proof: &theseus_native_helper_store::VerifiedLinuxGioBootstrap,
    config: &BrokerServerConfig,
    session: theseus_host::desktop::session::BoundSession,
) -> std::io::Result<theseus_host::desktop::linux::DedicatedGioBroker> {
    use theseus_host::desktop::linux as host;
    use theseus_native_protocol::v2;
    proof.revalidate().map_err(std::io::Error::other)?;
    let a = proof.attestation();
    a.validate_manifest(config.manifest())
        .map_err(std::io::Error::other)?;
    a.validate_broker_header(config.header())
        .map_err(std::io::Error::other)?;
    let digest = |value: &str| -> std::io::Result<[u8; 32]> {
        Ok(*v2::Digest::parse(value)
            .map_err(std::io::Error::other)?
            .as_bytes())
    };
    let abi = match a.sdk.native_abi {
        v2::NativeLinuxAbi::LinuxX86_64 => host::NativeElfAbi::LinuxX86_64,
        v2::NativeLinuxAbi::LinuxAarch64 => host::NativeElfAbi::LinuxAarch64,
    };
    let sdk_digest = *a.sdk.digest().map_err(std::io::Error::other)?.as_bytes();
    let sdk = host::QualifiedGioSdk {
        glib_version: a.sdk.glib_version,
        native_abi: abi,
        helper_length: a.helper.length,
        helper_sha256: digest(&a.helper.sha256)?,
        qualification_digest: sdk_digest,
    };
    let helper = host::ArtifactSlot {
        root: a.helper.root.clone().into(),
        relative: a.helper.relative_path.clone(),
        length: a.helper.length,
        sha256: digest(&a.helper.sha256)?,
    };
    let qualification = host::CompiledArtifactQualification {
        length: a.helper.length,
        sha256: helper.sha256,
        native_abi: abi,
        sdk_qualification_digest: sdk_digest,
        provenance: host::QualificationProvenance::ProviderBuildAttested {
            verified_receipt_digest: *a.helper.digest().map_err(std::io::Error::other)?.as_bytes(),
        },
    };
    let images = theseus_native_helper::broker_transport::qualified_linux_sdk_images(proof)?;
    let executable = config
        .manifest()
        .desktop_executable
        .as_ref()
        .ok_or_else(|| std::io::Error::other("qualified native executable declaration absent"))?;
    let environment = executable
        .environment
        .iter()
        .map(|e| (e.name.clone(), e.value.clone()))
        .collect();
    // This is the original stock dedicated binary before ANY GIO initialization,
    // with no other creator/reaper/SDK user and unchanged replacement startup
    // environment. The original trusted provider's private bootstrap capability
    // retains its actual valid compiled helper/app SOURCE/SDK graph and protected
    // exclusive quiescent namespace. Exact loader origin is checked by Host; a
    // decoded reference/config/ELF header alone cannot reach this admission.
    unsafe {
        host::DedicatedGioBroker::admit_with_images(
            session,
            environment,
            helper,
            Some(qualification),
            sdk,
            images,
        )
    }
}
#[cfg(target_os = "linux")]
fn finalize_linux_original(
    mut permit: theseus_native_helper::broker::LinuxReapPermit<'_>,
) -> std::io::Result<bool> {
    // The helper alone constructs this unforgeable permit after BOTH exact
    // original READY/CLOSE channels retire, with no future PID peer checks.
    // This same dedicated original process still owns default SIGCHLD/reaping.
    unsafe { permit.original().retire_primary_after_ipc() }
}
fn run() -> i32 {
    let Some(path) = config_path(std::env::args_os()) else {
        return 2;
    };
    #[cfg(target_os = "linux")]
    let loaded = BrokerServerConfig::load_stock(&path);
    #[cfg(not(target_os = "linux"))]
    let loaded = BrokerServerConfig::load(&path);
    let config = match loaded {
        Ok(config) => config,
        Err(_error) => return 2,
    };
    let watch = config.source_parent_eof();
    #[cfg(target_os = "linux")]
    let scope = match unsafe {
        // This binary is the explicitly supplied dedicated broker process, before
        // serving and before any child creator/reaper or unrelated native activation.
        // Only BrokerRuntime owns the active original; a later generation may
        // reuse only the actual completely settled original scope/SDK token.
        theseus_host::desktop::scope::DedicatedBrokerScope::establish_for_exclusive_process()
    } {
        Ok(scope) => scope,
        Err(_error) => return 2,
    };
    #[cfg(target_os = "linux")]
    let started = match config {
        theseus_native_helper::broker_transport::StockBrokerConfig::Standard(config) => {
            BrokerServer::start(*config, scope)
        }
        theseus_native_helper::broker_transport::StockBrokerConfig::QualifiedLinux(qualified) => {
            BrokerServer::start_qualified_linux(
                *qualified,
                scope,
                initialize_linux_factory,
                finalize_linux_original,
            )
        }
    };
    #[cfg(not(target_os = "linux"))]
    let started = BrokerServer::start(config);
    let mut server = match started {
        Ok(server) => server,
        Err(failure) => {
            let (_error, owner) = failure.into_parts();
            if let Some(mut owner) = owner {
                owner.request_stop();
                let deadline = Instant::now() + Duration::from_secs(5);
                while Instant::now() < deadline {
                    if matches!(owner.poll_settle(), Ok(true)) {
                        return 2;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                std::mem::forget(owner);
            }
            return 2;
        }
    };
    let mut parent = if watch {
        match Stdio::acquire() {
            Ok(stdio) => Some(stdio),
            Err(mut failure) => {
                server.request_stop();
                failure.request_stop();
                let deadline = Instant::now() + Duration::from_secs(5);
                while Instant::now() < deadline {
                    let server_done = matches!(server.poll_settle(), Ok(true));
                    let parent_done = matches!(failure.poll_stop_or_restore(), Ok(true));
                    if server_done && parent_done {
                        return 2;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                std::mem::forget(failure);
                std::mem::forget(server);
                return 2;
            }
        }
    } else {
        None
    };
    let mut failed = false;
    let mut stopping = None;
    let mut parent_buffer = [0; 4096];
    loop {
        if stopping.is_none()
            && let Some(parent) = &mut parent
        {
            match parent.try_read(&mut parent_buffer) {
                Ok(ReadStep::Eof) => {
                    if stopping.is_none() {
                        stopping = Some(Instant::now());
                        server.request_stop();
                        parent.request_stop();
                    }
                }
                Ok(ReadStep::Bytes(_)) => {
                    failed = true;
                    if stopping.is_none() {
                        stopping = Some(Instant::now());
                        server.request_stop();
                        parent.request_stop();
                    }
                }
                Ok(ReadStep::Pending) => {}
                Err(_error) => {
                    failed = true;
                    if stopping.is_none() {
                        stopping = Some(Instant::now());
                        server.request_stop();
                        parent.request_stop();
                    }
                }
            }
        }
        if let Some(began) = stopping {
            let server_done = matches!(server.poll_settle(), Ok(true));
            let parent_done = parent
                .as_mut()
                .is_none_or(|p| matches!(p.poll_stop_or_restore(), Ok(true)));
            if server_done && parent_done {
                return if failed { 2 } else { 0 };
            }
            if began.elapsed() >= Duration::from_secs(5) {
                std::mem::forget(server);
                std::mem::forget(parent);
                return 2;
            }
        } else {
            match server.poll() {
                Ok(ServerPoll::Settled) => return if failed { 2 } else { 0 },
                Ok(_) => {}
                Err(_error) => {
                    failed = true;
                    stopping = Some(Instant::now());
                    server.request_stop();
                    if let Some(parent) = &mut parent {
                        parent.request_stop();
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    let code = match std::panic::catch_unwind(run) {
        Ok(code) => code,
        Err(payload) => {
            std::mem::forget(payload);
            2
        }
    };
    if code != 0 {
        let _ = std::io::stderr()
            .lock()
            .write_all(b"broker: original context failed or remains unsettled\n");
    }
    std::process::exit(code)
}
