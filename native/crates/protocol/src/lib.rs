//! Rust side of the Host ↔ interface protocol carried by the `/rpc` WebSocket (`src/shared/rpc.ts`).
//!
//! Frames are JSON objects: a request `{ id, method, params }` from the interface, a response `{ id, result }` or
//! `{ id, error }` from the Host, and an event `{ event, data }` the Host broadcasts. Types follow the TypeScript
//! names in camelCase; fields this interface does not read yet are left out, and serde ignores them.

pub mod chat;
pub mod frames;
pub mod ide;
pub mod models;
pub mod prefs;
pub mod search;
pub mod stream;

pub use chat::*;
pub use frames::*;
pub use ide::*;
pub use models::*;
pub use prefs::*;
pub use search::*;
pub use stream::*;
