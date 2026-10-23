//! The one error every Host operation reports: a stable code, a message in the interface language, and for IDE failures
//! the authoritative observation the page needs (`RpcErrorBody` of `src/shared/rpc.ts` in 3.x).

use std::fmt;
use std::io;

use serde::Serialize;
use serde_json::{Map, Value};

/// A failed request as the page receives it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HostError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

pub type HostResult<T> = Result<T, HostError>;

impl HostError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into(), data: None }
    }

    /// A failure with no more specific code.
    pub fn other(message: impl Into<String>) -> Self {
        Self::new("error", message)
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid-request", message)
    }

    /// Attach one field of the failure details, such as `currentVersion`.
    pub fn with(mut self, name: &str, value: Value) -> Self {
        let data = self.data.get_or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(fields) = data {
            fields.insert(name.to_owned(), value);
        }
        self
    }

    /// An I/O failure under the IDE's stable codes (3.x `ideFailure`).
    pub fn io(error: &io::Error) -> Self {
        let code = match error.kind() {
            io::ErrorKind::NotFound => "not-found",
            io::ErrorKind::PermissionDenied => "permission-denied",
            io::ErrorKind::AlreadyExists | io::ErrorKind::DirectoryNotEmpty => "already-exists",
            io::ErrorKind::NotADirectory => "not-directory",
            io::ErrorKind::IsADirectory => "not-file",
            io::ErrorKind::Interrupted => "aborted",
            _ => "io-error",
        };
        Self::new(code, error.to_string())
    }
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HostError {}

impl From<io::Error> for HostError {
    fn from(error: io::Error) -> Self {
        Self::io(&error)
    }
}

impl From<anyhow::Error> for HostError {
    fn from(error: anyhow::Error) -> Self {
        match error.downcast::<HostError>() {
            Ok(host) => host,
            Err(error) => Self::other(format!("{error:#}")),
        }
    }
}

impl From<serde_json::Error> for HostError {
    fn from(error: serde_json::Error) -> Self {
        Self::invalid(error.to_string())
    }
}

/// Return early with a coded failure.
#[macro_export]
macro_rules! fail {
    ($code:expr, $($message:tt)+) => {
        return Err($crate::error::HostError::new($code, format!($($message)+)))
    };
}
