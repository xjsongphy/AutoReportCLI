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

use crate::provider::sse::sse_frame_end;
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

/// Shared SSE stream driver. Owns the byte-buffer frame split (`sse_frame_end`),
/// the `data:` prefix strip, `[DONE]` filtering, and dispatches each real JSON
/// payload to `protocol.parse_frame`. This is the single place the SSE framing
/// lives — the per-protocol logic is isolated in the [`SseProtocol`] impl.
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
    let mut buf = String::new();
    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res?;
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some((idx, delimiter_len)) = sse_frame_end(&buf) {
            let frame: String = buf.drain(..idx + delimiter_len).collect();
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
