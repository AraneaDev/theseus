//! Fixed trusted child fixtures. This entire module is absent without test support.
//! Stuck engine/finalizer cases model scheduling using real retained threads;
//! they do not claim to execute Docker or prove guest-resource cleanup.
use super::*;
use std::path::{Path, PathBuf};

struct DropWitness(PathBuf);
impl Drop for DropWitness {
    fn drop(&mut self) {
        std::fs::write(self.0.with_extension("dropped"), b"orderly return").unwrap();
    }
}

pub fn main() -> i32 {
    super::super::diagnostics::install_quiet_panic_hook();
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 4 {
        return 2;
    }
    let Some(case) = args[0].to_str() else {
        return 2;
    };
    if ![
        "query-stuck",
        "engine-stuck",
        "finalizer-stuck",
        "successful-joins",
        "lost-start-reply",
        "partial-cancel",
    ]
    .contains(&case)
    {
        return 2;
    }
    let receipt = PathBuf::from(&args[3]);
    let _witness = DropWitness(receipt.clone());
    let roots = Roots::open(Path::new(&args[1]), Path::new(&args[2])).map(Arc::new);
    let transport = theseus_host::stdio::Stdio::acquire();
    let stop = CancellationToken::default();
    let _signals = theseus_host::cancel::CancellationRegistration::install(stop.flag()).unwrap();
    let instance = Instance::parse("0123456789abcdef0123456789abcdef").unwrap();
    if case == "partial-cancel" {
        let transport = PartialTransport {
            native: NativeTransport::Acquired(
                transport.unwrap_or_else(|_| panic!("fixture acquisition")),
            ),
            wrote: false,
        };
        let mut runtime =
            ServerRuntime::base(transport, Some(roots.unwrap()), instance, 3600, stop);
        return drive(&mut runtime, &receipt);
    }
    let mut runtime = ServerRuntime::new(transport, roots, instance, 3600, stop, Instant::now());
    if case == "query-stuck" || case == "lost-start-reply" {
        let mut original = runtime
            .query
            .take()
            .expect("native roots and query acquired");
        original.request_stop();
        while original.try_join().is_none() {
            std::thread::sleep(IDLE_WAIT);
        }
        let stuck = case == "query-stuck";
        let entered = receipt.with_extension("entered");
        runtime.query = Some(
            QueryWorker::spawn_executor(move |_, _| {
                std::fs::write(&entered, b"actual query entered").unwrap();
                if stuck {
                    loop {
                        std::thread::park();
                    }
                }
                Err(QueryError::new(
                    ErrorCode::RecipeInvalid,
                    anyhow::anyhow!("fixed validation rejection"),
                ))
            })
            .unwrap(),
        );
    }
    if ["engine-stuck", "finalizer-stuck", "successful-joins"].contains(&case) {
        runtime.engine = Some(
            EngineWorker::fixture_thread(case != "successful-joins", case != "engine-stuck")
                .unwrap(),
        );
    }
    drive(&mut runtime, &receipt)
}
fn drive<T: Transport>(runtime: &mut ServerRuntime<T>, receipt: &Path) -> i32 {
    loop {
        let action = runtime.poll(Instant::now());
        if action == RuntimeAction::Continue {
            std::thread::sleep(IDLE_WAIT);
            continue;
        }
        let forced = matches!(action, RuntimeAction::ForceProcessExit(_));
        let data = serde_json::json!({"forced":forced,"workers_joined":runtime.query.is_none() && runtime.engine.is_none(),"transport_settled":runtime.transport.settled(),"query_retained":runtime.query.is_some(),"engine_retained":runtime.engine.is_some()});
        std::fs::write(receipt, serde_json::to_vec(&data).unwrap()).unwrap();
        match action {
            RuntimeAction::ForceProcessExit(code) => std::process::exit(code),
            RuntimeAction::OrderlyExit(code) => return code,
            RuntimeAction::Continue => unreachable!(),
        }
    }
}

/// Fixed gate proves cancellation after a real one-byte native write.
struct PartialTransport {
    native: NativeTransport,
    wrote: bool,
}
impl Transport for PartialTransport {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<ReadStep> {
        self.native.read(bytes)
    }
    fn write(&mut self, bytes: &[u8]) -> io::Result<WriteStep> {
        if self.wrote {
            return Ok(WriteStep::Pending);
        }
        let step = self.native.write(&bytes[..1])?;
        if matches!(step, WriteStep::Bytes(1)) {
            self.wrote = true;
        }
        Ok(step)
    }
    fn write_in_flight(&self) -> bool {
        self.native.write_in_flight()
    }
    fn stop(&mut self) {
        self.native.stop();
    }
    fn poll_stop(&mut self) -> io::Result<bool> {
        self.native.poll_stop()
    }
    fn settled(&self) -> bool {
        self.native.settled()
    }
}
