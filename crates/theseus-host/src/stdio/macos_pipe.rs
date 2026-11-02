//! Darwin pipe endpoints have different stat inode identities; query their peers.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "test-support", derive(serde::Serialize))]
pub(super) enum Identity {
    Anonymous { handle: u64, peer: u64 },
    Named { device: u64, inode: u64 },
}

pub(super) fn disjoint(input: Identity, output: Identity) -> bool {
    match (input, output) {
        (
            Identity::Anonymous {
                handle: a,
                peer: ap,
            },
            Identity::Anonymous {
                handle: b,
                peer: bp,
            },
        ) => a != 0 && b != 0 && a != b && a != bp && b != ap,
        (
            Identity::Named {
                device: a,
                inode: i,
            },
            Identity::Named {
                device: b,
                inode: j,
            },
        ) => (a, i) != (b, j),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pipe(handle: u64, peer: u64) -> Identity {
        Identity::Anonymous { handle, peer }
    }
    #[test]
    fn opposite_pipe_ends_and_same_end_aliases_are_not_disjoint() {
        assert!(!disjoint(pipe(10, 20), pipe(20, 10)));
        assert!(!disjoint(pipe(10, 20), pipe(10, 20)));
        assert!(!disjoint(pipe(10, 20), pipe(30, 10)));
        assert!(!disjoint(pipe(10, 20), pipe(20, 30)));
    }
    #[test]
    fn distinct_pipes_and_closed_peers_remain_disjoint() {
        assert!(disjoint(pipe(10, 20), pipe(30, 40)));
        assert!(disjoint(pipe(10, 0), pipe(30, 0)));
        assert!(!disjoint(pipe(0, 20), pipe(30, 40)));
        assert!(!disjoint(pipe(10, 20), pipe(0, 40)));
    }
    #[test]
    fn filesystem_fifo_identity_is_distinct_from_anonymous_identity() {
        let named = Identity::Named {
            device: 1,
            inode: 10,
        };
        assert!(!disjoint(named, named));
        assert!(disjoint(
            named,
            Identity::Named {
                device: 1,
                inode: 11
            }
        ));
        assert!(disjoint(named, pipe(10, 20)));
        assert!(disjoint(pipe(10, 20), named));
    }
}

#[cfg(target_os = "macos")]
pub(super) use native::identity;

#[cfg(target_os = "macos")]
mod native {
    use super::Identity;
    #[cfg(feature = "test-support")]
    use crate::stdio::test_support::{Event, Trace};
    use std::{
        io,
        mem::{MaybeUninit, offset_of, size_of},
    };
    // Apple public XNU proc_info.h: proc_fileinfo, pipe_info/pipe_fdinfo,
    // vnode_fdinfo. libc supplies the exact vinfo_stat/vnode_info components.
    // https://github.com/apple-oss-distributions/xnu/blob/xnu-11417.140.69/bsd/sys/proc_info.h
    #[repr(C)]
    struct FileInfo {
        openflags: u32,
        status: u32,
        offset: i64,
        kind: i32,
        guardflags: u32,
    }
    #[repr(C)]
    struct PipeInfo {
        stat: libc::vinfo_stat,
        handle: u64,
        peer: u64,
        status: i32,
        reserved: i32,
    }
    #[repr(C)]
    struct PipeFdInfo {
        file: FileInfo,
        pipe: PipeInfo,
    }
    #[repr(C)]
    struct VnodeFdInfo {
        file: FileInfo,
        vnode: libc::vnode_info,
    }
    // Both supported macOS ABIs are 64-bit. Pin the public header layouts,
    // including the peer fields, rather than parsing untyped byte offsets.
    const _: () = {
        assert!(size_of::<FileInfo>() == 24);
        assert!(size_of::<libc::vinfo_stat>() == 136);
        assert!(size_of::<PipeInfo>() == 160);
        assert!(size_of::<PipeFdInfo>() == 184);
        assert!(size_of::<VnodeFdInfo>() == 176);
        assert!(offset_of!(PipeInfo, handle) == 136);
        assert!(offset_of!(PipeInfo, peer) == 144);
    };

    fn query<T>(
        fd: i32,
        flavor: i32,
        #[cfg(feature = "test-support")] trace: &mut Trace,
        #[cfg(feature = "test-support")] endpoint: &'static str,
    ) -> io::Result<T> {
        let mut output = MaybeUninit::<T>::zeroed();
        let expected = size_of::<T>() as i32;
        // Current-process held duplicates cannot be closed/reused by the peer.
        // The public wrapper returns zero and preserves errno on syscall error.
        let actual = unsafe {
            libc::proc_pidfdinfo(
                libc::getpid(),
                fd,
                flavor,
                output.as_mut_ptr().cast(),
                expected,
            )
        };
        let error = (actual == 0).then(io::Error::last_os_error);
        #[cfg(feature = "test-support")]
        trace.push(Event { phase: "query", endpoint, api: "proc_pidfdinfo", ok: actual == expected, status: Some(actual as i64), error: error.as_ref().and_then(io::Error::raw_os_error), observed: serde_json::json!({"flavor":flavor,"expected_bytes":expected,"returned_bytes":actual,"held_descriptor":true}) });
        if let Some(error) = error {
            return Err(error);
        }
        if actual != expected {
            return Err(io::Error::other(
                "native pipe identity query returned wrong size",
            ));
        }
        // Only the two all-integer public C layouts above instantiate this
        // private helper, after the kernel reports their complete exact size.
        Ok(unsafe { output.assume_init() })
    }

    pub(in crate::stdio::unix) fn identity(
        fd: i32,
        expected: (libc::dev_t, libc::ino_t),
        #[cfg(feature = "test-support")] trace: &mut Trace,
        #[cfg(feature = "test-support")] endpoint: &'static str,
    ) -> io::Result<Identity> {
        let pipe = query::<PipeFdInfo>(
            fd,
            6,
            #[cfg(feature = "test-support")]
            trace,
            #[cfg(feature = "test-support")]
            endpoint,
        );
        match pipe {
            Ok(info) if info.file.kind == 6 && info.pipe.handle != 0 => Ok(Identity::Anonymous {
                handle: info.pipe.handle,
                peer: info.pipe.peer,
            }),
            Ok(_) => Err(io::Error::other(
                "native anonymous pipe identity is invalid",
            )),
            // EBADF is the documented DTYPE mismatch for a filesystem FIFO.
            // It is not identity evidence: require a successful vnode query.
            Err(error) if error.raw_os_error() == Some(libc::EBADF) => {
                let info = query::<VnodeFdInfo>(
                    fd,
                    1,
                    #[cfg(feature = "test-support")]
                    trace,
                    #[cfg(feature = "test-support")]
                    endpoint,
                )?;
                let stat = info.vnode.vi_stat;
                if info.file.kind != 1
                    || u32::from(stat.vst_mode) & libc::S_IFMT as u32 != libc::S_IFIFO as u32
                    || stat.vst_dev != expected.0 as u32
                    || stat.vst_ino != expected.1
                {
                    return Err(io::Error::other(
                        "native FIFO identity does not match held descriptor",
                    ));
                }
                Ok(Identity::Named {
                    device: u64::from(stat.vst_dev),
                    inode: stat.vst_ino,
                })
            }
            Err(error) => Err(error),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        fn pipe() -> (OwnedFd, OwnedFd) {
            let mut fds = [-1; 2];
            assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
            unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
        }
        fn held_identity(fd: &OwnedFd) -> Identity {
            let mut stat = MaybeUninit::<libc::stat>::uninit();
            assert_eq!(unsafe { libc::fstat(fd.as_raw_fd(), stat.as_mut_ptr()) }, 0);
            let stat = unsafe { stat.assume_init() };
            identity(
                fd.as_raw_fd(),
                (stat.st_dev, stat.st_ino),
                #[cfg(feature = "test-support")]
                &mut Trace::default(),
                #[cfg(feature = "test-support")]
                "test",
            )
            .unwrap()
        }
        #[test]
        fn actual_macos_peer_query_distinguishes_same_pair_from_two_pairs_and_closed_peer() {
            let (a, b) = pipe();
            let (c, d) = pipe();
            assert!(!super::super::disjoint(
                held_identity(&a),
                held_identity(&b)
            ));
            assert!(super::super::disjoint(held_identity(&a), held_identity(&d)));
            drop(c);
            assert!(super::super::disjoint(held_identity(&a), held_identity(&d)));
        }
    }
}
