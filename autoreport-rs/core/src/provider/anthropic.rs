//! Anthropic Messages API provider (native, streaming SSE).

use crate::provider::trait_def::LLMProvider;
use crate::provider::types::{LLMResponse, LLMStreamChunk, Message, ToolCall, ToolDef, Usage};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use bytes::Bytes;
use serde_json::{Value, json};

const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_BASE: &str = "https://api.anthropic.com";
const DEFAULT_MODEL: &str = "claude-sonnet-4-5-20251001";

pub struct AnthropicProvider {
    client: reqwest::Client,
    api_key: String,
    api_base: String,
    model: String,
    id: String,
}

impl AnthropicProvider {
    pub fn new(api_key: String, api_base: Option<String>, model: String) -> Self {
        let model = if model.is_empty() {
            DEFAULT_MODEL.to_string()
        } else {
            model
        };
        let api_base = api_base
            .unwrap_or_else(|| DEFAULT_BASE.to_string())
            .trim_end_matches('/')
            .to_string();
        Self {
            client: reqwest::Client::new(),
            api_key,
            api_base,
            id: format!("anthropic/{}", model),
            model,
        }
    }

    fn endpoint(&self) -> String {
        if self.api_base.ends_with("/v1") {
            format!("{}/messages", self.api_base)
        } else {
            format!("{}/v1/messages", self.api_base)
        }
    }

    /// POST the JSON body with request-level retry on transient failures
    /// (429 / 5xx / connection / timeout), using codex's jittered backoff.
    async fn send_with_retry(&self, body: &Value) -> Result<reqwest::Response> {
        let endpoint = self.endpoint();
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        let body = body.clone();
        crate::provider::retry::post_with_retry(
            move || {
                let client = client.clone();
                let body = body.clone();
                let api_key = api_key.clone();
                let endpoint = endpoint.clone();
                async move {
                    client
                        .post(&endpoint)
                        .header("x-api-key", &api_key)
                        .header("anthropic-version", ANTHROPIC_VERSION)
                        .header("content-type", "application/json")
                        .json(&body)
                        .send()
                        .await
                }
            },
            &self.id,
            crate::provider::retry::DEFAULT_MAX_ATTEMPTS,
            crate::provider::retry::DEFAULT_BASE_DELAY,
        )
        .await
    }
}

/// Convert the internal message list into Anthropic's request shape:
/// `(system_string, messages_array)`. Consecutive tool results are merged into
/// a single `user` turn with multiple `tool_result` blocks.
fn convert_messages(messages: &[Message]) -> (String, Vec<Value>) {
    let mut system_parts = Vec::new();
    let mut out: Vec<Value> = Vec::new();

    for msg in messages {
        match msg.role.as_str() {
            "system" | "developer" => system_parts.push(msg.content.clone()),
            "user" => out.push(json!({"role": "user", "content": msg.content})),
            "assistant" => {
                let mut blocks = Vec::new();
                // Echo back a signed thinking block BEFORE the text/tool_use so
                // extended thinking continues across turns. Only emit when we
                // have a signature — Anthropic rejects unsigned thinking.
                if let Some(text) = &msg.thinking {
                    if let Some(sig) = &msg.thinking_signature {
                        blocks.push(json!({
                            "type": "thinking",
                            "thinking": text,
                            "signature": sig,
                        }));
                    }
                }
                if !msg.content.is_empty() {
                    blocks.push(json!({"type": "text", "text": msg.content}));
                }
                if let Some(calls) = &msg.tool_calls {
                    for c in calls {
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": c.id,
                            "name": c.name,
                            "input": c.arguments,
                        }));
                    }
                }
                // Anthropic rejects an empty assistant content array. A
                // reasoning-only provider turn is kept privately by the
                // runtime and should not produce an invalid wire item.
                if !blocks.is_empty() {
                    out.push(json!({"role": "assistant", "content": blocks}));
                }
            }
            "tool" => {
                // Attach as a tool_result block to the trailing user turn, or
                // start a new one.
                let block = json!({
                    "type": "tool_result",
                    "tool_use_id": msg.tool_call_id,
                    "content": msg.content,
                });
                if let Some(last) = out.last_mut() {
                    if last.get("role").and_then(|r| r.as_str()) == Some("user")
                        && last.get("content").map(|c| c.is_array()).unwrap_or(false)
                    {
                        last["content"].as_array_mut().unwrap().push(block);
                        continue;
                    }
                }
                out.push(json!({"role": "user", "content": [block]}));
            }
            _ => {}
        }
    }
    (system_parts.join("\n\n"), out)
}

