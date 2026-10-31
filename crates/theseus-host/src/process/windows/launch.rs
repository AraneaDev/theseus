use super::{OwnedChild, encoding, job::Job};
use crate::process::{HostCleanup, LaunchPhase, NativeCommand, OwnedSpawnCleanup, SpawnFailure};
use std::fs::{File, OpenOptions};
use std::io;
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{
    DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_INSUFFICIENT_BUFFER, HANDLE,
};
use windows_sys::Win32::System::Memory::{GetProcessHeap, HeapAlloc, HeapFree};
use windows_sys::Win32::System::Threading::*;

// The opaque attribute list is allocated with the documented process-heap pattern.
struct Attributes {
    memory: *mut std::ffi::c_void,
    heap: HANDLE,
}
impl Attributes {
    fn new(handles: &[HANDLE], jobs: &[HANDLE], pseudoconsole: Option<isize>) -> io::Result<Self> {
        let mut bytes = 0;
        let result = unsafe { InitializeProcThreadAttributeList(null_mut(), 2, 0, &mut bytes) };
        if result != 0
            || io::Error::last_os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
            || bytes == 0
        {
            return Err(io::Error::other("invalid startup attribute size query"));
        }
        let heap = unsafe { GetProcessHeap() };
        if heap.is_null() {
            return Err(io::Error::last_os_error());
        }
        let memory = unsafe { HeapAlloc(heap, 0, bytes) };
        if memory.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "startup attributes allocation",
            ));
        }
        if unsafe { InitializeProcThreadAttributeList(memory, 2, 0, &mut bytes) } == 0 {
            let error = io::Error::last_os_error();
            unsafe {
                HeapFree(heap, 0, memory);
            }
            return Err(error);
        }
        let attributes = Self { memory, heap };
        // Caller owns both arrays through attribute-list destruction after creation.
        for (key, values) in [
            (PROC_THREAD_ATTRIBUTE_HANDLE_LIST, handles),
            (PROC_THREAD_ATTRIBUTE_JOB_LIST, jobs),
        ] {
            if key == PROC_THREAD_ATTRIBUTE_HANDLE_LIST && pseudoconsole.is_some() {
                continue;
            }
            if unsafe {
                UpdateProcThreadAttribute(
                    memory,
                    0,
                    key as usize,
                    values.as_ptr().cast(),
                    std::mem::size_of_val(values),
                    null_mut(),
                    null(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        if let Some(hpc) = pseudoconsole {
            // PSEUDOCONSOLE takes the VALUE, unlike JOB_LIST's pointer to an array.
            if unsafe {
                UpdateProcThreadAttribute(
                    memory,
                    0,
                    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                    hpc as *const std::ffi::c_void,
                    size_of::<isize>(),
                    null_mut(),
                    null(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(attributes)
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe {
            DeleteProcThreadAttributeList(self.memory);
            HeapFree(self.heap, 0, self.memory);
        }
    }
}

fn stdio(file: Option<File>, input: bool) -> io::Result<OwnedHandle> {
    let file = match file {
        Some(file) => file,
        None => OpenOptions::new().read(input).write(!input).open("NUL")?,
    };
    let mut duplicate = null_mut();
    // Only the duplicate is inheritable. Never mutate a caller's handle flags.
    let current = unsafe { GetCurrentProcess() };
    if unsafe {
        DuplicateHandle(
            current,
            file.as_raw_handle(),
            current,
            &mut duplicate,
            0,
            1,
            DUPLICATE_SAME_ACCESS,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(duplicate) })
}

pub(super) fn spawn(
    command: &NativeCommand,
    stdin: Option<File>,
    stdout: Option<File>,
    stderr: Option<File>,
) -> Result<OwnedChild, SpawnFailure> {
    spawn_impl(
        command,
        stdin,
        stdout,
        stderr,
        #[cfg(any(test, feature = "test-support"))]
        Fault::None,
    )
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Copy, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(super) enum Fault {
    None,
    BeforeCreate,
    InvalidJob,
    AfterCreate,
    BeforeResume,
    CleanupPending,
    ResumeUncertain,
    SuspendAgain,
    ReleaseThenResuspend,
    AlreadyResumed,
    ResumeFailure,
    AfterResume,
}

#[cfg(any(test, feature = "test-support"))]
fn wait_for_history_marker(
    ready: &std::path::Path,
    deadline: std::time::Instant,
) -> io::Result<()> {
    let mut observed = None;
    while std::time::Instant::now() < deadline {
        match std::fs::read(ready) {
            Ok(bytes) if bytes == b"history child ran" && std::time::Instant::now() < deadline => {
                return Ok(());
            }
            Ok(bytes) => observed = Some(bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        std::thread::sleep(
            std::time::Duration::from_millis(2)
                .min(deadline.saturating_duration_since(std::time::Instant::now())),
        );
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("history child did not publish complete marker; observed {observed:?}"),
    ))
}

fn fail(
    mut child: OwnedChild,
    launch: LaunchPhase,
    error: io::Error,
    #[cfg(any(test, feature = "test-support"))] pending: bool,
) -> SpawnFailure {
    #[cfg(any(test, feature = "test-support"))]
    if pending {
        return SpawnFailure {
            launch,
            cleanup: HostCleanup::Pending(OwnedSpawnCleanup {
                child: Some(Box::new(child)),
                readers: Vec::new(),
                writers: Vec::new(),
                #[cfg(feature = "test-support")]
                cleanup_faults: 0,
            }),
            error,
            cleanup_errors: vec![io::Error::other("injected cleanup failure")],
        };
    }
    let mut cleanup_errors = Vec::new();
    let complete = match child.cleanup() {
        Ok(complete) => complete,
        Err(error) => {
            cleanup_errors.push(error);
            false
        }
    };
    let cleanup = if complete {
        HostCleanup::Complete
    } else {
        HostCleanup::Pending(OwnedSpawnCleanup {
            child: Some(Box::new(child)),
            readers: Vec::new(),
            writers: Vec::new(),
            #[cfg(feature = "test-support")]
            cleanup_faults: 0,
        })
    };
    SpawnFailure {
        launch,
        cleanup,
        error,
        cleanup_errors,
    }
}

pub(super) fn spawn_impl(
    command: &NativeCommand,
    stdin: Option<File>,
    stdout: Option<File>,
    stderr: Option<File>,
    #[cfg(any(test, feature = "test-support"))] fault: Fault,
) -> Result<OwnedChild, SpawnFailure> {
    spawn_config(
        command,
        stdin,
        stdout,
        stderr,
        #[cfg(any(test, feature = "test-support"))]
        fault,
        None,
    )
}

#[cfg(feature = "test-support")]
pub(super) fn spawn_terminal(
    command: &NativeCommand,
    hpc: isize,
) -> Result<OwnedChild, SpawnFailure> {
    spawn_config(command, None, None, None, Fault::None, Some(hpc))
}
fn spawn_config(
    command: &NativeCommand,
    stdin: Option<File>,
    stdout: Option<File>,
    stderr: Option<File>,
    #[cfg(any(test, feature = "test-support"))] fault: Fault,
    pseudoconsole: Option<isize>,
) -> Result<OwnedChild, SpawnFailure> {
    command.validate().map_err(SpawnFailure::never)?;
    let application = encoding::application(command).map_err(SpawnFailure::never)?;
    let application_wide = encoding::wide(application.as_os_str()).map_err(SpawnFailure::never)?;
    let mut argv =
        encoding::command_line(&application, &command.args).map_err(SpawnFailure::never)?;
    let environment = encoding::environment(&command.environment).map_err(SpawnFailure::never)?;
    let cwd = command
        .cwd
        .as_ref()
        .map(|cwd| encoding::wide(cwd.as_os_str()))
        .transpose()
        .map_err(SpawnFailure::never)?;
    let input = stdio(stdin, true).map_err(SpawnFailure::never)?;
    let output = stdio(stdout, false).map_err(SpawnFailure::never)?;
    let errors = stdio(stderr, false).map_err(SpawnFailure::never)?;
    let job = Job::new().map_err(SpawnFailure::never)?;
    let handles = [
        input.as_raw_handle(),
        output.as_raw_handle(),
        errors.as_raw_handle(),
    ];
    let jobs = [job.raw()];
    // A real non-job kernel handle forces native JOB_LIST validation failure.
    #[cfg(any(test, feature = "test-support"))]
    let jobs = if fault == Fault::InvalidJob {
        [input.as_raw_handle()]
    } else {
        jobs
    };
    let attributes =
        Attributes::new(&handles, &jobs, pseudoconsole).map_err(SpawnFailure::never)?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    if pseudoconsole.is_none() {
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = handles[0];
        startup.StartupInfo.hStdOutput = handles[1];
        startup.StartupInfo.hStdError = handles[2];
    }
    startup.lpAttributeList = attributes.memory;
    let mut information = PROCESS_INFORMATION::default();
    #[cfg(any(test, feature = "test-support"))]
    if fault == Fault::BeforeCreate {
        return Err(SpawnFailure::never(io::Error::other(
            "injected before-create failure",
        )));
    }
    // Every pointer references owned live storage. STARTUPINFOEX is passed through
    // its first STARTUPINFO member with the extended size and creation flag.
    if unsafe {
        CreateProcessW(
            application_wide.as_ptr(),
            argv.as_mut_ptr(),
            null(),
            null(),
            i32::from(pseudoconsole.is_none()),
            CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
            environment.as_ref().map_or(null(), |v| v.as_ptr().cast()),
            cwd.as_ref().map_or(null(), |v| v.as_ptr()),
            &startup.StartupInfo,
            &mut information,
        )
    } == 0
    {
        return Err(SpawnFailure::never(io::Error::last_os_error()));
    }
    // Establish BOTH handle owners immediately, before any fallible check/resume.
    let process = unsafe { OwnedHandle::from_raw_handle(information.hProcess) };
    let thread = unsafe { OwnedHandle::from_raw_handle(information.hThread) };
    let child = OwnedChild {
        process,
        thread,
        job,
        released: false,
        status: None,
        job_termination_requested: false,
        process_termination_requested: false,
    };
    drop(attributes);
    #[cfg(any(test, feature = "test-support"))]
    if matches!(fault, Fault::AfterCreate | Fault::CleanupPending) {
        return Err(fail(
            child,
            LaunchPhase::CreatedSuspended,
            io::Error::other("injected after-create failure"),
            fault == Fault::CleanupPending,
        ));
    }
    let membership = child.job.contains(child.process.as_raw_handle());
    if !matches!(membership, Ok(true)) {
        let error = membership
            .err()
            .unwrap_or_else(|| io::Error::other("child absent from owned job"));
        return Err(fail(
            child,
            LaunchPhase::CreatedSuspended,
            error,
            #[cfg(any(test, feature = "test-support"))]
            false,
        ));
    }
    #[cfg(any(test, feature = "test-support"))]
    if fault == Fault::BeforeResume {
        return Err(fail(
            child,
            LaunchPhase::CreatedSuspended,
            io::Error::other("injected before-resume failure"),
            false,
        ));
    }
    #[cfg(any(test, feature = "test-support"))]
    match fault {
        Fault::SuspendAgain => {
            assert_eq!(unsafe { SuspendThread(child.thread.as_raw_handle()) }, 1);
        }
        Fault::ReleaseThenResuspend => {
            assert_eq!(unsafe { ResumeThread(child.thread.as_raw_handle()) }, 1);
            // The child test keeps its worker alive behind a bounded gate. Its
            // primary thread waits for that worker, so resuspension cannot race
            // process exit. The marker also proves actual execution after release.
            let crate::process::ChildEnvironment::Replace(entries) = &command.environment else {
                panic!("history fault requires explicit gate")
            };
            let ready = std::path::PathBuf::from(
                &entries
                    .iter()
                    .find(|(key, _)| key == "THESEUS_HOST_RESUME_HISTORY_GATE")
                    .unwrap()
                    .1,
            );
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            // Creation is observable before write completion. Resuspend only
            // after the entire execution witness is readable, under this same
            // gate deadline. A gate error retains actual cleanup authority.
            if let Err(error) = wait_for_history_marker(&ready, deadline) {
                return Err(fail(child, LaunchPhase::MayRun, error, false));
            }
            assert_eq!(unsafe { SuspendThread(child.thread.as_raw_handle()) }, 0);
            assert_eq!(unsafe { SuspendThread(child.thread.as_raw_handle()) }, 1);
        }
        Fault::AlreadyResumed => {
            assert_eq!(unsafe { ResumeThread(child.thread.as_raw_handle()) }, 1);
        }
        _ => {}
    }
    let resume_handle = child.thread.as_raw_handle();
    #[cfg(any(test, feature = "test-support"))]
    let resume_handle = if fault == Fault::ResumeFailure {
        child.process.as_raw_handle()
    } else {
        resume_handle
    };
    let previous = unsafe { ResumeThread(resume_handle) };
    if previous != 1 {
        let error = if previous == u32::MAX {
            io::Error::last_os_error()
        } else {
            io::Error::other("unexpected primary-thread suspend count")
        };
        // Any unexpected count has unknown history. Even >1 can follow an
        // earlier release and resuspension; current suspension cannot prove
        // that user code was never runnable.
        let phase = LaunchPhase::MayRun;
        return Err(fail(
            child,
            phase,
            error,
            #[cfg(any(test, feature = "test-support"))]
            false,
        ));
    }
    #[cfg(any(test, feature = "test-support"))]
    if matches!(fault, Fault::ResumeUncertain | Fault::AfterResume) {
        return Err(fail(
            child,
            LaunchPhase::MayRun,
            io::Error::other("injected uncertain/post-resume failure"),
            false,
        ));
    }
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{ChildEnvironment, HostCleanup, LaunchPhase};
    use std::time::{Duration, Instant};

    fn marker_command() -> NativeCommand {
        NativeCommand {
            program: std::env::current_exe().unwrap().into(),
            args: vec!["--list".into()],
            cwd: None,
            environment: ChildEnvironment::Inherit,
        }
    }
    fn complete(failure: &mut SpawnFailure) {
        if let HostCleanup::Pending(guard) = &mut failure.cleanup {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !guard.try_complete().unwrap() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
    #[test]
    fn actual_test_executable_produces_marker_after_resume() {
        let out = tempfile::NamedTempFile::new().unwrap();
        let mut child = spawn_impl(
            &marker_command(),
            None,
            Some(out.reopen().unwrap()),
            None,
            Fault::None,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !child.exited_reserved().unwrap() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(child.reap_and_release().unwrap().success());
        let marker = std::fs::read_to_string(out.path()).unwrap();
        assert!(marker.contains("process::windows::launch::tests::actual_test_executable_produces_marker_after_resume: test"), "{marker}");
    }
    #[test]
    fn setup_and_assignment_failures_never_execute_marker() {
        for (fault, phase) in [
            (Fault::BeforeCreate, LaunchPhase::NeverCreated),
            (Fault::InvalidJob, LaunchPhase::NeverCreated),
            (Fault::AfterCreate, LaunchPhase::CreatedSuspended),
            (Fault::BeforeResume, LaunchPhase::CreatedSuspended),
        ] {
            let out = tempfile::NamedTempFile::new().unwrap();
            let mut failure = spawn_impl(
                &marker_command(),
                None,
                Some(out.reopen().unwrap()),
                None,
                fault,
            )
            .unwrap_err();
            assert_eq!(failure.launch, phase);
            complete(&mut failure);
            assert!(std::fs::read(out.path()).unwrap().is_empty());
        }
    }
    #[test]
    fn cleanup_failure_retains_both_handles_and_original_cause() {
        let out = tempfile::NamedTempFile::new().unwrap();
        let mut failure = spawn_impl(
            &marker_command(),
            None,
            Some(out.reopen().unwrap()),
            None,
            Fault::CleanupPending,
        )
        .unwrap_err();
        assert_eq!(failure.launch, LaunchPhase::CreatedSuspended);
        assert_eq!(failure.error.to_string(), "injected after-create failure");
        assert_eq!(failure.cleanup_errors.len(), 1);
        let HostCleanup::Pending(guard) = &mut failure.cleanup else {
            panic!("lost cleanup authority")
        };
        assert!(
            !guard
                .child
                .as_mut()
                .unwrap()
                .process
                .as_raw_handle()
                .is_null()
        );
        assert!(
            !guard
                .child
                .as_mut()
                .unwrap()
                .thread
                .as_raw_handle()
                .is_null()
        );
        assert!(!guard.child.as_mut().unwrap().exited_reserved().unwrap());
        // Inspect the retained exact thread: previous suspend count one proves
        // CREATE_SUSPENDED was applied before any fixture instruction could run.
        assert_eq!(
            unsafe { SuspendThread(guard.child.as_mut().unwrap().thread.as_raw_handle()) },
            1
        );
        assert_eq!(
            unsafe { ResumeThread(guard.child.as_mut().unwrap().thread.as_raw_handle()) },
            2
        );
        assert!(std::fs::read(out.path()).unwrap().is_empty());
        guard.child.as_mut().unwrap().terminate_owned().unwrap();
        guard.child.as_mut().unwrap().terminate_owned().unwrap();
        complete(&mut failure);
        assert!(std::fs::read(out.path()).unwrap().is_empty());
    }
    #[test]
    fn native_resume_counts_and_failed_resume_preserve_phase() {
        for (fault, phase) in [
            (Fault::SuspendAgain, LaunchPhase::MayRun),
            (Fault::AlreadyResumed, LaunchPhase::MayRun),
            (Fault::ResumeFailure, LaunchPhase::MayRun),
        ] {
            let out = tempfile::NamedTempFile::new().unwrap();
            let mut failure = spawn_impl(
                &marker_command(),
                None,
                Some(out.reopen().unwrap()),
                None,
                fault,
            )
            .unwrap_err();
            assert_eq!(failure.launch, phase);
            complete(&mut failure);
            if !matches!(fault, Fault::AlreadyResumed | Fault::ReleaseThenResuspend) {
                assert!(std::fs::read(out.path()).unwrap().is_empty());
            }
        }
    }
    #[test]
    fn released_then_resuspended_history_is_always_may_run() {
        const GATE: &str = "THESEUS_HOST_RESUME_HISTORY_GATE";
        if let Some(ready) = std::env::var_os(GATE) {
            // Trusted subprocess role of this same test executable. Parent runs
            // the assertions; child stays alive for exact-thread resuspension.
            std::fs::write(ready, b"history child ran").unwrap();
            std::thread::sleep(Duration::from_secs(15));
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let ready = root.path().join("history-ready");
        let mut command = marker_command();
        command.args = vec![
            "--exact".into(),
            "process::windows::launch::tests::released_then_resuspended_history_is_always_may_run"
                .into(),
        ];
        command.environment = ChildEnvironment::Replace(vec![(GATE.into(), ready.clone().into())]);
        let mut failure =
            spawn_impl(&command, None, None, None, Fault::ReleaseThenResuspend).unwrap_err();
        assert_eq!(std::fs::read(ready).unwrap(), b"history child ran");
        assert_eq!(failure.launch, LaunchPhase::MayRun);
        assert_eq!(
            failure.error.to_string(),
            "unexpected primary-thread suspend count",
            "the history witness must reach exact-thread resuspension, not fail at its gate"
        );
        complete(&mut failure);
    }

    #[test]
    fn history_gate_requires_complete_publication_before_readiness() -> io::Result<()> {
        use std::io::Write;
        let root = tempfile::tempdir()?;
        let ready = root.path().join("partial-history");
        let rejects_incomplete = || {
            let error = wait_for_history_marker(&ready, Instant::now() + Duration::from_millis(20))
                .expect_err("absent, empty, partial and wrong markers cannot release the gate");
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        };
        rejects_incomplete();
        let mut publication = File::create(&ready)?;
        rejects_incomplete();
        publication.write_all(b"history ")?;
        publication.flush()?;
        rejects_incomplete();
        publication.write_all(b"child ran")?;
        publication.flush()?;
        wait_for_history_marker(&ready, Instant::now() + Duration::from_secs(1))?;
        drop(publication);
        std::fs::write(&ready, b"history child bad")?;
        rejects_incomplete();
        std::fs::write(&ready, b"history child ran")?;
        assert_eq!(
            wait_for_history_marker(&ready, Instant::now())
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut,
            "complete bytes cannot restart an expired gate"
        );
        Ok(())
    }

    #[test]
    fn history_gate_read_error_retains_may_run_cleanup_authority() {
        let root = tempfile::tempdir().unwrap();
        let mut command = marker_command();
        // A directory is an owned, deterministic read error. The real child is
        // released first, so failure must preserve MayRun and native cleanup.
        command.environment = ChildEnvironment::Replace(vec![(
            "THESEUS_HOST_RESUME_HISTORY_GATE".into(),
            root.path().as_os_str().to_owned(),
        )]);
        let mut failure =
            spawn_impl(&command, None, None, None, Fault::ReleaseThenResuspend).unwrap_err();
        assert_eq!(failure.launch, LaunchPhase::MayRun);
        assert_ne!(failure.error.kind(), io::ErrorKind::TimedOut);
        complete(&mut failure);
        assert_eq!(failure.launch, LaunchPhase::MayRun);
    }

    #[test]
    fn native_duplicate_termination_control_records_exact_api_results() {
        use std::io::Write;
        let mut failures = Vec::new();
        for attempt in 0..8 {
            let mut failure =
                spawn_impl(&marker_command(), None, None, None, Fault::CleanupPending).unwrap_err();
            let HostCleanup::Pending(guard) = &mut failure.cleanup else {
                panic!("requires suspended process")
            };
            let child = guard.child.as_mut().unwrap();
            assert!(child.job.contains(child.process.as_raw_handle()).unwrap());
            let job = guard.child.as_mut().unwrap().job.terminate();
            assert!(job.is_ok());
            // Reproduce the original native sequence independently of the fix.
            let process = unsafe {
                TerminateProcess(guard.child.as_mut().unwrap().process.as_raw_handle(), 1)
            };
            let error = if process == 0 {
                Some(io::Error::last_os_error().raw_os_error())
            } else {
                None
            };
            let signaled = guard.child.as_mut().unwrap().exited_reserved().unwrap();
            writeln!(std::io::stdout().lock(), "termination-control attempt={attempt} TerminateJobObject=success TerminateProcess={process} error={error:?} process_signaled={signaled}").unwrap();
            if let Some(Some(code)) = error {
                failures.push(code);
            }
            // The real successful job request covers this confirmed member.
            guard.child.as_mut().unwrap().job_termination_requested = true;
            guard.child.as_mut().unwrap().process_termination_requested = true;
            complete(&mut failure);
        }
        assert!(
            failures.iter().all(|code| *code == 5),
            "unexpected native failure: {failures:?}"
        );
        // Timing may allow a redundant request to succeed. The raw native results
        // establish which exact API failed; no denial is treated as completion.
    }

    #[test]
    fn uncertain_resume_and_post_resume_failure_preserve_may_run() {
        for fault in [Fault::ResumeUncertain, Fault::AfterResume] {
            let mut failure = spawn_impl(&marker_command(), None, None, None, fault).unwrap_err();
            assert_eq!(failure.launch, LaunchPhase::MayRun);
            complete(&mut failure);
            assert_eq!(failure.launch, LaunchPhase::MayRun);
        }
    }
}
