//! The act grant: an operator credential, minted down to ONE subject.
//!
//! IAM issues the token from the org credential the caller already holds plus a
//! grant naming the subject. It answers on IAM's OWN host and the path carries
//! the `iam` segment — the platform API does not serve this. The target rides in
//! the `id` query, never in a body, because IAM reads the grant off the key
//! itself.
//!
//! The token is reused until it nears expiry and dropped on a 401, so a caller
//! never manages one and a request never rides an about-to-die token.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::error::Error;
use crate::http::{Method, Request, Transport};

/// IAM's canonical mint. The platform API answers this with a 404.
pub const ISSUE: &str = "/v1/iam/tokens/issue";

/// Where IAM answers. Override for a private estate.
pub const ISSUER: &str = "https://hanzo.id";

/// Re-mint this far ahead of expiry.
const SKEW: Duration = Duration::from_secs(30);

/// The lifetime assumed when IAM states none.
const TTL: Duration = Duration::from_secs(300);

/// IAM answers camelCase.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Issued {
    access_token: Option<String>,
    /// Seconds from now.
    expires_in: Option<u64>,
}

struct Minted {
    token: String,
    expires: Instant,
}

/// Mints and holds the token that lets one credential act as one subject.
pub struct Grant {
    subject: String,
    issuer: String,
    held: Mutex<Option<Minted>>,
}

impl Grant {
    pub fn new(subject: impl Into<String>, issuer: impl Into<String>) -> Self {
        Grant { subject: subject.into(), issuer: issuer.into(), held: Mutex::new(None) }
    }

    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// The live token for this subject, minted only when there is not already a
    /// good one.
    pub async fn token<T: Transport>(&self, wire: &T, key: &str) -> Result<String, Error> {
        match self.cached() {
            Some(token) => Ok(token),
            None => self.mint(wire, key).await,
        }
    }

    /// Drop the held token so the next call re-mints. Called once after a 401.
    pub fn invalidate(&self) {
        *self.held.lock().unwrap() = None;
    }

    fn cached(&self) -> Option<String> {
        let held = self.held.lock().unwrap();
        let minted = held.as_ref()?;
        (minted.expires.saturating_duration_since(Instant::now()) > SKEW)
            .then(|| minted.token.clone())
    }

    async fn mint<T: Transport>(&self, wire: &T, key: &str) -> Result<String, Error> {
        let url = url(&self.issuer, &self.subject)?;
        let reply = wire.send(Request::new(Method::POST, url).token(key)).await?;
        let status = reply.status;
        let issued: Issued = serde_json::from_value(reply.ok()?)
            .map_err(|source| Error::Decode { status, source })?;
        let token = issued
            .access_token
            .filter(|t| !t.is_empty())
            .ok_or_else(|| Error::Auth { subject: self.subject.clone() })?;
        let life = issued.expires_in.map_or(TTL, Duration::from_secs);
        *self.held.lock().unwrap() =
            Some(Minted { token: token.clone(), expires: Instant::now() + life });
        Ok(token)
    }
}

/// The mint address. The subject goes through the URL crate's own encoder, so a
/// subject holding `&` or a space cannot forge a second parameter.
fn url(issuer: &str, subject: &str) -> Result<String, Error> {
    let base = format!("{}{ISSUE}", issuer.trim_end_matches('/'));
    let mut url = reqwest::Url::parse(&base)
        .map_err(|e| Error::Wire { url: base.clone(), cause: e.to_string() })?;
    url.query_pairs_mut().append_pair("id", subject);
    Ok(url.to_string())
}
