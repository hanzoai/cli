//! The wire, and the one shipped way to ride it.
//!
//! [`Transport`] is the whole surface a request travels over: one request in,
//! one `(status, body)` out. The status is HANDED BACK, never flattened — the
//! [`crate::Client`] above it decides what a 202 and a 401 mean, and a stub in a
//! test decides nothing at all. Only a request that never reached a server is an
//! `Err` here.

use std::future::Future;
use std::time::Duration;

use serde_json::Value;

use crate::error::Error;
use crate::retry;

pub use reqwest::Method;

/// One call: where, how, as whom, with what.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: Method,
    pub url: String,
    /// The bearer. The platform derives the tenant from the token it verifies,
    /// so a request naming no org acts in the caller's own.
    pub token: Option<String>,
    /// The org to act in, when the caller selected one. It rides as `X-Org-Id`
    /// and is a SELECTION, not an assertion: the gateway checks it against the
    /// IAM-signed `orgs` membership claim and discards a value outside that set.
    pub org: Option<String>,
    pub body: Option<Value>,
}

impl Request {
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Request { method, url: url.into(), token: None, org: None, body: None }
    }

    /// An empty bearer is no bearer: a local host with no IAM in front of it is
    /// called anonymously, and the server decides.
    pub fn token(mut self, token: &str) -> Self {
        self.token = (!token.is_empty()).then(|| token.to_string());
        self
    }

    pub fn org(mut self, org: &str) -> Self {
        self.org = (!org.is_empty()).then(|| org.to_string());
        self
    }

    pub fn body(mut self, body: Value) -> Self {
        self.body = Some(body);
        self
    }

    /// Whether the request may be made again after a failure that left it possibly processed: any but a
    /// POST or PATCH, and a POST or PATCH that names itself with a `requestId`, which the platform answers once.
    pub(crate) fn repeats(&self) -> bool {
        let named = self.body.as_ref().and_then(|b| b.get("requestId")).and_then(Value::as_str);
        !matches!(self.method, Method::POST | Method::PATCH) || named.is_some_and(|id| !id.is_empty())
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

    /// One try: the reply and the wait its `Retry-After` named, or the fault and whether it passes.
    async fn once(&self, request: &Request) -> Result<(Reply, Option<Duration>), (Error, bool)> {
        let mut call = self.client.request(request.method.clone(), &request.url);
        if let Some(token) = &request.token {
            call = call.bearer_auth(token);
        }
        if let Some(org) = &request.org {
            call = call.header("X-Org-Id", org);
        }
        if let Some(body) = &request.body {
            call = call.json(body);
        }
        let fault = |e: reqwest::Error| {
            (Error::Wire { url: request.url.clone(), cause: e.to_string() }, retry::passes(&e))
        };
        let answer = call.send().await.map_err(fault)?;
        let status = answer.status().as_u16();
        let named = answer
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(retry::wait_of);
        let text = answer.text().await.map_err(fault)?;
        // An error page is still the caller's to show, so text that is not JSON
        // rides back as a JSON string rather than becoming a parse failure.
        let body = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        Ok((Reply { status, body }, named))
    }
}

impl Transport for Http {
    /// The request is made again (see [`retry`]) while the server is busy or failing in a way that passes.
    async fn send(&self, request: Request) -> Result<Reply, Error> {
        let mut attempt = 1;
        loop {
            let tried = self.once(&request).await;
            let (again, named) = match &tried {
                Ok((reply, named)) => (retry::status(reply.status, request.repeats()), *named),
                Err((_, passes)) => (*passes && request.repeats(), None),
            };
            if !again || attempt == retry::TRIES || named.is_some_and(|wait| wait > retry::HOLD) {
                return tried.map(|(reply, _)| reply).map_err(|(error, _)| error);
            }
            tokio::time::sleep(named.unwrap_or_else(|| retry::backoff(attempt))).await;
            attempt += 1;
        }
    }
}
