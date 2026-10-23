//! JSON-RPC over the `/rpc` WebSocket (3.x `rpc.ts`): a method table, and events broadcast to every connected window.
//!
//! Frames: request `{id, method, params}`, response `{id, result}` or `{id, error}`, event `{event, data}`.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use axum::extract::ws::{Message, WebSocket};
use futures::future::BoxFuture;
use futures::{SinkExt as _, StreamExt as _};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::error::{HostError, HostResult};

type Handler = Arc<dyn Fn(Value) -> BoxFuture<'static, HostResult<Value>> + Send + Sync>;

const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

#[derive(Default)]
pub struct RpcHub {
    handlers: RwLock<HashMap<String, Handler>>,
    clients: Mutex<HashMap<u64, mpsc::UnboundedSender<Message>>>,
    next: AtomicU64,
}

impl RpcHub {
    /// Register the handler of one method; its result is serialized as the response.
    pub fn register<F, Fut, T>(&self, method: &str, handler: F)
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HostResult<T>> + Send + 'static,
        T: Serialize,
    {
        let handler: Handler = Arc::new(move |params| {
            let future = handler(params);
            Box::pin(async move { future.await.and_then(|value| serde_json::to_value(value).map_err(|error| HostError::other(error.to_string()))) })
        });
        let previous = self.handlers.write().unwrap().insert(method.to_owned(), handler);
        assert!(previous.is_none(), "RPC method {method} is already registered");
    }

    /// Send an event to every connected window.
    pub fn emit(&self, event: &str, data: impl Serialize) {
        let clients = self.clients.lock().unwrap();
        if clients.is_empty() {
            return;
        }
        let Ok(data) = serde_json::to_value(data) else { return };
        let text = json!({ "event": event, "data": data }).to_string();
        for client in clients.values() {
            let _ = client.send(Message::Text(text.clone().into()));
        }
    }

    /// Run one request; failures are encoded in the response.
    pub async fn dispatch(&self, method: &str, params: Value) -> HostResult<Value> {
        let handler = self.handlers.read().unwrap().get(method).cloned();
        match handler {
            Some(handler) => handler(params).await,
            None => Err(HostError::new("unknown-method", format!("Unknown method {method}"))),
        }
    }

    /// Serve one authenticated window until its socket closes. Requests run concurrently; answers go out as they finish.
    pub async fn serve(self: Arc<Self>, socket: WebSocket) {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (mut sink, mut stream) = socket.split();
        let (outgoing, mut queue) = mpsc::unbounded_channel::<Message>();
        self.clients.lock().unwrap().insert(id, outgoing.clone());
        let writer = tokio::spawn(async move {
            while let Some(message) = queue.recv().await {
                let close = matches!(message, Message::Close(_));
                if sink.send(message).await.is_err() || close {
                    break;
                }
            }
        });
        while let Some(Ok(message)) = stream.next().await {
            let text = match message {
                Message::Text(text) => text,
                Message::Close(_) => break,
                _ => continue,
            };
            if text.len() > MAX_FRAME_BYTES {
                let _ = outgoing.send(close_frame(1009, "frame too large"));
                break;
            }
            let frame: Value = match serde_json::from_str(text.as_str()) {
                Ok(frame) => frame,
                Err(_) => {
                    let _ = outgoing.send(close_frame(1003, "invalid frame"));
                    break;
                }
            };
            let (Some(request), Some(method)) = (frame.get("id").filter(|id| id.is_number()).cloned(), frame.get("method").and_then(Value::as_str)) else {
                let _ = outgoing.send(close_frame(1003, "invalid frame"));
                break;
            };
            let (hub, method, params, reply) = (self.clone(), method.to_owned(), frame.get("params").cloned().unwrap_or(Value::Null), outgoing.clone());
            tokio::spawn(async move {
                let response = match hub.dispatch(&method, params).await {
                    Ok(result) => json!({ "id": request, "result": result }),
                    Err(error) => json!({ "id": request, "error": error }),
                };
                let _ = reply.send(Message::Text(response.to_string().into()));
            });
        }
        self.clients.lock().unwrap().remove(&id);
        drop(outgoing);
        let _ = writer.await;
    }

    /// Close every window connection.
    pub fn close(&self) {
        for client in self.clients.lock().unwrap().drain().map(|(_, client)| client) {
            let _ = client.send(close_frame(1001, "host stopping"));
        }
    }
}

fn close_frame(code: u16, reason: &'static str) -> Message {
    Message::Close(Some(axum::extract::ws::CloseFrame { code, reason: reason.into() }))
}

/// Deserialize request parameters, failing with `invalid-request`.
pub fn params<T: serde::de::DeserializeOwned>(value: Value) -> HostResult<T> {
    serde_json::from_value(value).map_err(|error| HostError::invalid(error.to_string()))
}
