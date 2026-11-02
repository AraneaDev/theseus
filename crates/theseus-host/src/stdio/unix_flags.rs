//! Exact restoration comparison for qualified Unix pipe endpoints.
pub(super) fn restored_pipe_flags_match(saved: i32, actual: i32) -> bool {
    flags_match(saved, actual, cfg!(target_os = "macos"))
}

fn flags_match(saved: i32, actual: i32, darwin: bool) -> bool {
    // XNU exposes kernel-owned FWASWRITTEN through F_GETFL after a write;
    // F_SETFL cannot clear it because it is outside FCNTLFLAGS. Permit only
    // that monotonic history addition on a writable Darwin pipe. Every other
    // bit, including access/nonblocking flags and preexisting history, must match.
    // https://github.com/apple-oss-distributions/xnu/blob/xnu-11417.140.69/bsd/sys/fcntl.h
    // https://github.com/apple-oss-distributions/xnu/blob/xnu-11417.140.69/bsd/kern/kern_descrip.c
    // https://github.com/apple-oss-distributions/xnu/blob/xnu-11417.140.69/bsd/kern/sys_generic.c
    const DARWIN_FWASWRITTEN: i32 = 0x0001_0000;
    let darwin_writable = matches!(saved & 0x3, 1 | 2);
    saved >= 0
        && actual >= 0
        && (actual == saved
            || (darwin && darwin_writable && actual == (saved | DARWIN_FWASWRITTEN)))
}

#[cfg(test)]
mod tests {
    use super::*;
    const WRITTEN: i32 = 0x0001_0000;

    #[test]
    fn darwin_written_history_addition_is_not_a_restoration_failure() {
        // Actual macOS CI: stdout saved=1, successful F_SETFL, F_GETFL=65537.
        assert!(flags_match(1, 65537, true));
        assert!(flags_match(2, 2 | WRITTEN, true));
    }

    #[test]
    fn written_history_exception_is_darwin_only_and_never_clears_history() {
        assert!(!flags_match(1, 65537, false));
        assert!(!flags_match(1 | WRITTEN, 1, true));
        assert!(!flags_match(0, WRITTEN, true));
        assert!(!flags_match(3, 3 | WRITTEN, true));
        assert!(flags_match(1 | WRITTEN, 1 | WRITTEN, true));
        assert_eq!(
            restored_pipe_flags_match(1, 65537),
            cfg!(target_os = "macos")
        );
    }

    #[test]
    fn all_other_flag_changes_and_failed_queries_remain_errors() {
        for darwin in [false, true] {
            assert!(flags_match(0, 0, darwin));
            assert!(flags_match(1, 1, darwin));
            assert!(!flags_match(1, -1, darwin));
            assert!(!flags_match(-1, -1, darwin));
            for bit in 0..31 {
                if bit != 16 {
                    assert!(!flags_match(1, 1 ^ (1 << bit), darwin), "bit {bit}");
                    assert!(
                        !flags_match(1, (1 | WRITTEN) ^ (1 << bit), darwin),
                        "written + bit {bit}"
                    );
                }
            }
        }
    }
}
