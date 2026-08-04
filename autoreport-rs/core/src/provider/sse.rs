//! Minimal Server-Sent Events frame splitter shared by the streaming providers.
//!
//! codex consumes SSE via the `eventsource_stream` crate, which handles the
//! full SSE spec (CRLF/LF framing, multi-line `data:` concatenation, `event:`/
//! `id:` fields). Our providers parse the stream by hand so they can extract
//! provider-specific fields (Anthropic thinking `signature`, OpenAI terminal
//! `usage`, tool-call assembly) inline; rather than restructure that around an
//! async event decoder, we centralize just the frame-boundary detection here so
//! the LF/CRLF tolerance lives in one place. Returns the byte index of the
//! frame boundary and the length of the delimiter consumed.
/// Str-oriented convenience twin retained for the unit tests in
/// `anthropic.rs`; production (`drive_stream`) now buffers raw bytes and uses
/// [`sse_frame_end_bytes`] instead, so this str form is test-only.
#[cfg(test)]
pub(crate) fn sse_frame_end(buf: &str) -> Option<(usize, usize)> {
    // SSE allows either CRLF or LF line endings; a blank line terminates a
    // frame. Prefer the CRLF delimiter so its 4-byte length is reported
    // correctly when present.
    if let Some(idx) = buf.find("\r\n\r\n") {
        Some((idx, 4))
    } else {
        buf.find("\n\n").map(|idx| (idx, 2))
    }
}

/// Byte-oriented twin of [`sse_frame_end`]. Operates on the raw byte buffer so
/// `drive_stream` can accumulate `bytes::Bytes` chunks without decoding them
/// per-chunk (which would corrupt multi-byte UTF-8 split across TCP reads).
///
/// SSE framing is pure ASCII, so a byte-level scan is exact and avoids any
/// dependency on `memchr` — `slice::windows` is sufficient. Returns the byte
/// index of the frame boundary and the length of the delimiter consumed.
pub(crate) fn sse_frame_end_bytes(buf: &[u8]) -> Option<(usize, usize)> {
    // Prefer the CRLF delimiter so its 4-byte length is reported correctly
    // when present — same precedence as `sse_frame_end`.
    if buf.len() >= 4 {
        if let Some(idx) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            return Some((idx, 4));
        }
    }
    if buf.len() >= 2 {
        if let Some(idx) = buf.windows(2).position(|w| w == b"\n\n") {
            return Some((idx, 2));
        }
    }
    None
}
