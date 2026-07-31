//! OpenAI-compatible Chat Completions provider. Works for OpenAI, DeepSeek,
//! OpenRouter, and any custom OpenAI-style endpoint.

use crate::provider::trait_def::LLMProvider;
use crate::provider::types::{LLMResponse, LLMStreamChunk, Message, ToolCall, ToolDef, Usage};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};

pub struct OpenAICompatProvider {
    client: reqwest::Client,
    api_key: String,
    api_base: String,
    model: String,
    id: String,
}

impl OpenAICompatProvider {
    pub fn new(api_key: String, api_base: Option<String>, model: String, kind: &str) -> Self {
        let (default_base, default_model) = defaults(kind);
        let api_base = api_base
            .unwrap_or_else(|| default_base.to_string())
            .trim_end_matches('/')
            .to_string();
        let model = if model.is_empty() {
            default_model.to_string()
        } else {
            model
        };
        Self {
            client: crate::user_agent::http_client(),
            api_key,
            api_base,
            id: format!("{kind}/{model}"),
            model,
        }
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.api_base)
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

fn defaults(kind: &str) -> (&'static str, &'static str) {
    match kind {
        "deepseek" => ("https://api.deepseek.com/v1", "deepseek-chat"),
        "openrouter" => (
            "https://openrouter.ai/api/v1",
            "anthropic/claude-sonnet-4.5",
        ),
        "google" => (
            "https://generativelanguage.googleapis.com/v1beta/openai",
            "gemini-2.0-flash",
        ),
        "openai" => ("https://api.openai.com/v1", "gpt-4o"),
        _ => ("https://api.openai.com/v1", "gpt-4o"),
    }
}

fn convert_messages(messages: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    for msg in messages {
        match msg.role.as_str() {
            "system" | "developer" => out.push(json!({"role": "system", "content": msg.content})),
            "user" => out.push(json!({"role": "user", "content": msg.content})),
            "assistant" => {
                let mut m = json!({"role": "assistant", "content": msg.content});
                if let Some(calls) = &msg.tool_calls
                    && !calls.is_empty()
                {
                    // When tool_calls are present, content must be null (not "");
                    // several OpenAI-compatible backends 400 on an empty string
                    // here.
                    let arr: Vec<Value> = calls
                        .iter()
                        .map(|c| {
                            json!({
                                "id": c.id,
                                "type": "function",
                                "function": {
                                    "name": c.name,
                                    "arguments": serde_json::to_string(&c.arguments).unwrap_or_default(),
                                }
                            })
                        })
                        .collect();
                    m["content"] = Value::Null;
                    m["tool_calls"] = Value::Array(arr);
                }
                out.push(m);
            }
            "tool" => out.push(json!({
                "role": "tool",
                "content": msg.content,
                "tool_call_id": msg.tool_call_id,
            })),
            _ => {}
        }
    }
    out
}

fn tools_to_json(tools: &[ToolDef]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                }
            })
        })
        .collect()
}

fn build_body(
    messages: &[Value],
    tools: &[Value],
    model: &str,
    temperature: f32,
    max_tokens: u32,
    stream: bool,
) -> Value {
    let mut body = json!({
        "model": model,
        "messages": messages,
        "temperature": temperature,
        "max_tokens": max_tokens,
        "stream": stream,
    });
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    if stream {
        // Required to receive a usage block in the terminal stream chunk;
        // without it, streaming cost tracking is impossible.
        body["stream_options"] = json!({ "include_usage": true });
    }
    body
}

#[async_trait]
impl LLMProvider for OpenAICompatProvider {
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
        let msgs = convert_messages(messages);
        let tools_j = tools_to_json(tools);
        let body = build_body(&msgs, &tools_j, &self.model, temperature, max_tokens, false);

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
        let msgs = convert_messages(messages);
        let tools_j = tools_to_json(tools);
        let body = build_body(&msgs, &tools_j, &self.model, temperature, max_tokens, true);

        let resp = self.send_with_retry(&body).await?;

