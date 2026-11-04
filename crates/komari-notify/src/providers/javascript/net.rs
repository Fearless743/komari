//! Network bridge between the blocking script engine and the Tokio runtime.
//!
//! `fetch` and `XMLHttpRequest` must not block the engine thread while a
//! request is in flight, so every request is spawned on the application's
//! Tokio runtime and reported back through a plain [`std::sync::mpsc`]
//! channel. The engine loop drains that channel between microtask turns,
//! which is the Rust equivalent of goja's `runMicrotasks` ticker.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use boa_engine::builtins::promise::ResolvingFunctions;
use boa_engine::object::builtins::JsFunction;
use tokio::runtime::Handle;

use crate::error::NotifyError;
use crate::http::{HttpRequest, HttpResponse, HttpTransport};

/// Result of one bridged request, already stripped of secrets.
pub(super) type NetResult = std::result::Result<HttpResponse, String>;

/// Completion events delivered from Tokio to the engine thread.
pub(super) enum NetEvent {
    Fetch { id: u64, result: NetResult },
}

/// A timer scheduled with `setTimeout`.
pub(super) struct Timer {
    pub id: u64,
    pub deadline: Instant,
    pub callback: JsFunction,
}

/// Per-invocation network state, installed in a thread-local while a script
/// runs. The engine itself is single-threaded, so no locking is required.
pub(super) struct NetState {
    pub handle: Handle,
    pub transport: Arc<dyn HttpTransport>,
    pub tx: Sender<NetEvent>,
    pub rx: Receiver<NetEvent>,
    /// Pending `fetch` promises keyed by request id.
    pub pending: HashMap<u64, ResolvingFunctions>,
    pub timers: Vec<Timer>,
    pub request_timeout: Duration,
    /// Wall-clock deadline of the whole invocation.
    pub deadline: Instant,
    next_id: u64,
}

impl NetState {
    pub fn new(
        handle: Handle,
        transport: Arc<dyn HttpTransport>,
        request_timeout: Duration,
        deadline: Instant,
    ) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Self {
            handle,
            transport,
            tx,
            rx,
            pending: HashMap::new(),
            timers: Vec::new(),
            request_timeout,
            deadline,
            next_id: 0,
        }
    }

    /// Reserve an id for a request or timer.
    pub fn allocate(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Prepare the state for a new invocation.
    ///
    /// Pending promises and timers belong to the previous message: Go's
    /// goroutines outlive a send, but a notification never awaits them, so
    /// dropping the bookkeeping is equivalent and prevents cross-talk.
    pub fn begin_job(&mut self, deadline: Instant) {
        self.deadline = deadline;
        self.pending.clear();
        self.timers.clear();
        while self.rx.try_recv().is_ok() {}
    }

    /// Release per-invocation bookkeeping.
    pub fn end_job(&mut self) {
        self.pending.clear();
        self.timers.clear();
    }

    /// Spawn a request whose completion is reported as a [`NetEvent`].
    pub fn submit(&self, id: u64, request: HttpRequest) {
        let transport = self.transport.clone();
        let timeout = self.request_timeout;
        let tx = self.tx.clone();
        self.handle.spawn(async move {
            let result = perform(transport, timeout, request).await;
            let _ = tx.send(NetEvent::Fetch { id, result });
        });
    }

    /// Run one request to completion on the engine thread (sync XHR).
    pub fn submit_blocking(&self, request: HttpRequest, timeout: Duration) -> NetResult {
        let (tx, rx) = std::sync::mpsc::channel();
        let transport = self.transport.clone();
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        let request_timeout = timeout.min(self.request_timeout).min(remaining);
        if request_timeout.is_zero() {
            return Err("request timed out".to_owned());
        }
        self.handle.spawn(async move {
            let result = perform(transport, request_timeout, request).await;
            let _ = tx.send(result);
        });
        match rx.recv_timeout(request_timeout) {
            Ok(result) => result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err("request timed out".to_owned()),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err("request was cancelled".to_owned())
            }
        }
    }
}

/// Execute a request with a hard per-request timeout.
async fn perform(
    transport: Arc<dyn HttpTransport>,
    timeout: Duration,
    request: HttpRequest,
) -> NetResult {
    match tokio::time::timeout(timeout, transport.execute(request)).await {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(error)) => Err(sanitized_error(&error)),
        Err(_) => Err("request timed out".to_owned()),
    }
}

/// Keep only the transport's already-sanitized classification.
///
/// `NotifyError::Rejected` carries a remote body, which may contain a token,
/// so it is reduced to the status code.
fn sanitized_error(error: &NotifyError) -> String {
    match error {
        NotifyError::Http(message) => format!("request failed: {message}"),
        NotifyError::Rejected { status, .. } => {
            format!("remote endpoint returned status {status}")
        }
        NotifyError::EmptyMessage => "request failed".to_owned(),
        _ => "request failed".to_owned(),
    }
}
