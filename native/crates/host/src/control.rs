//! The stdio channel to the carrier (3.x `control.ts`).
//!
//! stdout lines `RAINY_CONTROL {json}`: `ready`, `fatal`, `activity`, and the Host's own requests (`credential-key`,
//! `system-proxy`, `preview`). stdin JSON lines: `stop`, `inspect-activity {id, mode?}` and `reply {id, …}` answering a
//! Host request. Nothing else is written to stdout.

use std::collections::HashMap;
use std::io::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde_json::{Map, Value, json};
use tokio::io::AsyncBufReadExt as _;
use tokio::sync::{mpsc, oneshot};

type Reply = Map<String, Value>;

/// Write one control message to stdout.
pub fn send(message: Value) {
    let line = format!("RAINY_CONTROL {message}\n");
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(line.as_bytes());
    let _ = stdout.flush();
}

/// A command from the carrier, other than a reply.
pub type Command = Map<String, Value>;

#[derive(Default)]
struct Inner {
    replies: HashMap<String, oneshot::Sender<Reply>>,
    closed: bool,
}

/// Lines from the carrier: replies settle the Host's requests at any time; commands go to [`ControlInput::commands`].
#[derive(Clone)]
pub struct ControlInput {
    inner: Arc<Mutex<Inner>>,
}

impl ControlInput {
    /// Read stdin until it closes; commands arrive on the returned receiver, which ends when the carrier is gone.
    pub fn start() -> (Self, mpsc::UnboundedReceiver<Command>) {
        let inner = Arc::new(Mutex::new(Inner::default()));
        let (commands, receiver) = mpsc::unbounded_channel();
        let reader = inner.clone();
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(Value::Object(command)) = serde_json::from_str::<Value>(&line) else {
                    log::warn!("Invalid control message");
                    continue;
                };
                if command.get("type").and_then(Value::as_str) == Some("reply") {
                    let Some(id) = command.get("id").and_then(Value::as_str) else { continue };
                    if let Some(pending) = reader.lock().unwrap().replies.remove(id) {
                        let _ = pending.send(command);
                    }
                } else if commands.send(command).is_err() {
                    break;
                }
            }
            let mut inner = reader.lock().unwrap();
            inner.closed = true;
            // Dropping the senders fails every pending request.
            inner.replies.clear();
        });
        (Self { inner }, receiver)
    }

    /// Ask the carrier and wait for its reply.
    pub async fn ask(&self, mut request: Map<String, Value>, timeout: Duration) -> Result<Reply> {
        let kind = request.get("type").and_then(Value::as_str).unwrap_or_default().to_owned();
        let id = uuid::Uuid::new_v4().to_string();
        let (sender, receiver) = oneshot::channel();
        {
            let mut inner = self.inner.lock().unwrap();
            if inner.closed {
                return Err(anyhow!("The carrier closed the control channel"));
            }
            inner.replies.insert(id.clone(), sender);
        }
        request.insert("id".into(), json!(id));
        send(Value::Object(request));
        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) => Err(anyhow!("The carrier closed the control channel")),
            Err(_) => {
                self.inner.lock().unwrap().replies.remove(&id);
                Err(anyhow!("The carrier did not answer {kind}"))
            }
        }
    }
}

/// A request object `{type, …fields}`.
pub fn request(kind: &str, fields: Value) -> Map<String, Value> {
    let mut map = match fields {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    map.insert("type".into(), json!(kind));
    map
}
