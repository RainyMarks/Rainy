//! The agent side of the Host (3.x `agent/`): chats, models, tools, MCP, skills and project memory.

pub mod budget;
pub mod compaction;
pub mod context;
pub mod llm;
pub mod models;
pub mod store;
pub mod tools;
pub mod instructions;
pub mod permissions;
pub mod prompt;
pub mod skills;
pub mod snapshots;
