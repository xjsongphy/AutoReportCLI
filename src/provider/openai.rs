//! OpenAI-compatible Chat Completions provider. Works for OpenAI, DeepSeek,
//! OpenRouter, and any custom OpenAI-style endpoint.

use crate::provider::trait_def::LLMProvider;
use crate::provider::types::{LLMResponse, LLMStreamChunk, Message, ToolCall, ToolDef, Usage};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::BTreeMap;

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
            client: reqwest::Client::new(),
            api_key,
            api_base,
            id: format!("{kind}/{model}"),
            model,
        }
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.api_base)
    }
}

fn defaults(kind: &str) -> (&'static str, &'static str) {
    match kind {
        "deepseek" => ("https://api.deepseek.com/v1", "deepseek-chat"),
        "openrouter" => ("https://openrouter.ai/api/v1", "anthropic/claude-sonnet-4.5"),
        "google" => ("https://generativelanguage.googleapis.com/v1beta/openai", "gemini-2.0-flash"),
        "openai" => ("https://api.openai.com/v1", "gpt-4o"),
        _ => ("https://api.openai.com/v1", "gpt-4o"),
    }
}

fn convert_messages(messages: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    for msg in messages {
        match msg.role.as_str() {
            "system" => out.push(json!({"role": "system", "content": msg.content})),
            "user" => out.push(json!({"role": "user", "content": msg.content})),
            "assistant" => {
                let mut m = json!({"role": "assistant", "content": msg.content});
                if let Some(calls) = &msg.tool_calls {
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

        let resp = self
            .client
            .post(self.endpoint())
            .bearer_auth(&self.api_key)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("{} error {status}: {text}", self.id));
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
        let msgs = convert_messages(messages);
        let tools_j = tools_to_json(tools);
        let body = build_body(&msgs, &tools_j, &self.model, temperature, max_tokens, true);

        let resp = self
            .client
            .post(self.endpoint())
            .bearer_auth(&self.api_key)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("{} error {status}: {text}", self.id));
        }

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
    let choice = v.pointer("/choices/0/message").cloned().unwrap_or(Value::Null);
    let content = choice
        .get("content")
        .and_then(|c| c.as_str())
        .map(|s| s.to_string());
    let tool_calls = choice
        .get("tool_calls")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .map(|c| {
                    let id = c.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    let name = c
                        .pointer("/function/name")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let args_str = c
                        .pointer("/function/arguments")
                        .and_then(|x| x.as_str())
                        .unwrap_or("{}");
                    let arguments = serde_json::from_str(args_str).unwrap_or(Value::Null);
                    ToolCall { id, name, arguments }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let usage = v.get("usage").map(|u| Usage {
        input_tokens: u.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
        output_tokens: u.get("completion_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
    });
    LLMResponse {
        content,
        tool_calls,
        thinking: None,
        usage,
    }
}

/// Accumulate tool-call fragments by index while streaming.
#[derive(Default)]
struct ToolAccum {
    id: String,
    name: String,
    args: String,
}

async fn run_stream(
    resp: reqwest::Response,
    tx: tokio::sync::mpsc::Sender<Result<LLMStreamChunk>>,
) -> Result<()> {
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let mut tool_acc: BTreeMap<u64, ToolAccum> = BTreeMap::new();

    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res?;
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(idx) = buf.find("\n\n") {
            let frame: String = buf.drain(..idx + 2).collect();
            for line in frame.lines() {
                let Some(payload) = line.strip_prefix("data: ") else {
                    continue;
                };
                let payload = payload.trim();
                if payload == "[DONE]" {
                    continue;
                }
                let Ok(ev) = serde_json::from_str::<Value>(payload) else {
                    continue;
                };
                if let Some(err) = ev.get("error") {
                    let msg = err
                        .get("message")
                        .and_then(|x| x.as_str())
                        .unwrap_or("stream error")
                        .to_string();
                    let _ = tx.send(Err(anyhow!(msg))).await;
                    continue;
                }
                let delta = match ev.pointer("/choices/0/delta") {
                    Some(d) => d.clone(),
                    None => continue,
                };
                if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                    if !content.is_empty() {
                        let _ = tx
                            .send(Ok(LLMStreamChunk {
                                delta: Some(content.to_string()),
                                thinking_delta: None,
                                tool_calls: None,
                                done: false,
                                usage: None,
                            }))
                            .await;
                    }
                }
                if let Some(reasoning) = delta
                    .get("reasoning_content")
                    .and_then(|c| c.as_str())
                {
                    if !reasoning.is_empty() {
                        let _ = tx
                            .send(Ok(LLMStreamChunk {
                                delta: None,
                                thinking_delta: Some(reasoning.to_string()),
                                tool_calls: None,
                                done: false,
                                usage: None,
                            }))
                            .await;
                    }
                }
                if let Some(calls) = delta.get("tool_calls").and_then(|c| c.as_array()) {
                    for c in calls {
                        let index = c.get("index").and_then(|x| x.as_u64()).unwrap_or(0);
                        let entry = tool_acc.entry(index).or_default();
                        if let Some(id) = c.get("id").and_then(|x| x.as_str()) {
                            entry.id = id.to_string();
                        }
                        if let Some(name) = c.pointer("/function/name").and_then(|x| x.as_str()) {
                            entry.name = name.to_string();
                        }
                        if let Some(args) = c.pointer("/function/arguments").and_then(|x| x.as_str())
                        {
                            entry.args.push_str(args);
                        }
                    }
                }
            }
        }
    }

    let tool_calls: Vec<ToolCall> = tool_acc
        .into_values()
        .map(|a| ToolCall {
            id: a.id,
            name: a.name,
            arguments: if a.args.trim().is_empty() {
                Value::Object(Default::default())
            } else {
                serde_json::from_str(&a.args).unwrap_or(Value::Null)
            },
        })
        .collect();

    let _ = tx
        .send(Ok(LLMStreamChunk {
            delta: None,
            thinking_delta: None,
            tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
            done: true,
            usage: None,
        }))
        .await;
    Ok(())
}
