//! The outcome of a call the platform may stop for a human.
//!
//! An approval-gated call answers HTTP 202 with the approval verbatim, and 202
//! is a success status — so a client that only asks "did it succeed?" reads a
//! queued call as a done one. Here that mistake cannot be made: the two states
//! are ARMS OF ONE ENUM that share no value, so there is no way to reach the
//! value without first naming which arm you are in. Ignoring the hold does not
//! misbehave at runtime; it fails to compile.

use serde::Deserialize;

use crate::error::Error;

/// A call the platform stopped for a human decision.
///
/// `id` is the handle to poll or resolve by, `clause` names the policy clause
/// that held the call, and `reason` says why. `GET /v1/approvals/{id}` answers
/// these same field names, so there is one shape to learn. The server omits an
/// empty field, and each one reads as `""`.
///
/// The wire also carries `status: "held"`. It is not a field here because the
/// [`Outcome::Held`] arm IS that status — one fact, one place, and a held
/// approval that claims to be done is not representable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Approval {
    /// Always `held`. It is carried rather than assumed because the same field
    /// names come back from `GET /v1/approvals/{id}`, where the status is how a
    /// reader learns the approval was since answered — one shape, read twice.
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub clause: String,
    #[serde(default)]
    pub reason: String,
}

/// Done with a value, or held on an approval. Never both, never neither.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome<T> {
    Done(T),
    Held(Approval),
}

impl<T> Outcome<T> {
    /// The value, or [`Error::Held`] carrying the approval that gates it — for
    /// a caller that has no branch to offer a held call.
    pub fn done(self) -> Result<T, Error> {
        match self {
            Outcome::Done(value) => Ok(value),
            Outcome::Held(approval) => Err(Error::Held(approval)),
        }
    }

    /// The approval, when this call is waiting on one.
    pub fn held(&self) -> Option<&Approval> {
        match self {
            Outcome::Held(approval) => Some(approval),
            Outcome::Done(_) => None,
        }
    }
}
