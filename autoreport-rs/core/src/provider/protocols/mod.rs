//! Pluggable SSE protocol modules — one per wire protocol.
//!
//! See [`crate::provider::sse_protocol`] for the shared driver and trait. Each
//! module here implements [`SseProtocol`] for one provider's event format:
//!
//! - `openai_chat`: OpenAI Chat Completions streaming.
//! - `openai_responses`: OpenAI Responses API streaming (mirrors codex).
//! - `anthropic`: Anthropic Messages streaming.

pub(crate) mod anthropic;
pub(crate) mod openai_chat;
pub(crate) mod openai_responses;

pub(crate) use anthropic::AnthropicProtocol;
pub(crate) use openai_chat::OpenAIChatProtocol;
pub(crate) use openai_responses::OpenAIResponsesProtocol;
