//! The Rust client for `api.hanzo.ai`.
//!
//! Four rules, and the types enforce all four.
//!
//! **One credential scopes the client.** [`Client::as`] mints a short-lived
//! token bound to ONE end user and hands back a client that can reach nothing
//! else. No method takes a user id, so a caller cannot pass the wrong one and
//! there is nothing to forget.
//!
//! **A held call cannot be read as done.** An approval-gated call answers 202,
//! which is a success status; [`Outcome`] makes that a separate arm carrying the
//! approval, and the two arms share no value.
//!
//! **A refusal is typed.** Non-2xx is [`Error::Api`] with the server's status
//! and body.
//!
//! **Addresses are `/v1`.** Never an `/api/` prefix, never a `v2`.
//!
//! ```no_run
//! # async fn demo() -> Result<(), hanzo_client::Error> {
//! use hanzo_client::{Client, Method, Outcome};
//! use serde_json::{json, Value};
//!
//! let cloud = Client::new(std::env::var("HANZO_KEY").unwrap());
//! let user = cloud.r#as("usr_42"); // every call below is that user's
//!
//! match user.call::<Value>(Method::POST, "/v1/card", Some(json!({"limit": 500}))).await? {
//!     Outcome::Done(card) => println!("issued {card}"),
//!     Outcome::Held(a) => println!("{} held it: {}", a.clause, a.reason),
//! }
//! # Ok(()) }
//! ```

mod error;
mod grant;
mod http;
mod result;

use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::Value;

pub use crate::error::Error;
pub use crate::grant::{Grant, ISSUE, ISSUER};
pub use crate::http::{Http, Method, Reply, Request, Transport};
pub use crate::result::{Approval, Outcome};

/// Where the platform answers.
pub const API: &str = "https://api.hanzo.ai";

/// A credential, an address, and — once scoped — a subject to act as.
pub struct Client<T: Transport = Http> {
    wire: Arc<T>,
    base: String,
    issuer: String,
    /// The operator credential. On a scoped client this is what MINTS; it is
    /// never what a call rides on.
    key: String,
    grant: Option<Arc<Grant>>,
}

impl Client<Http> {
    /// A client over HTTPS, holding an operator credential.
    pub fn new(key: impl Into<String>) -> Self {
        Client::over(Http::default(), key)
    }
}

impl<T: Transport> Client<T> {
    /// A client over a given transport — the shipped [`Http`], or a stub.
    pub fn over(wire: T, key: impl Into<String>) -> Self {
        Client {
            wire: Arc::new(wire),
            base: API.to_string(),
            issuer: ISSUER.to_string(),
            key: key.into(),
            grant: None,
        }
    }

    /// Where the platform answers, when it is not the public one.
    pub fn base(mut self, url: impl Into<String>) -> Self {
        self.base = url.into();
        self
    }

    /// Where IAM answers, when it is not the public issuer.
    pub fn issuer(mut self, url: impl Into<String>) -> Self {
        self.issuer = url.into();
        self
    }

    /// Act as one subject — a subject id, or the external id the operator filed
    /// the member under. The returned client shares this one's transport and
    /// credential, and reaches nothing but that subject.
    ///
    /// Spelled `r#as` at the call site because `as` is a Rust keyword. The name
    /// is `as`; the escape is syntax.
    pub fn r#as(&self, subject: impl Into<String>) -> Self {
        Client {
            wire: self.wire.clone(),
            base: self.base.clone(),
            issuer: self.issuer.clone(),
            key: self.key.clone(),
            grant: Some(Arc::new(Grant::new(subject, self.issuer.clone()))),
        }
    }

    /// Who this client acts as, or `None` when it is the operator itself.
    pub fn subject(&self) -> Option<&str> {
        self.grant.as_deref().map(Grant::subject)
    }

    /// Send one `/v1` call.
    ///
    /// A 202 is [`Outcome::Held`] carrying the approval; any other 2xx is
    /// [`Outcome::Done`] with the parsed body; a non-2xx is [`Error::Api`]. A
    /// 401 on a scoped client drops the minted token and retries ONCE, so an
    /// expired token is invisible to the caller and a truly rejected credential
    /// still surfaces.
    pub async fn call<R: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Outcome<R>, Error> {
        let url = join(&self.base, path);
        let mut reply = self.once(&url, &method, &body).await?;
        if reply.status == 401 {
            if let Some(grant) = &self.grant {
                grant.invalidate();
                reply = self.once(&url, &method, &body).await?;
            }
        }
        if reply.status == 202 {
            // THE BODY DECIDES, NOT THE CODE. A 202 alone does not mean a person
            // was asked: a dozen platform operations answer 202 for "accepted,
            // working on it" and carry a real schema — a deployment, a preview, a
            // build. Reading the code alone turns each of those into an approval
            // nobody is waiting on. The server omits an empty field, so a partial
            // body still reads.
            let held: Approval = serde_json::from_value(reply.body.clone()).unwrap_or_default();
            if held.status == "held" {
                return Ok(Outcome::Held(held));
            }
        }
        let status = reply.status;
        let value = reply.ok()?;
        serde_json::from_value(value)
            .map(Outcome::Done)
            .map_err(|source| Error::Decode { status, source })
    }

    /// The wire under this client, for a caller addressing a host that is not
    /// the platform (a local probe, a hosted plane on its own origin).
    pub fn wire(&self) -> &T {
        &self.wire
    }

    async fn once(&self, url: &str, method: &Method, body: &Option<Value>) -> Result<Reply, Error> {
        let token = match &self.grant {
            Some(grant) => grant.token(&*self.wire, &self.key).await?,
            None => self.key.clone(),
        };
        let mut request = Request::new(method.clone(), url).token(&token);
        if let Some(body) = body {
            request = request.body(body.clone());
        }
        self.wire.send(request).await
    }
}

impl<T: Transport> Clone for Client<T> {
    fn clone(&self) -> Self {
        Client {
            wire: self.wire.clone(),
            base: self.base.clone(),
            issuer: self.issuer.clone(),
            key: self.key.clone(),
            grant: self.grant.clone(),
        }
    }
}

/// Join an origin with a `/v1/...` path. Never adds an `/api/` prefix.
fn join(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    if path.starts_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    }
}