fn tools_to_json(tools: &[ToolDef]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.input_schema,
            })
        })
        .collect()
}

fn build_body(
    system: &str,
    messages: &[Value],
    tools: &[Value],
    model: &str,
    temperature: f32,
    max_tokens: u32,
    stream: bool,
) -> Value {
    let mut body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "temperature": temperature,
        "messages": messages,
        "stream": stream,
    });
    if !system.is_empty() {
        body["system"] = json!(system);
    }
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    body
}

#[async_trait]
impl LLMProvider for AnthropicProvider {
    fn id(&self) -> &str {
        &self.id
    }

    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        temperature: f32,
        max_tokens: u32,
    ) -> Result<LLMResponse> {
        let (system, msgs) = convert_messages(messages);
        let tools_j = tools_to_json(tools);
        let body = build_body(
            &system,
            &msgs,
            &tools_j,
            &self.model,
            temperature,
            max_tokens,
            false,
        );

        let resp = self.send_with_retry(&body).await?;
        let v: Value = resp.json().await?;
        Ok(parse_final(&v))
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        temperature: f32,
        max_tokens: u32,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk>>> {
        let (system, msgs) = convert_messages(messages);
        let tools_j = tools_to_json(tools);
        let body = build_body(
            &system,
            &msgs,
            &tools_j,
            &self.model,
            temperature,
            max_tokens,
            true,
        );

        let resp = self.send_with_retry(&body).await?;

        let (tx, rx) = tokio::sync::mpsc::channel(32);
        let client = self.client.clone();
        let id = self.id.clone();
        tokio::spawn(async move {
            let _ = client; // keep client alive
            if let Err(e) = run_stream(resp, tx.clone()).await {
                let _ = tx.send(Err(anyhow!("anthropic stream ({id}): {e}"))).await;
            }
        });
        Ok(rx)
    }
}

fn parse_final(v: &Value) -> LLMResponse {
    let mut content = String::new();
    let mut thinking = String::new();
    let mut thinking_signature: Option<String> = None;
    let mut tool_calls = Vec::new();
    if let Some(blocks) = v.get("content").and_then(|c| c.as_array()) {
        for b in blocks {
            match b.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                        content.push_str(t);
                    }
                }
                Some("thinking") => {
                    if let Some(t) = b.get("thinking").and_then(|t| t.as_str()) {
                        thinking.push_str(t);
                    }
                    if let Some(sig) = b.get("signature").and_then(|s| s.as_str()) {
                        thinking_signature = Some(sig.to_string());
                    }
                }
                Some("tool_use") => {
                    let id = b
                        .get("id")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = b
                        .get("name")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let input = b.get("input").cloned().unwrap_or(Value::Null);
                    tool_calls.push(ToolCall {
                        id,
                        name,
                        arguments: input,
                    });
                }
                _ => {}
            }
        }
    }
    let usage = v.get("usage").map(|u| Usage {
        input_tokens: u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
        output_tokens: u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
    });
    LLMResponse {
        content: if content.is_empty() {
            None
        } else {
            Some(content)
        },
        tool_calls,
        thinking: (!thinking.is_empty()).then_some(thinking),
        thinking_signature,
        usage,
    }
}

async fn run_stream(
    resp: reqwest::Response,
    tx: tokio::sync::mpsc::Sender<Result<LLMStreamChunk>>,
) -> Result<()> {
    run_stream_bytes(resp.bytes_stream(), tx).await
}

async fn run_stream_bytes<S, E>(
    stream: S,
    tx: tokio::sync::mpsc::Sender<Result<LLMStreamChunk>>,
) -> Result<()>
where
    S: futures_util::Stream<Item = std::result::Result<Bytes, E>> + Unpin,
    E: std::error::Error + Send + Sync + 'static,
{
    // The Anthropic Messages event parser lives in the `anthropic` protocol
    // module; this provider supplies only the transport. Frame splitting is
    // shared by `drive_stream` (codex-alignment: framing once, per-protocol
    // parsing isolated).
    crate::provider::sse_protocol::drive_stream(
        stream,
        tx,
        crate::provider::protocols::AnthropicProtocol::new(),
    )
    .await
}

