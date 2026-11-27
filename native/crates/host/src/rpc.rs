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
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;

use crate::error::{HostError, HostResult};

type Handler = Arc<dyn Fn(Value) -> BoxFuture<'static, HostResult<Value>> + Send + Sync>;

const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
const CLIENT_QUEUE_CAPACITY: usize = 512;
const MAX_IN_FLIGHT_REQUESTS: usize = 64;

#[derive(Default)]
pub struct RpcHub {
    handlers: RwLock<HashMap<String, Handler>>,
    clients: Mutex<HashMap<u64, mpsc::Sender<Message>>>,
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

    /// Send an event to every connected window. Serialization runs outside the clients lock, and slow/closed
    /// connections cannot cause unbounded memory growth.
    pub fn emit(&self, event: &str, data: impl Serialize) {
        if self.clients.lock().unwrap().is_empty() {
            return;
        }
        let Ok(data) = serde_json::to_value(data) else { return };
        let text = json!({ "event": event, "data": data }).to_string();
        let mut clients = self.clients.lock().unwrap();
        let mut stale = Vec::new();
        for (&id, client) in clients.iter() {
            match client.try_send(Message::Text(text.clone().into())) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Closed(_)) => stale.push(id),
                Err(mpsc::error::TrySendError::Full(_)) => {
                    log::debug!("RPC event {event} dropped for slow client {id}");
                }
            }
        }
        for id in stale {
            clients.remove(&id);
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

    /// Serve one authenticated window until its socket closes. Requests run concurrently up to a bounded limit;
    /// answers go out as they finish, and in-flight tasks are cancelled when the socket disconnects.
    pub async fn serve(self: Arc<Self>, socket: WebSocket) {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (mut sink, mut stream) = socket.split();
        let (outgoing, mut queue) = mpsc::channel::<Message>(CLIENT_QUEUE_CAPACITY);
        self.clients.lock().unwrap().insert(id, outgoing.clone());
        let writer = tokio::spawn(async move {
            while let Some(message) = queue.recv().await {
                let close = matches!(message, Message::Close(_));
                if sink.send(message).await.is_err() || close {
                    break;
                }
            }
        });
        let permits = Arc::new(Semaphore::new(MAX_IN_FLIGHT_REQUESTS));
        let mut in_flight = JoinSet::new();
        while let Some(Ok(message)) = stream.next().await {
            while in_flight.try_join_next().is_some() {}
            let text = match message {
                Message::Text(text) => text,
                Message::Close(_) => break,
                _ => continue,
            };
            if text.len() > MAX_FRAME_BYTES {
                let _ = outgoing.try_send(close_frame(1009, "frame too large"));
                break;
            }
            let frame: Value = match serde_json::from_str(text.as_str()) {
                Ok(frame) => frame,
                Err(_) => {
                    let _ = outgoing.try_send(close_frame(1003, "invalid frame"));
                    break;
                }
            };
            let (Some(request), Some(method)) = (frame.get("id").filter(|id| id.is_number()).cloned(), frame.get("method").and_then(Value::as_str)) else {
                let _ = outgoing.try_send(close_frame(1003, "invalid frame"));
                break;
            };
            let Ok(permit) = permits.clone().acquire_owned().await else { break };
            let (hub, method, params, reply) = (self.clone(), method.to_owned(), frame.get("params").cloned().unwrap_or(Value::Null), outgoing.clone());
            in_flight.spawn(async move {
                let _permit = permit;
                let response = match hub.dispatch(&method, params).await {
                    Ok(result) => json!({ "id": request, "result": result }),
                    Err(error) => json!({ "id": request, "error": error }),
                };
                let _ = reply.send(Message::Text(response.to_string().into())).await;
            });
        }
        in_flight.abort_all();
        self.clients.lock().unwrap().remove(&id);
        drop(outgoing);
        let _ = writer.await;
    }

    /// Close every window connection.
    pub fn close(&self) {
        for client in self.clients.lock().unwrap().drain().map(|(_, client)| client) {
            let _ = client.try_send(close_frame(1001, "host stopping"));
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
