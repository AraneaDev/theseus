//! Capacity-one synchronous worker ownership. No autonomous redispatch.
use super::MAX_OPERATION;
use std::any::Any;
use std::io;
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, SyncSender, TryRecvError},
};
use std::thread::{self, JoinHandle};
use windows_sys::Win32::{Foundation::*, Storage::FileSystem::*, System::IO::CancelSynchronousIo};

pub(super) type Panic = Box<dyn Any + Send + 'static>;
pub(super) struct ReadBuffer {
    pub bytes: [u8; MAX_OPERATION],
}
pub(super) enum Command {
    Read(Box<ReadBuffer>, usize),
    Write(Arc<[u8; MAX_OPERATION]>, usize),
}
pub(super) struct Completion {
    pub buffer: Option<Box<ReadBuffer>>,
    pub count: usize,
    pub status: i32,
    pub error: Option<i32>,
    pub requested: usize,
}
impl Completion {
    pub(super) fn result(&self, reading: bool) -> io::Result<Option<usize>> {
        if self.count > self.requested || self.requested > MAX_OPERATION {
            return Err(io::Error::other("invalid native stdio completion count"));
        }
        if self.status != 0 {
            return Ok(Some(self.count));
        }
        if reading && self.error == Some(ERROR_BROKEN_PIPE as i32) {
            return Ok(None);
        }
        Err(io::Error::from_raw_os_error(
            self.error.unwrap_or(ERROR_GEN_FAILURE as i32),
        ))
    }
}
pub(super) struct Worker {
    command: SyncSender<Command>,
    completion: Receiver<Completion>,
    pub join: Option<JoinHandle<Option<Panic>>>,
    pub panic: Option<Panic>,
    stop: Arc<AtomicBool>,
    #[cfg(feature = "test-support")]
    pub barrier: Arc<AtomicBool>,
    #[cfg(feature = "test-support")]
    pub release: Arc<AtomicBool>,
}
impl Worker {
    pub(super) fn spawn(
        endpoint: Arc<OwnedHandle>,
        reading: bool,
        #[cfg(feature = "test-support")] hold: bool,
        #[cfg(test)] panic_witness: Option<Arc<AtomicBool>>,
    ) -> io::Result<Self> {
        let (command, commands) = mpsc::sync_channel::<Command>(1);
        let (completed, completion) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        #[cfg(feature = "test-support")]
        let (barrier, release) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        #[cfg(feature = "test-support")]
        let (worker_barrier, worker_release) = (barrier.clone(), release.clone());
        let join = thread::Builder::new()
            .name(
                if reading {
                    "stdio-reader"
                } else {
                    "stdio-writer"
                }
                .into(),
            )
            .spawn(move || {
                // Original payload is held by the real JoinHandle until collected.
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    while !worker_stop.load(Ordering::Acquire) {
                        let command =
                            match commands.recv_timeout(std::time::Duration::from_millis(2)) {
                                Ok(command) => command,
                                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            };
                        if worker_stop.load(Ordering::Acquire) {
                            break;
                        }
                        #[cfg(test)]
                        if let Some(witness) = panic_witness.as_ref() {
                            std::panic::panic_any(PanicWitness(witness.clone()));
                        }
                        #[cfg(feature = "test-support")]
                        if hold {
                            worker_barrier.store(true, Ordering::Release);
                            while !worker_release.load(Ordering::Acquire) {
                                thread::sleep(std::time::Duration::from_millis(2));
                            }
                        }
                        // A stop can race this synchronous call. The owner repeatedly
                        // cancels the real thread until actual completion and join.
                        let mut count = 0;
                        let (status, buffer, requested) = match command {
                            Command::Read(mut buffer, len) => {
                                let status = unsafe {
                                    ReadFile(
                                        endpoint.as_raw_handle(),
                                        buffer.bytes.as_mut_ptr(),
                                        len as u32,
                                        &mut count,
                                        std::ptr::null_mut(),
                                    )
                                };
                                (status, Some(buffer), len)
                            }
                            Command::Write(buffer, len) => {
                                let status = unsafe {
                                    WriteFile(
                                        endpoint.as_raw_handle(),
                                        buffer.as_ptr(),
                                        len as u32,
                                        &mut count,
                                        std::ptr::null_mut(),
                                    )
                                };
                                // Keep immutable storage through the native call.
                                (status, None, len)
                            }
                        };
                        let error = (status == 0).then(|| unsafe { GetLastError() } as i32);
                        if completed
                            .try_send(Completion {
                                buffer,
                                count: count as usize,
                                status,
                                error,
                                requested,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                }))
                .err()
            })?;
        Ok(Self {
            command,
            completion,
            join: Some(join),
            panic: None,
            stop,
            #[cfg(feature = "test-support")]
            barrier,
            #[cfg(feature = "test-support")]
            release,
        })
    }
    pub(super) fn dispatch(&self, command: Command) -> Result<(), Command> {
        if self.stop.load(Ordering::Acquire) {
            return Err(command);
        }
        self.command.try_send(command).map_err(|error| match error {
            mpsc::TrySendError::Full(command) | mpsc::TrySendError::Disconnected(command) => {
                command
            }
        })
    }
    pub(super) fn completion(&self) -> io::Result<Option<Completion>> {
        match self.completion.try_recv() {
            Ok(completed) => Ok(Some(completed)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(io::Error::other("stdio worker stopped")),
        }
    }
    pub(super) fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
    pub(super) fn cancel(&self) -> Option<(i32, Option<i32>)> {
        self.join.as_ref().map(|join| {
            let status = unsafe { CancelSynchronousIo(join.as_raw_handle()) };
            let error = (status == 0).then(|| unsafe { GetLastError() } as i32);
            (status, error)
        })
    }
    pub(super) fn poll_join(&mut self) -> bool {
        if self.join.as_ref().is_some_and(JoinHandle::is_finished) {
            self.panic = match self.join.take().unwrap().join() {
                Ok(payload) => payload,
                Err(payload) => Some(payload),
            };
        }
        self.join.is_none()
    }
}

#[cfg(test)]
struct PanicWitness(Arc<AtomicBool>);
#[cfg(test)]
impl Drop for PanicWitness {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::io::FromRawHandle;
    #[test]
    fn native_worker_keeps_original_non_string_panic_until_join_owner_drops() {
        let (mut read, mut write) = (std::ptr::null_mut(), std::ptr::null_mut());
        assert_ne!(
            unsafe {
                windows_sys::Win32::System::Pipes::CreatePipe(
                    &mut read,
                    &mut write,
                    std::ptr::null(),
                    4096,
                )
            },
            0
        );
        let read = unsafe { OwnedHandle::from_raw_handle(read) };
        let _peer = unsafe { OwnedHandle::from_raw_handle(write) };
        let dropped = Arc::new(AtomicBool::new(false));
        let mut worker = Worker::spawn(
            Arc::new(read),
            true,
            #[cfg(feature = "test-support")]
            false,
            Some(dropped.clone()),
        )
        .unwrap();
        assert!(
            worker
                .dispatch(Command::Read(
                    Box::new(ReadBuffer {
                        bytes: [0; MAX_OPERATION]
                    }),
                    4096
                ))
                .is_ok()
        );
        let start = std::time::Instant::now();
        while !worker.poll_join() {
            assert!(start.elapsed() < std::time::Duration::from_secs(5));
            thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(
            worker
                .panic
                .as_ref()
                .unwrap()
                .downcast_ref::<PanicWitness>()
                .is_some()
        );
        assert!(!dropped.load(Ordering::Acquire));
        drop(worker);
        assert!(dropped.load(Ordering::Acquire));
    }
}