/// Return the next SSE event boundary, accepting both LF and CRLF framing.
/// Providers commonly emit LF, while the SSE specification permits CRLF.
#[cfg(test)]
mod tests {
    use super::{build_body, convert_messages, parse_final};
    use crate::provider::sse::sse_frame_end;
    use crate::provider::trait_def::LLMProvider;
    use crate::provider::types::LLMStreamChunk;
    use bytes::Bytes;
    use futures_util::stream;
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn endpoint_does_not_duplicate_anthropic_v1_prefix() {
        let provider = super::AnthropicProvider::new(
            "key".into(),
            Some("https://api.anthropic.com/v1".into()),
            "claude-sonnet".into(),
        );
        assert_eq!(provider.endpoint(), "https://api.anthropic.com/v1/messages");
    }

    #[test]
    fn parses_text_tool_and_signed_thinking_blocks() {
        let response = json!({
            "content": [
                {"type": "thinking", "thinking": "internal", "signature": "sig"},
                {"type": "text", "text": "answer"},
                {"type": "tool_use", "id": "call_1", "name": "exec", "input": {"command": "pwd"}}
            ]
        });
        let parsed = parse_final(&response);
        assert_eq!(parsed.content.as_deref(), Some("answer"));
        assert_eq!(parsed.thinking.as_deref(), Some("internal"));
        assert_eq!(parsed.thinking_signature.as_deref(), Some("sig"));
        assert_eq!(parsed.tool_calls[0].id, "call_1");
    }

    #[test]
    fn builds_anthropic_message_shape_with_signed_thinking_and_tool_result() {
        use crate::provider::types::{Message, ToolCall};

        let mut assistant = Message::assistant("");
        assistant.thinking = Some("internal".into());
        assistant.thinking_signature = Some("sig".into());
        assistant.tool_calls = Some(vec![ToolCall {
            id: "call_1".into(),
            name: "exec".into(),
            arguments: json!({"command": "pwd"}),
        }]);
        let (system, messages) = convert_messages(&[
            Message::system("instructions"),
            Message::user("run it"),
            assistant,
            Message::tool_result("call_1", "done"),
        ]);
        assert_eq!(system, "instructions");
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"][0]["type"], "thinking");
        assert_eq!(messages[1]["content"][0]["signature"], "sig");
        assert_eq!(messages[1]["content"][1]["type"], "tool_use");
        assert_eq!(messages[2]["content"][0]["type"], "tool_result");
        assert_eq!(messages[2]["content"][0]["tool_use_id"], "call_1");

        let body = build_body(&system, &messages, &[], "claude", 0.1, 256, true);
        assert_eq!(body["max_tokens"], 256);
        assert_eq!(body["stream"], true);
        assert!((body["temperature"].as_f64().unwrap_or_default() - 0.1).abs() < 1e-6);
    }

    #[test]
    fn accepts_lf_and_crlf_sse_boundaries() {
        assert_eq!(sse_frame_end("data: {}\n\nrest"), Some((8, 2)));
        assert_eq!(sse_frame_end("data: {}\r\n\r\nrest"), Some((8, 4)));
    }

    async fn collect_stream(body: &str) -> Vec<LLMStreamChunk> {
        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let stream = stream::iter([Ok::<Bytes, std::io::Error>(Bytes::copy_from_slice(
            body.as_bytes(),
        ))]);
        super::run_stream_bytes(stream, tx).await.expect("stream");
        let mut chunks = Vec::new();
        while let Some(chunk) = rx.recv().await {
            chunks.push(chunk.expect("chunk"));
        }
        chunks
    }

    async fn collect_split_stream(body: &str, split_at: usize) -> Vec<LLMStreamChunk> {
        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let bytes = body.as_bytes();
        let split_at = split_at.min(bytes.len());
        let stream = stream::iter([
            Ok::<Bytes, std::io::Error>(Bytes::copy_from_slice(&bytes[..split_at])),
            Ok::<Bytes, std::io::Error>(Bytes::copy_from_slice(&bytes[split_at..])),
        ]);
        super::run_stream_bytes(stream, tx).await.expect("stream");
        let mut chunks = Vec::new();
        while let Some(chunk) = rx.recv().await {
            chunks.push(chunk.expect("chunk"));
        }
        chunks
    }

