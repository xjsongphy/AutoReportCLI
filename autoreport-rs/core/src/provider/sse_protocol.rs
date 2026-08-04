//! Pluggable SSE-protocol parser.
//!
//! Each provider implements [`SseProtocol`] to turn one SSE `data:` payload
//! into zero or more [`LLMStreamChunk`]s plus protocol-level terminal/error
//! signals. The raw SSE frame splitting (CRLF/LF blank-line boundaries,
//! `data:` prefix stripping, `[DONE]` filtering) is shared by [`drive_stream`]
//! in this module and lives exactly once — mirroring the seam codex creates
//! between `eventsource_stream` (framing) and `process_responses_event`
//! (per-frame parsing).
//!
//! The three protocols are drop-in equals:
//! - `protocols::openai_chat` — OpenAI Chat Completions (no codex precedent;
//!   codex removed the chat wire API).
//! - `protocols::openai_responses` — OpenAI Responses API; mirrors codex's
//!   `codex-api/src/sse/responses.rs` (`ResponsesStreamEvent` +
//!   `process_responses_event`).
//! - `protocols::anthropic` — Anthropic Messages streaming (no codex
//!   precedent).

use crate::provider::sse::sse_frame_end_bytes;
use crate::provider::types::LLMStreamChunk;
use anyhow::Result;
use futures_util::StreamExt;

/// Output emitted by a protocol parser for a single SSE `data:` payload.
pub(crate) enum FrameOutcome {
    /// One or more streaming chunks (token deltas, thinking deltas, …).
    Chunks(Vec<LLMStreamChunk>),
    /// Stream is over; emit the supplied terminal chunk (done:true + usage +
    /// any final tool_calls). The parser already accumulated everything.
    Terminal(LLMStreamChunk),
    /// Parser saw a fatal provider error (Anthropic `error` event, OpenAI
    /// `response.failed`). The stream loop forwards this and stops.
    Error(anyhow::Error),
    /// Frame produced no output (keep-alive, unhandled event, `[DONE]`).
    Ignore,
}

/// Stateful SSE protocol parser. One instance per `chat_stream` invocation.
///
/// `parse_frame` is stateful (`&mut self`) because the protocols accumulate
/// across frames (Chat Completions tool-call index map, Responses per-call_id
/// `FunctionAccum`, Anthropic per-block thinking signature). Final results are
/// flushed through [`SseProtocol::flush`] when the byte stream ends without an
/// explicit terminal frame.
pub(crate) trait SseProtocol: Send {
    /// Feed one SSE `data:` payload (already stripped of the `data:` prefix
    /// and trimmed). `[DONE]` and empty payloads are filtered by `drive_stream`
    /// and never reach here.
    fn parse_frame(&mut self, payload: &str) -> FrameOutcome;

    /// Called when the underlying byte stream ends without an explicit
    /// terminal frame. Lets the parser flush accumulated tool calls / usage
    /// and emit a synthesized terminal chunk, or signal a protocol-level
    /// error (e.g. Responses API stream that closed without
    /// `response.completed`). Default: nothing to flush.
    fn flush(&mut self) -> FrameOutcome {
        FrameOutcome::Ignore
    }
}

