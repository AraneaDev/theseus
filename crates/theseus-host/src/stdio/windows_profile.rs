//! Checked native name lengths and conservative unnamed-pipe disjointness.

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

pub(super) fn disjoint_unnamed(
    input_name_bytes: u32,
    output_name_bytes: u32,
    input_configuration: u32,
    output_configuration: u32,
) -> bool {
    // NamedPipeConfiguration describes the entire pipe's direction relative
    // to its server, unlike NamedPipeEnd or the access rights of one handle.
    // One pipe cannot be both FILE_PIPE_INBOUND (0) and FILE_PIPE_OUTBOUND (1).
    // https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_pipe_local_information
    input_name_bytes == 0
        && output_name_bytes == 0
        && matches!((input_configuration, output_configuration), (0, 1) | (1, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

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
            (0, 2),
            (2, 1),
            (1, 2),
            (2, 0),
            (0, 3),
            (3, 1),
            (u32::MAX, 0),
            (1, u32::MAX),
        ] {
            assert!(!disjoint_unnamed(0, 0, input, output));
        }
        for (input, output) in [(2, 0), (0, 2), (2, 2)] {
            assert!(!disjoint_unnamed(input, output, 0, 1));
            assert!(!disjoint_unnamed(input, output, 1, 0));
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
