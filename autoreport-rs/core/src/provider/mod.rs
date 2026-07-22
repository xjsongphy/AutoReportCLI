//! LLM provider abstraction.
//!
//! Direct HTTP calls to Anthropic and OpenAI-compatible endpoints — no SDK,
//! no abstraction framework, matching AutoReport's `core/providers/`.

pub mod anthropic;
pub mod factory;
pub mod openai;
pub mod openai_responses;
pub(crate) mod protocols;
pub mod retry;
pub(crate) mod sse;
pub(crate) mod sse_protocol;
pub mod trait_def;
pub mod types;

pub use factory::build_provider;
pub use trait_def::LLMProvider;