/// Shared SSE stream driver. Owns the byte-buffer frame split
/// (`sse_frame_end_bytes`), the `data:` prefix strip, `[DONE]` filtering, and
/// dispatches each real JSON payload to `protocol.parse_frame`. This is the
/// single place the SSE framing lives — the per-protocol logic is isolated in
/// the [`SseProtocol`] impl.
///
/// The buffer holds raw bytes (not a `String`) so a multi-byte UTF-8 codepoint
/// split across TCP reads is reassembled before decoding. Each incoming
/// `bytes::Bytes` chunk is appended verbatim; only a *complete* SSE frame (once
/// the trailing blank line is seen) is drained and decoded with
/// `String::from_utf8_lossy`. Decoding the live buffer tail after every chunk —
/// as a previous `String`-buffered version did — destroyed split codepoints
/// into U+FFFD. This mirrors codex's approach of buffering raw bytes and
/// decoding only complete event payloads.
pub(crate) async fn drive_stream<S, E, P>(
    mut stream: S,
    tx: tokio::sync::mpsc::Sender<Result<LLMStreamChunk>>,
    mut protocol: P,
) -> Result<()>
where
    S: futures_util::Stream<Item = std::result::Result<bytes::Bytes, E>> + Unpin,
    E: std::error::Error + Send + Sync + 'static,
    P: SseProtocol,
{
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res?;
        buf.extend_from_slice(&chunk);
        while let Some((idx, delimiter_len)) = sse_frame_end_bytes(&buf) {
            // Drain the complete frame (including its trailing blank-line
            // delimiter) and decode only those bytes — never the live buffer
            // tail, which may end mid-codepoint.
            let frame_bytes: Vec<u8> = buf.drain(..idx + delimiter_len).collect();
            let frame = String::from_utf8_lossy(&frame_bytes);
            for line in frame.lines() {
                let Some(payload) = line.strip_prefix("data:").map(str::trim_start) else {
                    continue;
                };
                let payload = payload.trim();
                if payload.is_empty() || payload == "[DONE]" {
                    continue;
                }
                match protocol.parse_frame(payload) {
                    FrameOutcome::Chunks(chunks) => {
                        for c in chunks {
                            if tx.send(Ok(c)).await.is_err() {
                                return Ok(());
                            }
                        }
                    }
                    FrameOutcome::Terminal(c) => {
                        let _ = tx.send(Ok(c)).await;
                        return Ok(());
                    }
                    FrameOutcome::Error(e) => {
                        // Stop processing: a trailing synthesized `done:true`
                        // would mask the error from the consumer.
                        let _ = tx.send(Err(e)).await;
                        return Ok(());
                    }
                    FrameOutcome::Ignore => {}
                }
            }
        }
    }
    match protocol.flush() {
        FrameOutcome::Chunks(chunks) => {
            for c in chunks {
                if tx.send(Ok(c)).await.is_err() {
                    return Ok(());
                }
            }
        }
        FrameOutcome::Terminal(c) => {
            let _ = tx.send(Ok(c)).await;
        }
        FrameOutcome::Error(e) => {
            let _ = tx.send(Err(e)).await;
        }
        FrameOutcome::Ignore => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use futures_util::stream;

    /// Test-only protocol that echoes each `data:` payload verbatim as a delta
    /// chunk, and synthesizes a terminal chunk on `flush`. Used to inspect the
    /// exact string `drive_stream` hands to `parse_frame` independent of any
    /// real provider wire format.
    struct CaptureProtocol;
    impl SseProtocol for CaptureProtocol {
        fn parse_frame(&mut self, payload: &str) -> FrameOutcome {
            FrameOutcome::Chunks(vec![LLMStreamChunk {
                delta: Some(payload.to_string()),
                thinking_delta: None,
                thinking_signature: None,
                tool_calls: None,
                done: false,
                usage: None,
            }])
        }
        fn flush(&mut self) -> FrameOutcome {
            FrameOutcome::Terminal(LLMStreamChunk {
                delta: None,
                thinking_delta: None,
                thinking_signature: None,
                tool_calls: None,
                done: true,
                usage: None,
            })
        }
    }

    async fn collect_payloads(chunks: Vec<Bytes>) -> Vec<String> {
        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let stream = stream::iter(
            chunks
                .into_iter()
                .map(|b| Ok::<Bytes, std::io::Error>(b)),
        );
        drive_stream(stream, tx, CaptureProtocol)
            .await
            .expect("drive_stream");
        let mut payloads = Vec::new();
        while let Some(Ok(chunk)) = rx.recv().await {
            if !chunk.done {
                if let Some(delta) = chunk.delta {
                    payloads.push(delta);
                }
            }
        }
        payloads
    }

    /// Regression: a multi-byte UTF-8 codepoint split across two network
    /// chunks must round-trip intact. Previously `drive_stream` decoded each
    /// chunk independently with `String::from_utf8_lossy`, so a codepoint
    /// straddling the boundary (common for CJK/emoji token deltas) was
    /// destroyed into two U+FFFD.
    #[tokio::test]
    async fn preserves_multibyte_utf8_split_across_chunks() {
        // 你 = U+4F60 = UTF-8 e4 bd a0. Frame it inside a `data:` line so the
        // SSE blank-line boundary is unambiguously after the full codepoint.
        let frame = "data: hello\u{4f60}world\n\n";
        let bytes = frame.as_bytes();
        // Split right in the middle of the 3-byte codepoint: chunk1 ends with
        // the lead bytes `e4 bd`, chunk2 begins with the trailing byte `a0`.
        let split_at = frame.find('\u{4f60}').unwrap() + 2; // after `e4 bd`
        assert_eq!(
            &bytes[split_at - 2..split_at],
            &[0xe4, 0xbd],
            "sanity: split lands inside the multibyte codepoint"
        );
        assert_eq!(bytes[split_at], 0xa0, "sanity: trailing byte in chunk2");

        let chunks = vec![
            Bytes::copy_from_slice(&bytes[..split_at]),
            Bytes::copy_from_slice(&bytes[split_at..]),
        ];
        let payloads = collect_payloads(chunks).await;
        assert_eq!(payloads, vec!["hello\u{4f60}world"]);
        // No U+FFFD replacement chars should survive.
        assert!(
            !payloads.iter().any(|p| p.contains('\u{fffd}')),
            "found U+FFFD replacement char: {payloads:?}"
        );
    }
}
