//! Native subprocess acceptance: no production test switches or Docker execution.
use clap::Parser;
use theseus::cli::Cli;

#[test]
fn serve_parses_required_native_roots_and_bounded_deadline_offline() {
    let parsed = Cli::try_parse_from([
        "theseus",
        "serve",
        "--project-root",
        "missing project",
        "--output-root",
        "missing output",
    ]);
    assert!(parsed.is_ok(), "serve must parse offline: {parsed:?}");
    let debug = format!("{:?}", parsed.unwrap());
    assert!(debug.contains("max_run_seconds: 3600"));
    for value in ["1", "86400"] {
        assert!(
            Cli::try_parse_from([
                "theseus",
                "serve",
                "--project-root",
                "a",
                "--output-root",
                "b",
                "--max-run-seconds",
                value
            ])
            .is_ok()
        );
    }
    for value in ["0", "86401", "-1", "no", "18446744073709551616"] {
        assert!(
            Cli::try_parse_from([
                "theseus",
                "serve",
                "--project-root",
                "a",
                "--output-root",
                "b",
                "--max-run-seconds",
                value
            ])
            .is_err()
        );
    }
    for args in [
        vec!["theseus", "serve"],
        vec!["theseus", "serve", "--project-root", "a"],
        vec!["theseus", "serve", "--output-root", "b"],
    ] {
        assert!(Cli::try_parse_from(args).is_err());
    }
}

#[cfg(unix)]
#[test]
fn serve_roots_preserve_non_unicode_native_paths() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let root = OsString::from_vec(vec![b'p', 0xff]);
    let args = vec![
        OsString::from("theseus"),
        "serve".into(),
        "--project-root".into(),
        root.clone(),
        "--output-root".into(),
        root,
    ];
    assert!(Cli::try_parse_from(args).is_ok());
}

fn production_binary() -> std::path::PathBuf {
    std::env::var_os("THESEUS_MCP_BINARY")
        .map(Into::into)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_theseus").into())
}
fn python() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

