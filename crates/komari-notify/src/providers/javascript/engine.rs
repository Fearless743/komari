//! `boa_engine` runtime hosting one user script invocation.
//!
//! Each call creates a fresh [`Context`], injects the Go-compatible globals,
//! evaluates the script and then drives promises/microtasks until the entry
//! point settles or the wall-clock deadline expires. The engine is
//! single-threaded, so it lives inside [`tokio::task::spawn_blocking`] and
//! communicates with the async HTTP transport through [`super::net`].

use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use boa_engine::builtins::promise::PromiseState;
use boa_engine::object::builtins::{JsArray, JsFunction, JsPromise};
use boa_engine::property::{Attribute, PropertyKey};
use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsValue, NativeFunction, Source, js_string,
};
use komari_db::models::EventMessage;
use tokio::runtime::Handle;

use super::net::{NetEvent, NetState, Timer};
use crate::error::{NotifyError, Result};
use crate::http::{HttpRequest, HttpResponse, HttpTransport};

/// Globals injected before the user script (see `prelude.js`).
const PRELUDE: &str = include_str!("prelude.js");

/// Hard cap on loop iterations per stack frame.
///
/// Boa has no interrupt handler, so without this an accidental `while (true)`
/// would keep a runtime thread busy until the process exits. The cap only
/// applies to one stack frame and is far above anything a notification script
/// needs.
const LOOP_ITERATION_LIMIT: u64 = 1_000_000;

/// Per-request limit, matching the Go provider's `http.Client{Timeout: 30s}`.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Longest delay accepted by `setTimeout` (10 minutes).
const MAX_TIMER_DELAY_MS: i64 = 600_000;

/// How long the driver sleeps before re-checking promises and timers.
const IDLE_POLL: Duration = Duration::from_millis(2);

thread_local! {
    /// Network state for the script currently running on this thread.
    static ACTIVE: RefCell<Option<NetState>> = const { RefCell::new(None) };
}

/// What the caller wants the script to do.
pub(super) enum Invocation {
    /// Call `sendMessage(message, title)`.
    Text { message: String, title: String },
    /// Call `sendEvent(event)`, falling back to `sendMessage`.
    Event(Box<EventMessage>),
}

/// Clears the thread-local network state when the runtime thread exits.
struct StateGuard;

impl Drop for StateGuard {
    fn drop(&mut self) {
        let _ = ACTIVE.try_with(|cell| *cell.borrow_mut() = None);
    }
}

/// One `boa_engine` context owned by a [`super::worker::Worker`] thread.
///
/// The context is created and the script loaded once; every invocation reuses
/// the same realm, so top-level state persists like Go's `JavaScriptSender.vm`.
pub(super) struct Runtime {
    context: Context,
    deadline: Instant,
    _guard: StateGuard,
}

impl Runtime {
    /// Create the context, inject globals and install the network bridge.
    pub(super) fn new(handle: Handle, transport: Arc<dyn HttpTransport>) -> Result<Self> {
        ACTIVE.with(|cell| {
            // During `Init` there is no invocation deadline yet; give top-level
            // network calls the same budget as one request.
            *cell.borrow_mut() = Some(NetState::new(
                handle,
                transport,
                REQUEST_TIMEOUT,
                Instant::now() + REQUEST_TIMEOUT,
            ));
        });
        let guard = StateGuard;

        let mut context = Context::default();
        context
            .runtime_limits_mut()
            .set_loop_iteration_limit(LOOP_ITERATION_LIMIT);

        let installed = install_globals(&mut context);
        if let Err(error) = installed {
            return Err(setup_error(&error, &mut context));
        }
        let prelude = context.eval(Source::from_bytes(PRELUDE));
        if let Err(error) = prelude {
            return Err(setup_error(&error, &mut context));
        }

        Ok(Self {
            context,
            deadline: Instant::now(),
            _guard: guard,
        })
    }

