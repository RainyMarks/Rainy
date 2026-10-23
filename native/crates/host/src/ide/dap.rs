//! One Debug Adapter Protocol connection (3.x `ide/debug-protocol.ts`): requests matched to responses, events and the
//! adapter's own requests handed to the session, and every waiting request failed when the connection ends.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::sync::{mpsc, oneshot, watch};

use super::framing::{Decoder, encode};
use crate::error::HostError;

static NEXT_PEER: AtomicU64 = AtomicU64::new(1);

/// What a connection hands to its session.
pub enum Incoming {
    Event { event: String, body: Value },
    Request { seq: i64, command: String, arguments: Value },
    /// The connection failed or the adapter closed it.
    Failed(String),
}

type Pending = oneshot::Sender<Result<Value, String>>;

pub struct Peer {
    pub id: u64,
    /// Taken when the connection ends, which ends the writer task.
    outgoing: Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>>,
    pending: Mutex<HashMap<i64, (String, Pending)>>,
    seq: AtomicI64,
    closed: AtomicBool,
    initialized: watch::Sender<u32>,
    max_message: usize,
    timeout: Duration,
}

impl Peer {
    /// Serve a connection: `incoming` receives its events, reverse requests and failure.
    pub fn start(
        mut reader: impl AsyncRead + Unpin + Send + 'static,
        mut writer: impl AsyncWrite + Unpin + Send + 'static,
        max_message: usize,
        timeout: Duration,
        incoming: mpsc::UnboundedSender<(Arc<Peer>, Incoming)>,
    ) -> Arc<Self> {
        let (outgoing, mut queue) = mpsc::unbounded_channel::<Vec<u8>>();
        let (initialized, _) = watch::channel(0);
        let peer = Arc::new(Self { id: NEXT_PEER.fetch_add(1, Ordering::Relaxed), outgoing: Mutex::new(Some(outgoing)), pending: Mutex::default(), seq: AtomicI64::new(0), closed: AtomicBool::new(false), initialized, max_message, timeout });
        let failing = peer.clone();
        let failures = incoming.clone();
        tokio::spawn(async move {
            while let Some(bytes) = queue.recv().await {
                if writer.write_all(&bytes).await.is_err() || writer.flush().await.is_err() {
                    failing.finish("The debug adapter stopped reading.", Some(&failures));
                    break;
                }
            }
        });
        let reading = peer.clone();
        tokio::spawn(async move {
            let mut decoder = Decoder::new(reading.max_message);
            let mut buffer = vec![0u8; 65536];
            let failure = loop {
                match reader.read(&mut buffer).await {
                    Ok(0) => break "The debug adapter closed its output stream.".to_owned(),
                    Err(error) => break error.to_string(),
                    Ok(read) => match decoder.push(&buffer[..read]) {
                        Ok(messages) => {
                            for message in messages {
                                if let Err(error) = reading.receive(message, &incoming) {
                                    reading.finish(&error, Some(&incoming));
                                    return;
                                }
                            }
                        }
                        Err(error) => break error,
                    },
                }
            };
            reading.finish(&failure, Some(&incoming));
        });
        peer
    }

    fn receive(self: &Arc<Self>, message: Value, incoming: &mpsc::UnboundedSender<(Arc<Peer>, Incoming)>) -> Result<(), String> {
        match message["type"].as_str() {
            Some("response") => {
                let seq = message["request_seq"].as_i64().ok_or("A debug response has no request number.")?;
                let Some((command, pending)) = self.pending.lock().unwrap().remove(&seq) else { return Ok(()) };
                let result = if message["command"].as_str() != Some(command.as_str()) {
                    Err("The debug adapter answered another command.".to_owned())
                } else if message["success"] == true {
                    Ok(message.get("body").cloned().unwrap_or(Value::Null))
                } else {
                    Err(message["message"].as_str().map(str::to_owned).unwrap_or_else(|| format!("The debug adapter rejected {command}.")))
                };
                let _ = pending.send(result);
            }
            Some("event") => {
                let event = message["event"].as_str().ok_or("A debug event has no name.")?.to_owned();
                if event == "initialized" {
                    self.initialized.send_modify(|count| *count += 1);
                }
                let _ = incoming.send((self.clone(), Incoming::Event { event, body: message.get("body").cloned().unwrap_or(Value::Null) }));
            }
            Some("request") => {
                let seq = message["seq"].as_i64().ok_or("A debug request has no number.")?;
                let command = message["command"].as_str().ok_or("A debug request has no command.")?.to_owned();
                let _ = incoming.send((self.clone(), Incoming::Request { seq, command, arguments: message.get("arguments").cloned().unwrap_or(Value::Null) }));
            }
            _ => return Err("The debug adapter sent an unknown message.".into()),
        }
        Ok(())
    }

