//! The wire, and the one shipped way to ride it.
//!
//! [`Transport`] is the whole surface a request travels over: one request in,
//! one `(status, body)` out. The status is HANDED BACK, never flattened — the
//! [`crate::Client`] above it decides what a 202 and a 401 mean, and a stub in a
//! test decides nothing at all. Only a request that never reached a server is an
//! `Err` here.

use std::future::Future;

use serde_json::Value;

use crate::error::Error;

pub use reqwest::Method;

/// One call: where, how, as whom, with what.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: Method,
    pub url: String,
    /// The bearer AND NOTHING ELSE goes on the wire. The platform derives the
    /// tenant from the token it verifies, so nothing here can name one.
    pub token: Option<String>,
    pub body: Option<Value>,
}

impl Request {
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Request { method, url: url.into(), token: None, body: None }
    }

    /// An empty bearer is no bearer: a local host with no IAM in front of it is
    /// called anonymously, and the server decides.
    pub fn token(mut self, token: &str) -> Self {
        self.token = (!token.is_empty()).then(|| token.to_string());
        self
    }

    pub fn body(mut self, body: Value) -> Self {
        self.body = Some(body);
        self
    }
}

/// What a server said: its status, and its body parsed where it parses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub status: u16,
    pub body: Value,
}

impl Reply {
    /// The body of a 2xx, or [`Error::Api`] carrying the server's own status and
    /// body. The one place a status becomes a refusal.
    pub fn ok(self) -> Result<Value, Error> {
        if (200..300).contains(&self.status) {
            Ok(self.body)
        } else {
            Err(Error::Api { status: self.status, body: self.body })
        }
    }
}

/// Anything that can carry one request and bring back one reply.
pub trait Transport: Send + Sync {
    fn send(&self, request: Request) -> impl Future<Output = Result<Reply, Error>> + Send;
}

/// HTTPS, over `reqwest`. The shipped transport.
#[derive(Debug, Clone, Default)]
pub struct Http {
    client: reqwest::Client,
}

impl Http {
    pub fn new(client: reqwest::Client) -> Self {
        Http { client }
    }
}

impl Transport for Http {
    async fn send(&self, request: Request) -> Result<Reply, Error> {
        let mut call = self.client.request(request.method, &request.url);
        if let Some(token) = &request.token {
            call = call.bearer_auth(token);
        }
        if let Some(body) = &request.body {
            call = call.json(body);
        }
        let fault = |cause: String| Error::Wire { url: request.url.clone(), cause };
        let answer = call.send().await.map_err(|e| fault(e.to_string()))?;
        let status = answer.status().as_u16();
        let text = answer.text().await.map_err(|e| fault(e.to_string()))?;
        // An error page is still the caller's to show, so text that is not JSON
        // rides back as a JSON string rather than becoming a parse failure.
        let body = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        Ok(Reply { status, body })
    }
}