    #[tokio::test]
    async fn streams_text_tool_thinking_signature_and_usage() {
        let chunks = collect_stream(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":0}}}\n\n\
             event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n\
             event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"internal\"}}\n\n\
             event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig\"}}\n\n\
             event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
             event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
             event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"answer\"}}\n\n\
             event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n\
             event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tool_1\",\"name\":\"exec\",\"input\":{}}}\n\n\
             event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"command\\\":\\\"pwd\\\"}\"}}\n\n\
             event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":2}\n\n\
             event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":12}}\n\n\
             event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        )
        .await;
        assert!(
            chunks
                .iter()
                .any(|chunk| chunk.delta.as_deref() == Some("answer"))
        );
        assert!(
            chunks
                .iter()
                .any(|chunk| chunk.thinking_delta.as_deref() == Some("internal"))
        );
        assert!(
            chunks
                .iter()
                .any(|chunk| chunk.thinking_signature.as_deref() == Some("sig"))
        );
        let done = chunks.last().expect("terminal chunk");
        let call = done
            .tool_calls
            .as_ref()
            .and_then(|calls| calls.first())
            .expect("tool call");
        assert_eq!(call.id, "tool_1");
        assert_eq!(call.name, "exec");
        assert_eq!(call.arguments["command"], "pwd");
        assert_eq!(done.usage.as_ref().map(|usage| usage.input_tokens), Some(7));
        assert_eq!(
            done.usage.as_ref().map(|usage| usage.output_tokens),
            Some(12)
        );
        assert!(done.done);
    }

    #[tokio::test]
    async fn accepts_anthropic_sse_frame_split_across_network_chunks() {
        let body = concat!(
            "event: message_start\r\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1}}}\r\n\r\n",
            "event: content_block_start\r\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\r\n\r\n",
            "event: content_block_delta\r\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\r\n\r\n",
            "event: message_delta\r\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":2}}\r\n\r\n",
            "event: message_stop\r\ndata: {\"type\":\"message_stop\"}\r\n\r\n",
        );
        let chunks = collect_split_stream(body, body.find("content_block_delta").unwrap()).await;
        assert_eq!(
            chunks
                .iter()
                .filter_map(|chunk| chunk.delta.as_deref())
                .collect::<Vec<_>>(),
            vec!["hello"]
        );
        let done = chunks.last().expect("terminal chunk");
        assert!(done.done);
        assert_eq!(done.usage.as_ref().map(|usage| usage.input_tokens), Some(1));
        assert_eq!(
            done.usage.as_ref().map(|usage| usage.output_tokens),
            Some(2)
        );
    }

    #[tokio::test]
    async fn chat_stream_sends_messages_wire_shape_and_emits_body() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = Vec::new();
            let mut scratch = [0_u8; 4096];
            let header_end = loop {
                let read = socket.read(&mut scratch).await.expect("read request");
                assert!(read > 0, "request closed before headers");
                request.extend_from_slice(&scratch[..read]);
                if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
            assert!(headers.contains("x-api-key: test-key"));
            assert!(headers.contains("anthropic-version: 2023-06-01"));
            let content_length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .expect("content length");
            while request.len() < header_end + content_length {
                let read = socket.read(&mut scratch).await.expect("read body");
                assert!(read > 0, "request closed before body");
                request.extend_from_slice(&scratch[..read]);
            }
            let body: Value =
                serde_json::from_slice(&request[header_end..header_end + content_length])
                    .expect("json request body");
            assert_eq!(body["model"], "claude-test");
            assert_eq!(body["messages"][0]["role"], "user");
            assert_eq!(body["messages"][0]["content"], "hello");
            assert_eq!(body["stream"], true);
            assert_eq!(body["max_tokens"], 128);

            let payload = concat!(
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":2,\"output_tokens\":0}}}\n\n",
                "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"answer\"}}\n\n",
                "event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":1}}\n\n",
                "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        });

        let provider = super::AnthropicProvider::new(
            "test-key".into(),
            Some(format!("http://{address}/v1")),
            "claude-test".into(),
        );
        let mut chunks = provider
            .chat_stream(
                &[crate::provider::types::Message::user("hello")],
                &[],
                0.0,
                128,
            )
            .await
            .expect("chat stream");
        let mut body = String::new();
        while let Some(chunk) = chunks.recv().await {
            let chunk = chunk.expect("stream chunk");
            if let Some(delta) = chunk.delta {
                body.push_str(&delta);
            }
            if chunk.done {
                assert_eq!(
                    chunk.usage.as_ref().map(|usage| usage.output_tokens),
                    Some(1)
                );
                break;
            }
        }
        assert_eq!(body, "answer");
        server.await.expect("server task");
    }
}
