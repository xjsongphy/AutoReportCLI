//! Agent runtime: the message-bus-driven agent loop and the loop manager that
//! owns one loop per agent type.

pub mod agent_loop;
pub mod manager;

pub use agent_loop::AgentLoop;
pub use manager::LoopManager;