        let (tx, rx) = tokio::sync::mpsc::channel(32);
        let id = self.id.clone();
        tokio::spawn(async move {
            if let Err(e) = run_stream(resp, tx.clone()).await {
                let _ = tx.send(Err(anyhow!("{id} stream: {e}"))).await;
            }
        });
        Ok(rx)
    }
}

fn parse_final(v: &Value) -> LLMResponse {
    let choice = v
        .pointer("/choices/0/message")
        .cloned()
        .unwrap_or(Value::Null);
    // OpenAI-compatible gateways normally return a string here, but newer
    // OpenAI/OpenRouter responses may use an array of typed content parts.
    // Normalize both shapes so a valid text answer cannot disappear merely
    // because the gateway selected multipart output.
    let content = extract_text_content(choice.get("content")).or_else(|| {
        choice
            .get("refusal")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    });
    let tool_calls = choice
        .get("tool_calls")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .map(|c| {
                    let id = c
                        .get("id")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = c
                        .pointer("/function/name")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let args_str = c
                        .pointer("/function/arguments")
                        .and_then(|x| x.as_str())
                        .unwrap_or("{}");
                    let arguments = serde_json::from_str(args_str).unwrap_or_else(|_| {
                        // Truncated/invalid tool args → empty object (clean
                        // "missing argument" error) rather than null, which
                        // tools would try to execute against.
                        Value::Object(Default::default())
                    });
                    ToolCall {
                        id,
                        name,
                        arguments,
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let usage = v.get("usage").map(|u| Usage {
        input_tokens: u.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
        output_tokens: u
            .get("completion_tokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(0),
    });
    LLMResponse {
        content,
        tool_calls,
        thinking: None,
        thinking_signature: None,
        usage,
    }
}

fn extract_text_content(value: Option<&Value>) -> Option<String> {
    let value = value?;
    if let Some(text) = value.as_str() {
        return (!text.is_empty()).then_some(text.to_string());
    }
    let parts = value.as_array()?;
    let mut text = String::new();
    for part in parts {
        if let Some(fragment) = part.as_str() {
            text.push_str(fragment);
        } else if let Some(fragment) = part
            .get("text")
            .and_then(Value::as_str)
            .or_else(|| part.get("content").and_then(Value::as_str))
        {
            text.push_str(fragment);
        }
    }
    (!text.is_empty()).then_some(text)
}

async fn run_stream(
    resp: reqwest::Response,
    tx: tokio::sync::mpsc::Sender<Result<LLMStreamChunk>>,
) -> Result<()> {
    // The Chat Completions event parser lives in the `openai_chat` protocol
    // module; this provider only supplies the transport (HTTP body → byte
    // stream) and the protocol instance. Frame splitting is shared by
    // `drive_stream` (codex-alignment: framing once, per-protocol parsing
    // isolated).
    crate::provider::sse_protocol::drive_stream(
        resp.bytes_stream(),
        tx,
        crate::provider::protocols::OpenAIChatProtocol::new(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::{extract_text_content, parse_final};
    use serde_json::json;

    #[test]
    fn parses_multipart_chat_content_without_losing_body() {
        let response = json!({
            "choices": [{
                "message": {
                    "content": [
                        {"type": "text", "text": "first"},
                        {"type": "text", "text": " second"}
                    ]
                }
            }]
        });
        assert_eq!(
            parse_final(&response).content.as_deref(),
            Some("first second")
        );
    }

    #[test]
    fn extracts_string_and_part_content_shapes() {
        assert_eq!(
            extract_text_content(Some(&json!("hello"))).as_deref(),
            Some("hello")
        );
        assert_eq!(
            extract_text_content(Some(&json!([{"content": "world"}]))).as_deref(),
            Some("world")
        );
    }

    #[test]
    fn preserves_chat_refusal_when_no_content_part_exists() {
        let response = json!({
            "choices": [{"message": {"refusal": "I can't help with that."}}]
        });
        assert_eq!(
            parse_final(&response).content.as_deref(),
            Some("I can't help with that.")
        );
    }
}