// Ordinary Rust pipes may block on every native platform. Run all pipe work
// inside a fixed test-only helper process; the outer test owns its whole tree
// and polls only process state. Its stdout/stderr go to regular log files.
mod native_supervisor {
    use std::{
        io::{self, BufRead, BufReader, Read, Write},
        process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio},
        time::{Duration, Instant},
    };
    #[cfg(unix)]
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
        fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
        #[cfg(target_os = "linux")]
        fn prctl(option: i32, arg2: usize, arg3: usize, arg4: usize, arg5: usize) -> i32;
    }
    #[cfg(windows)]
    mod windows {
        use std::{
            ffi::c_void,
            io,
            mem::size_of,
            os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
            process::Child,
            ptr::{null, null_mut},
        };
        type Handle = *mut c_void;
        #[repr(C)]
        #[derive(Default)]
        struct Basic {
            process_time: i64,
            job_time: i64,
            flags: u32,
            minimum: usize,
            maximum: usize,
            active: u32,
            affinity: usize,
            priority: u32,
            scheduling: u32,
        }
        #[repr(C)]
        #[derive(Default)]
        struct Limits {
            basic: Basic,
            io: [u64; 6],
            process_memory: usize,
            job_memory: usize,
            peak_process: usize,
            peak_job: usize,
        }
        #[repr(C)]
        #[derive(Default)]
        struct Thread {
            size: u32,
            usage: u32,
            tid: u32,
            pid: u32,
            base: i32,
            delta: i32,
            flags: u32,
        }
        #[repr(C)]
        #[derive(Default)]
        struct Accounting {
            times: [i64; 4],
            faults: u32,
            total: u32,
            active: u32,
            terminated: u32,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> Handle;
            fn SetInformationJobObject(
                job: Handle,
                class: i32,
                info: *const c_void,
                size: u32,
            ) -> i32;
            fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
            fn TerminateJobObject(job: Handle, code: u32) -> i32;
            fn QueryInformationJobObject(
                job: Handle,
                class: i32,
                info: *mut c_void,
                size: u32,
                returned: *mut u32,
            ) -> i32;
            fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> Handle;
            fn Thread32First(snapshot: Handle, entry: *mut Thread) -> i32;
            fn Thread32Next(snapshot: Handle, entry: *mut Thread) -> i32;
            fn OpenThread(access: u32, inherit: i32, tid: u32) -> Handle;
            fn ResumeThread(thread: Handle) -> u32;
            fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
            fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;

        }
        fn check(result: i32) -> io::Result<()> {
            if result == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
        unsafe fn owned(handle: Handle) -> io::Result<OwnedHandle> {
            if handle.is_null() || handle as isize == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
            }
        }
        pub fn gone(pid: u32) -> bool {
            let handle = unsafe { OpenProcess(0x100000, 0, pid) };
            if handle.is_null() {
                assert_eq!(io::Error::last_os_error().raw_os_error(), Some(87));
                return true;
            }
            let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
            unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) == 0 }
        }
        pub struct Job(OwnedHandle);
        impl Job {
            pub fn assign_and_resume(child: &Child) -> io::Result<Self> {
                let job = Self(unsafe { owned(CreateJobObjectW(null(), null()))? });
                let mut limits = Limits::default();
                limits.basic.flags = 0x2000; // KILL_ON_JOB_CLOSE, no breakaway.
                unsafe {
                    check(SetInformationJobObject(
                        job.0.as_raw_handle(),
                        9,
                        (&limits as *const Limits).cast(),
                        size_of::<Limits>() as u32,
                    ))?;
                    check(AssignProcessToJobObject(
                        job.0.as_raw_handle(),
                        child.as_raw_handle(),
                    ))?;
                    let snapshot = owned(CreateToolhelp32Snapshot(4, 0))?;
                    let mut entry = Thread {
                        size: size_of::<Thread>() as u32,
                        ..Thread::default()
                    };
                    let mut found = Thread32First(snapshot.as_raw_handle(), &mut entry);
                    let mut primary = None;
                    while found != 0 {
                        if entry.pid == child.id() && primary.replace(entry.tid).is_some() {
                            return Err(io::Error::other("multiple suspended primary threads"));
                        }
                        entry.size = size_of::<Thread>() as u32;
                        found = Thread32Next(snapshot.as_raw_handle(), &mut entry);
                    }
                    let thread = owned(OpenThread(
                        2,
                        0,
                        primary
                            .ok_or_else(|| io::Error::other("missing suspended primary thread"))?,
                    ))?;
                    if ResumeThread(thread.as_raw_handle()) != 1 {
                        return Err(io::Error::other("primary thread not suspended once"));
                    }
                }
                Ok(job)
            }
            pub fn terminate(&self) -> io::Result<()> {
                unsafe { check(TerminateJobObject(self.0.as_raw_handle(), 2)) }
            }
            pub fn empty(&self) -> io::Result<bool> {
                let mut info = Accounting::default();
                unsafe {
                    check(QueryInformationJobObject(
                        self.0.as_raw_handle(),
                        1,
                        (&mut info as *mut Accounting).cast(),
                        size_of::<Accounting>() as u32,
                        null_mut(),
                    ))?;
                }
                Ok(info.active == 0)
            }
        }
    }

    struct Tree {
        child: Child,
        cleaned: bool,
        #[cfg(windows)]
        job: windows::Job,
    }
    impl Tree {
        fn spawn(command: &mut Command) -> io::Result<Self> {
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                #[cfg(target_os = "linux")]
                if unsafe { prctl(36, 1, 0, 0, 0) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                command.process_group(0);
            }
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x204);
            }
            let child = command.spawn()?;
            #[cfg(windows)]
            let job = match windows::Job::assign_and_resume(&child) {
                Ok(job) => job,
                Err(error) => {
                    let mut child = child;
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            };
            Ok(Self {
                child,
                cleaned: false,
                #[cfg(windows)]
                job,
            })
        }
        fn cleanup(&mut self) -> io::Result<()> {
            if self.cleaned {
                return Ok(());
            }
            #[cfg(unix)]
            if unsafe { kill(-(self.child.id() as i32), 9) } != 0
                && io::Error::last_os_error().raw_os_error() != Some(3)
            {
                return Err(io::Error::last_os_error());
            }
            #[cfg(windows)]
            self.job.terminate()?;
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let leader_done = self.child.try_wait()?.is_some();
                #[cfg(unix)]
                let empty = {
                    if leader_done {
                        let mut status = 0;
                        while unsafe { waitpid(-(self.child.id() as i32), &mut status, 1) } > 0 {}
                    }
                    let result = unsafe { kill(-(self.child.id() as i32), 0) };
                    result != 0 && io::Error::last_os_error().raw_os_error() == Some(3)
                };
                #[cfg(windows)]
                let empty = self.job.empty()?;
                if leader_done && empty {
                    self.cleaned = true;
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "whole child tree did not reap",
                    ));
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
    impl Drop for Tree {
        fn drop(&mut self) {
            if let Err(error) = self.cleanup() {
                eprintln!("native supervisor cleanup failed: {error}");
            }
        }
    }

    pub fn assert_descendant_gone(root: &std::path::Path) {
        let pid: u32 = std::fs::read_to_string(root.join("descendant.pid"))
            .unwrap()
            .parse()
            .unwrap();
        #[cfg(unix)]
        assert!(
            unsafe { kill(pid as i32, 0) } != 0
                && io::Error::last_os_error().raw_os_error() == Some(3),
            "descendant {pid} survived exited-leader cleanup"
        );
        #[cfg(windows)]
        assert!(
            windows::gone(pid),
            "descendant {pid} survived exited-leader cleanup"
        );
    }
    pub fn helper(
        test: &str,
        mode: &str,
        timeout: Duration,
        expected_timeout: bool,
    ) -> std::path::PathBuf {
        let temp = match std::env::var_os("THESEUS_HOST_EVIDENCE") {
            Some(root) => tempfile::Builder::new()
                .prefix("rust-helper-")
                .tempdir_in(root)
                .unwrap(),
            None => tempfile::Builder::new()
                .prefix("rust-helper-")
                .tempdir()
                .unwrap(),
        }
        .keep();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", test, "--nocapture"])
            .env("THESEUS_MCP_RUST_HELPER", mode)
            .env("THESEUS_MCP_HELPER_EVIDENCE", temp.as_path());
        command
            .stdin(Stdio::null())
            .stdout(std::fs::File::create(temp.as_path().join("helper.stdout")).unwrap())
            .stderr(std::fs::File::create(temp.as_path().join("helper.stderr")).unwrap());
        let mut tree = Tree::spawn(&mut command).unwrap();
        let deadline = Instant::now() + timeout;
        let status = loop {
            if let Some(status) = tree.child.try_wait().unwrap() {
                break Some(status);
            }
            if Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        // Always clean descendants, including when the leader has exited normally.
        tree.cleanup().unwrap();
        assert_eq!(
            status.is_none(),
            expected_timeout,
            "unexpected helper watchdog result; evidence {}",
            temp.as_path().display()
        );
        if let Some(status) = status {
            assert!(
                status.success(),
                "helper failed: {}\n{}",
                std::fs::read_to_string(temp.as_path().join("helper.stdout")).unwrap(),
                std::fs::read_to_string(temp.as_path().join("helper.stderr")).unwrap()
            );
        }
        std::fs::write(temp.join("supervisor.json"), serde_json::to_vec_pretty(&serde_json::json!({"mode":mode,"timed_out":status.is_none(),"tree_empty":tree.cleaned,"exit":status.and_then(|value| value.code())})).unwrap()).unwrap();
        temp
    }

    pub fn compact_line(line: &[u8]) -> bool {
        if !line.ends_with(b"\n") || serde_json::from_slice::<serde_json::Value>(line).is_err() {
            return false;
        }
        let (mut quoted, mut escaped) = (false, false);
        for &byte in &line[..line.len() - 1] {
            if escaped {
                escaped = false;
            } else if quoted && byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = !quoted;
            } else if !quoted && byte.is_ascii_whitespace() {
                return false;
            }
        }
        !line.contains(&0x1b)
    }

    // This entire blocking adapter runs only inside the supervised helper process.
    pub struct Server {
        child: Child,
        input: Option<ChildStdin>,
        output: BufReader<ChildStdout>,
        error: ChildStderr,
        pub stdout: Vec<u8>,
        pub stderr: Vec<u8>,
    }
    impl Server {
        pub fn spawn(command: &mut Command) -> io::Result<Self> {
            let mut child = command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            Ok(Self {
                input: child.stdin.take(),
                output: BufReader::new(child.stdout.take().unwrap()),
                error: child.stderr.take().unwrap(),
                child,
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
        pub fn send(&mut self, value: &serde_json::Value) {
            let input = self.input.as_mut().unwrap();
            writeln!(input, "{value}").unwrap();
            input.flush().unwrap();
        }
        pub fn response(&mut self) -> serde_json::Value {
            let mut line = Vec::new();
            assert_ne!(
                self.output.read_until(b'\n', &mut line).unwrap(),
                0,
                "server closed before response"
            );
            self.stdout.extend_from_slice(&line);
            let value: serde_json::Value = serde_json::from_slice(&line).unwrap();
            assert!(compact_line(&line), "stdout must be compact newline JSON");
            assert_eq!(value["jsonrpc"], "2.0");
            value
        }
        pub fn finish(&mut self) -> io::Result<std::process::ExitStatus> {
            self.input.take();
            let status = self.child.wait()?;
            self.output.read_to_end(&mut self.stdout)?;
            self.error.read_to_end(&mut self.stderr)?;
            Ok(status)
        }
    }
}

#[test]
fn ordinary_rust_pipe_transcript_has_no_terminal_or_diagnostic_output() {
    if std::env::var("THESEUS_MCP_RUST_HELPER").as_deref() != Ok("transcript") {
        native_supervisor::helper(
            "ordinary_rust_pipe_transcript_has_no_terminal_or_diagnostic_output",
            "transcript",
            std::time::Duration::from_secs(15),
            false,
        );
        return;
    }
    use serde_json::{Value, json};
    use std::process::Command;
    const NAMES: [&str; 6] = [
        "recipe_inspect",
        "run_start",
        "run_list",
        "run_status",
        "run_cancel",
        "run_evidence",
    ];
    for era in ["legacy", "modern"] {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("target")).unwrap();
        std::fs::copy(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/recipes/v2_clean.toml"
            ),
            temp.path().join("theseus.toml"),
        )
        .unwrap();
        let mut command = Command::new(production_binary());
        command
            .args(["serve", "--project-root"])
            .arg(temp.path())
            .arg("--output-root")
            .arg(temp.path().join("output"));
        let mut server = native_supervisor::Server::spawn(&mut command).unwrap();
        let mut serial = 0;
        let mut request = |server: &mut native_supervisor::Server,
                           method: &str,
                           mut params: Value| {
            serial += 1;
            if era == "modern" {
                params["_meta"] = json!({"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}});
            }
            server.send(&json!({"jsonrpc":"2.0","id":serial,"method":method,"params":params}));
            let value = server.response();
            assert_eq!(value["id"], serial);
            value
        };
        if era == "legacy" {
            let initialized = request(
                &mut server,
                "initialize",
                json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"native-rust-test","version":"1"}}),
            );
            assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
            server.send(&json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}));
        } else {
            assert!(
                request(&mut server, "server/discover", json!({}))
                    .get("result")
                    .is_some()
            );
        }
        let ping = request(&mut server, "ping", json!({}));
        if era == "legacy" {
            assert_eq!(ping["result"], json!({}));
        } else {
            assert_eq!(ping["result"]["resultType"], "complete");
        }
        let listing = request(&mut server, "tools/list", json!({}));
        let tools = listing["result"]["tools"].as_array().unwrap();
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            NAMES
        );
        assert!(
            tools
                .iter()
                .all(|tool| tool["inputSchema"]["additionalProperties"] == false)
        );
        let mut tool = |server: &mut native_supervisor::Server, name: &str, arguments: Value| {
            let value = request(
                server,
                "tools/call",
                json!({"name":name,"arguments":arguments}),
            );
            let result = &value["result"];
            let application = result.get("structuredContent").cloned().unwrap_or_else(|| {
                serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
            });
            assert_eq!(result["isError"], !application["ok"].as_bool().unwrap());
            application
        };
        let inspect = tool(
            &mut server,
            NAMES[0],
            json!({"recipe_path":"theseus.toml","selector":{"kind":"overview"}}),
        );
        assert_eq!(inspect["ok"], true);
        assert_eq!(inspect["data"]["max_run_seconds"], 3600);
        let instance = inspect["server_instance"].as_str().unwrap();
        assert_eq!(instance.len(), 32);
        assert!(instance.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(
            tool(&mut server, NAMES[2], json!({"server_instance":instance}))["ok"],
            true
        );
        let handle = format!("{instance}:1");
        for (name, arguments) in [
            (
                NAMES[3],
                json!({"server_instance":instance,"handle":handle}),
            ),
            (
                NAMES[4],
                json!({"server_instance":instance,"handle":handle}),
            ),
            (
                NAMES[5],
                json!({"server_instance":instance,"handle":handle,"selector":{"kind":"catalog","collection":"run"}}),
            ),
        ] {
            assert_eq!(tool(&mut server, name, arguments)["ok"], false);
        }
        let stale = if instance == "00000000000000000000000000000000" {
            "11111111111111111111111111111111"
        } else {
            "00000000000000000000000000000000"
        };
        let rejected = tool(
            &mut server,
            NAMES[1],
            json!({"server_instance":stale,"request_key":"no-engine","recipe_path":"theseus.toml","recipe_digest":inspect["data"]["recipe_digest"],"profiles":["user"],"scenarios":["clean"]}),
        );
        assert_eq!(rejected["ok"], false);
        assert_eq!(rejected["error"]["code"], "stale_instance");
        let invalid = tool(
            &mut server,
            NAMES[0],
            json!({"recipe_path":"theseus.toml","selector":{"kind":"overview"},"extra":true}),
        );
        assert_eq!(invalid["ok"], false);
        assert_eq!(invalid["error"]["code"], "invalid_arguments");
        assert_eq!(server.finish().unwrap().code(), Some(0));
        assert!(server.stderr.is_empty());
        // Parse every captured byte, including anything sent after the last reply.
        let mut all = server.stdout.split_inclusive(|b| *b == b'\n');
        assert!(all.all(native_supervisor::compact_line));
        assert_eq!(
            std::fs::read_dir(temp.path().join("output"))
                .unwrap()
                .count(),
            0,
            "read-only transcript dispatched engine"
        );
        if let Some(evidence) = std::env::var_os("THESEUS_HOST_EVIDENCE") {
            std::fs::write(
                std::path::Path::new(&evidence).join(format!("rust-{era}.stdout.jsonl")),
                &server.stdout,
            )
            .unwrap();
        }
    }
}