    /// Run one invocation with a fresh wall-clock deadline.
    pub(super) fn invoke(&mut self, invocation: Invocation, timeout: Duration) -> Result<()> {
        self.deadline = Instant::now() + timeout;
        ACTIVE.with(|cell| {
            if let Some(state) = cell.borrow_mut().as_mut() {
                state.begin_job(self.deadline);
            }
        });

        match invocation {
            Invocation::Text { message, title } => self.send_text(&message, &title),
            Invocation::Event(event) => self.send_event(&event),
        }
    }

    /// Evaluate the script and verify `sendMessage` (`Init`).
    pub(super) fn load(&mut self, script: &str) -> Result<()> {
        if script.is_empty() {
            return Err(NotifyError::InvalidConfig(
                "JavaScript script is empty".to_owned(),
            ));
        }
        let result = self.context.eval(Source::from_bytes(script));
        if let Err(error) = result {
            let summary = summarize(&error, &mut self.context);
            return Err(NotifyError::InvalidConfig(format!(
                "failed to load JavaScript script: {summary}"
            )));
        }
        self.send_message().map(|_| ())
    }

    fn send_message(&mut self) -> Result<JsFunction> {
        self.lookup_callable("sendMessage")
    }

    /// Resolve a global and require it to be callable.
    fn lookup_callable(&mut self, name: &str) -> Result<JsFunction> {
        let global = self.context.global_object().clone();
        let value = match global.get(js_string!(name), &mut self.context) {
            Ok(value) => value,
            Err(error) => return Err(runtime_error(&error, &mut self.context)),
        };
        // Go: a missing global is undefined ("not defined"), while `null` is a
        // present value that fails the callable assertion.
        if value.is_undefined() {
            return Err(NotifyError::InvalidConfig(format!(
                "{name} function not defined in script"
            )));
        }
        match value
            .as_object()
            .and_then(|object| JsFunction::from_object(object.clone()))
        {
            Some(function) => Ok(function),
            None => Err(NotifyError::InvalidConfig(format!(
                "{name} is not a function"
            ))),
        }
    }

    /// Resolve an optional global; undefined or non-callable yields `None`.
    fn lookup_optional(&mut self, name: &str) -> Result<Option<JsFunction>> {
        let global = self.context.global_object().clone();
        let value = match global.get(js_string!(name), &mut self.context) {
            Ok(value) => value,
            Err(error) => return Err(runtime_error(&error, &mut self.context)),
        };
        if value.is_undefined() || value.is_null() {
            return Ok(None);
        }
        Ok(value
            .as_object()
            .and_then(|object| JsFunction::from_object(object.clone())))
    }

    fn send_text(&mut self, message: &str, title: &str) -> Result<()> {
        let function = self.send_message()?;
        let result = function.call(
            &JsValue::undefined(),
            &[
                JsValue::from(js_string!(message)),
                JsValue::from(js_string!(title)),
            ],
            &mut self.context,
        );
        let value = match result {
            Ok(value) => value,
            Err(error) => return Err(entry_error("sendMessage", &error, &mut self.context)),
        };
        if self.settle(value, "sendMessage")? {
            Ok(())
        } else {
            Err(NotifyError::Send("sendMessage returned false".to_owned()))
        }
    }

    fn send_event(&mut self, event: &EventMessage) -> Result<()> {
        let Some(function) = self.lookup_optional("sendEvent")? else {
            return self.send_fallback(event);
        };

        let json = super::JavaScriptSender::event_json(event);
        let argument = match JsValue::from_json(&json, &mut self.context) {
            Ok(value) => value,
            Err(error) => return Err(runtime_error(&error, &mut self.context)),
        };
        let result = function.call(&JsValue::undefined(), &[argument], &mut self.context);
        let value = match result {
            Ok(value) => value,
            Err(error) => return Err(entry_error("sendEvent", &error, &mut self.context)),
        };
        if self.settle(value, "sendEvent")? {
            Ok(())
        } else {
            Err(NotifyError::Send("sendEvent returned false".to_owned()))
        }
    }

    /// Go's `fallbackToTextMessage`.
    fn send_fallback(&mut self, event: &EventMessage) -> Result<()> {
        let message = super::JavaScriptSender::fallback_message(event);
        let title = event.event.clone();
        self.send_text(&message, &title)
    }

