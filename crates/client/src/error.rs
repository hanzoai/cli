//! Every failure this client can surface, as ONE typed enum.
//!
//! A non-2xx is never a string and never a loose map: it is [`Error::Api`],
//! carrying the server's own status and body, so a caller can branch on the
//! number and still show the server's words.

use serde_json::Value;

use crate::result::Approval;

/// A failure from the platform, the wire, or a body that did not fit.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The server answered, and refused. Status and body are the SERVER's,
    /// verbatim — this client never invents a message.
    #[error("{status}: {}", message(.body))]
    Api { status: u16, body: Value },

    /// The value of a call that was held for a human decision was read anyway.
    /// Returned by [`crate::Outcome::done`], never sent by a server.
    #[error("held for approval {}: {}", .0.id, .0.clause)]
    Held(Approval),

    /// IAM answered the mint but named no token, so no call can be made as
    /// this subject.
    #[error("iam issued no token to act as {subject}")]
    Auth { subject: String },

    /// The request never reached a server, or the target could not be addressed.
    #[error("{url}: {cause}")]
    Wire { url: String, cause: String },

    /// A 2xx body that did not fit the shape the caller asked for.
    #[error("decoding the {status} body: {source}")]
    Decode {
        status: u16,
        #[source]
        source: serde_json::Error,
    },
}

/// The human sentence inside an error body: a `/v1` envelope's `msg`, a bare
/// `error`, or an `error.message`. Falls back to the body itself, so nothing the
/// server said is ever swallowed.
fn message(body: &Value) -> String {
    fn pick(v: Option<&Value>) -> Option<&str> {
        v.and_then(Value::as_str).filter(|s| !s.is_empty())
    }
    if let Some(obj) = body.as_object() {
        if let Some(m) = pick(obj.get("msg")) {
            return m.to_string();
        }
        match obj.get("error") {
            Some(Value::String(s)) if !s.is_empty() => return s.clone(),
            Some(Value::Object(e)) => {
                if let Some(m) = pick(e.get("message")) {
                    return m.to_string();
                }
            }
            _ => {}
        }
    }
    match body {
        Value::Null => String::new(),
        Value::String(s) => s.trim().to_string(),
        v => v.to_string(),
    }
}
