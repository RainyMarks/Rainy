//! The carrier: what Electron main did for the Host. It starts the Host for one execution target, answers the Host's
//! control requests (credential key, system proxy, preview), and connects the interface to the Host's `/rpc` socket.

pub mod askpass;
pub mod control;
pub mod credential;
pub mod diagnostics;
pub mod download;
mod http;
#[cfg(windows)]
pub mod instance;
pub mod keyring;
pub mod launch;
pub mod locale;
pub mod logging;
pub mod lsp;
pub mod maintenance;
pub mod mcp_json;
pub mod modules;
pub mod paths;
pub mod proxy;
pub mod rpc;
pub mod shell;
pub mod ssh;
pub mod strata;
pub mod tarball;
pub mod targets;
pub mod text;
pub mod trust;
pub mod unzip;
pub mod update;
pub mod window_state;
pub mod wsl;

#[cfg(test)]
mod test_server;

pub use control::{HostCommand, HostEvents, HostProcess, HostReady, HostStopped};
pub use rpc::{ConnectionState, RpcClient, RpcEvent};
pub use targets::ExecutionTarget;
