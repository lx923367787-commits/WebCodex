#[cfg(test)]
mod utf8_truncation_tests {
    use crate::test_support::*;

    #[test]
    fn runner_stream_truncation_keeps_utf8_boundary() {
        let mut stream = ShellJobStreamSnapshot::default();
        let chunk = format!(
            "█{}",
            "x".repeat(JOB_SNAPSHOT_STREAM_MAX_BYTES - "█".len() + 1)
        );
        assert_eq!(chunk.len(), JOB_SNAPSHOT_STREAM_MAX_BYTES + 1);

        append_runner_stream(&mut stream, Some(&chunk));

        assert!(stream.truncated);
        assert!(stream.tail.len() <= JOB_SNAPSHOT_STREAM_MAX_BYTES);
        assert!(stream.tail.bytes().all(|byte| byte == b'x'));
    }

    #[test]
    fn runner_inventory_trim_keeps_utf8_boundary() {
        let mut stream = ShellJobStreamSnapshot {
            tail: format!("█{}", "x".repeat(64)),
            ..Default::default()
        };
        let max_bytes = stream.tail.len() - 1;

        trim_runner_stream_to(&mut stream, max_bytes);

        assert!(stream.truncated);
        assert!(stream.tail.len() <= max_bytes);
        assert!(stream.tail.bytes().all(|byte| byte == b'x'));
    }
}