    fn send(&self, message: &Value) -> Result<(), String> {
        let bytes = encode(message);
        if bytes.len() > self.max_message {
            return Err("A debug request exceeds the configured message size limit.".into());
        }
        let outgoing = self.outgoing.lock().unwrap();
        outgoing.as_ref().ok_or_else(|| "The debug adapter connection is closed.".to_owned())?.send(bytes).map_err(|_| "The debug adapter connection is closed.".to_owned())
    }

    /// Send one request and wait for its response body.
    pub async fn request(&self, command: &str, arguments: Value) -> Result<Value, HostError> {
        let error = |message: String| HostError::new("execution-error", message);
        if self.closed.load(Ordering::Relaxed) {
            return Err(error("The debug adapter connection is closed.".into()));
        }
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().unwrap().insert(seq, (command.to_owned(), sender));
        self.send(&json!({ "seq": seq, "type": "request", "command": command, "arguments": arguments })).map_err(error)?;
        match tokio::time::timeout(self.timeout, receiver).await {
            Ok(Ok(result)) => result.map_err(error),
            Ok(Err(_)) => Err(error("The debug adapter connection is closed.".into())),
            Err(_) => {
                self.pending.lock().unwrap().remove(&seq);
                Err(error(format!("The debug adapter did not answer {command} before its deadline.")))
            }
        }
    }

    /// Answer a request the adapter sent.
    pub fn respond(&self, request_seq: i64, command: &str, result: Result<Value, String>) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let message = match result {
            Ok(body) => json!({ "seq": seq, "type": "response", "request_seq": request_seq, "command": command, "success": true, "body": body }),
            Err(message) => json!({ "seq": seq, "type": "response", "request_seq": request_seq, "command": command, "success": false, "message": message }),
        };
        let _ = self.send(&message);
    }

    /// How many `initialized` events this connection has seen.
    pub fn initializations(&self) -> u32 {
        *self.initialized.borrow()
    }

    /// Wait for an `initialized` event beyond the `after`-th one.
    pub async fn initialized_after(&self, after: u32) -> Result<(), HostError> {
        let mut receiver = self.initialized.subscribe();
        loop {
            if *receiver.borrow() > after {
                return Ok(());
            }
            if self.closed.load(Ordering::Relaxed) || receiver.changed().await.is_err() {
                return Err(HostError::new("execution-error", "The debug adapter connection is closed."));
            }
        }
    }

    /// End the connection: every waiting request fails; `notify` tells the session.
    pub fn finish(self: &Arc<Self>, reason: &str, notify: Option<&mpsc::UnboundedSender<(Arc<Peer>, Incoming)>>) {
        if self.closed.swap(true, Ordering::Relaxed) {
            return;
        }
        self.outgoing.lock().unwrap().take();
        for (_, (_, pending)) in self.pending.lock().unwrap().drain() {
            let _ = pending.send(Err(reason.to_owned()));
        }
        // Waiters for `initialized` see the closed flag on their next wake-up.
        self.initialized.send_modify(|_| {});
        if let Some(notify) = notify {
            let _ = notify.send((self.clone(), Incoming::Failed(reason.to_owned())));
        }
    }

    pub fn close(self: &Arc<Self>) {
        self.finish("The debug adapter connection was closed.", None);
    }
}