    /// Interpret the entry point's return value (Promise or boolean).
    fn settle(&mut self, value: JsValue, label: &str) -> Result<bool> {
        let promise = value
            .as_object()
            .and_then(|object| JsPromise::from_object(object.clone()).ok());
        match promise {
            Some(promise) => self.drive(&promise, label),
            None => Ok(value.to_boolean()),
        }
    }

    /// Run microtasks and pending network work until the promise settles.
    fn drive(&mut self, promise: &JsPromise, label: &str) -> Result<bool> {
        loop {
            pump(&mut self.context)?;
            fire_timers(&mut self.context)?;
            self.context.run_jobs();

            match promise.state() {
                PromiseState::Fulfilled(value) => return Ok(value.to_boolean()),
                PromiseState::Rejected(_) => {
                    return Err(NotifyError::Send(format!("Promise rejected by {label}")));
                }
                PromiseState::Pending => {}
            }

            if Instant::now() >= self.deadline {
                return Err(NotifyError::Send("JavaScript execution timeout".to_owned()));
            }
            // Never drop a received event: `recv_timeout` removes it from the
            // channel, so it must be handled before the next `pump` turn.
            if let Some(event) = self.wait() {
                handle_event(&mut self.context, event)?;
            }
        }
    }

    /// Sleep until a network event arrives, a timer is due, or the poll cap.
    fn wait(&self) -> Option<NetEvent> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        let wait = remaining.min(IDLE_POLL);
        if wait.is_zero() {
            return None;
        }
        ACTIVE.with(|cell| {
            let guard = cell.borrow();
            match guard.as_ref() {
                Some(state) => state.rx.recv_timeout(wait).ok(),
                None => {
                    std::thread::sleep(wait);
                    None
                }
            }
        })
    }
}

/// Drain completed network requests and settle their promises.
fn pump(context: &mut Context) -> Result<()> {
    loop {
        let event = ACTIVE.with(|cell| {
            let guard = cell.borrow();
            guard.as_ref().and_then(|state| state.rx.try_recv().ok())
        });
        let Some(event) = event else {
            break;
        };
        handle_event(context, event)?;
    }
    Ok(())
}

/// Settle the promise belonging to one completed request.
fn handle_event(context: &mut Context, event: NetEvent) -> Result<()> {
    let NetEvent::Fetch { id, result } = event;

    let resolvers = ACTIVE.with(|cell| {
        let mut guard = cell.borrow_mut();
        guard.as_mut().and_then(|state| state.pending.remove(&id))
    });
    let Some(resolvers) = resolvers else {
        return Ok(());
    };

    match result {
        Ok(response) => {
            let value = match response_value(context, response) {
                Ok(value) => value,
                Err(error) => return Err(runtime_error(&error, context)),
            };
            if let Err(error) = resolvers
                .resolve
                .call(&JsValue::undefined(), &[value], context)
            {
                return Err(runtime_error(&error, context));
            }
        }
        Err(reason) => {
            // Go rejects with a string; `fetch` adds the `Fetch failed: `
            // prefix in the prelude, while XHR uses it as `statusText`.
            if let Err(error) = resolvers.reject.call(
                &JsValue::undefined(),
                &[JsValue::from(js_string!(reason))],
                context,
            ) {
                return Err(runtime_error(&error, context));
            }
        }
    }
    Ok(())
}

