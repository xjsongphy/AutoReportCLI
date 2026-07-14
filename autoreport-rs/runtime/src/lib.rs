//! Agent runtime: the message-bus-driven agent loop and the loop manager that
//! owns one loop per agent type.

pub mod codex_thread;
pub(crate) mod history;
pub mod thread_manager;

pub use codex_thread::AgentLoop;
pub use thread_manager::LoopManager;