#[test]
fn ordinary_rust_supervisor_reaps_exited_leader_with_inherited_pipes() {
    use std::{
        io::Read,
        process::{Command, Stdio},
        time::Duration,
    };
    match std::env::var("THESEUS_MCP_RUST_HELPER").as_deref() {
        Ok("blocked-reader") | Ok("exited-leader") => {
            let root =
                std::path::PathBuf::from(std::env::var_os("THESEUS_MCP_HELPER_EVIDENCE").unwrap());
            let script = "import subprocess,sys,pathlib; p=subprocess.Popen([sys.executable,'-c',\"import sys,time; print('descendant-out',flush=True); print('descendant-err',file=sys.stderr,flush=True); time.sleep(60)\"]); pathlib.Path(sys.argv[1]).write_text(str(p.pid))";
            let mut child = Command::new(python())
                .args(["-c", script])
                .arg(root.join("descendant.pid"))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            assert!(child.wait().unwrap().success());
            child.stdout.as_mut().unwrap().read_exact(&mut [0]).unwrap();
            child.stderr.as_mut().unwrap().read_exact(&mut [0]).unwrap();
            std::fs::write(root.join("leader-exited"), b"0").unwrap();
            if std::env::var("THESEUS_MCP_RUST_HELPER").as_deref() == Ok("blocked-reader") {
                // Outer supervisor must kill this blocked helper and the pipe holder.
                child
                    .stdout
                    .take()
                    .unwrap()
                    .read_to_end(&mut Vec::new())
                    .unwrap();
                panic!("descendant unexpectedly closed inherited stdout");
            }
            // Leave a live descendant after this helper's own successful exit.
            return;
        }
        _ => {}
    }
    let exited = native_supervisor::helper(
        "ordinary_rust_supervisor_reaps_exited_leader_with_inherited_pipes",
        "exited-leader",
        Duration::from_secs(5),
        false,
    );
    assert!(exited.join("leader-exited").exists());
    native_supervisor::assert_descendant_gone(&exited);
    let blocked = native_supervisor::helper(
        "ordinary_rust_supervisor_reaps_exited_leader_with_inherited_pipes",
        "blocked-reader",
        Duration::from_secs(2),
        true,
    );
    assert!(blocked.join("leader-exited").exists());
    native_supervisor::assert_descendant_gone(&blocked);
}

#[test]
fn python_native_supervisor_keeps_transcripts_and_process_identity() {
    let evidence = match std::env::var_os("THESEUS_HOST_EVIDENCE") {
        Some(root) => tempfile::Builder::new()
            .prefix("mcp-server-")
            .tempdir_in(root)
            .unwrap(),
        None => tempfile::Builder::new()
            .prefix("mcp-server-")
            .tempdir()
            .unwrap(),
    }
    .keep();
    let mut command = std::process::Command::new(python());
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/mcp_server_stdio.py"
        ))
        .arg("--binary")
        .arg(production_binary())
        .arg("--evidence")
        .arg(&evidence);
    #[cfg(feature = "mcp-test-support")]
    command.arg("--fixture").arg(
        std::env::var_os("THESEUS_MCP_FIXTURE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_theseus-mcp-fixture").into()),
    );
    let status = command.status().unwrap();
    assert!(
        status.success(),
        "native supervision failed; evidence {}",
        evidence.display()
    );
}