/// Fire every `setTimeout` callback whose deadline has passed.
fn fire_timers(context: &mut Context) -> Result<()> {
    let now = Instant::now();
    let due = ACTIVE.with(|cell| {
        let mut guard = cell.borrow_mut();
        match guard.as_mut() {
            Some(state) => {
                let mut due = Vec::new();
                state.timers.retain(|timer| {
                    if timer.deadline <= now {
                        due.push(timer.callback.clone());
                        false
                    } else {
                        true
                    }
                });
                due
            }
            None => Vec::new(),
        }
    });

    for callback in due {
        if let Err(error) = callback.call(&JsValue::undefined(), &[], context) {
            return Err(runtime_error(&error, context));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Injected globals
// ---------------------------------------------------------------------------

fn install_globals(context: &mut Context) -> JsResult<()> {
    // Go's setupGlobals only defines console.log and console.error.
    let console = JsObject::with_object_proto(context.intrinsics());
    for (name, function) in [
        ("log", NativeFunction::from_fn_ptr(native_console_log)),
        ("error", NativeFunction::from_fn_ptr(native_console_error)),
    ] {
        console.set(
            js_string!(name),
            function.to_js_function(context.realm()),
            false,
            context,
        )?;
    }
    context.register_global_property(js_string!("console"), console, Attribute::all())?;

    context.register_global_callable(
        js_string!("__komari_http"),
        1,
        NativeFunction::from_fn_ptr(native_http),
    )?;
    context.register_global_callable(
        js_string!("__komari_http_sync"),
        1,
        NativeFunction::from_fn_ptr(native_http_sync),
    )?;
    context.register_global_callable(
        js_string!("setTimeout"),
        2,
        NativeFunction::from_fn_ptr(native_set_timeout),
    )?;

    Ok(())
}

fn console_text(args: &[JsValue], context: &mut Context) -> String {
    args.iter()
        .map(|argument| {
            argument
                .to_string(context)
                .map(|text| text.to_std_string_escaped())
                .unwrap_or_else(|_| "<unprintable>".to_owned())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn native_console_log(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    tracing::info!(target: "komari_notify::javascript", "{}", console_text(args, context));
    Ok(JsValue::undefined())
}

fn native_console_error(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    tracing::error!(target: "komari_notify::javascript", "{}", console_text(args, context));
    Ok(JsValue::undefined())
}

/// `__komari_http(request) -> Promise<response>`, used by `fetch`/XHR.
fn native_http(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let request = parse_request(args.first(), context)?;
    let (promise, resolvers) = JsPromise::new_pending(context);

    ACTIVE.with(|cell| -> JsResult<()> {
        let mut guard = cell.borrow_mut();
        let state = guard.as_mut().ok_or_else(unavailable)?;
        let id = state.allocate();
        state.pending.insert(id, resolvers);
        state.submit(id, request);
        Ok(())
    })?;

    Ok(promise.into())
}

/// `__komari_http_sync(request) -> response`, used by synchronous XHR.
fn native_http_sync(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let request = parse_request(args.first(), context)?;

    let result = ACTIVE.with(|cell| -> JsResult<super::net::NetResult> {
        let guard = cell.borrow();
        let state = guard.as_ref().ok_or_else(unavailable)?;
        Ok(state.submit_blocking(request, REQUEST_TIMEOUT))
    })?;

    match result {
        Ok(response) => response_value(context, response),
        // Go's synchronous XHR path reports the transport error as a string.
        Err(reason) => Err(JsError::from_opaque(JsValue::from(js_string!(reason)))),
    }
}

fn native_set_timeout(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let callback = args
        .first()
        .and_then(JsValue::as_object)
        .and_then(|object| JsFunction::from_object(object.clone()))
        .ok_or_else(|| {
            JsNativeError::typ().with_message("setTimeout requires a callback function")
        })?;

    let delay = match args.get(1) {
        Some(value) => {
            let delay = value.to_number(context)?;
            if delay.is_finite() {
                delay.clamp(0.0, MAX_TIMER_DELAY_MS as f64) as u64
            } else {
                0
            }
        }
        None => 0,
    };

    ACTIVE.with(|cell| -> JsResult<()> {
        let mut guard = cell.borrow_mut();
        let state = guard.as_mut().ok_or_else(unavailable)?;
        state.timers.push(Timer {
            deadline: Instant::now() + Duration::from_millis(delay),
            callback,
        });
        Ok(())
    })?;

    // Go's setTimeout returns undefined.
    Ok(JsValue::undefined())
}

fn unavailable() -> JsError {
    JsNativeError::error()
        .with_message("JavaScript network APIs are only available while a script runs")
        .into()
}

// ---------------------------------------------------------------------------
// Value conversion
// ---------------------------------------------------------------------------

/// Convert the JS request description into an [`HttpRequest`].
fn parse_request(value: Option<&JsValue>, context: &mut Context) -> JsResult<HttpRequest> {
    let Some(object) = value.and_then(JsValue::as_object) else {
        return Err(JsNativeError::typ()
            .with_message("request object required")
            .into());
    };

    let url = property_string(object, "url", context)?.unwrap_or_default();
    let method = property_string(object, "method", context)?.unwrap_or_else(|| "GET".to_owned());
    let mut request = HttpRequest::new(method, url);

    let headers_value = object.get(js_string!("headers"), context)?;
    if let Some(headers) = headers_value.as_object() {
        for key in headers.own_property_keys(context)? {
            let PropertyKey::String(name) = key else {
                continue;
            };
            let value = headers.get(name.clone(), context)?;
            if value.is_undefined() || value.is_null() {
                continue;
            }
            let value = value.to_string(context)?.to_std_string_escaped();
            request = request.header(name.to_std_string_escaped(), value);
        }
    }

    let body_value = object.get(js_string!("body"), context)?;
    if !body_value.is_undefined() && !body_value.is_null() {
        let body = body_value.to_string(context)?.to_std_string_escaped();
        request = request.body(body);
    }

    Ok(request)
}

fn property_string(
    object: &JsObject,
    name: &str,
    context: &mut Context,
) -> JsResult<Option<String>> {
    let value = object.get(js_string!(name), context)?;
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    Ok(Some(value.to_string(context)?.to_std_string_escaped()))
}

/// Response object handed to the prelude (status/statusText/ok/headers/body).
fn response_value(context: &mut Context, response: HttpResponse) -> JsResult<JsValue> {
    let object = JsObject::with_object_proto(context.intrinsics());
    object.set(js_string!("status"), response.status as i32, false, context)?;

    object.set(
        js_string!("statusText"),
        js_string!(response.status_line()),
        false,
        context,
    )?;
    object.set(
        js_string!("ok"),
        (200..300).contains(&response.status),
        false,
        context,
    )?;

    let headers = JsArray::new(context);
    for (name, value) in &response.headers {
        let pair = JsArray::new(context);
        pair.push(js_string!(name.as_str()), context)?;
        pair.push(js_string!(value.as_str()), context)?;
        headers.push(pair, context)?;
    }
    object.set(js_string!("headers"), headers, false, context)?;
    object.set(
        js_string!("body"),
        js_string!(response.body_text()),
        false,
        context,
    )?;

    Ok(object.into())
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Summarize an engine error without its message.
///
/// The message is user-controlled and typically interpolates script literals
/// (tokens, URLs, passwords), so only the error class survives.
fn summarize(error: &JsError, context: &mut Context) -> String {
    match error.try_native(context) {
        Ok(native) => native.kind.to_string(),
        Err(_) => "Error".to_owned(),
    }
}

/// Error raised while calling an entry point directly.
fn entry_error(label: &str, error: &JsError, context: &mut Context) -> NotifyError {
    NotifyError::Send(format!(
        "JavaScript error in {label}: {}",
        summarize(error, context)
    ))
}

fn runtime_error(error: &JsError, context: &mut Context) -> NotifyError {
    NotifyError::Send(format!("JavaScript error: {}", summarize(error, context)))
}

fn setup_error(error: &JsError, context: &mut Context) -> NotifyError {
    NotifyError::Send(format!(
        "JavaScript runtime setup failed: {}",
        summarize(error, context)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_keeps_only_the_error_class() {
        let mut context = Context::default();
        let error = context
            .eval(Source::from_bytes("let x = 1;\nnull.foo"))
            .unwrap_err();
        assert_eq!(summarize(&error, &mut context), "TypeError");
    }

    #[test]
    fn summarize_never_includes_the_error_message() {
        let mut context = Context::default();
        let error = context
            .eval(Source::from_bytes("throw new Error('JS_MESSAGE_SECRET')"))
            .unwrap_err();
        let summary = summarize(&error, &mut context);
        assert!(!summary.contains("JS_MESSAGE_SECRET"), "{summary}");
        assert!(summary.starts_with("Error"), "{summary}");
    }
}
