//! Request, response and event frames.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A request the interface sends.
#[derive(Debug, Serialize)]
pub struct RequestFrame<'a> {
    pub id: u64,
    pub method: &'a str,
    pub params: &'a Value,
}

/// Error body of a failed request; `data` carries IDE failure details for IDE error codes.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RpcError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RpcError {}

/// A frame the Host sends: the answer to one request, or a broadcast event.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerFrame {
    Response { id: u64, result: Result<Value, RpcError> },
    Event { event: String, data: Value },
}

impl ServerFrame {
    /// Parse one text frame; `None` for anything that is neither a response nor an event.
    pub fn parse(text: &str) -> Option<Self> {
        let mut value: serde_json::Map<String, Value> = serde_json::from_str(text).ok()?;
        if let Some(Value::String(event)) = value.remove("event") {
            return Some(Self::Event { event, data: value.remove("data").unwrap_or(Value::Null) });
        }
        let id = value.get("id")?.as_u64()?;
        if let Some(error) = value.remove("error") {
            let error = serde_json::from_value(error).unwrap_or_else(|_| RpcError {
                code: "invalid".into(),
                message: "The Host returned an invalid error.".into(),
                data: None,
            });
            return Some(Self::Response { id, result: Err(error) });
        }
        Some(Self::Response { id, result: Ok(value.remove("result").unwrap_or(Value::Null)) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_responses_errors_and_events() {
        assert_eq!(
            ServerFrame::parse(r#"{"id":3,"result":{"a":1}}"#),
            Some(ServerFrame::Response { id: 3, result: Ok(json!({"a":1})) })
        );
        assert_eq!(ServerFrame::parse(r#"{"id":4}"#), Some(ServerFrame::Response { id: 4, result: Ok(Value::Null) }));
        let Some(ServerFrame::Response { id: 5, result: Err(error) }) =
            ServerFrame::parse(r#"{"id":5,"error":{"code":"bad","message":"no"}}"#)
        else {
            panic!("error frame")
        };
        assert_eq!(error.code, "bad");
        assert_eq!(
            ServerFrame::parse(r#"{"event":"prefs.changed","data":{"locale":"zh"}}"#),
            Some(ServerFrame::Event { event: "prefs.changed".into(), data: json!({"locale":"zh"}) })
        );
        assert_eq!(ServerFrame::parse("[]"), None);
        assert_eq!(ServerFrame::parse(r#"{"result":1}"#), None);
    }
}
