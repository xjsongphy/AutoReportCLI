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
