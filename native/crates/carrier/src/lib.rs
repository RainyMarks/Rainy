//! The carrier: what Electron main did for the Host. It starts the Host for one execution target, answers the Host's
//! control requests (credential key, system proxy, preview), and connects the interface to the Host's `/rpc` socket.

pub mod control;
pub mod credential;
pub mod proxy;
pub mod rpc;
pub mod targets;
pub mod wsl;

pub use control::{HostCommand, HostEvents, HostProcess, HostReady};
pub use rpc::{ConnectionState, RpcClient, RpcEvent};
pub use targets::ExecutionTarget;
