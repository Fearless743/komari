//! Shared fakes for provider tests.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use komari_notify::providers::email::{EmailEnvelope, EmailTransport};
use komari_notify::{HttpRequest, HttpResponse, HttpTransport, NotifyError, Result};

pub struct MockHttp {
    requests: Mutex<Vec<HttpRequest>>,
    responses: Mutex<VecDeque<HttpResponse>>,
}

impl MockHttp {
    pub fn new(responses: Vec<HttpResponse>) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            responses: Mutex::new(responses.into()),
        }
    }

    pub fn with_json(value: &serde_json::Value) -> Self {
        Self::new(vec![HttpResponse::ok_json(value)])
    }

    pub fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }

    pub fn last_request(&self) -> HttpRequest {
        self.requests.lock().unwrap().last().cloned().unwrap()
    }
}

#[async_trait]
impl HttpTransport for MockHttp {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse> {
        self.requests.lock().unwrap().push(request);
        Ok(self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| HttpResponse::new(200, b"{}".to_vec())))
    }
}

pub struct MockEmail {
    envelopes: Mutex<Vec<EmailEnvelope>>,
}

impl MockEmail {
    pub fn new() -> Self {
        Self {
            envelopes: Mutex::new(Vec::new()),
        }
    }

    pub fn envelopes(&self) -> Vec<EmailEnvelope> {
        self.envelopes.lock().unwrap().clone()
    }
}

#[async_trait]
impl EmailTransport for MockEmail {
    async fn send(&self, envelope: EmailEnvelope) -> Result<()> {
        self.envelopes.lock().unwrap().push(envelope);
        Ok(())
    }
}

pub fn unavailable_error() -> NotifyError {
    NotifyError::Send("mock failure".to_owned())
}
