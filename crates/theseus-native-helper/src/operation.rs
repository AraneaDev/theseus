use crate::contract::{FinalBoundary, RunKey};
use std::io;
use theseus_native_protocol::v2;
pub(crate) struct OutputState {
    #[cfg(feature = "test-support")]
    pub(crate) fixture: Option<crate::test_support::FixtureControl>,
    pub(crate) key: RunKey,
    pub(crate) stdout: u64,
    pub(crate) stderr: u64,
    pub(crate) truncated: bool,
    pub(crate) ordinal: u32,
    reserved: u32,
}
impl OutputState {
    pub(crate) fn new(key: RunKey) -> Self {
        Self {
            #[cfg(feature = "test-support")]
            fixture: None,
            key,
            stdout: 0,
            stderr: 0,
            truncated: false,
            ordinal: 0,
            reserved: 0,
        }
    }
    /// Counters observe all bytes; the lifetime reservation is never refunded.
    pub(crate) fn observe(
        &mut self,
        channel: v2::Channel,
        n: usize,
        has_token: bool,
    ) -> io::Result<(u64, usize)> {
        let count = match channel {
            v2::Channel::Stdout => &mut self.stdout,
            v2::Channel::Stderr => &mut self.stderr,
        };
        let offset = *count;
        *count = count
            .checked_add(n as u64)
            .ok_or_else(|| io::Error::other("output counter overflow"))?;
        let retain = if has_token {
            n.min((16384 - self.reserved) as usize)
        } else {
            0
        };
        #[cfg(feature = "test-support")]
        let retain = if let Some(control) = &self.fixture {
            if n > 0 && !has_token {
                control.prepare_capture_saturated_facts(self.reserved, self.ordinal);
            }
            if control.is_output_pressure() {
                if self.ordinal < 128 {
                    retain.min(512)
                } else {
                    0
                }
            } else {
                retain
            }
        } else {
            retain
        };
        self.reserved += retain as u32;
        self.truncated |= retain < n;
        Ok((offset, retain))
    }
    pub(crate) fn boundary(&self) -> FinalBoundary {
        FinalBoundary {
            key: self.key,
            final_ordinal: self.ordinal,
            stdout_bytes: self.stdout,
            stderr_bytes: self.stderr,
            truncated: self.truncated,
        }
    }
}
pub(crate) fn status(status: std::process::ExitStatus) -> io::Result<v2::NativeStatus> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(code) = status.code() {
            return Ok(v2::NativeStatus::PosixExit { code: code as u8 });
        }
        if let Some(signal) = status.signal() {
            return Ok(v2::NativeStatus::PosixSignal {
                signal: signal as u16,
            });
        }
        Err(io::Error::other("unproved exit status"))
    }
    #[cfg(windows)]
    {
        status
            .code()
            .map(|code| v2::NativeStatus::WindowsExit { code: code as u32 })
            .ok_or_else(|| io::Error::other("unproved exit status"))
    }
}
pub(crate) fn cleanup_fact() -> v2::Cleanup {
    #[cfg(unix)]
    {
        v2::Cleanup::PosixLeaderReapedGroupTerminationRequested
    }
    #[cfg(windows)]
    {
        v2::Cleanup::WindowsProcessExitedJobEmpty
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lifetime_budget_never_refunds_recycled_storage() {
        let key = crate::contract::RunKey {
            lease: theseus_native_protocol::v2::LeaseId::from_bytes([1; 16]),
            operation: theseus_native_protocol::v2::OperationId::from_bytes([2; 16]),
            generation: 1,
        };
        let mut out = OutputState::new(key);
        for _ in 0..4 {
            assert_eq!(
                out.observe(theseus_native_protocol::v2::Channel::Stdout, 4096, true)
                    .unwrap(),
                (out.stdout - 4096, 4096)
            );
        }
        assert_eq!(
            out.observe(theseus_native_protocol::v2::Channel::Stderr, 4096, true)
                .unwrap(),
            (0, 0)
        );
        assert!(out.truncated);
        assert_eq!(out.stdout + out.stderr, 20480);
    }
}

#[cfg(all(test, feature = "test-support"))]
mod pressure_tests {
    use super::*;
    use crate::test_support::{FixtureControl, FixtureMode};
    fn output() -> OutputState {
        let mut output = OutputState::new(RunKey {
            lease: v2::LeaseId::from_bytes([1; 16]),
            operation: v2::OperationId::from_bytes([2; 16]),
            generation: 1,
        });
        output.fixture = Some(FixtureControl::new(FixtureMode::OutputPressure));
        output
    }
    #[test]
    fn pressure_profile_caps_retention_and_never_refunds_lifetime_bytes() {
        let mut state = output();
        for _ in 0..32 {
            assert_eq!(
                state.observe(v2::Channel::Stdout, 4096, true).unwrap().1,
                512
            );
            state.ordinal += 1;
        }
        assert_eq!(state.reserved, 16384);
        assert!(state.truncated);
        assert_eq!(state.stdout, 131072);
        assert_eq!(state.observe(v2::Channel::Stderr, 4096, true).unwrap().1, 0);
        assert_eq!(state.reserved, 16384);
        for _ in 0..100 {
            assert_eq!(state.observe(v2::Channel::Stdout, 1, true).unwrap().1, 0);
        }
    }
    #[test]
    fn pressure_profile_caps_successful_small_publications_independently_of_raw_budget() {
        let mut state = output();
        for _ in 0..128 {
            assert_eq!(state.observe(v2::Channel::Stdout, 1, true).unwrap().1, 1);
            state.ordinal += 1;
        }
        assert_eq!(state.observe(v2::Channel::Stderr, 1, true).unwrap().1, 0);
        assert_eq!(state.reserved, 128);
        assert!(state.truncated);
    }
    #[test]
    fn pressure_first_no_free_discard_publishes_once_and_budget_only_never_fabricates_capture() {
        let mut state = output();
        let control = state.fixture.as_ref().unwrap().clone();
        for _ in 0..4 {
            state.observe(v2::Channel::Stdout, 4096, true).unwrap();
            state.ordinal += 1;
        }
        assert!(!control.snapshot().capture_saturated);
        assert_eq!(state.observe(v2::Channel::Stderr, 7, false).unwrap().1, 0);
        // Model the existing worker hook, which publishes only after observe
        // returns zero retention for the actual nonempty native read.
        assert!(!control.snapshot().capture_saturated);
        control.mark_capture_saturated();
        let first = control.snapshot();
        assert!(first.capture_saturated);
        assert_eq!(first.saturation_wire_bytes, 2048);
        assert_eq!(first.saturation_published_frames, 4);
        state.observe(v2::Channel::Stdout, 4096, true).unwrap();
        state.ordinal += 1;
        state.observe(v2::Channel::Stderr, 1, false).unwrap();
        let later = control.snapshot();
        assert_eq!(later.saturation_wire_bytes, first.saturation_wire_bytes);
        assert_eq!(
            later.saturation_published_frames,
            first.saturation_published_frames
        );
        let mut exhausted = output();
        for _ in 0..32 {
            exhausted.observe(v2::Channel::Stdout, 4096, true).unwrap();
            exhausted.ordinal += 1;
        }
        assert_eq!(
            exhausted
                .observe(v2::Channel::Stderr, 4096, true)
                .unwrap()
                .1,
            0
        );
        assert!(
            !exhausted
                .fixture
                .as_ref()
                .unwrap()
                .snapshot()
                .capture_saturated
        );
    }
    #[test]
    fn ordinary_observe_profile_keeps_full_compact_raw_chunks() {
        let mut state = output();
        state.fixture = Some(FixtureControl::new(FixtureMode::Observe));
        assert_eq!(
            state.observe(v2::Channel::Stdout, 4096, true).unwrap().1,
            4096
        );
    }
}
