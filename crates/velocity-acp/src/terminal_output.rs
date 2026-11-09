//! How much terminal output to keep under the agent's `outputByteLimit` (ACP `terminal/create`).
//!
//! ACP: when the output passes the limit, the client truncates it from the beginning, at a
//! character boundary. `proofs/AcpClient/Design.lean` (`retainedStart`) proves that this keeps
//! at most the limit, starts on a character, and drops the fewest bytes that allows.

/// UTF-8 continuation bytes, `10xxxxxx`: no character starts on one.
fn is_continuation(byte: u8) -> bool {
    if byte >= 128 { byte < 192 } else { false }
}

/// The first byte of `output` to keep: all of it when it fits in `limit`, otherwise the
/// first character boundary from which the rest fits.
pub fn retained_output_start(output: &[u8], limit: usize) -> usize {
    if output.len() <= limit {
        0
    } else {
        let mut start = output.len() - limit;
        while start < output.len() && is_continuation(output[start]) {
            start += 1;
        }
        start
    }
}
