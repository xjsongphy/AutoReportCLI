//! Native OpenAI Responses API provider.
//!
//! This is deliberately separate from `openai.rs`: that module implements the
//! older Chat Completions-compatible protocol used by DeepSeek/OpenRouter and
//! custom gateways. Responses has different input items and SSE event names.

use crate::provider::trait_def::LLMProvider;
use crate::provider::types::{LLMResponse, LLMStreamChunk, Message, ToolCall, ToolDef, Usage};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use bytes::Bytes;
use serde_json::{Value, json};

pub struct OpenAIResponsesProvider {
    client: reqwest::Client,
    api_key: String,
    api_base: String,
    model: String,
    id: String,
}

impl OpenAIResponsesProvider {
    pub fn new(api_key: String, api_base: Option<String>, model: String) -> Self {
        let api_base = api_base
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string())
            .trim_end_matches('/')
            .to_string();
        let model = if model.is_empty() {
            "gpt-4o".to_string()
        } else {
            model
        };
        Self {
            client: reqwest::Client::new(),
            api_key,
            api_base,
            id: format!("openai-responses/{model}"),
            model,
        }
    }

    fn endpoint(&self) -> String {
        format!("{}/responses", self.api_base)
    }

    async fn send_with_retry(&self, body: &Value) -> Result<reqwest::Response> {
        let endpoint = self.endpoint();
        let client = self.client.clone();
        let api_key = self.api_key.clone();
        let body = body.clone();
        crate::provider::retry::post_with_retry(
            move || {
                let client = client.clone();
                let endpoint = endpoint.clone();
                let api_key = api_key.clone();
                let body = body.clone();
                async move {
                    client
                        .post(&endpoint)
                        .bearer_auth(&api_key)
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

fn messages_to_request(messages: &[Message]) -> (String, Vec<Value>) {
    let instructions = messages
        .iter()
        .filter(|message| message.role == "system")
        .map(|message| message.content.as_str())
        .filter(|content| !content.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut items = Vec::new();
    for message in messages {
        match message.role.as_str() {
            "system" => {}
            "developer" => {
                if !message.content.is_empty() {
                    items.push(json!({
                        "type": "message",
                        "role": "developer",
                        "content": [{
                            "type": "input_text",
                            "text": message.content,
                        }],
                    }));
                }
            }
            "user" => items.push(json!({
                "type": "message",
                "role": message.role,
                "content": [{
                    "type": "input_text",
                    "text": message.content,
                }],
            })),
            "assistant" => {
                if message
                    .thinking
                    .as_deref()
                    .is_some_and(|thinking| !thinking.is_empty())
                    || message
                        .thinking_signature
                        .as_deref()
                        .is_some_and(|signature| !signature.is_empty())
                {
                    let mut reasoning = json!({
                        "type": "reasoning",
                        "summary": message
                            .thinking
                            .as_deref()
                            .filter(|thinking| !thinking.is_empty())
                            .map(|thinking| json!([{"type": "summary_text", "text": thinking}]))
                            .unwrap_or_else(|| json!([])),
                    });
                    if let Some(encrypted) = message
                        .thinking_signature
                        .as_deref()
                        .filter(|signature| !signature.is_empty())
                    {
                        reasoning["encrypted_content"] = json!(encrypted);
                    }
                    items.push(reasoning);
                }
                if !message.content.is_empty() {
                    items.push(json!({
                        "type": "message",
                        "role": "assistant",
                        // Codex's Responses request conversion preserves
                        // prior assistant output as `output_text`; the
                        // Responses API explicitly accepts this content type
                        // for assistant history.
                        "content": [{"type": "output_text", "text": message.content}],
                    }));
                }
                if let Some(calls) = &message.tool_calls {
                    for call in calls {
                        items.push(json!({
                            "type": "function_call",
                            "call_id": call.id,
                            "name": call.name,
                            "arguments": serde_json::to_string(&call.arguments).unwrap_or_default(),
                        }));
                    }
                }
            }
            "tool" => items.push(json!({
                "type": "function_call_output",
                "call_id": message.tool_call_id,
                "output": message.content,
            })),
            _ => {}
        }
    }
    (instructions, items)
}

fn tools_to_json(tools: &[ToolDef]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                // Codex's ResponsesApiTool serializes this explicitly even
                // for non-strict schemas. Keep the wire shape identical.
                "strict": false,
                "parameters": tool.input_schema,
            })
        })
        .collect()
}

fn build_body(
    messages: &[Message],
    tools: &[ToolDef],
    model: &str,
    _temperature: f32,
    max_tokens: u32,
    stream: bool,
) -> Value {
    let (instructions, input) = messages_to_request(messages);
    let mut body = json!({
        "model": model,
        "input": input,
        // These are the same explicit defaults Codex sends in
        // `ResponsesApiRequest`; keeping them on the wire avoids provider
        // gateways silently applying a different tool policy.
        "tool_choice": "auto",
        "parallel_tool_calls": false,
        // AutoReport resends the full conversation on every turn, matching
        // Codex's non-persisted Responses request path.
        "store": false,
        "max_output_tokens": max_tokens,
        "stream": stream,
        // Codex always asks Responses for the opaque reasoning payload when
        // it uses non-persisted requests.  We do not render that payload in
        // the TUI, but retaining it is required to replay a reasoning turn
        // on the next request without corrupting the provider history.
        "include": ["reasoning.encrypted_content"],
    });
    if !instructions.is_empty() {
        body["instructions"] = json!(instructions);
    }
    if !tools.is_empty() {
        body["tools"] = json!(tools_to_json(tools));
    }
    body
}

fn parse_response(v: &Value) -> LLMResponse {
    let mut content = String::new();
    let mut thinking = String::new();
    let mut thinking_signature = None;
    let mut tool_calls = Vec::new();
    if let Some(items) = v.get("output").and_then(Value::as_array) {
        for item in items {
            match item.get("type").and_then(Value::as_str) {
                Some("message") => {
                    if let Some(parts) = item.get("content").and_then(Value::as_array) {
                        for part in parts {
                            if let Some(text) = part.get("text").and_then(Value::as_str) {
                                content.push_str(text);
                            } else if let Some(refusal) =
                                part.get("refusal").and_then(Value::as_str)
                            {
                                content.push_str(refusal);
                            }
                        }
                    }
                }
                Some("function_call") => tool_calls.push(parse_function_call(item)),
                Some("reasoning") => {
                    if let Some(summary) = item.get("summary").and_then(Value::as_array) {
                        for part in summary {
                            if let Some(text) = part.get("text").and_then(Value::as_str) {
                                thinking.push_str(text);
                            }
                        }
                    }
                    // `encrypted_content` is independent of the optional
                    // summary array. Codex replays it even when the provider
                    // returns an empty/omitted summary, so never gate the
                    // private signature on summary presence.
                    thinking_signature = item
                        .get("encrypted_content")
                        .and_then(Value::as_str)
                        .filter(|signature| !signature.is_empty())
                        .map(ToOwned::to_owned);
                }
                _ => {}
            }
        }
    }
    if content.is_empty() {
        if let Some(text) = v.get("output_text").and_then(Value::as_str) {
            content.push_str(text);
        }
    }
    LLMResponse {
        content: (!content.is_empty()).then_some(content),
        tool_calls,
        thinking: (!thinking.is_empty()).then_some(thinking),
        thinking_signature,
        usage: v.get("usage").map(parse_usage),
    }
}

fn parse_usage(value: &Value) -> Usage {
    Usage {
        input_tokens: value
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: value
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    }
}

fn parse_function_call(item: &Value) -> ToolCall {
    let arguments = item
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or("{}");
    ToolCall {
        id: item
            .get("call_id")
            .and_then(Value::as_str)
            .or_else(|| item.get("id").and_then(Value::as_str))
            .unwrap_or_default()
            .to_string(),
        name: item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        arguments: serde_json::from_str(arguments).unwrap_or_else(|_| json!({})),
    }
}


#[async_trait]
impl LLMProvider for OpenAIResponsesProvider {
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
        let response = self
            .send_with_retry(&build_body(
                messages,
                tools,
                &self.model,
                temperature,
                max_tokens,
                false,
            ))
            .await?;
        Ok(parse_response(&response.json().await?))
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        temperature: f32,
        max_tokens: u32,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk>>> {
        let response = self
            .send_with_retry(&build_body(
                messages,
                tools,
                &self.model,
                temperature,
                max_tokens,
                true,
            ))
            .await?;
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        let id = self.id.clone();
        tokio::spawn(async move {
            if let Err(error) = run_stream(response, tx.clone()).await {
                let _ = tx.send(Err(anyhow!("{id} stream: {error}"))).await;
            }
        });
        Ok(rx)
    }
}

async fn run_stream(
    response: reqwest::Response,
    tx: tokio::sync::mpsc::Sender<Result<LLMStreamChunk>>,
) -> Result<()> {
    run_stream_bytes(response.bytes_stream(), tx).await
}

async fn run_stream_bytes<S, E>(
    stream: S,
    tx: tokio::sync::mpsc::Sender<Result<LLMStreamChunk>>,
) -> Result<()>
where
    S: futures_util::Stream<Item = std::result::Result<Bytes, E>> + Unpin,
    E: std::error::Error + Send + Sync + 'static,
{
    // The Responses API event parser lives in the `openai_responses`
    // protocol module (mirrors codex `process_responses_event`); this
    // provider supplies only the transport. Frame splitting is shared by
    // `drive_stream`.
    crate::provider::sse_protocol::drive_stream(
        stream,
        tx,
        crate::provider::protocols::OpenAIResponsesProtocol::new(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use futures_util::stream;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn collect_stream(body: &str) -> Vec<LLMStreamChunk> {
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let stream = stream::iter([Ok::<Bytes, std::io::Error>(Bytes::copy_from_slice(
            body.as_bytes(),
        ))]);
        run_stream_bytes(stream, tx).await.expect("stream");
        let mut chunks = Vec::new();
        while let Some(chunk) = rx.recv().await {
            chunks.push(chunk.expect("chunk"));
        }
        chunks
    }

    async fn collect_split_stream(body: &str, split_at: usize) -> Vec<LLMStreamChunk> {
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let bytes = body.as_bytes();
        let split_at = split_at.min(bytes.len());
        let stream = stream::iter([
            Ok::<Bytes, std::io::Error>(Bytes::copy_from_slice(&bytes[..split_at])),
            Ok::<Bytes, std::io::Error>(Bytes::copy_from_slice(&bytes[split_at..])),
        ]);
        run_stream_bytes(stream, tx).await.expect("stream");
        let mut chunks = Vec::new();
        while let Some(chunk) = rx.recv().await {
            chunks.push(chunk.expect("chunk"));
        }
        chunks
    }

    #[test]
    fn converts_function_call_output_to_responses_item() {
        let messages = vec![Message::tool_result("call_1", "ok")];
        let items = messages_to_request(&messages).1;
        assert_eq!(items[0]["type"], "function_call_output");
        assert_eq!(items[0]["call_id"], "call_1");
    }

    #[test]
    fn replays_assistant_text_as_responses_output_text() {
        let items = messages_to_request(&[Message::assistant("previous answer")]).1;
        assert_eq!(items[0]["type"], "message");
        assert_eq!(items[0]["role"], "assistant");
        assert_eq!(items[0]["content"][0]["type"], "output_text");
        assert_eq!(items[0]["content"][0]["text"], "previous answer");
    }

    #[test]
    fn replays_reasoning_as_private_responses_item_before_assistant_output() {
        let mut message = Message::assistant("answer");
        message.thinking = Some("private".into());
        message.thinking_signature = Some("encrypted".into());
        let items = messages_to_request(&[message]).1;
        assert_eq!(items[0]["type"], "reasoning");
        assert_eq!(items[0]["summary"][0]["text"], "private");
        assert_eq!(items[0]["encrypted_content"], "encrypted");
        assert_eq!(items[1]["content"][0]["type"], "output_text");
    }

    #[test]
    fn replays_encrypted_reasoning_without_summary() {
        let mut message = Message::assistant("answer");
        message.thinking_signature = Some("opaque-signature".into());
        let items = messages_to_request(&[message]).1;
        assert_eq!(items[0]["type"], "reasoning");
        assert_eq!(items[0]["summary"], json!([]));
        assert_eq!(items[0]["encrypted_content"], "opaque-signature");
    }

    #[test]
    fn moves_system_and_developer_messages_to_responses_instructions() {
        let (instructions, items) = messages_to_request(&[
            Message::system("system prompt"),
            Message::developer("current time"),
            Message::user("hello"),
        ]);
        assert_eq!(instructions, "system prompt");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["role"], "developer");
        assert_eq!(items[1]["role"], "user");
    }

    #[test]
    fn parses_response_output_text_and_usage() {
        let response = json!({
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "Hello from Responses"}]
            }],
            "usage": {"input_tokens": 3, "output_tokens": 4}
        });
        let parsed = parse_response(&response);
        assert_eq!(parsed.content.as_deref(), Some("Hello from Responses"));
        assert_eq!(parsed.usage.as_ref().map(|u| u.input_tokens), Some(3));
        assert_eq!(parsed.usage.as_ref().map(|u| u.output_tokens), Some(4));
    }

    #[test]
    fn preserves_non_streaming_reasoning_as_private_provider_state() {
        let response = json!({
            "output": [{
                "type": "reasoning",
                "summary": [{"type": "summary_text", "text": "private"}],
                "encrypted_content": "opaque-signature"
            }]
        });
        let parsed = parse_response(&response);
        assert_eq!(parsed.thinking.as_deref(), Some("private"));
        assert_eq!(
            parsed.thinking_signature.as_deref(),
            Some("opaque-signature")
        );
    }

    #[test]
    fn preserves_reasoning_signature_when_summary_is_omitted() {
        let response = json!({
            "output": [{
                "type": "reasoning",
                "encrypted_content": "opaque-signature"
            }]
        });
        let parsed = parse_response(&response);
        assert_eq!(parsed.thinking, None);
        assert_eq!(
            parsed.thinking_signature.as_deref(),
            Some("opaque-signature")
        );
    }

    #[test]
    fn preserves_response_refusal_as_visible_body() {
        let response = json!({
            "output": [{
                "type": "message",
                "content": [{"type": "refusal", "refusal": "I can't help with that."}]
            }]
        });
        assert_eq!(
            parse_response(&response).content.as_deref(),
            Some("I can't help with that.")
        );
    }

    #[test]
    fn parses_function_call_id_fallback_and_arguments() {
        let response = json!({
            "output": [{
                "type": "function_call",
                "id": "fc_item_1",
                "name": "exec",
                "arguments": "{\"command\":\"pwd\"}"
            }]
        });
        let parsed = parse_response(&response);
        assert_eq!(parsed.tool_calls[0].id, "fc_item_1");
        assert_eq!(parsed.tool_calls[0].arguments["command"], "pwd");
    }

    #[test]
    fn responses_body_uses_api_max_output_tokens_without_chat_temperature() {
        let body = build_body(&[], &[], "gpt-5", 0.1, 123, true);
        assert_eq!(body["max_output_tokens"], 123);
        assert!(body.get("temperature").is_none());
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(body["parallel_tool_calls"], false);
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert_eq!(body["include"][0], "reasoning.encrypted_content");
    }

    #[test]
    fn responses_tools_match_codex_top_level_function_shape() {
        let tools = vec![ToolDef {
            name: "exec".into(),
            description: "run a command".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"]
            }),
        }];
        let body = build_body(&[], &tools, "gpt-5", 0.1, 123, true);
        let tool = &body["tools"][0];
        assert_eq!(tool["type"], "function");
        assert_eq!(tool["name"], "exec");
        assert_eq!(tool["strict"], false);
        assert_eq!(tool["parameters"]["type"], "object");
        assert!(tool["function"].is_null());
    }

    #[tokio::test]
    async fn emits_done_only_output_text_without_duplicate_output_item() {
        let chunks = collect_stream(
            "data: {\"type\":\"response.output_text.done\",\"item_id\":\"msg_1\",\"text\":\"hello\"}\n\n\
             data: {\"type\":\"response.output_item.done\",\"item\":{\"id\":\"msg_1\",\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"hello\"}]}}\n\n\
             data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}}\n\n",
        )
        .await;
        let deltas = chunks
            .iter()
            .filter_map(|chunk| chunk.delta.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(deltas, vec!["hello"]);
        assert!(chunks.last().is_some_and(|chunk| chunk.done));
    }

    #[tokio::test]
    async fn accepts_responses_sse_frame_split_across_network_chunks() {
        let body = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"m\",\"delta\":\"hello\"}\r\n\r\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\r\n\r\n",
        );
        let chunks = collect_split_stream(body, body.find("delta").unwrap()).await;
        assert_eq!(
            chunks
                .iter()
                .filter_map(|chunk| chunk.delta.as_deref())
                .collect::<Vec<_>>(),
            vec!["hello"]
        );
        assert!(chunks.last().is_some_and(|chunk| chunk.done));
    }

    #[tokio::test]
    async fn emits_done_only_refusal_as_visible_body_once() {
        let chunks = collect_stream(
            "data: {\"type\":\"response.refusal.done\",\"item_id\":\"msg_1\",\"refusal\":\"no\"}\n\n\
             data: {\"type\":\"response.output_item.done\",\"item\":{\"id\":\"msg_1\",\"type\":\"message\",\"content\":[{\"type\":\"refusal\",\"refusal\":\"no\"}]}}\n\n\
             data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
        )
        .await;
        let deltas = chunks
            .iter()
            .filter_map(|chunk| chunk.delta.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(deltas, vec!["no"]);
    }

    #[tokio::test]
    async fn emits_content_part_done_text_without_duplicate_output_item() {
        let chunks = collect_stream(
            "data: {\"type\":\"response.content_part.done\",\"item_id\":\"msg_1\",\"part\":{\"type\":\"output_text\",\"text\":\"hello\"}}\n\n\
             data: {\"type\":\"response.output_item.done\",\"item\":{\"id\":\"msg_1\",\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"hello\"}]}}\n\n\
             data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
        )
        .await;
        let deltas = chunks
            .iter()
            .filter_map(|chunk| chunk.delta.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(deltas, vec!["hello"]);
    }

    #[tokio::test]
    async fn preserves_codex_reasoning_events_as_private_chunks() {
        let chunks = collect_stream(
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"checking\"}\n\n\
             data: {\"type\":\"response.reasoning_summary_text.done\",\"text\":\"checking\"}\n\n\
             data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
        )
        .await;
        assert_eq!(
            chunks
                .iter()
                .filter_map(|chunk| chunk.thinking_delta.as_deref())
                .collect::<Vec<_>>(),
            vec!["checking"]
        );
        assert!(chunks.iter().all(|chunk| chunk.delta.is_none()));
    }

    #[tokio::test]
    async fn preserves_codex_reasoning_encrypted_content_privately() {
        let chunks = collect_stream(
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"checking\"}\n\n\
             data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"reasoning\",\"encrypted_content\":\"sig\"}}\n\n\
             data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
        )
        .await;
        assert_eq!(
            chunks
                .last()
                .and_then(|chunk| chunk.thinking_signature.as_deref()),
            Some("sig")
        );
    }

    #[tokio::test]
    async fn parses_codex_custom_tool_input_deltas() {
        let chunks = collect_stream(
            "data: {\"type\":\"response.output_item.added\",\"item_id\":\"ctc_1\",\"item\":{\"type\":\"custom_tool_call\",\"call_id\":\"call_1\",\"name\":\"apply_patch\",\"input\":\"\"}}\n\n\
             data: {\"type\":\"response.custom_tool_call_input.delta\",\"item_id\":\"ctc_1\",\"call_id\":\"call_1\",\"delta\":\"{\\\"patch\\\":\\\"ok\\\"}\"}\n\n\
             data: {\"type\":\"response.output_item.done\",\"item_id\":\"ctc_1\",\"item\":{\"type\":\"custom_tool_call\",\"call_id\":\"call_1\",\"name\":\"apply_patch\",\"input\":\"{\\\"patch\\\":\\\"ok\\\"}\"}}\n\n\
             data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
        )
        .await;
        let call = chunks
            .last()
            .and_then(|chunk| chunk.tool_calls.as_ref())
            .and_then(|calls| calls.first())
            .expect("custom tool call");
        assert_eq!(call.id, "call_1");
        assert_eq!(call.name, "apply_patch");
        assert_eq!(call.arguments["patch"], "ok");
    }

    #[tokio::test]
    async fn follows_codex_function_call_argument_events() {
        let chunks = collect_stream(
            "data: {\"type\":\"response.output_item.added\",\"item_id\":\"fc_1\",\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"exec\"}}\n\n\
             data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"delta\":\"{\\\"command\\\":\\\"pwd\\\"}\"}\n\n\
             data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"exec\",\"arguments\":\"{\\\"command\\\":\\\"pwd\\\"}\"}\n\n\
             data: {\"type\":\"response.output_item.done\",\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"exec\",\"arguments\":\"{\\\"command\\\":\\\"pwd\\\"}\"}}\n\n\
             data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
        )
        .await;
        let call = chunks
            .last()
            .and_then(|chunk| chunk.tool_calls.as_ref())
            .and_then(|calls| calls.first())
            .expect("function call");
        assert_eq!(call.id, "call_1");
        assert_eq!(call.name, "exec");
        assert_eq!(call.arguments["command"], "pwd");
    }

    #[tokio::test]
    async fn preserves_multiple_function_call_emission_order() {
        let chunks = collect_stream(
            "data: {\"type\":\"response.output_item.added\",\"item_id\":\"z\",\"item\":{\"type\":\"function_call\",\"call_id\":\"z-call\",\"name\":\"first\"}}\n\n\
             data: {\"type\":\"response.output_item.added\",\"item_id\":\"a\",\"item\":{\"type\":\"function_call\",\"call_id\":\"a-call\",\"name\":\"second\"}}\n\n\
             data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"id\":\"z\",\"call_id\":\"z-call\",\"name\":\"first\",\"arguments\":\"{}\"}}\n\n\
             data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"id\":\"a\",\"call_id\":\"a-call\",\"name\":\"second\",\"arguments\":\"{}\"}}\n\n\
             data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
        )
        .await;
        let calls = chunks
            .last()
            .and_then(|chunk| chunk.tool_calls.as_ref())
            .expect("tool calls");
        assert_eq!(
            calls
                .iter()
                .map(|call| call.name.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
    }

    #[tokio::test]
    async fn chat_stream_sends_responses_wire_shape_and_emits_body() {
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
            assert_eq!(body["model"], "gpt-test");
            assert_eq!(body["input"][0]["role"], "user");
            assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
            assert_eq!(body["tool_choice"], "auto");
            assert_eq!(body["include"][0], "reasoning.encrypted_content");
            assert_eq!(body["store"], false);
            assert_eq!(body["stream"], true);

            let payload = concat!(
                "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"delta\":\"hello\"}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}}\n\n",
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

        let provider = OpenAIResponsesProvider::new(
            "test-key".into(),
            Some(format!("http://{address}/v1")),
            "gpt-test".into(),
        );
        let mut chunks = provider
            .chat_stream(&[Message::user("hello")], &[], 0.0, 128)
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
        assert_eq!(body, "hello");
        server.await.expect("server task");
    }
}
