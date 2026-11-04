//! Dedicated runtime thread.
//!
//! Go keeps one `goja.Runtime` per provider instance (`JavaScriptSender.vm`)
//! and reuses it for every message. `boa_engine::Context` is `!Send`, so the
//! same lifetime is achieved with a thread that owns the context and receives
//! jobs over a channel: top-level script state and side effects run once, at
//! `Init`, exactly like Go.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::Duration;

use tokio::runtime::Handle;
use tokio::sync::oneshot;

use super::engine::{Invocation, Runtime};
use crate::error::{NotifyError, Result};
use crate::http::HttpTransport;

/// Messages accepted by the runtime thread.
enum Command {
    Invoke {
        invocation: Invocation,
        timeout: Duration,
        reply: oneshot::Sender<Result<()>>,
    },
}

/// Handle to the runtime thread that owns the `boa_engine` context.
pub(super) struct Worker {
    commands: Mutex<Sender<Command>>,
}

impl Worker {
    /// Start the thread and evaluate the script (`Init`).
    ///
    /// The returned worker is only usable when the ready result is `Ok`.
    pub(super) fn spawn(
        script: String,
        transport: Arc<dyn HttpTransport>,
        handle: Handle,
    ) -> (Self, oneshot::Receiver<Result<()>>) {
        let (commands, receiver) = channel();
        let (ready, ready_receiver) = oneshot::channel();

        let spawned = thread::Builder::new()
            .name("komari-js".to_owned())
            .spawn(move || worker_main(receiver, ready, script, transport, handle));

        if let Err(error) = spawned {
            let (ready, ready_receiver) = oneshot::channel();
            let _ = ready.send(Err(NotifyError::Send(format!(
                "failed to start JavaScript runtime: {error}"
            ))));
            return (
                Self {
                    commands: Mutex::new(commands),
                },
                ready_receiver,
            );
        }

        (
            Self {
                commands: Mutex::new(commands),
            },
            ready_receiver,
        )
    }

    /// Queue one invocation and return the reply channel.
    pub(super) fn invoke(
        &self,
        invocation: Invocation,
        timeout: Duration,
    ) -> Result<oneshot::Receiver<Result<()>>> {
        let (reply, receiver) = oneshot::channel();
        let sender = self
            .commands
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        sender
            .send(Command::Invoke {
                invocation,
                timeout,
                reply,
            })
            .map_err(|_| NotifyError::Send("JavaScript runtime is not available".to_owned()))?;
        Ok(receiver)
    }
}

fn worker_main(
    commands: Receiver<Command>,
    ready: oneshot::Sender<Result<()>>,
    script: String,
    transport: Arc<dyn HttpTransport>,
    handle: Handle,
) {
    let mut runtime = match Runtime::new(handle, transport) {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    if let Err(error) = runtime.load(&script) {
        let _ = ready.send(Err(error));
        return;
    }
    if ready.send(Ok(())).is_err() {
        // The sender was dropped (init cancelled); nothing to do.
        return;
    }

    while let Ok(command) = commands.recv() {
        let Command::Invoke {
            invocation,
            timeout,
            reply,
        } = command;
        // A panic must not leave a half-dead worker behind: drop the reply so
        // the caller poisons the slot and creates a fresh runtime.
        match catch_unwind(AssertUnwindSafe(|| runtime.invoke(invocation, timeout))) {
            Ok(result) => {
                let _ = reply.send(result);
            }
            Err(_) => {
                tracing::error!(
                    target: "komari_notify::javascript",
                    "JavaScript runtime panicked; the next call will start a new runtime"
                );
                break;
            }
        }
    }
}
