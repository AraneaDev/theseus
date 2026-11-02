//! Checked native byte-pipe profiles, name lengths and pipe disjointness.

pub(super) fn valid_name_reply(
    status: i32,
    completion: i32,
    returned: usize,
    name_bytes: u32,
    capacity: usize,
) -> bool {
    status >= 0
        && status != 259 // STATUS_PENDING is not a completed query.
        && completion >= 0
        && completion != 259
        && returned >= 4
        && returned <= 4 + capacity
        && name_bytes.is_multiple_of(2)
        && name_bytes as usize <= capacity
        && returned == 4 + name_bytes as usize
}

pub(super) fn byte_stream_type(native_type: u32) -> bool {
    // Native FILE_PIPE_MESSAGE_TYPE is 1; FILE_PIPE_REJECT_REMOTE_CLIENTS is 2.
    // Accept byte streams with either remote-client policy, never message or
    // unknown type bits. Locality is independently checked by the caller.
    // https://github.com/microsoft/windows-rs/blob/32c3144490c016fe496a0aed769bce60987a2e9d/crates/libs/sys/src/Windows/Wdk/Storage/FileSystem/mod.rs
    matches!(native_type, 0 | 2)
}

pub(super) fn disjoint_unnamed(
    input_name_bytes: u32,
    output_name_bytes: u32,
    input_configuration: u32,
    output_configuration: u32,
) -> bool {
    // NamedPipeConfiguration describes the entire pipe's direction relative
    // to its server, unlike NamedPipeEnd or the access rights of one handle.
    // One pipe cannot have two distinct configurations: FILE_PIPE_INBOUND (0),
    // FILE_PIPE_OUTBOUND (1), or FILE_PIPE_FULL_DUPLEX (2).
    // https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_pipe_local_information
    (input_name_bytes == 0 || output_name_bytes == 0)
        && input_configuration <= 2
        && output_configuration <= 2
        && input_configuration != output_configuration
}

pub(super) fn disjoint_internal_ids(
    input_configuration: u32,
    output_configuration: u32,
    input: u64,
    output: u64,
) -> bool {
    input_configuration <= 2
        && output_configuration <= 2
        && input != 0
        && output != 0
        && input != u64::MAX
        && output != u64::MAX
        && input != output
}

pub(super) fn local_npfs(
    device: u32,
    characteristics: u32,
    name_bytes: u32,
    name: &[u16; 4],
) -> bool {
    device == 17 // FILE_DEVICE_NAMED_PIPE
        && characteristics & (0x10 | 0x80000) == 0 // FILE_REMOTE_DEVICE / VSMB
        && name_bytes == 8
        && name == &[b'N' as u16, b'P' as u16, b'F' as u16, b'S' as u16]
}

pub(super) fn complete_identity_reply(
    status: i32,
    completion: i32,
    returned: usize,
    expected: usize,
) -> bool {
    status == 0 && completion == 0 && returned == expected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_ids_require_known_profiles_and_distinct_supported_identity() {
        assert!(disjoint_internal_ids(2, 2, 0x388, 0x389));
        for (input, output) in [(0x3a7, 0x3a7), (0, 2), (2, 0), (u64::MAX, 2), (2, u64::MAX)] {
            assert!(!disjoint_internal_ids(2, 2, input, output));
        }
        assert!(!disjoint_internal_ids(3, 2, 1, 2));
        assert!(!disjoint_internal_ids(2, u32::MAX, 1, 2));
    }

    #[test]
    fn internal_identity_domain_requires_exact_local_npfs_provider() {
        let name = [78, 80, 70, 83];
        assert!(local_npfs(17, 0x20000, 8, &name));
        for (device, flags, bytes, name) in [
            (7, 0, 8, name),
            (17, 0x10, 8, name),
            (17, 0x80000, 8, name),
            (17, 0, 6, name),
            (17, 0, 10, name),
            (17, 0, 8, [78, 84, 70, 83]),
        ] {
            assert!(!local_npfs(device, flags, bytes, &name));
        }
    }

    #[test]
    fn identity_requires_exact_successful_completion_not_pending_or_short() {
        for size in [8, 20] {
            assert!(complete_identity_reply(0, 0, size, size));
            for (status, completion, returned) in [
                (-1, 0, size),
                (0, -1, size),
                (259, 0, size),
                (0, 259, size),
                (1, 0, size),
                (0, 0, size - 1),
                (0, 0, size + 1),
            ] {
                assert!(!complete_identity_reply(status, completion, returned, size));
            }
        }
    }

    // Break caught: a successful zero-name FILE_NAME_INFORMATION reply is
    // structurally rejected before the separate native identity proof.
    #[test]
    fn complete_empty_name_reply_is_valid_without_claiming_identity() {
        assert!(valid_name_reply(0, 0, 4, 0, 4092));
        assert!(!disjoint_unnamed(0, 0, 0, 0));
    }

    // Break caught: restoring the nonempty-name-only profile rejects ordinary
    // Rust pipes even when immutable pipe configurations prove disjointness.
    #[test]
    fn opposite_known_one_way_configurations_prove_disjoint_unnamed_pipes() {
        assert!(disjoint_unnamed(0, 0, 0, 1));
        assert!(disjoint_unnamed(0, 0, 1, 0));
    }

    #[test]
    fn empty_names_never_establish_identity_without_exact_configurations() {
        for (input, output) in [
            (0, 0),
            (1, 1),
            (2, 2),
            (0, 3),
            (3, 1),
            (u32::MAX, 0),
            (1, u32::MAX),
        ] {
            assert!(!disjoint_unnamed(0, 0, input, output));
        }
        for (input, output) in [(2, 2), (4, 6)] {
            assert!(!disjoint_unnamed(input, output, 0, 1));
            assert!(!disjoint_unnamed(input, output, 1, 0));
        }
    }

    #[test]
    fn byte_stream_accepts_only_known_type_and_remote_rejection_flag() {
        assert!(byte_stream_type(0));
        assert!(byte_stream_type(2));
        for native_type in [1, 3, 4, 6, 8, u32::MAX] {
            assert!(!byte_stream_type(native_type));
        }
    }

    #[test]
    fn differing_known_configurations_prove_mixed_name_pipes_disjoint() {
        for (input, output) in [(0, 1), (1, 0), (0, 2), (2, 0), (1, 2), (2, 1)] {
            for (input_name, output_name) in [(0, 0), (64, 0), (0, 64)] {
                assert!(disjoint_unnamed(input_name, output_name, input, output));
            }
        }
        for (input, output) in [(0, 0), (1, 1), (2, 2), (0, 3), (3, 2), (u32::MAX, 0)] {
            for (input_name, output_name) in [(0, 0), (64, 0), (0, 64)] {
                assert!(!disjoint_unnamed(input_name, output_name, input, output));
            }
        }
    }

    #[test]
    fn name_reply_rejects_failure_pending_truncation_and_invalid_lengths() {
        assert!(valid_name_reply(0, 0, 8, 4, 4092));
        for (status, completion, returned, length) in [
            (-1, 0, 4, 0),
            (0, -1, 4, 0),
            (259, 0, 4, 0),
            (0, 259, 4, 0),
            (0, 0, 3, 0),
            (0, 0, 5, 0),
            (0, 0, 4, 2),
            (0, 0, 6, 0),
            (0, 0, 7, 3),
            (0, 0, 4098, 4094),
        ] {
            assert!(!valid_name_reply(
                status, completion, returned, length, 4092
            ));
        }
    }
}
