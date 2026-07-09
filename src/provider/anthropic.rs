//! Anthropic Messages API provider (native, streaming SSE).

use crate::provider::trait_def::LLMProvider;
use crate::provider::types::{LLMResponse, LLMStreamChunk, Message, ToolCall, ToolDef, Usage};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use futures_util::StreamExt;
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
        format!("{}/v1/messages", self.api_base)
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
            "system" => system_parts.push(msg.content.clone()),
            "user" => out.push(json!({"role": "user", "content": msg.content})),
            "assistant" => {
                let mut blocks = Vec::new();
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
                out.push(json!({"role": "assistant", "content": blocks}));
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

        let resp = self
            .client
            .post(self.endpoint())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("anthropic error {status}: {text}"));
        }
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

        let resp = self
            .client
            .post(self.endpoint())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("anthropic error {status}: {text}"));
        }

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
    let mut tool_calls = Vec::new();
    if let Some(blocks) = v.get("content").and_then(|c| c.as_array()) {
        for b in blocks {
            match b.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                        content.push_str(t);
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
        thinking: None,
        usage,
    }
}

/// Track per-block state while consuming the SSE stream.
struct BlockState {
    text: Option<String>,
    tool: Option<(String, String, String)>, // (id, name, accumulated input json)
}

async fn run_stream(
    resp: reqwest::Response,
    tx: tokio::sync::mpsc::Sender<Result<LLMStreamChunk>>,
) -> Result<()> {
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let mut current: Option<BlockState> = None;
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut usage: Option<Usage> = None;

    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res?;
        buf.push_str(&String::from_utf8_lossy(&chunk));

        // SSE frames separated by blank lines; process complete ones.
        while let Some(idx) = buf.find("\n\n") {
            let frame: String = buf.drain(..idx + 2).collect();
            for line in frame.lines() {
                let Some(payload) = line.strip_prefix("data: ") else {
                    continue;
                };
                if payload.trim() == "[DONE]" {
                    continue;
                }
                let Ok(ev) = serde_json::from_str::<Value>(payload) else {
                    continue;
                };
                let event_type = ev.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match event_type {
                    "content_block_start" => {
                        let block = ev.pointer("/content_block").cloned().unwrap_or(Value::Null);
                        match block.get("type").and_then(|t| t.as_str()) {
                            Some("text") => {
                                current = Some(BlockState {
                                    text: Some(String::new()),
                                    tool: None,
                                })
                            }
                            Some("tool_use") => {
                                let id = block
                                    .get("id")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let name = block
                                    .get("name")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                current = Some(BlockState {
                                    text: None,
                                    tool: Some((id, name, String::new())),
                                });
                            }
                            _ => {}
                        }
                    }
                    "content_block_delta" => {
                        let delta = ev.get("delta").cloned().unwrap_or(Value::Null);
                        match delta.get("type").and_then(|t| t.as_str()) {
                            Some("text_delta") => {
                                if let Some(t) = delta.get("text").and_then(|x| x.as_str()) {
                                    let _ = tx
                                        .send(Ok(LLMStreamChunk {
                                            delta: Some(t.to_string()),
                                            thinking_delta: None,
                                            tool_calls: None,
                                            done: false,
                                            usage: None,
                                        }))
                                        .await;
                                }
                            }
                            Some("input_json_delta") => {
                                if let (Some(s), Some((_, _, acc))) = (
                                    delta.get("partial_json").and_then(|x| x.as_str()),
                                    current.as_mut().and_then(|c| c.tool.as_mut()),
                                ) {
                                    acc.push_str(s);
                                }
                            }
                            Some("thinking_delta") => {
                                if let Some(t) = delta.get("thinking").and_then(|x| x.as_str()) {
                                    let _ = tx
                                        .send(Ok(LLMStreamChunk {
                                            delta: None,
                                            thinking_delta: Some(t.to_string()),
                                            tool_calls: None,
                                            done: false,
                                            usage: None,
                                        }))
                                        .await;
                                }
                            }
                            _ => {}
                        }
                    }
                    "content_block_stop" => {
                        if let Some(state) = current.take() {
                            if let Some((id, name, json_str)) = state.tool {
                                let args = if json_str.trim().is_empty() {
                                    Value::Object(Default::default())
                                } else {
                                    serde_json::from_str(&json_str).unwrap_or(Value::Null)
                                };
                                tool_calls.push(ToolCall {
                                    id,
                                    name,
                                    arguments: args,
                                });
                            }
                        }
                    }
                    "message_delta" => {
                        if let Some(u) = ev.pointer("/usage") {
                            usage = Some(Usage {
                                input_tokens: u
                                    .get("input_tokens")
                                    .and_then(|x| x.as_u64())
                                    .unwrap_or(0),
                                output_tokens: u
                                    .get("output_tokens")
                                    .and_then(|x| x.as_u64())
                                    .unwrap_or(0),
                            });
                        }
                    }
                    "message_stop" => {}
                    "error" => {
                        let msg = ev
                            .pointer("/error/message")
                            .and_then(|x| x.as_str())
                            .unwrap_or("anthropic stream error")
                            .to_string();
                        let _ = tx.send(Err(anyhow!(msg))).await;
                    }
                    _ => {}
                }
            }
        }
    }

    let _ = tx
        .send(Ok(LLMStreamChunk {
            delta: None,
            thinking_delta: None,
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            done: true,
            usage,
        }))
        .await;
    Ok(())
}
