//! The provider trait shared by all backends.

use crate::provider::types::{LLMResponse, LLMStreamChunk, Message, ToolDef};
use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait LLMProvider: Send + Sync {
    /// Human-readable identifier (kind + model).
    fn id(&self) -> &str;

    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        temperature: f32,
        max_tokens: u32,
    ) -> Result<LLMResponse>;

    /// Stream a completion. Default impl falls back to `chat` (single chunk).
    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        temperature: f32,
        max_tokens: u32,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk>>> {
        let resp = self.chat(messages, tools, temperature, max_tokens).await;
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            match resp {
                Ok(r) => {
                    if let Some(content) = r.content {
                        let _ = tx.try_send(Ok(LLMStreamChunk {
                            delta: Some(content),
                            thinking_delta: None,
                            tool_calls: None,
                            done: false,
                            usage: None,
                        }));
                    }
                    let _ = tx.try_send(Ok(LLMStreamChunk {
                        delta: None,
                        thinking_delta: None,
                        tool_calls: if r.tool_calls.is_empty() {
                            None
                        } else {
                            Some(r.tool_calls)
                        },
                        done: true,
                        usage: r.usage,
                    }));
                }
                Err(e) => {
                    let _ = tx.try_send(Err(e));
                }
            }
        });
        Ok(rx)
    }
}
